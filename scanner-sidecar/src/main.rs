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

mod twain_ffi;

use twain_ffi::*;

// ---------------------------------------------------------------------------
// IPC Protocol Messages
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

fn main() {
    // Log to stderr so stdout is reserved for the IPC protocol
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter("scanner_sidecar=debug")
        .init();

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
                warn!("Cancel not yet implemented in sidecar");
                send_response(&SidecarResponse::Error {
                    message: "Cancel not implemented".to_string(),
                });
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

// ---------------------------------------------------------------------------
// Command handlers (32-bit TWAIN operations)
// ---------------------------------------------------------------------------

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
    app_id.Manufacturer = str_to_tw_str32("ScanAgent");
    app_id.ProductFamily = str_to_tw_str32("Scanner");
    app_id.ProductName = str_to_tw_str32("ScanAgent32");

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
    // TODO: Implement full scanning via 32-bit TWAIN
    // This follows the same pattern as the native scanner but runs in-process
    // in the 32-bit sidecar. Pages are sent as base64-encoded data via stdout.
    send_response(&SidecarResponse::Error {
        message: "32-bit scanning not yet fully implemented".to_string(),
    });
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

// ---------------------------------------------------------------------------
// Hidden message window for TWAIN
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn create_message_window() -> Result<isize, String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, RegisterClassW, HWND_MESSAGE, WNDCLASSW, WS_OVERLAPPED,
    };
    use windows::core::w;

    unsafe {
        let hinstance = GetModuleHandleW(None).map_err(|e| e.to_string())?;

        let class_name = w!("ScanAgent32Hidden");

        let wc = WNDCLASSW {
            lpfnWndProc: Some(DefWindowProcW),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };

        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            Default::default(),
            class_name,
            w!("ScanAgent32 TWAIN"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance.into()),
            None,
        )
        .map_err(|e| e.to_string())?;

        Ok(hwnd.0 as isize)
    }
}
