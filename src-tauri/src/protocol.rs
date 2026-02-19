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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ClientMessage deserialization ----

    #[test]
    fn deserialize_ping() {
        let json = r#"{"type": "ping", "id": "abc-123"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Ping { id } => assert_eq!(id, "abc-123"),
            _ => panic!("Expected Ping"),
        }
    }

    #[test]
    fn deserialize_list_scanners() {
        let json = r#"{"type": "list_scanners", "id": "req-1"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::ListScanners { id } => assert_eq!(id, "req-1"),
            _ => panic!("Expected ListScanners"),
        }
    }

    #[test]
    fn deserialize_start_scan_minimal() {
        let json = r#"{
            "type": "start_scan",
            "id": "scan-1",
            "options": {}
        }"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::StartScan { id, options } => {
                assert_eq!(id, "scan-1");
                assert_eq!(options.resolution, 300); // default
                assert!(matches!(options.color_mode, ColorMode::Color)); // default
                assert!(matches!(options.format, OutputFormat::Pdf)); // default
                assert!(!options.duplex);
                assert!(!options.use_adf);
                assert!(!options.show_scanner_ui);
                assert!(options.scanner_id.is_none());
            }
            _ => panic!("Expected StartScan"),
        }
    }

    #[test]
    fn deserialize_start_scan_full() {
        let json = r#"{
            "type": "start_scan",
            "id": "scan-2",
            "options": {
                "scanner_id": "HP Scanner",
                "resolution": 600,
                "color_mode": "grayscale",
                "duplex": true,
                "use_adf": true,
                "format": "png",
                "show_scanner_ui": true
            }
        }"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::StartScan { id, options } => {
                assert_eq!(id, "scan-2");
                assert_eq!(options.scanner_id.as_deref(), Some("HP Scanner"));
                assert_eq!(options.resolution, 600);
                assert!(matches!(options.color_mode, ColorMode::Grayscale));
                assert!(options.duplex);
                assert!(options.use_adf);
                assert!(matches!(options.format, OutputFormat::Png));
                assert!(options.show_scanner_ui);
            }
            _ => panic!("Expected StartScan"),
        }
    }

    #[test]
    fn deserialize_cancel_scan() {
        let json = r#"{"type": "cancel_scan", "id": "req-3", "scan_id": "scan-abc"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::CancelScan { id, scan_id } => {
                assert_eq!(id, "req-3");
                assert_eq!(scan_id, "scan-abc");
            }
            _ => panic!("Expected CancelScan"),
        }
    }

    #[test]
    fn deserialize_invalid_type() {
        let json = r#"{"type": "unknown_command", "id": "x"}"#;
        let result = serde_json::from_str::<ClientMessage>(json);
        assert!(result.is_err());
    }

    // ---- AgentMessage serialization ----

    #[test]
    fn serialize_pong() {
        let msg = AgentMessage::Pong {
            id: "abc-123".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "pong");
        assert_eq!(v["id"], "abc-123");
    }

    #[test]
    fn serialize_scanner_list() {
        let msg = AgentMessage::ScannerList {
            id: "req-1".to_string(),
            scanners: vec![
                ScannerListEntry {
                    id: "1".to_string(),
                    name: "HP Scanner".to_string(),
                    manufacturer: "HP".to_string(),
                },
            ],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "scanner_list");
        assert_eq!(v["scanners"][0]["name"], "HP Scanner");
    }

    #[test]
    fn serialize_error() {
        let msg = AgentMessage::Error {
            id: "req-5".to_string(),
            code: ErrorCode::PaperJam,
            message: "Paper jam detected".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "error");
        assert_eq!(v["code"], "PAPER_JAM");
        assert_eq!(v["message"], "Paper jam detected");
    }

    #[test]
    fn serialize_scan_complete_without_pdf() {
        let msg = AgentMessage::ScanComplete {
            id: "req-6".to_string(),
            scan_id: "scan-1".to_string(),
            total_pages: 3,
            pdf_data: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "scan_complete");
        assert_eq!(v["total_pages"], 3);
        // pdf_data should be absent (skip_serializing_if = None)
        assert!(v.get("pdf_data").is_none());
    }

    #[test]
    fn serialize_scan_progress() {
        let msg = AgentMessage::ScanProgress {
            id: "req-7".to_string(),
            scan_id: "scan-2".to_string(),
            page: 2,
            status: ScanStatus::Scanning,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "scan_progress");
        assert_eq!(v["page"], 2);
        assert_eq!(v["status"], "scanning");
    }

    // ---- Color mode serialization ----

    #[test]
    fn color_mode_serde() {
        // Serialize
        assert_eq!(serde_json::to_string(&ColorMode::Color).unwrap(), r#""color""#);
        assert_eq!(serde_json::to_string(&ColorMode::Grayscale).unwrap(), r#""grayscale""#);
        assert_eq!(serde_json::to_string(&ColorMode::BlackWhite).unwrap(), r#""bw""#);

        // Deserialize
        let c: ColorMode = serde_json::from_str(r#""color""#).unwrap();
        assert!(matches!(c, ColorMode::Color));
        let g: ColorMode = serde_json::from_str(r#""grayscale""#).unwrap();
        assert!(matches!(g, ColorMode::Grayscale));
        let bw: ColorMode = serde_json::from_str(r#""bw""#).unwrap();
        assert!(matches!(bw, ColorMode::BlackWhite));
    }

    // ---- Error code serialization ----

    #[test]
    fn error_codes_are_screaming_snake_case() {
        let codes = vec![
            (ErrorCode::ScannerNotFound, "SCANNER_NOT_FOUND"),
            (ErrorCode::ScannerBusy, "SCANNER_BUSY"),
            (ErrorCode::PaperJam, "PAPER_JAM"),
            (ErrorCode::TwainNotInstalled, "TWAIN_NOT_INSTALLED"),
            (ErrorCode::NoScannersAvailable, "NO_SCANNERS_AVAILABLE"),
            (ErrorCode::InternalError, "INTERNAL_ERROR"),
        ];
        for (code, expected) in codes {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!(r#""{}""#, expected));
        }
    }
}
