//! 32-bit TWAIN Scanner Sidecar
//!
//! This is a small 32-bit executable that loads the 32-bit TWAINDSM.dll and
//! communicates with the 64-bit parent process via stdin/stdout JSON-line protocol.
//!
//! It exists because 64-bit processes cannot load 32-bit DLLs, and many enterprise
//! scanners only ship 32-bit TWAIN drivers.
//!
//! Protocol:
//! - Parent writes JSON commands to stdin (one per line)
//! - Sidecar writes JSON responses to stdout (one per line)
//! - Sidecar writes log/debug info to stderr

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

mod logging;
mod twain_ffi;

#[cfg(windows)]
use twain_ffi::*;

// IPC Protocol Messages

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum SidecarCommand {
    ListScanners,
    Scan {
        scanner_name: String,
        resolution: u32,
        color_mode: String,
        duplex: bool,
        use_adf: bool,
        show_ui: bool,
    },
    Cancel,
    Shutdown,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)] // Variants constructed only on Windows via cfg(windows) scan handlers
enum SidecarResponse {
    ScannerList {
        scanners: Vec<ScannerEntry>,
    },
    ScanProgress {
        page: u32,
        status: String,
    },
    ScanPage {
        page: u32,
        width: u32,
        height: u32,
        bits_per_pixel: u16,
        data: String, // base64-encoded
    },
    ScanComplete {
        total_pages: u32,
    },
    Error {
        message: String,
    },
    Ready,
    Shutdown,
}

#[derive(Debug, Serialize)]
struct ScannerEntry {
    id: String,
    name: String,
    manufacturer: String,
}

// Main loop

fn main() {
    let log_dir = std::env::var("RSWEBTWAIN_LOG_DIR")
        .ok()
        .map(std::path::PathBuf::from);
    let _log_guard = logging::init_logging(log_dir.as_deref());

    info!("32-bit TWAIN scanner sidecar starting");

    // Signal readiness
    send_response(&SidecarResponse::Ready);

    let stdin = io::stdin();
    let reader = stdin.lock();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to read from stdin: {}", e);
                break;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        debug!("Received command: {}", line);

        let command: SidecarCommand = match serde_json::from_str(&line) {
            Ok(cmd) => cmd,
            Err(e) => {
                warn!("Invalid command: {}", e);
                send_response(&SidecarResponse::Error {
                    message: format!("Invalid command: {}", e),
                });
                continue;
            }
        };

        match command {
            SidecarCommand::ListScanners => handle_list_scanners(),
            SidecarCommand::Scan {
                scanner_name,
                resolution,
                color_mode,
                duplex,
                use_adf,
                show_ui,
            } => handle_scan(scanner_name, resolution, color_mode, duplex, use_adf, show_ui),
            SidecarCommand::Cancel => {
                // Cancel outside of a scan is a no-op (cancel during scan is
                // handled inline via stdin_has_data() polling in handle_scan)
                debug!("Cancel received outside of scan — ignored");
            }
            SidecarCommand::Shutdown => {
                info!("Shutdown requested");
                send_response(&SidecarResponse::Shutdown);
                break;
            }
        }
    }

    info!("32-bit TWAIN scanner sidecar exiting");
}

fn send_response(response: &SidecarResponse) {
    if let Ok(json) = serde_json::to_string(response) {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let _ = writeln!(out, "{}", json);
        let _ = out.flush();
    }
}

// Command handlers (32-bit TWAIN operations)

#[cfg(windows)]
fn handle_list_scanners() {
    use std::ptr;

    // Load 32-bit TWAINDSM.dll
    let library = match unsafe { libloading::Library::new("TWAINDSM.dll") } {
        Ok(lib) => lib,
        Err(e) => {
            send_response(&SidecarResponse::Error {
                message: format!("Failed to load 32-bit TWAINDSM.dll: {}", e),
            });
            return;
        }
    };

    let entry: DSM_Entry = match unsafe { library.get::<DSM_Entry>(b"DSM_Entry") } {
        Ok(e) => *e,
        Err(e) => {
            send_response(&SidecarResponse::Error {
                message: format!("DSM_Entry not found: {}", e),
            });
            return;
        }
    };

    let mut app_id = TW_IDENTITY::default();
    app_id.Version.MajorNum = 0;
    app_id.Version.MinorNum = 1;
    app_id.Version.Language = TWLG_ENGLISH_USA;
    app_id.Version.Country = TWCY_USA;
    app_id.Version.Info = str_to_tw_str32("0.1.0");
    app_id.ProtocolMajor = TWON_PROTOCOLMAJOR;
    app_id.ProtocolMinor = TWON_PROTOCOLMINOR;
    app_id.SupportedGroups = DG_CONTROL | DG_IMAGE;
    app_id.Manufacturer = str_to_tw_str32("RSWebTWAIN");
    app_id.ProductFamily = str_to_tw_str32("Scanner");
    app_id.ProductName = str_to_tw_str32("RSWebTWAIN32");

    // Create a hidden message window for TWAIN
    let hwnd = match create_message_window() {
        Ok(h) => h,
        Err(e) => {
            send_response(&SidecarResponse::Error {
                message: format!("Failed to create hidden window: {}", e),
            });
            return;
        }
    };

    // Open DSM
    let rc = unsafe {
        entry(
            &mut app_id,
            ptr::null_mut(),
            DG_CONTROL,
            DAT_PARENT,
            MSG_OPENDSM,
            hwnd as TW_MEMREF,
        )
    };

    if rc != TWRC_SUCCESS {
        send_response(&SidecarResponse::Error {
            message: format!("Failed to open DSM: rc={}", rc),
        });
        return;
    }

    // Enumerate sources
    let mut scanners = Vec::new();
    let mut identity = TW_IDENTITY::default();

    let rc = unsafe {
        entry(
            &mut app_id,
            ptr::null_mut(),
            DG_CONTROL,
            DAT_IDENTITY,
            MSG_GETFIRST,
            &mut identity as *mut TW_IDENTITY as TW_MEMREF,
        )
    };

    if rc == TWRC_SUCCESS {
        scanners.push(ScannerEntry {
            id: identity.Id.to_string(),
            name: tw_str32_to_string(&identity.ProductName),
            manufacturer: tw_str32_to_string(&identity.Manufacturer),
        });

        loop {
            identity = TW_IDENTITY::default();
            let rc = unsafe {
                entry(
                    &mut app_id,
                    ptr::null_mut(),
                    DG_CONTROL,
                    DAT_IDENTITY,
                    MSG_GETNEXT,
                    &mut identity as *mut TW_IDENTITY as TW_MEMREF,
                )
            };

            if rc != TWRC_SUCCESS {
                break;
            }

            scanners.push(ScannerEntry {
                id: identity.Id.to_string(),
                name: tw_str32_to_string(&identity.ProductName),
                manufacturer: tw_str32_to_string(&identity.Manufacturer),
            });
        }
    }

    // Close DSM
    let _ = unsafe {
        entry(
            &mut app_id,
            ptr::null_mut(),
            DG_CONTROL,
            DAT_PARENT,
            MSG_CLOSEDSM,
            hwnd as TW_MEMREF,
        )
    };

    send_response(&SidecarResponse::ScannerList { scanners });
}

#[cfg(not(windows))]
fn handle_list_scanners() {
    send_response(&SidecarResponse::Error {
        message: "32-bit TWAIN sidecar only runs on Windows".to_string(),
    });
}

#[cfg(windows)]
fn handle_scan(
    scanner_name: String,
    resolution: u32,
    color_mode: String,
    duplex: bool,
    use_adf: bool,
    show_ui: bool,
) {
    use std::ptr;
    use base64::Engine;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };

    info!(
        "Starting scan: scanner={}, res={}, color={}, duplex={}, adf={}, ui={}",
        scanner_name, resolution, color_mode, duplex, use_adf, show_ui
    );

    // --- Load TWAINDSM.dll ---
    let library = match unsafe { libloading::Library::new("TWAINDSM.dll") } {
        Ok(lib) => lib,
        Err(e) => {
            send_response(&SidecarResponse::Error {
                message: format!("Failed to load 32-bit TWAINDSM.dll: {}", e),
            });
            return;
        }
    };

    let entry: DSM_Entry = match unsafe { library.get::<DSM_Entry>(b"DSM_Entry") } {
        Ok(e) => *e,
        Err(e) => {
            send_response(&SidecarResponse::Error {
                message: format!("DSM_Entry not found: {}", e),
            });
            return;
        }
    };

    // --- App identity ---
    let mut app_id = TW_IDENTITY::default();
    app_id.Version.MajorNum = 0;
    app_id.Version.MinorNum = 1;
    app_id.Version.Language = TWLG_ENGLISH_USA;
    app_id.Version.Country = TWCY_USA;
    app_id.Version.Info = str_to_tw_str32("0.1.0");
    app_id.ProtocolMajor = TWON_PROTOCOLMAJOR;
    app_id.ProtocolMinor = TWON_PROTOCOLMINOR;
    app_id.SupportedGroups = DG_CONTROL | DG_IMAGE;
    app_id.Manufacturer = str_to_tw_str32("RSWebTWAIN");
    app_id.ProductFamily = str_to_tw_str32("Scanner");
    app_id.ProductName = str_to_tw_str32("RSWebTWAIN32");

    // --- Hidden message window ---
    let hwnd = match create_message_window() {
        Ok(h) => h,
        Err(e) => {
            send_response(&SidecarResponse::Error {
                message: format!("Failed to create hidden window: {}", e),
            });
            return;
        }
    };

    // --- Open DSM ---
    let rc = unsafe {
        entry(
            &mut app_id,
            ptr::null_mut(),
            DG_CONTROL,
            DAT_PARENT,
            MSG_OPENDSM,
            hwnd as TW_MEMREF,
        )
    };
    if rc != TWRC_SUCCESS {
        send_response(&SidecarResponse::Error {
            message: format!("Failed to open DSM: rc={}", rc),
        });
        return;
    }

    // --- Find and open source by name ---
    let mut source_id = TW_IDENTITY::default();
    let mut found = false;

    let mut identity = TW_IDENTITY::default();
    let rc = unsafe {
        entry(
            &mut app_id,
            ptr::null_mut(),
            DG_CONTROL,
            DAT_IDENTITY,
            MSG_GETFIRST,
            &mut identity as *mut TW_IDENTITY as TW_MEMREF,
        )
    };
    if rc == TWRC_SUCCESS {
        if tw_str32_to_string(&identity.ProductName) == scanner_name {
            source_id = identity;
            found = true;
        } else {
            loop {
                identity = TW_IDENTITY::default();
                let rc = unsafe {
                    entry(
                        &mut app_id,
                        ptr::null_mut(),
                        DG_CONTROL,
                        DAT_IDENTITY,
                        MSG_GETNEXT,
                        &mut identity as *mut TW_IDENTITY as TW_MEMREF,
                    )
                };
                if rc != TWRC_SUCCESS {
                    break;
                }
                if tw_str32_to_string(&identity.ProductName) == scanner_name {
                    source_id = identity;
                    found = true;
                    break;
                }
            }
        }
    }

    if !found {
        let _ = unsafe {
            entry(
                &mut app_id,
                ptr::null_mut(),
                DG_CONTROL,
                DAT_PARENT,
                MSG_CLOSEDSM,
                hwnd as TW_MEMREF,
            )
        };
        send_response(&SidecarResponse::Error {
            message: format!("Scanner not found: {}", scanner_name),
        });
        return;
    }

    // Open the source
    let rc = unsafe {
        entry(
            &mut app_id,
            ptr::null_mut(),
            DG_CONTROL,
            DAT_IDENTITY,
            MSG_OPENDS,
            &mut source_id as *mut TW_IDENTITY as TW_MEMREF,
        )
    };
    if rc != TWRC_SUCCESS {
        let _ = unsafe {
            entry(
                &mut app_id,
                ptr::null_mut(),
                DG_CONTROL,
                DAT_PARENT,
                MSG_CLOSEDSM,
                hwnd as TW_MEMREF,
            )
        };
        send_response(&SidecarResponse::Error {
            message: format!("Failed to open scanner: rc={}", rc),
        });
        return;
    }

    info!("Source opened: {}", scanner_name);

    // --- Configure capabilities ---
    let pixel_type = match color_mode.as_str() {
        "color" => TWPT_RGB,
        "grayscale" => TWPT_GRAY,
        "bw" => TWPT_BW,
        _ => TWPT_RGB,
    };

    set_capability_u16(&mut app_id, &mut source_id, entry, ICAP_PIXELTYPE, pixel_type);
    set_capability_fix32(&mut app_id, &mut source_id, entry, ICAP_XRESOLUTION, resolution as f32);
    set_capability_fix32(&mut app_id, &mut source_id, entry, ICAP_YRESOLUTION, resolution as f32);
    set_capability_u16(&mut app_id, &mut source_id, entry, ICAP_XFERMECH, TWSX_MEMORY);

    if use_adf {
        set_capability_bool(&mut app_id, &mut source_id, entry, CAP_FEEDERENABLED, true);
        set_capability_bool(&mut app_id, &mut source_id, entry, CAP_AUTOFEED, true);
        set_capability_i16(&mut app_id, &mut source_id, entry, CAP_XFERCOUNT, -1);
    } else {
        set_capability_i16(&mut app_id, &mut source_id, entry, CAP_XFERCOUNT, 1);
    }

    if duplex {
        set_capability_bool(&mut app_id, &mut source_id, entry, CAP_DUPLEXENABLED, true);
    }

    // --- Enable source ---
    let mut ui = TW_USERINTERFACE {
        ShowUI: if show_ui { 1 } else { 0 },
        ModalUI: if show_ui { 1 } else { 0 },
        hParent: hwnd as TW_HANDLE,
    };
    let rc = unsafe {
        entry(
            &mut app_id,
            &mut source_id,
            DG_CONTROL,
            DAT_USERINTERFACE,
            MSG_ENABLEDS,
            &mut ui as *mut TW_USERINTERFACE as TW_MEMREF,
        )
    };
    if rc != TWRC_SUCCESS {
        cleanup_close_source_and_dsm(&mut app_id, &mut source_id, entry, hwnd);
        send_response(&SidecarResponse::Error {
            message: format!("Failed to enable scanner: rc={}", rc),
        });
        return;
    }

    info!("Source enabled, waiting for transfer");

    // --- Message pump: wait for MSG_XFERREADY ---
    let mut transfer_ready = false;

    loop {
        // Check for cancel from stdin (non-blocking)
        if stdin_has_data() {
            let mut line = String::new();
            if io::stdin().read_line(&mut line).is_ok() && !line.is_empty() {
                if let Ok(cmd) = serde_json::from_str::<SidecarCommand>(line.trim()) {
                    if matches!(cmd, SidecarCommand::Cancel) {
                        info!("Cancel received from parent during message pump");
                        break;
                    }
                }
            }
        }

        let mut win_msg = MSG::default();
        let has_msg = unsafe { PeekMessageW(&mut win_msg, None, 0, 0, PM_REMOVE) };

        if !has_msg.as_bool() {
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }

        // Pass to TWAIN
        let mut tw_event = TW_EVENT {
            pEvent: &mut win_msg as *mut MSG as TW_MEMREF,
            TWMessage: 0,
        };
        let rc = unsafe {
            entry(
                &mut app_id,
                &mut source_id,
                DG_CONTROL,
                DAT_EVENT,
                MSG_PROCESSEVENT,
                &mut tw_event as *mut TW_EVENT as TW_MEMREF,
            )
        };

        if rc == TWRC_DSEVENT {
            match tw_event.TWMessage {
                MSG_XFERREADY => {
                    info!("Scanner signals transfer ready");
                    transfer_ready = true;
                    break;
                }
                MSG_CLOSEDSREQ | MSG_CLOSEDSOK => {
                    info!("Scanner requests close");
                    break;
                }
                _ => {}
            }
        } else {
            unsafe {
                let _ = TranslateMessage(&win_msg);
                DispatchMessageW(&win_msg);
            }
        }
    }

    // --- Transfer pages ---
    let mut page_num = 0u32;

    if transfer_ready {
        loop {
            page_num += 1;

            send_response(&SidecarResponse::ScanProgress {
                page: page_num,
                status: "scanning".to_string(),
            });

            // Get image info
            let mut image_info = TW_IMAGEINFO::default();
            let rc = unsafe {
                entry(
                    &mut app_id,
                    &mut source_id,
                    DG_IMAGE,
                    DAT_IMAGEINFO,
                    MSG_GET,
                    &mut image_info as *mut TW_IMAGEINFO as TW_MEMREF,
                )
            };
            if rc != TWRC_SUCCESS {
                error!("Failed to get image info: rc={}", rc);
                break;
            }

            // Get memory transfer setup
            let mut setup = TW_SETUPMEMXFER::default();
            let rc = unsafe {
                entry(
                    &mut app_id,
                    &mut source_id,
                    DG_CONTROL,
                    DAT_SETUPMEMXFER,
                    MSG_GET,
                    &mut setup as *mut TW_SETUPMEMXFER as TW_MEMREF,
                )
            };
            if rc != TWRC_SUCCESS {
                error!("Failed to get setup mem xfer: rc={}", rc);
                break;
            }

            let buf_size = setup.Preferred as usize;
            let mut buffer = vec![0u8; buf_size];
            let mut image_data = Vec::new();

            // Memory transfer loop for this page
            loop {
                let mut mem_xfer = TW_IMAGEMEMXFER {
                    Memory: TW_MEMORY {
                        Flags: TWMF_APPOWNS | TWMF_POINTER,
                        Length: buf_size as TW_UINT32,
                        TheMem: buffer.as_mut_ptr() as TW_MEMREF,
                    },
                    ..Default::default()
                };

                let rc = unsafe {
                    entry(
                        &mut app_id,
                        &mut source_id,
                        DG_IMAGE,
                        DAT_IMAGEMEMXFER,
                        MSG_GET,
                        &mut mem_xfer as *mut TW_IMAGEMEMXFER as TW_MEMREF,
                    )
                };

                if rc == TWRC_SUCCESS || rc == TWRC_XFERDONE {
                    let bytes_written = mem_xfer.BytesWritten as usize;
                    image_data.extend_from_slice(&buffer[..bytes_written]);
                    if rc == TWRC_XFERDONE {
                        break;
                    }
                } else {
                    error!("Memory transfer failed: rc={}", rc);
                    break;
                }
            }

            // Send page with base64-encoded raw bitmap
            let encoded = base64::engine::general_purpose::STANDARD.encode(&image_data);
            send_response(&SidecarResponse::ScanPage {
                page: page_num,
                width: image_info.ImageWidth as u32,
                height: image_info.ImageLength as u32,
                bits_per_pixel: image_info.BitsPerPixel as u16,
                data: encoded,
            });

            // End transfer, check pending
            let mut pending = TW_PENDINGXFERS::default();
            let rc = unsafe {
                entry(
                    &mut app_id,
                    &mut source_id,
                    DG_CONTROL,
                    DAT_PENDINGXFERS,
                    MSG_ENDXFER,
                    &mut pending as *mut TW_PENDINGXFERS as TW_MEMREF,
                )
            };

            if rc != TWRC_SUCCESS || pending.Count == 0 {
                break;
            }

            // Check for cancel between pages
            if stdin_has_data() {
                let mut line = String::new();
                if io::stdin().read_line(&mut line).is_ok() && !line.is_empty() {
                    if let Ok(cmd) = serde_json::from_str::<SidecarCommand>(line.trim()) {
                        if matches!(cmd, SidecarCommand::Cancel) {
                            info!("Cancel received between pages");
                            let mut pending = TW_PENDINGXFERS::default();
                            let _ = unsafe {
                                entry(
                                    &mut app_id,
                                    &mut source_id,
                                    DG_CONTROL,
                                    DAT_PENDINGXFERS,
                                    MSG_RESET,
                                    &mut pending as *mut TW_PENDINGXFERS as TW_MEMREF,
                                )
                            };
                            break;
                        }
                    }
                }
            }
        }
    }

    // --- Cleanup ---
    cleanup_disable_close(&mut app_id, &mut source_id, entry, hwnd);
    send_response(&SidecarResponse::ScanComplete {
        total_pages: page_num,
    });
    info!("Scan complete: {} pages", page_num);
}

#[cfg(not(windows))]
fn handle_scan(
    _scanner_name: String,
    _resolution: u32,
    _color_mode: String,
    _duplex: bool,
    _use_adf: bool,
    _show_ui: bool,
) {
    send_response(&SidecarResponse::Error {
        message: "32-bit TWAIN sidecar only runs on Windows".to_string(),
    });
}

// TWAIN capability helpers

#[cfg(windows)]
fn set_capability_u16(
    app_id: &mut TW_IDENTITY,
    source_id: &mut TW_IDENTITY,
    entry: DSM_Entry,
    cap: TW_UINT16,
    value: TW_UINT16,
) {
    let one_value = TW_ONEVALUE {
        ItemType: 4, // TWTY_UINT16
        Item: value as TW_UINT32,
    };
    let mut capability = TW_CAPABILITY {
        Cap: cap,
        ConType: TWON_ONEVALUE,
        hContainer: Box::into_raw(Box::new(one_value)) as TW_HANDLE,
    };
    let rc = unsafe {
        entry(
            app_id,
            source_id,
            DG_CONTROL,
            DAT_CAPABILITY,
            MSG_SET,
            &mut capability as *mut TW_CAPABILITY as TW_MEMREF,
        )
    };
    unsafe {
        let _ = Box::from_raw(capability.hContainer as *mut TW_ONEVALUE);
    }
    if rc != TWRC_SUCCESS {
        warn!(
            "Failed to set capability 0x{:04X} to {}: rc={}",
            cap, value, rc
        );
    }
}

#[cfg(windows)]
fn set_capability_fix32(
    app_id: &mut TW_IDENTITY,
    source_id: &mut TW_IDENTITY,
    entry: DSM_Entry,
    cap: TW_UINT16,
    value: f32,
) {
    let fix32 = TW_FIX32::from_f32(value);
    let item_value = unsafe { std::mem::transmute::<TW_FIX32, u32>(fix32) };
    let one_value = TW_ONEVALUE {
        ItemType: 7, // TWTY_FIX32
        Item: item_value,
    };
    let mut capability = TW_CAPABILITY {
        Cap: cap,
        ConType: TWON_ONEVALUE,
        hContainer: Box::into_raw(Box::new(one_value)) as TW_HANDLE,
    };
    let rc = unsafe {
        entry(
            app_id,
            source_id,
            DG_CONTROL,
            DAT_CAPABILITY,
            MSG_SET,
            &mut capability as *mut TW_CAPABILITY as TW_MEMREF,
        )
    };
    unsafe {
        let _ = Box::from_raw(capability.hContainer as *mut TW_ONEVALUE);
    }
    if rc != TWRC_SUCCESS {
        warn!(
            "Failed to set capability 0x{:04X} to {}: rc={}",
            cap, value, rc
        );
    }
}

#[cfg(windows)]
fn set_capability_bool(
    app_id: &mut TW_IDENTITY,
    source_id: &mut TW_IDENTITY,
    entry: DSM_Entry,
    cap: TW_UINT16,
    value: bool,
) {
    set_capability_u16(app_id, source_id, entry, cap, if value { 1 } else { 0 });
}

#[cfg(windows)]
fn set_capability_i16(
    app_id: &mut TW_IDENTITY,
    source_id: &mut TW_IDENTITY,
    entry: DSM_Entry,
    cap: TW_UINT16,
    value: i16,
) {
    let one_value = TW_ONEVALUE {
        ItemType: 3, // TWTY_INT16
        Item: value as u16 as TW_UINT32,
    };
    let mut capability = TW_CAPABILITY {
        Cap: cap,
        ConType: TWON_ONEVALUE,
        hContainer: Box::into_raw(Box::new(one_value)) as TW_HANDLE,
    };
    let rc = unsafe {
        entry(
            app_id,
            source_id,
            DG_CONTROL,
            DAT_CAPABILITY,
            MSG_SET,
            &mut capability as *mut TW_CAPABILITY as TW_MEMREF,
        )
    };
    unsafe {
        let _ = Box::from_raw(capability.hContainer as *mut TW_ONEVALUE);
    }
    if rc != TWRC_SUCCESS {
        warn!(
            "Failed to set capability 0x{:04X} to {}: rc={}",
            cap, value, rc
        );
    }
}

// Cleanup helpers

/// Disable source, close source, close DSM
#[cfg(windows)]
fn cleanup_disable_close(
    app_id: &mut TW_IDENTITY,
    source_id: &mut TW_IDENTITY,
    entry: DSM_Entry,
    hwnd: isize,
) {
    use std::ptr;

    let mut ui = TW_USERINTERFACE {
        hParent: hwnd as TW_HANDLE,
        ..Default::default()
    };
    let _ = unsafe {
        entry(
            app_id,
            source_id,
            DG_CONTROL,
            DAT_USERINTERFACE,
            MSG_DISABLEDS,
            &mut ui as *mut TW_USERINTERFACE as TW_MEMREF,
        )
    };
    let _ = unsafe {
        entry(
            app_id,
            ptr::null_mut(),
            DG_CONTROL,
            DAT_IDENTITY,
            MSG_CLOSEDS,
            source_id as *mut TW_IDENTITY as TW_MEMREF,
        )
    };
    let _ = unsafe {
        entry(
            app_id,
            ptr::null_mut(),
            DG_CONTROL,
            DAT_PARENT,
            MSG_CLOSEDSM,
            hwnd as TW_MEMREF,
        )
    };
}

/// Close source + close DSM (when source was never enabled)
#[cfg(windows)]
fn cleanup_close_source_and_dsm(
    app_id: &mut TW_IDENTITY,
    source_id: &mut TW_IDENTITY,
    entry: DSM_Entry,
    hwnd: isize,
) {
    use std::ptr;

    let _ = unsafe {
        entry(
            app_id,
            ptr::null_mut(),
            DG_CONTROL,
            DAT_IDENTITY,
            MSG_CLOSEDS,
            source_id as *mut TW_IDENTITY as TW_MEMREF,
        )
    };
    let _ = unsafe {
        entry(
            app_id,
            ptr::null_mut(),
            DG_CONTROL,
            DAT_PARENT,
            MSG_CLOSEDSM,
            hwnd as TW_MEMREF,
        )
    };
}

/// Non-blocking check whether stdin has data available (for cancel detection).
#[cfg(windows)]
fn stdin_has_data() -> bool {
    use windows::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};
    use windows::Win32::System::Pipes::PeekNamedPipe;

    unsafe {
        let handle = match GetStdHandle(STD_INPUT_HANDLE) {
            Ok(h) => h,
            Err(_) => return false,
        };
        let mut available: u32 = 0;
        PeekNamedPipe(handle, None, 0, None, Some(&mut available), None).is_ok()
            && available > 0
    }
}

// Hidden message window for TWAIN

#[cfg(windows)]
fn create_message_window() -> Result<isize, String> {
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, RegisterClassW, HWND_MESSAGE, WNDCLASSW, WS_OVERLAPPED,
    };
    use windows::core::w;

    // Thin extern "system" shim: WNDCLASSW.lpfnWndProc is a raw fn pointer,
    // but DefWindowProcW in windows-rs is a generic Rust fn — can't use directly.
    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let hinstance: HINSTANCE = hmodule.into();

        let class_name = w!("RSWebTWAIN32Hidden");

        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            lpszClassName: class_name,
            ..Default::default()
        };

        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            Default::default(),
            class_name,
            w!("RSWebTWAIN32 TWAIN"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            hinstance,
            None,
        )
        .map_err(|e| e.to_string())?;

        Ok(hwnd.0 as isize)
    }
}
