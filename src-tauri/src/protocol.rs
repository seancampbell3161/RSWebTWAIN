//! WebSocket message protocol types.
//!
//! All communication between the Angular client and the scan agent uses JSON messages
//! over WebSocket. Each message has a `type` field and a correlation `id` for
//! request/response matching.

use serde::{Deserialize, Serialize};

use crate::scanner::twain::{ColorMode, SourceInfo};

// ---------------------------------------------------------------------------
// Client → Agent messages
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    ListScanners {
        id: String,
    },
    StartScan {
        id: String,
        options: ScanRequestOptions,
    },
    CancelScan {
        id: String,
        scan_id: String,
    },
    Ping {
        id: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct ScanRequestOptions {
    /// Scanner name to use. If empty/null, uses the default scanner.
    pub scanner_id: Option<String>,
    #[serde(default = "default_resolution")]
    pub resolution: u32,
    #[serde(default = "default_color_mode")]
    pub color_mode: ColorMode,
    #[serde(default)]
    pub duplex: bool,
    #[serde(default)]
    pub use_adf: bool,
    #[serde(default = "default_format")]
    pub format: OutputFormat,
    #[serde(default)]
    pub show_scanner_ui: bool,
}

fn default_resolution() -> u32 {
    300
}
fn default_color_mode() -> ColorMode {
    ColorMode::Color
}
fn default_format() -> OutputFormat {
    OutputFormat::Pdf
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Pdf,
    Png,
    Jpeg,
}

// ---------------------------------------------------------------------------
// Agent → Client messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessage {
    ScannerList {
        id: String,
        scanners: Vec<ScannerListEntry>,
    },
    ScanProgress {
        id: String,
        scan_id: String,
        page: u32,
        status: ScanStatus,
    },
    ScanPage {
        id: String,
        scan_id: String,
        page: u32,
        data: String,
        mime: String,
    },
    ScanComplete {
        id: String,
        scan_id: String,
        total_pages: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        pdf_data: Option<String>,
    },
    Error {
        id: String,
        code: ErrorCode,
        message: String,
    },
    Pong {
        id: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ScannerListEntry {
    pub id: String,
    pub name: String,
    pub manufacturer: String,
}

impl From<SourceInfo> for ScannerListEntry {
    fn from(source: SourceInfo) -> Self {
        Self {
            id: source.id.to_string(),
            name: source.name,
            manufacturer: source.manufacturer,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanStatus {
    Scanning,
    Processing,
    Complete,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    ScannerNotFound,
    ScannerBusy,
    ScanCancelled,
    PaperJam,
    PaperDoubleFeed,
    TwainNotInstalled,
    NoScannersAvailable,
    InternalError,
    InvalidRequest,
}
