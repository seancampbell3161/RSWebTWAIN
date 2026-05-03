//! Raw TWAIN FFI bindings to TWAINDSM.dll
//!
//! These types are `#[repr(C)]` translations of the structures defined in twain.h (v2.3).
//! All field names and sizes match the C definitions exactly for correct ABI compatibility.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::ffi::c_void;
use std::os::raw::c_char;

// Primitive type aliases (matching twain.h typedefs)

pub type TW_UINT16 = u16;
pub type TW_UINT32 = u32;
pub type TW_INT16 = i16;
pub type TW_INT32 = i32;
pub type TW_UINT8 = u8;
pub type TW_BOOL = u16;
pub type TW_HANDLE = *mut c_void;
pub type TW_MEMREF = *mut c_void;

pub type TW_STR32 = [c_char; 34];
pub type TW_STR255 = [c_char; 256];

// Core structures

/// Fixed-point number: whole + frac/65536
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TW_FIX32 {
    pub Whole: TW_INT16,
    pub Frac: TW_UINT16,
}

impl TW_FIX32 {
    pub fn from_f32(val: f32) -> Self {
        let whole = val as TW_INT16;
        let frac = ((val - whole as f32) * 65536.0) as TW_UINT16;
        Self { Whole: whole, Frac: frac }
    }

    pub fn to_f32(self) -> f32 {
        self.Whole as f32 + self.Frac as f32 / 65536.0
    }
}

/// Rectangle coordinates in ICAP_UNITS
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TW_FRAME {
    pub Left: TW_FIX32,
    pub Top: TW_FIX32,
    pub Right: TW_FIX32,
    pub Bottom: TW_FIX32,
}

/// Software version information
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TW_VERSION {
    pub MajorNum: TW_UINT16,
    pub MinorNum: TW_UINT16,
    pub Language: TW_UINT16,
    pub Country: TW_UINT16,
    pub Info: TW_STR32,
}

impl Default for TW_VERSION {
    fn default() -> Self {
        Self {
            MajorNum: 0,
            MinorNum: 0,
            Language: 0,
            Country: 0,
            Info: [0; 34],
        }
    }
}

/// Identifies a TWAIN entity (application or data source)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TW_IDENTITY {
    pub Id: TW_UINT32,
    pub Version: TW_VERSION,
    pub ProtocolMajor: TW_UINT16,
    pub ProtocolMinor: TW_UINT16,
    pub SupportedGroups: TW_UINT32,
    pub Manufacturer: TW_STR32,
    pub ProductFamily: TW_STR32,
    pub ProductName: TW_STR32,
}

impl Default for TW_IDENTITY {
    fn default() -> Self {
        Self {
            Id: 0,
            Version: TW_VERSION::default(),
            ProtocolMajor: 0,
            ProtocolMinor: 0,
            SupportedGroups: 0,
            Manufacturer: [0; 34],
            ProductFamily: [0; 34],
            ProductName: [0; 34],
        }
    }
}

/// Describes a scanned image's attributes
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TW_IMAGEINFO {
    pub XResolution: TW_FIX32,
    pub YResolution: TW_FIX32,
    pub ImageWidth: TW_INT32,
    pub ImageLength: TW_INT32,
    pub SamplesPerPixel: TW_INT16,
    pub BitsPerSample: [TW_INT16; 8],
    pub BitsPerPixel: TW_INT16,
    pub Planar: TW_BOOL,
    pub PixelType: TW_INT16,
    pub Compression: TW_UINT16,
}

/// Capability negotiation container
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TW_CAPABILITY {
    pub Cap: TW_UINT16,
    pub ConType: TW_UINT16,
    pub hContainer: TW_HANDLE,
}

impl Default for TW_CAPABILITY {
    fn default() -> Self {
        Self {
            Cap: 0,
            ConType: 0,
            hContainer: std::ptr::null_mut(),
        }
    }
}

/// Controls user interface display during scanning
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TW_USERINTERFACE {
    pub ShowUI: TW_BOOL,
    pub ModalUI: TW_BOOL,
    pub hParent: TW_HANDLE,
}

impl Default for TW_USERINTERFACE {
    fn default() -> Self {
        Self {
            ShowUI: 0,
            ModalUI: 0,
            hParent: std::ptr::null_mut(),
        }
    }
}

/// Remaining transfers available
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TW_PENDINGXFERS {
    pub Count: TW_UINT16,
    pub EOJ: TW_UINT32,
}

/// Memory transfer buffer size information
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TW_SETUPMEMXFER {
    pub MinBufSize: TW_UINT32,
    pub MaxBufSize: TW_UINT32,
    pub Preferred: TW_UINT32,
}

/// Memory buffer descriptor
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TW_MEMORY {
    pub Flags: TW_UINT32,
    pub Length: TW_UINT32,
    pub TheMem: TW_MEMREF,
}

impl Default for TW_MEMORY {
    fn default() -> Self {
        Self {
            Flags: 0,
            Length: 0,
            TheMem: std::ptr::null_mut(),
        }
    }
}

/// Image data transferred via memory
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TW_IMAGEMEMXFER {
    pub Compression: TW_UINT16,
    pub BytesPerRow: TW_UINT32,
    pub Columns: TW_UINT32,
    pub Rows: TW_UINT32,
    pub XOffset: TW_UINT32,
    pub YOffset: TW_UINT32,
    pub BytesWritten: TW_UINT32,
    pub Memory: TW_MEMORY,
}

/// Image layout on the page
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TW_IMAGELAYOUT {
    pub Frame: TW_FRAME,
    pub DocumentNumber: TW_UINT32,
    pub PageNumber: TW_UINT32,
    pub FrameNumber: TW_UINT32,
}

/// Windows event for TWAIN message processing
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TW_EVENT {
    pub pEvent: TW_MEMREF,
    pub TWMessage: TW_UINT16,
}

impl Default for TW_EVENT {
    fn default() -> Self {
        Self {
            pEvent: std::ptr::null_mut(),
            TWMessage: 0,
        }
    }
}

/// TWAIN status information
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TW_STATUS {
    pub ConditionCode: TW_UINT16,
    pub Data: TW_UINT16,
}

// Capability container structures

/// Single value container
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TW_ONEVALUE {
    pub ItemType: TW_UINT16,
    pub Item: TW_UINT32,
}

/// Range of values container
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TW_RANGE {
    pub ItemType: TW_UINT16,
    pub MinValue: TW_UINT32,
    pub MaxValue: TW_UINT32,
    pub StepSize: TW_UINT32,
    pub DefaultValue: TW_UINT32,
    pub CurrentValue: TW_UINT32,
}

/// Enumeration container (variable-length, ItemList is the first element)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TW_ENUMERATION {
    pub ItemType: TW_UINT16,
    pub NumItems: TW_UINT32,
    pub CurrentIndex: TW_UINT32,
    pub DefaultIndex: TW_UINT32,
    pub ItemList: [TW_UINT8; 1], // Variable-length array
}

// Constants: Data Groups

pub const DG_CONTROL: TW_UINT32 = 0x0001;
pub const DG_IMAGE: TW_UINT32 = 0x0002;
pub const DG_AUDIO: TW_UINT32 = 0x0004;

// Constants: Data Argument Types (DAT_*)

pub const DAT_CAPABILITY: TW_UINT16 = 0x0001;
pub const DAT_EVENT: TW_UINT16 = 0x0002;
pub const DAT_IDENTITY: TW_UINT16 = 0x0003;
pub const DAT_PARENT: TW_UINT16 = 0x0004;
pub const DAT_PENDINGXFERS: TW_UINT16 = 0x0005;
pub const DAT_SETUPMEMXFER: TW_UINT16 = 0x0006;
pub const DAT_SETUPFILEXFER: TW_UINT16 = 0x0007;
pub const DAT_STATUS: TW_UINT16 = 0x0008;
pub const DAT_USERINTERFACE: TW_UINT16 = 0x0009;
pub const DAT_IMAGEINFO: TW_UINT16 = 0x0101;
pub const DAT_IMAGELAYOUT: TW_UINT16 = 0x0102;
pub const DAT_IMAGEMEMXFER: TW_UINT16 = 0x0103;
pub const DAT_IMAGENATIVEXFER: TW_UINT16 = 0x0104;

// Constants: Messages (MSG_*)

// Generic messages
pub const MSG_GET: TW_UINT16 = 0x0001;
pub const MSG_GETCURRENT: TW_UINT16 = 0x0002;
pub const MSG_GETDEFAULT: TW_UINT16 = 0x0003;
pub const MSG_GETFIRST: TW_UINT16 = 0x0004;
pub const MSG_GETNEXT: TW_UINT16 = 0x0005;
pub const MSG_SET: TW_UINT16 = 0x0006;
pub const MSG_RESET: TW_UINT16 = 0x0007;

// DSM messages
pub const MSG_OPENDSM: TW_UINT16 = 0x0301;
pub const MSG_CLOSEDSM: TW_UINT16 = 0x0302;

// Data source messages
pub const MSG_OPENDS: TW_UINT16 = 0x0401;
pub const MSG_CLOSEDS: TW_UINT16 = 0x0402;

// Source UI messages
pub const MSG_DISABLEDS: TW_UINT16 = 0x0501;
pub const MSG_ENABLEDS: TW_UINT16 = 0x0502;

// Event processing
pub const MSG_PROCESSEVENT: TW_UINT16 = 0x0601;

// Transfer messages
pub const MSG_ENDXFER: TW_UINT16 = 0x0701;

// Notification messages (from DS to app via event)
pub const MSG_XFERREADY: TW_UINT16 = 0x0101;
pub const MSG_CLOSEDSREQ: TW_UINT16 = 0x0102;
pub const MSG_CLOSEDSOK: TW_UINT16 = 0x0103;

// Constants: Return Codes (TWRC_*)

pub const TWRC_SUCCESS: TW_UINT16 = 0;
pub const TWRC_FAILURE: TW_UINT16 = 1;
pub const TWRC_CHECKSTATUS: TW_UINT16 = 2;
pub const TWRC_CANCEL: TW_UINT16 = 3;
pub const TWRC_DSEVENT: TW_UINT16 = 4;
pub const TWRC_NOTDSEVENT: TW_UINT16 = 5;
pub const TWRC_XFERDONE: TW_UINT16 = 6;
pub const TWRC_ENDOFLIST: TW_UINT16 = 7;

// Constants: Condition Codes (TWCC_*)

pub const TWCC_SUCCESS: TW_UINT16 = 0;
pub const TWCC_BUMMER: TW_UINT16 = 1;
pub const TWCC_LOWMEMORY: TW_UINT16 = 2;
pub const TWCC_NODS: TW_UINT16 = 3;
pub const TWCC_OPERATIONERROR: TW_UINT16 = 6;
pub const TWCC_BADCAP: TW_UINT16 = 9;
pub const TWCC_BADVALUE: TW_UINT16 = 10;
pub const TWCC_SEQERROR: TW_UINT16 = 11;
pub const TWCC_BADDEST: TW_UINT16 = 12;
pub const TWCC_CAPUNSUPPORTED: TW_UINT16 = 13;
pub const TWCC_CAPBADOPERATION: TW_UINT16 = 14;
pub const TWCC_PAPERJAM: TW_UINT16 = 16;
pub const TWCC_PAPERDOUBLEFEED: TW_UINT16 = 17;

// Constants: Capabilities (CAP_* / ICAP_*)

pub const CAP_XFERCOUNT: TW_UINT16 = 0x0001;
pub const CAP_FEEDERENABLED: TW_UINT16 = 0x1002;
pub const CAP_FEEDERLOADED: TW_UINT16 = 0x1003;
pub const CAP_AUTOFEED: TW_UINT16 = 0x1007;
pub const CAP_DUPLEX: TW_UINT16 = 0x1012;
pub const CAP_DUPLEXENABLED: TW_UINT16 = 0x1013;

pub const ICAP_COMPRESSION: TW_UINT16 = 0x0100;
pub const ICAP_PIXELTYPE: TW_UINT16 = 0x0101;
pub const ICAP_UNITS: TW_UINT16 = 0x0102;
pub const ICAP_XFERMECH: TW_UINT16 = 0x0103;
pub const ICAP_BITDEPTH: TW_UINT16 = 0x112B;
pub const ICAP_XRESOLUTION: TW_UINT16 = 0x1118;
pub const ICAP_YRESOLUTION: TW_UINT16 = 0x1119;

// Constants: Pixel Types (TWPT_*)

pub const TWPT_BW: TW_UINT16 = 0;
pub const TWPT_GRAY: TW_UINT16 = 1;
pub const TWPT_RGB: TW_UINT16 = 2;
pub const TWPT_PALETTE: TW_UINT16 = 3;
pub const TWPT_CMY: TW_UINT16 = 4;
pub const TWPT_CMYK: TW_UINT16 = 5;

// Constants: Container Types (TWON_*)

pub const TWON_ARRAY: TW_UINT16 = 3;
pub const TWON_ENUMERATION: TW_UINT16 = 4;
pub const TWON_ONEVALUE: TW_UINT16 = 5;
pub const TWON_RANGE: TW_UINT16 = 6;

// Constants: Transfer Mechanisms (TWSX_*)

pub const TWSX_NATIVE: TW_UINT16 = 0;
pub const TWSX_FILE: TW_UINT16 = 1;
pub const TWSX_MEMORY: TW_UINT16 = 2;
pub const TWSX_MEMFILE: TW_UINT16 = 4;

// Constants: Compression Types (TWCP_*)

pub const TWCP_NONE: TW_UINT16 = 0;
pub const TWCP_GROUP31D: TW_UINT16 = 2;
pub const TWCP_GROUP32D: TW_UINT16 = 3;
pub const TWCP_GROUP4: TW_UINT16 = 5;
pub const TWCP_JPEG: TW_UINT16 = 6;

// Constants: Units (TWUN_*)

pub const TWUN_INCHES: TW_UINT16 = 0;
pub const TWUN_CENTIMETERS: TW_UINT16 = 1;
pub const TWUN_PIXELS: TW_UINT16 = 5;

// Constants: Memory Flags (TWMF_*)

pub const TWMF_APPOWNS: TW_UINT32 = 0x0001;
pub const TWMF_DSMOWNS: TW_UINT32 = 0x0002;
pub const TWMF_DSOWNS: TW_UINT32 = 0x0004;
pub const TWMF_POINTER: TW_UINT32 = 0x0008;
pub const TWMF_HANDLE: TW_UINT32 = 0x0010;

// Constants: TWAIN Protocol Version

pub const TWON_PROTOCOLMAJOR: TW_UINT16 = 2;
pub const TWON_PROTOCOLMINOR: TW_UINT16 = 3;

// Constants: Language (TWLG_*) and Country (TWCY_*) — subset

pub const TWLG_ENGLISH_USA: TW_UINT16 = 13;
pub const TWCY_USA: TW_UINT16 = 1;

// DSM Entry function pointer type

/// The single entry point into the TWAIN Data Source Manager.
/// All TWAIN operations go through this function with different DG/DAT/MSG combinations.
pub type DSM_Entry = unsafe extern "system" fn(
    pOrigin: *mut TW_IDENTITY,
    pDest: *mut TW_IDENTITY,
    DG: TW_UINT32,
    DAT: TW_UINT16,
    MSG: TW_UINT16,
    pData: TW_MEMREF,
) -> TW_UINT16;

// Helper: Convert a Rust string to TW_STR32 / TW_STR255

pub fn str_to_tw_str32(s: &str) -> TW_STR32 {
    let mut buf: TW_STR32 = [0; 34];
    let bytes = s.as_bytes();
    let len = bytes.len().min(33);
    for (i, &b) in bytes[..len].iter().enumerate() {
        buf[i] = b as c_char;
    }
    buf
}

pub fn str_to_tw_str255(s: &str) -> TW_STR255 {
    let mut buf: TW_STR255 = [0; 256];
    let bytes = s.as_bytes();
    let len = bytes.len().min(255);
    for (i, &b) in bytes[..len].iter().enumerate() {
        buf[i] = b as c_char;
    }
    buf
}

pub fn tw_str32_to_string(s: &TW_STR32) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    s[..end].iter().map(|&c| c as u8 as char).collect()
}

pub fn tw_str255_to_string(s: &TW_STR255) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    s[..end].iter().map(|&c| c as u8 as char).collect()
}
