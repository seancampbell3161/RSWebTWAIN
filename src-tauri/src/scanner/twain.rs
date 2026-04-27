//! Safe TWAIN wrapper implementing the TWAIN state machine via Rust's typestate pattern.
//!
//! Each TWAIN state (1-7) is a separate struct. State transitions consume the current state
//! and return the next, making invalid transitions a compile-time error.
//!
//! States:
//! 1. PreSession       — nothing loaded
//! 2. DsmLoaded        — TWAINDSM.dll loaded, DSM_Entry resolved
//! 3. DsmOpened        — DSM opened with MSG_OPENDSM
//! 4. SourceOpened     — Data source opened with MSG_OPENDS
//! 5. SourceEnabled    — Source enabled (scanning UI shown or suppressed)
//! 6. TransferReady    — Source signals MSG_XFERREADY
//! 7. Transferring     — Image data being transferred

use std::ptr;

use thiserror::Error;
use tracing::{debug, error, info, warn};

use super::twain_ffi::*;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum TwainError {
    #[error("Failed to load TWAINDSM.dll: {0}")]
    DsmLoadFailed(String),

    #[error("DSM_Entry symbol not found in TWAINDSM.dll")]
    EntryPointNotFound,

    #[error("TWAIN operation failed: DG=0x{dg:04X} DAT=0x{dat:04X} MSG=0x{msg:04X} RC={rc} CC={cc}")]
    OperationFailed {
        dg: u32,
        dat: u16,
        msg: u16,
        rc: u16,
        cc: u16,
    },

    #[error("TWAIN operation cancelled by user")]
    Cancelled,

    #[error("No TWAIN data sources available")]
    NoSources,

    #[error("Scanner paper jam detected")]
    PaperJam,

    #[error("Scanner paper double feed detected")]
    PaperDoubleFeed,

    #[error("Capability not supported: 0x{0:04X}")]
    CapabilityNotSupported(u16),

    #[error("Invalid state transition")]
    InvalidState,

    #[error("Memory allocation failed")]
    MemoryError,

    #[error("Hidden window creation failed: {0}")]
    WindowCreationFailed(String),
}

pub type TwainResult<T> = Result<T, TwainError>;

// ---------------------------------------------------------------------------
// Scanner source information (serializable for the protocol layer)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SourceInfo {
    pub id: u32,
    pub name: String,
    pub manufacturer: String,
    pub product_family: String,
    pub version: String,
}

impl From<&TW_IDENTITY> for SourceInfo {
    fn from(id: &TW_IDENTITY) -> Self {
        Self {
            id: id.Id,
            name: tw_str32_to_string(&id.ProductName),
            manufacturer: tw_str32_to_string(&id.Manufacturer),
            product_family: tw_str32_to_string(&id.ProductFamily),
            version: format!(
                "{}.{}",
                id.Version.MajorNum, id.Version.MinorNum
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Scan options (what the Angular client sends)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScanOptions {
    #[serde(default = "default_resolution")]
    pub resolution: u32,
    #[serde(default = "default_color_mode")]
    pub color_mode: ColorMode,
    #[serde(default)]
    pub duplex: bool,
    #[serde(default)]
    pub use_adf: bool,
    #[serde(default)]
    pub show_scanner_ui: bool,
}

fn default_resolution() -> u32 {
    300
}
fn default_color_mode() -> ColorMode {
    ColorMode::Color
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            resolution: 300,
            color_mode: ColorMode::Color,
            duplex: false,
            use_adf: false,
            show_scanner_ui: false,
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    Color,
    Grayscale,
    #[serde(rename = "bw")]
    BlackWhite,
}

impl ColorMode {
    pub fn to_twain_pixel_type(self) -> TW_UINT16 {
        match self {
            ColorMode::BlackWhite => TWPT_BW,
            ColorMode::Grayscale => TWPT_GRAY,
            ColorMode::Color => TWPT_RGB,
        }
    }
}

// ---------------------------------------------------------------------------
// Scanned page data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ScannedPage {
    pub page_number: u32,
    pub width: u32,
    pub height: u32,
    pub bits_per_pixel: u16,
    pub x_resolution: f32,
    pub y_resolution: f32,
    pub data: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Internal: Shared DSM state carried through transitions
// ---------------------------------------------------------------------------

struct DsmHandle {
    #[cfg(windows)]
    _library: libloading::Library,
    entry: DSM_Entry,
    app_identity: TW_IDENTITY,
}

impl DsmHandle {
    /// Call DSM_Entry with proper error handling
    unsafe fn call(
        &mut self,
        dest: *mut TW_IDENTITY,
        dg: TW_UINT32,
        dat: TW_UINT16,
        msg: TW_UINT16,
        data: TW_MEMREF,
    ) -> TwainResult<TW_UINT16> {
        let rc = (self.entry)(
            &mut self.app_identity as *mut TW_IDENTITY,
            dest,
            dg,
            dat,
            msg,
            data,
        );
        Ok(rc)
    }

    /// Call DSM_Entry and check for success, returning condition code on failure
    unsafe fn call_checked(
        &mut self,
        dest: *mut TW_IDENTITY,
        dg: TW_UINT32,
        dat: TW_UINT16,
        msg: TW_UINT16,
        data: TW_MEMREF,
    ) -> TwainResult<()> {
        let rc = self.call(dest, dg, dat, msg, data)?;
        if rc == TWRC_SUCCESS {
            return Ok(());
        }
        if rc == TWRC_CANCEL {
            return Err(TwainError::Cancelled);
        }

        // Get condition code
        let mut status = TW_STATUS::default();
        let _ = (self.entry)(
            &mut self.app_identity as *mut TW_IDENTITY,
            dest,
            DG_CONTROL,
            DAT_STATUS,
            MSG_GET,
            &mut status as *mut TW_STATUS as TW_MEMREF,
        );

        match status.ConditionCode {
            TWCC_PAPERJAM => Err(TwainError::PaperJam),
            TWCC_PAPERDOUBLEFEED => Err(TwainError::PaperDoubleFeed),
            TWCC_CAPUNSUPPORTED => Err(TwainError::CapabilityNotSupported(dat)),
            cc => Err(TwainError::OperationFailed {
                dg,
                dat,
                msg,
                rc,
                cc,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// State 1: PreSession — nothing loaded
// ---------------------------------------------------------------------------

pub struct PreSession;

impl Default for PreSession {
    fn default() -> Self {
        Self
    }
}

impl PreSession {
    pub fn new() -> Self {
        Self
    }

    /// Load TWAINDSM.dll and resolve DSM_Entry (transition to State 2)
    #[cfg(windows)]
    pub fn load_dsm(self) -> TwainResult<DsmLoaded> {
        info!("Loading TWAINDSM.dll");

        let library = unsafe { libloading::Library::new("TWAINDSM.dll") }
            .map_err(|e| TwainError::DsmLoadFailed(e.to_string()))?;

        let entry: DSM_Entry = unsafe {
            *library
                .get::<DSM_Entry>(b"DSM_Entry")
                .map_err(|_| TwainError::EntryPointNotFound)?
        };

        let mut app_identity = TW_IDENTITY::default();
        app_identity.Version.MajorNum = 0;
        app_identity.Version.MinorNum = 1;
        app_identity.Version.Language = TWLG_ENGLISH_USA;
        app_identity.Version.Country = TWCY_USA;
        app_identity.Version.Info = str_to_tw_str32("0.1.0");
        app_identity.ProtocolMajor = TWON_PROTOCOLMAJOR;
        app_identity.ProtocolMinor = TWON_PROTOCOLMINOR;
        app_identity.SupportedGroups = DG_CONTROL | DG_IMAGE;
        app_identity.Manufacturer = str_to_tw_str32("RSWebTWAIN");
        app_identity.ProductFamily = str_to_tw_str32("Scanner");
        app_identity.ProductName = str_to_tw_str32("RSWebTWAIN");

        info!("TWAINDSM.dll loaded successfully");

        Ok(DsmLoaded {
            handle: Some(DsmHandle {
                _library: library,
                entry,
                app_identity,
            }),
        })
    }

    #[cfg(not(windows))]
    pub fn load_dsm(self) -> TwainResult<DsmLoaded> {
        Err(TwainError::DsmLoadFailed(
            "TWAIN is only supported on Windows".to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// State 2: DsmLoaded — DSM DLL loaded
// ---------------------------------------------------------------------------

pub struct DsmLoaded {
    handle: Option<DsmHandle>,
}

impl Drop for DsmLoaded {
    fn drop(&mut self) {
        if let Some(_handle) = self.handle.take() {
            debug!("DsmLoaded dropped — DSM library will unload");
            // DsmHandle drops here, unloading the library
        }
    }
}

impl DsmLoaded {
    /// Open the DSM (transition to State 3)
    ///
    /// `hwnd` is the window handle for TWAIN's message pump.
    /// Pass a hidden message-only window handle.
    pub fn open_dsm(mut self, hwnd: isize) -> TwainResult<DsmOpened> {
        info!("Opening TWAIN DSM");

        let mut handle = self.handle.take().expect("DsmLoaded: handle already consumed");

        unsafe {
            handle.call_checked(
                ptr::null_mut(),
                DG_CONTROL,
                DAT_PARENT,
                MSG_OPENDSM,
                hwnd as TW_MEMREF,
            )?;
        }

        info!("TWAIN DSM opened");
        Ok(DsmOpened {
            handle: Some(handle),
            hwnd,
        })
    }
}

// ---------------------------------------------------------------------------
// State 3: DsmOpened — DSM is open, can enumerate/open sources
// ---------------------------------------------------------------------------

pub struct DsmOpened {
    handle: Option<DsmHandle>,
    hwnd: isize,
}

impl Drop for DsmOpened {
    fn drop(&mut self) {
        if let Some(mut handle) = self.handle.take() {
            warn!("DsmOpened dropped without clean close — closing DSM");
            unsafe {
                let _ = handle.call_checked(
                    ptr::null_mut(),
                    DG_CONTROL,
                    DAT_PARENT,
                    MSG_CLOSEDSM,
                    self.hwnd as TW_MEMREF,
                );
            }
        }
    }
}

impl DsmOpened {
    /// List all available TWAIN data sources
    pub fn list_sources(&mut self) -> TwainResult<Vec<SourceInfo>> {
        let handle = self.handle.as_mut().expect("DsmOpened: no handle");
        let mut sources = Vec::new();
        let mut identity = TW_IDENTITY::default();

        // Get first source
        let rc = unsafe {
            handle.call(
                ptr::null_mut(),
                DG_CONTROL,
                DAT_IDENTITY,
                MSG_GETFIRST,
                &mut identity as *mut TW_IDENTITY as TW_MEMREF,
            )?
        };

        if rc == TWRC_SUCCESS {
            sources.push(SourceInfo::from(&identity));

            // Get remaining sources
            loop {
                identity = TW_IDENTITY::default();
                let rc = unsafe {
                    handle.call(
                        ptr::null_mut(),
                        DG_CONTROL,
                        DAT_IDENTITY,
                        MSG_GETNEXT,
                        &mut identity as *mut TW_IDENTITY as TW_MEMREF,
                    )?
                };

                if rc != TWRC_SUCCESS {
                    break;
                }
                sources.push(SourceInfo::from(&identity));
            }
        }

        debug!("Found {} TWAIN source(s)", sources.len());
        Ok(sources)
    }

    /// Get the default data source
    pub fn get_default_source(&mut self) -> TwainResult<TW_IDENTITY> {
        let handle = self.handle.as_mut().expect("DsmOpened: no handle");
        let mut identity = TW_IDENTITY::default();

        unsafe {
            handle.call_checked(
                ptr::null_mut(),
                DG_CONTROL,
                DAT_IDENTITY,
                MSG_GETDEFAULT,
                &mut identity as *mut TW_IDENTITY as TW_MEMREF,
            )?;
        }

        Ok(identity)
    }

    /// Open a specific data source by name (transition to State 4)
    pub fn open_source(mut self, source_name: &str) -> TwainResult<SourceOpened> {
        // Find the source by name
        let sources = self.list_sources()?;
        let source = sources
            .iter()
            .find(|s| s.name == source_name)
            .ok_or(TwainError::NoSources)?;

        let mut identity = TW_IDENTITY {
            Id: source.id,
            ProductName: str_to_tw_str32(&source.name),
            ..Default::default()
        };

        info!("Opening data source: {}", source.name);

        let mut handle = self.handle.take().expect("DsmOpened: handle already consumed");

        unsafe {
            handle.call_checked(
                ptr::null_mut(),
                DG_CONTROL,
                DAT_IDENTITY,
                MSG_OPENDS,
                &mut identity as *mut TW_IDENTITY as TW_MEMREF,
            )?;
        }

        info!("Data source opened: {}", source.name);

        Ok(SourceOpened {
            handle: Some(handle),
            hwnd: self.hwnd,
            source_identity: identity,
        })
    }

    /// Open the default data source (transition to State 4)
    pub fn open_default_source(mut self) -> TwainResult<SourceOpened> {
        let mut identity = self.get_default_source()?;
        let name = tw_str32_to_string(&identity.ProductName);

        info!("Opening default data source: {}", name);

        let mut handle = self.handle.take().expect("DsmOpened: handle already consumed");

        unsafe {
            handle.call_checked(
                ptr::null_mut(),
                DG_CONTROL,
                DAT_IDENTITY,
                MSG_OPENDS,
                &mut identity as *mut TW_IDENTITY as TW_MEMREF,
            )?;
        }

        info!("Default data source opened: {}", name);

        Ok(SourceOpened {
            handle: Some(handle),
            hwnd: self.hwnd,
            source_identity: identity,
        })
    }

    /// Close the DSM (transition back to State 2, consuming self)
    pub fn close_dsm(mut self) -> TwainResult<DsmLoaded> {
        info!("Closing TWAIN DSM");
        let mut handle = self.handle.take().expect("DsmOpened: handle already consumed");
        unsafe {
            handle.call_checked(
                ptr::null_mut(),
                DG_CONTROL,
                DAT_PARENT,
                MSG_CLOSEDSM,
                self.hwnd as TW_MEMREF,
            )?;
        }
        Ok(DsmLoaded {
            handle: Some(handle),
        })
    }
}

// ---------------------------------------------------------------------------
// State 4: SourceOpened — source is open, can negotiate capabilities
// ---------------------------------------------------------------------------

pub struct SourceOpened {
    handle: Option<DsmHandle>,
    hwnd: isize,
    source_identity: TW_IDENTITY,
}

impl Drop for SourceOpened {
    fn drop(&mut self) {
        if let Some(mut handle) = self.handle.take() {
            warn!("SourceOpened dropped without clean close — closing source then DSM");
            unsafe {
                let _ = handle.call_checked(
                    ptr::null_mut(),
                    DG_CONTROL,
                    DAT_IDENTITY,
                    MSG_CLOSEDS,
                    &mut self.source_identity as *mut TW_IDENTITY as TW_MEMREF,
                );
                let _ = handle.call_checked(
                    ptr::null_mut(),
                    DG_CONTROL,
                    DAT_PARENT,
                    MSG_CLOSEDSM,
                    self.hwnd as TW_MEMREF,
                );
            }
        }
    }
}

impl SourceOpened {
    /// Set scan options (resolution, color mode, duplex, ADF)
    pub fn configure(&mut self, options: &ScanOptions) -> TwainResult<()> {
        info!("Configuring scanner: {:?}", options);

        // Set pixel type (color mode)
        self.set_capability_u16(
            ICAP_PIXELTYPE,
            options.color_mode.to_twain_pixel_type(),
        );

        // Set resolution
        self.set_capability_fix32(ICAP_XRESOLUTION, options.resolution as f32);
        self.set_capability_fix32(ICAP_YRESOLUTION, options.resolution as f32);

        // Set transfer mechanism to memory
        self.set_capability_u16(ICAP_XFERMECH, TWSX_MEMORY);

        // Configure ADF (automatic document feeder)
        if options.use_adf {
            self.set_capability_bool(CAP_FEEDERENABLED, true);
            self.set_capability_bool(CAP_AUTOFEED, true);
            // Set transfer count to -1 (scan all pages in feeder)
            self.set_capability_i16(CAP_XFERCOUNT, -1);
        } else {
            self.set_capability_i16(CAP_XFERCOUNT, 1);
        }

        // Configure duplex
        if options.duplex {
            self.set_capability_bool(CAP_DUPLEXENABLED, true);
        }

        Ok(())
    }

    /// Set a u16 capability value
    fn set_capability_u16(&mut self, cap: TW_UINT16, value: TW_UINT16) {
        let handle = self.handle.as_mut().expect("SourceOpened: no handle");
        let one_value = TW_ONEVALUE {
            ItemType: 4, // TWTY_UINT16
            Item: value as TW_UINT32,
        };

        let mut capability = TW_CAPABILITY {
            Cap: cap,
            ConType: TWON_ONEVALUE,
            // In a real implementation, this would be a GlobalAlloc'd handle
            // containing the TW_ONEVALUE. For now we pass a pointer.
            hContainer: Box::into_raw(Box::new(one_value)) as TW_HANDLE,
        };

        let result = unsafe {
            handle.call(
                &mut self.source_identity as *mut TW_IDENTITY,
                DG_CONTROL,
                DAT_CAPABILITY,
                MSG_SET,
                &mut capability as *mut TW_CAPABILITY as TW_MEMREF,
            )
        };

        // Reclaim the box to avoid leak
        unsafe {
            let _ = Box::from_raw(capability.hContainer as *mut TW_ONEVALUE);
        }

        if let Ok(rc) = result {
            if rc != TWRC_SUCCESS {
                warn!("Failed to set capability 0x{:04X} to {}: rc={}", cap, value, rc);
            }
        }
    }

    /// Set a boolean capability value
    fn set_capability_bool(&mut self, cap: TW_UINT16, value: bool) {
        self.set_capability_u16(cap, if value { 1 } else { 0 });
    }

    /// Set an i16 capability value (e.g., transfer count of -1)
    fn set_capability_i16(&mut self, cap: TW_UINT16, value: i16) {
        let handle = self.handle.as_mut().expect("SourceOpened: no handle");
        let one_value = TW_ONEVALUE {
            ItemType: 3, // TWTY_INT16
            Item: value as u16 as TW_UINT32,
        };

        let mut capability = TW_CAPABILITY {
            Cap: cap,
            ConType: TWON_ONEVALUE,
            hContainer: Box::into_raw(Box::new(one_value)) as TW_HANDLE,
        };

        let result = unsafe {
            handle.call(
                &mut self.source_identity as *mut TW_IDENTITY,
                DG_CONTROL,
                DAT_CAPABILITY,
                MSG_SET,
                &mut capability as *mut TW_CAPABILITY as TW_MEMREF,
            )
        };

        unsafe {
            let _ = Box::from_raw(capability.hContainer as *mut TW_ONEVALUE);
        }

        if let Ok(rc) = result {
            if rc != TWRC_SUCCESS {
                warn!("Failed to set capability 0x{:04X} to {}: rc={}", cap, value, rc);
            }
        }
    }

    /// Set a FIX32 capability value (e.g., resolution)
    fn set_capability_fix32(&mut self, cap: TW_UINT16, value: f32) {
        let handle = self.handle.as_mut().expect("SourceOpened: no handle");
        let fix32 = TW_FIX32::from_f32(value);
        let item_value = unsafe {
            std::mem::transmute::<TW_FIX32, u32>(fix32)
        };

        let one_value = TW_ONEVALUE {
            ItemType: 7, // TWTY_FIX32
            Item: item_value,
        };

        let mut capability = TW_CAPABILITY {
            Cap: cap,
            ConType: TWON_ONEVALUE,
            hContainer: Box::into_raw(Box::new(one_value)) as TW_HANDLE,
        };

        let result = unsafe {
            handle.call(
                &mut self.source_identity as *mut TW_IDENTITY,
                DG_CONTROL,
                DAT_CAPABILITY,
                MSG_SET,
                &mut capability as *mut TW_CAPABILITY as TW_MEMREF,
            )
        };

        unsafe {
            let _ = Box::from_raw(capability.hContainer as *mut TW_ONEVALUE);
        }

        if let Ok(rc) = result {
            if rc != TWRC_SUCCESS {
                warn!("Failed to set capability 0x{:04X} to {}: rc={}", cap, value, rc);
            }
        }
    }

    /// Enable the source to begin scanning (transition to State 5)
    pub fn enable(mut self, show_ui: bool) -> TwainResult<SourceEnabled> {
        info!("Enabling data source (show_ui={})", show_ui);

        let mut handle = self.handle.take().expect("SourceOpened: handle already consumed");

        let mut ui = TW_USERINTERFACE {
            ShowUI: if show_ui { 1 } else { 0 },
            ModalUI: if show_ui { 1 } else { 0 },
            hParent: self.hwnd as TW_HANDLE,
        };

        unsafe {
            handle.call_checked(
                &mut self.source_identity as *mut TW_IDENTITY,
                DG_CONTROL,
                DAT_USERINTERFACE,
                MSG_ENABLEDS,
                &mut ui as *mut TW_USERINTERFACE as TW_MEMREF,
            )?;
        }

        info!("Data source enabled");

        Ok(SourceEnabled {
            handle: Some(handle),
            hwnd: self.hwnd,
            source_identity: self.source_identity,
        })
    }

    /// Close the source without scanning (transition back to State 3)
    pub fn close(mut self) -> TwainResult<DsmOpened> {
        info!("Closing data source");
        let mut handle = self.handle.take().expect("SourceOpened: handle already consumed");
        unsafe {
            handle.call_checked(
                ptr::null_mut(),
                DG_CONTROL,
                DAT_IDENTITY,
                MSG_CLOSEDS,
                &mut self.source_identity as *mut TW_IDENTITY as TW_MEMREF,
            )?;
        }
        Ok(DsmOpened {
            handle: Some(handle),
            hwnd: self.hwnd,
        })
    }
}

// ---------------------------------------------------------------------------
// State 5: SourceEnabled — waiting for MSG_XFERREADY
// ---------------------------------------------------------------------------

pub struct SourceEnabled {
    handle: Option<DsmHandle>,
    hwnd: isize,
    source_identity: TW_IDENTITY,
}

impl Drop for SourceEnabled {
    fn drop(&mut self) {
        if let Some(mut handle) = self.handle.take() {
            warn!("SourceEnabled dropped without clean close — disabling, closing source, closing DSM");
            unsafe {
                let mut ui = TW_USERINTERFACE {
                    ShowUI: 0,
                    ModalUI: 0,
                    hParent: self.hwnd as TW_HANDLE,
                };
                let _ = handle.call_checked(
                    &mut self.source_identity as *mut TW_IDENTITY,
                    DG_CONTROL,
                    DAT_USERINTERFACE,
                    MSG_DISABLEDS,
                    &mut ui as *mut TW_USERINTERFACE as TW_MEMREF,
                );
                let _ = handle.call_checked(
                    ptr::null_mut(),
                    DG_CONTROL,
                    DAT_IDENTITY,
                    MSG_CLOSEDS,
                    &mut self.source_identity as *mut TW_IDENTITY as TW_MEMREF,
                );
                let _ = handle.call_checked(
                    ptr::null_mut(),
                    DG_CONTROL,
                    DAT_PARENT,
                    MSG_CLOSEDSM,
                    self.hwnd as TW_MEMREF,
                );
            }
        }
    }
}

impl SourceEnabled {
    /// Process Windows messages and wait for TWAIN events.
    /// Returns `TransferReady` when the scanner signals MSG_XFERREADY,
    /// or transitions back on MSG_CLOSEDSREQ.
    ///
    /// Accepts an optional cancellation flag; when set, the method returns
    /// `WaitResult::CloseRequested` after disabling the source.
    #[cfg(windows)]
    pub fn wait_for_transfer(
        mut self,
        cancel_flag: Option<&std::sync::atomic::AtomicBool>,
    ) -> TwainResult<WaitResult> {
        use std::sync::atomic::Ordering;
        use windows::Win32::UI::WindowsAndMessaging::{
            PeekMessageW, TranslateMessage, DispatchMessageW, MSG, PM_REMOVE,
        };

        info!("Waiting for scanner transfer ready signal");

        loop {
            // Check for cancellation
            if let Some(flag) = cancel_flag {
                if flag.load(Ordering::Acquire) {
                    info!("Cancelled while waiting for transfer");
                    return Ok(WaitResult::CloseRequested(self.disable()?));
                }
            }

            let mut win_msg = MSG::default();
            let has_msg = unsafe {
                PeekMessageW(&mut win_msg, None, 0, 0, PM_REMOVE)
            };

            if !has_msg.as_bool() {
                // No message available, sleep briefly to avoid busy-spinning
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }

            // Pass the message to TWAIN for processing
            let handle = self.handle.as_mut().expect("SourceEnabled: no handle");
            let mut tw_event = TW_EVENT {
                pEvent: &mut win_msg as *mut MSG as TW_MEMREF,
                TWMessage: 0,
            };

            let rc = unsafe {
                handle.call(
                    &mut self.source_identity as *mut TW_IDENTITY,
                    DG_CONTROL,
                    DAT_EVENT,
                    MSG_PROCESSEVENT,
                    &mut tw_event as *mut TW_EVENT as TW_MEMREF,
                )?
            };

            if rc == TWRC_DSEVENT {
                match tw_event.TWMessage {
                    MSG_XFERREADY => {
                        info!("Scanner signals transfer ready");
                        let handle = self.handle.take().expect("SourceEnabled: handle consumed");
                        return Ok(WaitResult::TransferReady(TransferReady {
                            handle: Some(handle),
                            hwnd: self.hwnd,
                            source_identity: self.source_identity,
                        }));
                    }
                    MSG_CLOSEDSREQ => {
                        info!("Scanner requests close");
                        return Ok(WaitResult::CloseRequested(self.disable()?));
                    }
                    MSG_CLOSEDSOK => {
                        info!("Scanner confirms close OK");
                        return Ok(WaitResult::CloseRequested(self.disable()?));
                    }
                    other => {
                        debug!("Unhandled TWAIN message: 0x{:04X}", other);
                    }
                }
            } else {
                // Not a TWAIN event, dispatch normally
                unsafe {
                    let _ = TranslateMessage(&win_msg);
                    DispatchMessageW(&win_msg);
                }
            }
        }
    }

    #[cfg(not(windows))]
    pub fn wait_for_transfer(
        self,
        _cancel_flag: Option<&std::sync::atomic::AtomicBool>,
    ) -> TwainResult<WaitResult> {
        Err(TwainError::InvalidState)
    }

    /// Disable the source (transition back to State 4)
    pub fn disable(mut self) -> TwainResult<SourceOpened> {
        info!("Disabling data source");

        let mut handle = self.handle.take().expect("SourceEnabled: handle already consumed");

        let mut ui = TW_USERINTERFACE {
            ShowUI: 0,
            ModalUI: 0,
            hParent: self.hwnd as TW_HANDLE,
        };

        unsafe {
            handle.call_checked(
                &mut self.source_identity as *mut TW_IDENTITY,
                DG_CONTROL,
                DAT_USERINTERFACE,
                MSG_DISABLEDS,
                &mut ui as *mut TW_USERINTERFACE as TW_MEMREF,
            )?;
        }

        Ok(SourceOpened {
            handle: Some(handle),
            hwnd: self.hwnd,
            source_identity: self.source_identity,
        })
    }
}

pub enum WaitResult {
    TransferReady(TransferReady),
    CloseRequested(SourceOpened),
}

// ---------------------------------------------------------------------------
// State 6: TransferReady — scanner has data ready to transfer
// ---------------------------------------------------------------------------

pub struct TransferReady {
    handle: Option<DsmHandle>,
    hwnd: isize,
    source_identity: TW_IDENTITY,
}

impl Drop for TransferReady {
    fn drop(&mut self) {
        if let Some(mut handle) = self.handle.take() {
            warn!("TransferReady dropped without clean close — resetting, disabling, closing source, closing DSM");
            unsafe {
                // Reset pending transfers
                let mut pending = TW_PENDINGXFERS::default();
                let _ = handle.call_checked(
                    &mut self.source_identity as *mut TW_IDENTITY,
                    DG_CONTROL,
                    DAT_PENDINGXFERS,
                    MSG_RESET,
                    &mut pending as *mut TW_PENDINGXFERS as TW_MEMREF,
                );
                // Disable source
                let mut ui = TW_USERINTERFACE {
                    ShowUI: 0,
                    ModalUI: 0,
                    hParent: self.hwnd as TW_HANDLE,
                };
                let _ = handle.call_checked(
                    &mut self.source_identity as *mut TW_IDENTITY,
                    DG_CONTROL,
                    DAT_USERINTERFACE,
                    MSG_DISABLEDS,
                    &mut ui as *mut TW_USERINTERFACE as TW_MEMREF,
                );
                // Close source
                let _ = handle.call_checked(
                    ptr::null_mut(),
                    DG_CONTROL,
                    DAT_IDENTITY,
                    MSG_CLOSEDS,
                    &mut self.source_identity as *mut TW_IDENTITY as TW_MEMREF,
                );
                // Close DSM
                let _ = handle.call_checked(
                    ptr::null_mut(),
                    DG_CONTROL,
                    DAT_PARENT,
                    MSG_CLOSEDSM,
                    self.hwnd as TW_MEMREF,
                );
            }
        }
    }
}

impl TransferReady {
    /// Get image information for the current page
    pub fn get_image_info(&mut self) -> TwainResult<TW_IMAGEINFO> {
        let handle = self.handle.as_mut().expect("TransferReady: no handle");
        let mut info = TW_IMAGEINFO::default();

        unsafe {
            handle.call_checked(
                &mut self.source_identity as *mut TW_IDENTITY,
                DG_IMAGE,
                DAT_IMAGEINFO,
                MSG_GET,
                &mut info as *mut TW_IMAGEINFO as TW_MEMREF,
            )?;
        }

        debug!(
            "Image info: {}x{} @ {} bpp, {:.0}x{:.0} dpi",
            info.ImageWidth,
            info.ImageLength,
            info.BitsPerPixel,
            info.XResolution.to_f32(),
            info.YResolution.to_f32()
        );

        Ok(info)
    }

    /// Transfer the current page using memory transfer mode.
    /// Returns the page data and transitions based on pending transfers.
    pub fn transfer_memory(mut self) -> TwainResult<TransferResult> {
        let image_info = self.get_image_info()?;

        // Get memory transfer setup
        let mut setup = TW_SETUPMEMXFER::default();
        {
            let handle = self.handle.as_mut().expect("TransferReady: no handle");
            unsafe {
                handle.call_checked(
                    &mut self.source_identity as *mut TW_IDENTITY,
                    DG_CONTROL,
                    DAT_SETUPMEMXFER,
                    MSG_GET,
                    &mut setup as *mut TW_SETUPMEMXFER as TW_MEMREF,
                )?;
            }
        }

        let buf_size = setup.Preferred as usize;
        debug!("Memory transfer buffer: {} bytes (min={}, max={})",
            buf_size, setup.MinBufSize, setup.MaxBufSize);

        // Allocate buffer and collect image strips
        let mut buffer = vec![0u8; buf_size];
        let mut image_data = Vec::new();

        loop {
            let handle = self.handle.as_mut().expect("TransferReady: no handle");
            let mut mem_xfer = TW_IMAGEMEMXFER {
                Memory: TW_MEMORY {
                    Flags: TWMF_APPOWNS | TWMF_POINTER,
                    Length: buf_size as TW_UINT32,
                    TheMem: buffer.as_mut_ptr() as TW_MEMREF,
                },
                ..Default::default()
            };

            let rc = unsafe {
                handle.call(
                    &mut self.source_identity as *mut TW_IDENTITY,
                    DG_IMAGE,
                    DAT_IMAGEMEMXFER,
                    MSG_GET,
                    &mut mem_xfer as *mut TW_IMAGEMEMXFER as TW_MEMREF,
                )?
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

        // End the transfer
        let mut pending = TW_PENDINGXFERS::default();
        {
            let handle = self.handle.as_mut().expect("TransferReady: no handle");
            unsafe {
                handle.call_checked(
                    &mut self.source_identity as *mut TW_IDENTITY,
                    DG_CONTROL,
                    DAT_PENDINGXFERS,
                    MSG_ENDXFER,
                    &mut pending as *mut TW_PENDINGXFERS as TW_MEMREF,
                )?;
            }
        }

        let page = ScannedPage {
            page_number: 0, // Caller sets this
            width: image_info.ImageWidth as u32,
            height: image_info.ImageLength as u32,
            bits_per_pixel: image_info.BitsPerPixel as u16,
            x_resolution: image_info.XResolution.to_f32(),
            y_resolution: image_info.YResolution.to_f32(),
            data: image_data,
        };

        let handle = self.handle.take().expect("TransferReady: handle already consumed");

        if pending.Count == 0 {
            info!("All transfers complete");
            Ok(TransferResult::Done {
                page,
                source: SourceOpened {
                    handle: Some(handle),
                    hwnd: self.hwnd,
                    source_identity: self.source_identity,
                },
            })
        } else {
            debug!("Pending transfers: {}", pending.Count);
            Ok(TransferResult::MorePages {
                page,
                next: TransferReady {
                    handle: Some(handle),
                    hwnd: self.hwnd,
                    source_identity: self.source_identity,
                },
            })
        }
    }

    /// Cancel the transfer and reset (transition back to State 4)
    pub fn cancel(mut self) -> TwainResult<SourceOpened> {
        info!("Cancelling transfer");

        let mut handle = self.handle.take().expect("TransferReady: handle already consumed");
        let mut pending = TW_PENDINGXFERS::default();
        unsafe {
            handle.call_checked(
                &mut self.source_identity as *mut TW_IDENTITY,
                DG_CONTROL,
                DAT_PENDINGXFERS,
                MSG_RESET,
                &mut pending as *mut TW_PENDINGXFERS as TW_MEMREF,
            )?;
        }

        Ok(SourceOpened {
            handle: Some(handle),
            hwnd: self.hwnd,
            source_identity: self.source_identity,
        })
    }
}

/// Result of a page transfer: either more pages to come, or we're done
pub enum TransferResult {
    MorePages {
        page: ScannedPage,
        next: TransferReady,
    },
    Done {
        page: ScannedPage,
        source: SourceOpened,
    },
}

// ---------------------------------------------------------------------------
// Hidden message-only window for TWAIN message pump
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub fn create_hidden_hwnd() -> TwainResult<isize> {
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, RegisterClassW, HWND_MESSAGE,
        WNDCLASSW, WS_OVERLAPPED,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
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
        let hmodule = GetModuleHandleW(None)
            .map_err(|e| TwainError::WindowCreationFailed(e.to_string()))?;
        let hinstance: HINSTANCE = hmodule.into();

        let class_name = w!("RSWebTWAINTwainHidden");

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
            w!("RSWebTWAIN TWAIN Window"),
            WS_OVERLAPPED,
            0, 0, 0, 0,
            HWND_MESSAGE,
            None,
            hinstance,
            None,
        )
        .map_err(|e| TwainError::WindowCreationFailed(e.to_string()))?;

        Ok(hwnd.0 as isize)
    }
}

#[cfg(not(windows))]
pub fn create_hidden_hwnd() -> TwainResult<isize> {
    Err(TwainError::WindowCreationFailed(
        "Hidden HWND only supported on Windows".to_string(),
    ))
}
