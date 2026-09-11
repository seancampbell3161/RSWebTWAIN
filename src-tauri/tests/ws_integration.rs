//! WebSocket integration tests.
//!
//! These tests spin up the real WS server and connect a client,
//! verifying the full message round-trip without needing scanner hardware.

use futures_util::{SinkExt, StreamExt};
use scan_agent_lib::ws_server::{self, WsServerConfig};
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn ping_pong() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::AllowAll,
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;

    // Spawn the command handler in the background
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    // Connect a WebSocket client
    let url = format!("ws://127.0.0.1:{}", port);
    let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut tx, mut rx) = ws_stream.split();

    // Send ping
    let ping_msg = r#"{"type": "ping", "id": "test-1"}"#;
    tx.send(Message::Text(ping_msg.into())).await.unwrap();

    // Receive pong
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), rx.next())
        .await
        .expect("Timeout waiting for pong")
        .expect("Stream ended")
        .expect("WS error");

    let text = response.into_text().unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["type"], "pong");
    assert_eq!(v["id"], "test-1");

    handler.abort();
}

#[tokio::test]
async fn list_scanners_returns_valid_response() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::AllowAll,
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    let url = format!("ws://127.0.0.1:{}", port);
    let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut tx, mut rx) = ws_stream.split();

    // Send list_scanners request
    let msg = r#"{"type": "list_scanners", "id": "ls-1"}"#;
    tx.send(Message::Text(msg.into())).await.unwrap();

    // We should get back either a scanner_list or an error
    // (on macOS/CI without scanners, we'll get an error since TWAIN isn't available)
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), rx.next())
        .await
        .expect("Timeout waiting for response")
        .expect("Stream ended")
        .expect("WS error");

    let text = response.into_text().unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();

    // Should be either scanner_list or error, both with matching id
    let msg_type = v["type"].as_str().unwrap();
    assert!(
        msg_type == "scanner_list" || msg_type == "error",
        "Unexpected message type: {}",
        msg_type
    );
    assert_eq!(v["id"], "ls-1");

    handler.abort();
}

#[tokio::test]
async fn invalid_json_returns_error() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::AllowAll,
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    let url = format!("ws://127.0.0.1:{}", port);
    let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut tx, mut rx) = ws_stream.split();

    // Send garbage
    tx.send(Message::Text("not valid json".into())).await.unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), rx.next())
        .await
        .expect("Timeout waiting for error response")
        .expect("Stream ended")
        .expect("WS error");

    let text = response.into_text().unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "INVALID_REQUEST");

    handler.abort();
}

#[tokio::test]
async fn multiple_clients_can_connect() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::AllowAll,
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    let url = format!("ws://127.0.0.1:{}", port);

    // Connect two clients
    let (ws1, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (ws2, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let (mut tx1, mut rx1) = ws1.split();
    let (mut tx2, mut rx2) = ws2.split();

    // Both send pings with different ids
    tx1.send(Message::Text(r#"{"type":"ping","id":"c1"}"#.into()))
        .await
        .unwrap();
    tx2.send(Message::Text(r#"{"type":"ping","id":"c2"}"#.into()))
        .await
        .unwrap();

    // Both should receive their pongs
    let r1 = tokio::time::timeout(std::time::Duration::from_secs(5), rx1.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let r2 = tokio::time::timeout(std::time::Duration::from_secs(5), rx2.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let v1: serde_json::Value = serde_json::from_str(&r1.into_text().unwrap()).unwrap();
    let v2: serde_json::Value = serde_json::from_str(&r2.into_text().unwrap()).unwrap();

    assert_eq!(v1["id"], "c1");
    assert_eq!(v2["id"], "c2");

    handler.abort();
}

// Auth token tests

#[tokio::test]
async fn auth_token_valid_connects() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::AllowAll,
        auth_token: Some("secret".to_string()),
    };

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    let url = format!("ws://127.0.0.1:{}/?token=secret", port);
    let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut tx, mut rx) = ws_stream.split();

    tx.send(Message::Text(r#"{"type":"ping","id":"auth-1"}"#.into()))
        .await
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), rx.next())
        .await
        .expect("Timeout")
        .expect("Stream ended")
        .expect("WS error");

    let v: serde_json::Value = serde_json::from_str(&response.into_text().unwrap()).unwrap();
    assert_eq!(v["type"], "pong");
    assert_eq!(v["id"], "auth-1");

    handler.abort();
}

#[tokio::test]
async fn auth_token_invalid_rejected() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::AllowAll,
        auth_token: Some("secret".to_string()),
    };

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let _handler = tokio::spawn(scan_agent_lib::command_handler(
        handle.command_rx,
        handle.event_tx.clone(),
        None,
    ));

    let url = format!("ws://127.0.0.1:{}/?token=wrong", port);
    let result = tokio_tungstenite::connect_async(&url).await;

    match result {
        Err(tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status(), 401);
        }
        other => panic!("Expected HTTP 401 error, got: {:?}", other),
    }
}

#[tokio::test]
async fn auth_token_missing_rejected() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::AllowAll,
        auth_token: Some("secret".to_string()),
    };

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let _handler = tokio::spawn(scan_agent_lib::command_handler(
        handle.command_rx,
        handle.event_tx.clone(),
        None,
    ));

    let url = format!("ws://127.0.0.1:{}", port);
    let result = tokio_tungstenite::connect_async(&url).await;

    match result {
        Err(tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status(), 401);
        }
        other => panic!("Expected HTTP 401 error, got: {:?}", other),
    }
}

// Origin validation tests

/// Build a WebSocket client request with an explicit Origin header.
fn ws_request_with_origin(port: u16, origin: &str) -> tungstenite::http::Request<()> {
    tungstenite::http::Request::builder()
        .uri(format!("ws://127.0.0.1:{}", port))
        .header("Host", format!("127.0.0.1:{}", port))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .header("Origin", origin)
        .body(())
        .unwrap()
}

#[tokio::test]
async fn origin_allowed_connects() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::Restricted {
            allow_localhost: false,
            extra: vec!["https://app.example.com".to_string()],
        },
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    let req = ws_request_with_origin(port, "https://app.example.com");
    let (ws_stream, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let (mut tx, mut rx) = ws_stream.split();

    tx.send(Message::Text(r#"{"type":"ping","id":"origin-1"}"#.into()))
        .await
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), rx.next())
        .await
        .expect("Timeout")
        .expect("Stream ended")
        .expect("WS error");

    let v: serde_json::Value = serde_json::from_str(&response.into_text().unwrap()).unwrap();
    assert_eq!(v["type"], "pong");
    assert_eq!(v["id"], "origin-1");

    handler.abort();
}

#[tokio::test]
async fn origin_disallowed_rejected() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::Restricted {
            allow_localhost: false,
            extra: vec!["https://app.example.com".to_string()],
        },
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let _handler = tokio::spawn(scan_agent_lib::command_handler(
        handle.command_rx,
        handle.event_tx.clone(),
        None,
    ));

    let req = ws_request_with_origin(port, "https://evil.example.com");
    let result = tokio_tungstenite::connect_async(req).await;

    match result {
        Err(tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status(), 403);
        }
        other => panic!("Expected HTTP 403 error, got: {:?}", other),
    }
}

#[tokio::test]
async fn origin_missing_rejected() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::Restricted {
            allow_localhost: false,
            extra: vec!["https://app.example.com".to_string()],
        },
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let _handler = tokio::spawn(scan_agent_lib::command_handler(
        handle.command_rx,
        handle.event_tx.clone(),
        None,
    ));

    // connect_async with a plain URL does not send an Origin header
    let url = format!("ws://127.0.0.1:{}", port);
    let result = tokio_tungstenite::connect_async(&url).await;

    match result {
        Err(tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status(), 403);
        }
        other => panic!("Expected HTTP 403 error, got: {:?}", other),
    }
}

// Cancel scan error path

#[tokio::test]
async fn cancel_unknown_scan_returns_error() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::AllowAll,
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    let url = format!("ws://127.0.0.1:{}", port);
    let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut tx, mut rx) = ws_stream.split();

    let cancel_msg = r#"{"type":"cancel_scan","id":"cancel-1","scan_id":"nonexistent"}"#;
    tx.send(Message::Text(cancel_msg.into())).await.unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), rx.next())
        .await
        .expect("Timeout")
        .expect("Stream ended")
        .expect("WS error");

    let v: serde_json::Value = serde_json::from_str(&response.into_text().unwrap()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["id"], "cancel-1");
    assert_eq!(v["code"], "INVALID_REQUEST");

    handler.abort();
}

// --- OriginPolicy: localhost-only default behavior ---

async fn connect_with_origin(
    port: u16,
    origin: Option<&str>,
) -> Result<(), tungstenite::Error> {
    let url = format!("ws://127.0.0.1:{port}");
    let mut req = tungstenite::http::Request::builder()
        .uri(&url)
        .header("Host", format!("127.0.0.1:{port}"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key());
    if let Some(o) = origin {
        req = req.header("Origin", o);
    }
    let req = req.body(()).unwrap();
    tokio_tungstenite::connect_async(req).await.map(|_| ())
}

#[tokio::test]
async fn restricted_allows_http_localhost() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::Restricted {
            allow_localhost: true,
            extra: vec![],
        },
        auth_token: None,
    };
    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    connect_with_origin(port, Some("http://localhost:4200")).await.expect("localhost should connect");
    handler.abort();
}

#[tokio::test]
async fn restricted_allows_127_0_0_1() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::Restricted {
            allow_localhost: true,
            extra: vec![],
        },
        auth_token: None,
    };
    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    connect_with_origin(port, Some("http://127.0.0.1:8080")).await.expect("127.0.0.1 should connect");
    handler.abort();
}

#[tokio::test]
async fn restricted_allows_ipv6_loopback() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::Restricted {
            allow_localhost: true,
            extra: vec![],
        },
        auth_token: None,
    };
    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    connect_with_origin(port, Some("http://[::1]:4200")).await.expect("[::1] should connect");
    handler.abort();
}

#[tokio::test]
async fn restricted_rejects_internet_origin() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::Restricted {
            allow_localhost: true,
            extra: vec![],
        },
        auth_token: None,
    };
    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    let err = connect_with_origin(port, Some("https://evil.example.com")).await
        .expect_err("evil.example.com must be rejected");
    let msg = format!("{err}");
    assert!(msg.contains("403") || msg.contains("Forbidden"), "expected 403, got {msg}");
    handler.abort();
}

#[tokio::test]
async fn restricted_rejects_missing_origin() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::Restricted {
            allow_localhost: true,
            extra: vec![],
        },
        auth_token: None,
    };
    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    let err = connect_with_origin(port, None).await
        .expect_err("missing Origin must be rejected under Restricted");
    let msg = format!("{err}");
    assert!(msg.contains("403") || msg.contains("Forbidden"), "expected 403, got {msg}");
    handler.abort();
}

#[tokio::test]
async fn restricted_extra_origin_exact_match_allowed() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::Restricted {
            allow_localhost: false,
            extra: vec!["https://app.example.com".to_string()],
        },
        auth_token: None,
    };
    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    connect_with_origin(port, Some("https://app.example.com")).await.expect("exact match");
    let err = connect_with_origin(port, Some("http://localhost:4200")).await
        .expect_err("localhost rejected when allow_localhost=false");
    let msg = format!("{err}");
    assert!(msg.contains("403") || msg.contains("Forbidden"), "expected 403, got {msg}");
    handler.abort();
}

// Concurrent scan rejection (uses fake sidecar as mock scanner)

const FAKE_SIDECAR: &str = env!("CARGO_BIN_EXE_fake_sidecar");

/// The fake sidecar is configured through environment variables, which are
/// process-global and therefore shared by every test in this binary. Tests
/// that depend on a particular fake-sidecar configuration take this lock so
/// they cannot observe each other's settings.
/// A tokio mutex rather than a `std` one: the guard is held across awaits for
/// the whole body of a test, which a blocking mutex must not be.
static SIDECAR_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Sets fake-sidecar environment variables and restores the previous values
/// on drop, so a failing assertion cannot leak configuration into the next
/// test that takes `SIDECAR_ENV`.
struct SidecarEnv {
    previous: Vec<(&'static str, Option<String>)>,
}

impl SidecarEnv {
    fn set(vars: &[(&'static str, &str)]) -> Self {
        let previous = vars
            .iter()
            .map(|(k, v)| {
                let old = std::env::var(k).ok();
                std::env::set_var(k, v);
                (*k, old)
            })
            .collect();
        Self { previous }
    }
}

impl Drop for SidecarEnv {
    fn drop(&mut self) {
        for (key, value) in &self.previous {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// Drive a scan against the fake sidecar and confirm a second concurrent
/// `start_scan` is rejected with `SCANNER_BUSY` while the first is in-flight.
#[tokio::test]
async fn concurrent_start_scan_returns_busy() {
    // Relies on the default scan delay holding the first scan open.
    let _env = SIDECAR_ENV.lock().await;

    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::AllowAll,
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(
        handle.command_rx,
        event_tx,
        Some(FAKE_SIDECAR.to_string()),
    ));

    let url = format!("ws://127.0.0.1:{}", port);
    let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut tx, mut rx) = ws_stream.split();

    // 1) Discover scanners — fake sidecar reports "Fake Scanner".
    tx.send(Message::Text(
        r#"{"type":"list_scanners","id":"ls-1"}"#.into(),
    ))
    .await
    .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(15), rx.next())
        .await
        .expect("Timeout waiting for scanner_list")
        .expect("Stream ended")
        .expect("WS error");
    let v: serde_json::Value = serde_json::from_str(&response.into_text().unwrap()).unwrap();
    assert_eq!(
        v["type"], "scanner_list",
        "expected scanner_list, got: {v}"
    );
    let scanners = v["scanners"].as_array().expect("scanners is an array");
    assert!(
        scanners.iter().any(|s| s["name"] == "Fake Scanner"),
        "Fake Scanner not present in {scanners:?}"
    );

    // 2) Kick off a slow scan against the fake.
    tx.send(Message::Text(
        r#"{"type":"start_scan","id":"scan-1","options":{"scanner_id":"Fake Scanner","format":"png"}}"#.into(),
    ))
    .await
    .unwrap();

    // Yield long enough for task A to set the scanning flag and begin the
    // sidecar handshake before we send the second start_scan. The fake's
    // configured scan delay (1s) gives a comfortable window.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    // 3) Concurrent start_scan — must be rejected immediately.
    tx.send(Message::Text(
        r#"{"type":"start_scan","id":"scan-2","options":{"scanner_id":"Fake Scanner","format":"png"}}"#.into(),
    ))
    .await
    .unwrap();

    // 4) Read responses until we observe both:
    //    - SCANNER_BUSY error correlated to scan-2
    //    - scan_complete correlated to scan-1 (proves the first scan was
    //      driven through the orchestrator + sidecar end-to-end and finished
    //      cleanly, releasing the busy flag).
    let mut saw_busy = false;
    let mut saw_complete = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline && !(saw_busy && saw_complete) {
        let remaining = deadline - std::time::Instant::now();
        let next = tokio::time::timeout(remaining, rx.next()).await;
        let msg = match next {
            Ok(Some(Ok(m))) => m,
            _ => break,
        };
        let text = match msg.into_text() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let msg_type = v["type"].as_str().unwrap_or("");
        let msg_id = v["id"].as_str().unwrap_or("");
        match (msg_type, msg_id) {
            ("error", "scan-2") => {
                assert_eq!(
                    v["code"], "SCANNER_BUSY",
                    "expected SCANNER_BUSY for scan-2, got: {v}"
                );
                saw_busy = true;
            }
            ("scan_complete", "scan-1") => {
                saw_complete = true;
            }
            ("error", "scan-1") => {
                panic!("scan-1 unexpectedly errored: {v}");
            }
            _ => { /* ignore progress / other frames */ }
        }
    }

    assert!(saw_busy, "did not observe SCANNER_BUSY rejection for scan-2");
    assert!(
        saw_complete,
        "first scan did not complete cleanly within deadline"
    );

    handler.abort();
}

#[tokio::test]
async fn allow_all_accepts_anything() {
    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::AllowAll,
        auth_token: None,
    };
    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    connect_with_origin(port, Some("https://anything.example.com")).await.expect("AllowAll");
    connect_with_origin(port, None).await.expect("AllowAll missing-origin");
    handler.abort();
}

/// One completed binary transfer: what it carried and the bytes that arrived.
struct Transfer {
    kind: String,
    page: Option<u64>,
    bytes: Vec<u8>,
}

/// Reassembles `binary_start` announcements and the binary frames that follow.
///
/// Driven by `total_bytes`, exactly as a real client must be: the chunk size
/// is an implementation detail, so no frame count is assumed anywhere here.
#[derive(Default)]
struct TransferCollector {
    /// kind, page, announced size, bytes so far.
    pending: Option<(String, Option<u64>, usize, Vec<u8>)>,
}

impl TransferCollector {
    fn begin(&mut self, msg: &serde_json::Value) {
        assert!(
            self.pending.is_none(),
            "a new transfer began before the previous one finished"
        );
        self.pending = Some((
            msg["kind"].as_str().expect("kind").to_string(),
            msg["page"].as_u64(),
            msg["total_bytes"].as_u64().expect("total_bytes") as usize,
            Vec::new(),
        ));
    }

    /// Absorb one binary frame, yielding the transfer once it is whole.
    fn chunk(&mut self, chunk: &[u8]) -> Option<Transfer> {
        let (kind, page, total, buf) = self
            .pending
            .as_mut()
            .expect("binary frame arrived with no preceding binary_start");
        buf.extend_from_slice(chunk);
        if buf.len() < *total {
            return None;
        }
        assert_eq!(buf.len(), *total, "transfer overshot total_bytes");
        let transfer = Transfer {
            kind: kind.clone(),
            page: *page,
            bytes: std::mem::take(buf),
        };
        self.pending = None;
        Some(transfer)
    }

    fn is_idle(&self) -> bool {
        self.pending.is_none()
    }
}

/// A sidecar that emits real page bitmaps exercises the stride-handling path:
/// the source reports padded rows, and the agent must strip that padding
/// rather than hand a mis-sized buffer to the image encoder. The reassembled
/// frames are decoded, so a mangled stride shows up as a broken image rather
/// than as bytes nobody looked at.
#[tokio::test]
async fn sidecar_pages_with_padded_rows_reach_the_client() {
    let _env = SIDECAR_ENV.lock().await;

    let config = WsServerConfig {
        port: 0,
        origin_policy: scan_agent_lib::ws_server::OriginPolicy::AllowAll,
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(
        handle.command_rx,
        event_tx,
        Some(FAKE_SIDECAR.to_string()),
    ));

    let _sidecar_env = SidecarEnv::set(&[
        ("FAKE_SIDECAR_EMIT_PAGES", "1"),
        ("FAKE_SIDECAR_SCAN_DELAY_MS", "0"),
    ]);

    let url = format!("ws://127.0.0.1:{}", port);
    let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut tx, mut rx) = ws_stream.split();

    // Discovery has to run before the orchestrator can resolve a scanner name.
    tx.send(Message::Text(
        r#"{"type":"list_scanners","id":"ls-pad"}"#.into(),
    ))
    .await
    .unwrap();
    let listed = tokio::time::timeout(std::time::Duration::from_secs(15), rx.next())
        .await
        .expect("Timeout waiting for scanner_list")
        .expect("Stream ended")
        .expect("WS error");
    let listed: serde_json::Value = serde_json::from_str(&listed.into_text().unwrap()).unwrap();
    assert_eq!(listed["type"], "scanner_list", "expected scanner_list, got: {listed}");

    tx.send(Message::Text(
        r#"{"type":"start_scan","id":"scan-pad","options":{"scanner_id":"Fake Scanner","format":"png"}}"#.into(),
    ))
    .await
    .unwrap();

    let mut collector = TransferCollector::default();
    let mut pages: Vec<Transfer> = Vec::new();
    let mut completed = false;

    for _ in 0..200 {
        let response = tokio::time::timeout(std::time::Duration::from_secs(15), rx.next())
            .await
            .expect("Timeout waiting for scan messages")
            .expect("Stream ended")
            .expect("WS error");

        match response {
            Message::Text(text) => {
                let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                match v["type"].as_str() {
                    Some("binary_start") => collector.begin(&v),
                    Some("scan_complete") => {
                        assert_eq!(v["total_pages"], 2, "got: {v}");
                        completed = true;
                        break;
                    }
                    Some("error") => panic!("scan failed: {v}"),
                    _ => {}
                }
            }
            Message::Binary(chunk) => {
                if let Some(transfer) = collector.chunk(&chunk) {
                    pages.push(transfer);
                }
            }
            _ => {}
        }
    }

    handler.abort();

    assert!(completed, "never received scan_complete");
    assert!(collector.is_idle(), "a transfer was left unfinished");
    assert_eq!(pages.len(), 2, "expected both emitted pages to arrive");

    // PNG output sends the full-resolution image, one transfer per page, and
    // the decoded sizes are the fake's — padding stripped, nothing shifted.
    for (index, expected) in [(0usize, (3u32, 2u32)), (1, (16, 2))] {
        let page = &pages[index];
        assert_eq!(page.kind, "page", "PNG output must send full pages");
        assert_eq!(page.page, Some(index as u64 + 1), "pages arrived out of order");
        let decoded =
            image::load_from_memory(&page.bytes).expect("reassembled bytes must decode as PNG");
        assert_eq!(
            (decoded.width(), decoded.height()),
            expected,
            "page {} decoded at the wrong size",
            index + 1
        );
    }
}

/// A full ADF hopper. The old pipeline held every page plus the assembled
/// document in memory; this asserts the batch completes and arrives intact.
#[tokio::test]
async fn a_sixty_page_batch_streams_to_completion() {
    let _env = SIDECAR_ENV.lock().await;
    let _sidecar_env = SidecarEnv::set(&[
        ("FAKE_SIDECAR_EMIT_PAGES", "1"),
        ("FAKE_SIDECAR_PAGE_COUNT", "60"),
        ("FAKE_SIDECAR_SCAN_DELAY_MS", "0"),
    ]);

    let agent_config = scan_agent_lib::config::AgentConfig::default();
    let mut config: WsServerConfig = (&agent_config).into();
    // The default config's origin policy is what this test wants; its fixed
    // port is not, since every server in this binary runs concurrently.
    config.port = 0;
    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(
        handle.command_rx,
        event_tx,
        Some(FAKE_SIDECAR.to_string()),
    ));

    let request = ws_request_with_origin(port, "http://localhost:4200");
    let (ws_stream, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let (mut tx, mut rx) = ws_stream.split();

    // Discovery must run before the orchestrator can resolve a scanner name.
    tx.send(Message::Text(r#"{"type":"list_scanners","id":"ls-60"}"#.into()))
        .await
        .unwrap();
    let listed = tokio::time::timeout(std::time::Duration::from_secs(15), rx.next())
        .await
        .expect("timeout waiting for scanner_list")
        .expect("stream ended")
        .expect("ws error");
    let listed: serde_json::Value =
        serde_json::from_str(&listed.into_text().unwrap()).unwrap();
    assert_eq!(listed["type"], "scanner_list", "got: {listed}");

    tx.send(Message::Text(
        r#"{"type":"start_scan","id":"scan-60","options":{"scanner_id":"Fake Scanner","format":"pdf"}}"#.into(),
    ))
    .await
    .unwrap();

    let mut collector = TransferCollector::default();
    let mut thumbnails: Vec<Transfer> = Vec::new();
    let mut pdf_bytes: Vec<u8> = Vec::new();
    let mut total_pages: Option<u64> = None;

    for _ in 0..20_000 {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(30), rx.next())
            .await
            .expect("timeout waiting for scan traffic")
            .expect("stream ended")
            .expect("ws error");

        match msg {
            Message::Text(text) => {
                let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                match v["type"].as_str() {
                    Some("binary_start") => collector.begin(&v),
                    Some("scan_complete") => {
                        assert!(
                            v.get("pdf_data").is_none(),
                            "pdf_data must no longer be inlined"
                        );
                        total_pages = v["total_pages"].as_u64();
                        break;
                    }
                    Some("error") => panic!("scan failed: {v}"),
                    _ => {}
                }
            }
            Message::Binary(chunk) => {
                if let Some(transfer) = collector.chunk(&chunk) {
                    match transfer.kind.as_str() {
                        "thumbnail" => thumbnails.push(transfer),
                        "pdf" => pdf_bytes = transfer.bytes,
                        other => panic!("unexpected transfer kind: {other}"),
                    }
                }
            }
            _ => {}
        }
    }

    handler.abort();

    assert!(collector.is_idle(), "a transfer was left unfinished");
    assert_eq!(total_pages, Some(60), "scan_complete should report 60 pages");
    assert_eq!(thumbnails.len(), 60, "expected one thumbnail per page");
    assert_eq!(
        thumbnails.iter().filter_map(|t| t.page).collect::<Vec<_>>(),
        (1..=60).collect::<Vec<u64>>(),
        "thumbnails must be numbered 1..60 in order"
    );
    for thumb in &thumbnails {
        image::load_from_memory(&thumb.bytes).expect("thumbnail must decode as an image");
    }
    assert!(
        pdf_bytes.starts_with(b"%PDF-1.4"),
        "reassembled bytes are not a PDF"
    );
    assert!(
        String::from_utf8_lossy(&pdf_bytes).contains("/Count 60"),
        "PDF should declare 60 pages"
    );
    assert!(
        String::from_utf8_lossy(&pdf_bytes).contains("%%EOF"),
        "PDF was never closed"
    );
}

/// A producer that fails only after its last page has been handed over must
/// not have cost the client a `Processing` message or the agent a finished
/// document: the error that follows would throw both away. The ordering here
/// — producer checked, then PDF finalized — is the whole point of the split
/// between `consume_pages` and `finalize_pdf`.
#[tokio::test]
async fn a_late_producer_failure_finalizes_no_pdf() {
    let _env = SIDECAR_ENV.lock().await;
    let _sidecar_env = SidecarEnv::set(&[
        ("FAKE_SIDECAR_EMIT_PAGES", "1"),
        ("FAKE_SIDECAR_PAGE_COUNT", "3"),
        ("FAKE_SIDECAR_SCAN_DELAY_MS", "0"),
        ("FAKE_SIDECAR_ERROR_AFTER_PAGES", "1"),
    ]);

    let agent_config = scan_agent_lib::config::AgentConfig::default();
    let mut config: WsServerConfig = (&agent_config).into();
    // The default config's origin policy is what this test wants; its fixed
    // port is not, since every server in this binary runs concurrently.
    config.port = 0;
    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(
        handle.command_rx,
        event_tx,
        Some(FAKE_SIDECAR.to_string()),
    ));

    let request = ws_request_with_origin(port, "http://localhost:4200");
    let (ws_stream, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let (mut tx, mut rx) = ws_stream.split();

    tx.send(Message::Text(r#"{"type":"list_scanners","id":"ls-late"}"#.into()))
        .await
        .unwrap();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), rx.next())
        .await
        .expect("timeout")
        .expect("stream ended")
        .expect("ws error");

    tx.send(Message::Text(
        r#"{"type":"start_scan","id":"scan-late","options":{"scanner_id":"Fake Scanner","format":"pdf"}}"#.into(),
    ))
    .await
    .unwrap();

    let mut thumbnails = 0usize;
    let mut kinds: Vec<String> = Vec::new();
    let mut statuses: Vec<String> = Vec::new();
    let mut failed = false;

    let mut collector = TransferCollector::default();
    for _ in 0..500 {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(30), rx.next())
            .await
            .expect("timeout waiting for scan traffic")
            .expect("stream ended")
            .expect("ws error");

        match msg {
            Message::Text(text) => {
                let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                match v["type"].as_str() {
                    Some("binary_start") => {
                        kinds.push(v["kind"].as_str().unwrap_or_default().to_string());
                        collector.begin(&v);
                    }
                    Some("scan_progress") => {
                        statuses.push(v["status"].as_str().unwrap_or_default().to_string());
                    }
                    Some("scan_complete") => panic!("a failed scan must not report completion: {v}"),
                    Some("error") if v["id"] == "scan-late" => {
                        failed = true;
                        break;
                    }
                    _ => {}
                }
            }
            Message::Binary(chunk) => {
                if let Some(transfer) = collector.chunk(&chunk) {
                    if transfer.kind == "thumbnail" {
                        thumbnails += 1;
                    }
                }
            }
            _ => {}
        }
    }

    handler.abort();

    assert!(failed, "the scan never reported the producer's failure");
    assert_eq!(thumbnails, 3, "every page should still have been previewed");
    assert!(
        !kinds.iter().any(|k| k == "pdf"),
        "a document was transferred for a scan that failed: {kinds:?}"
    );
    assert!(
        !statuses.iter().any(|s| s == "processing"),
        "client was told the PDF was being assembled, then handed an error: {statuses:?}"
    );
}

/// Cancelling mid-batch must not leave the partially written document behind.
#[tokio::test]
async fn cancelling_a_scan_removes_the_temp_pdf() {
    let _env = SIDECAR_ENV.lock().await;

    /// Temp directories holding a partially written scan, if any leaked.
    fn leaked_scan_files() -> std::collections::HashSet<std::path::PathBuf> {
        let mut found = std::collections::HashSet::new();
        if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("scan.pdf");
                if candidate.exists() {
                    found.insert(candidate);
                }
            }
        }
        found
    }

    let before = leaked_scan_files();

    let _sidecar_env = SidecarEnv::set(&[
        ("FAKE_SIDECAR_EMIT_PAGES", "1"),
        ("FAKE_SIDECAR_PAGE_COUNT", "40"),
        ("FAKE_SIDECAR_SCAN_DELAY_MS", "50"),
    ]);

    let agent_config = scan_agent_lib::config::AgentConfig::default();
    let mut config: WsServerConfig = (&agent_config).into();
    // The default config's origin policy is what this test wants; its fixed
    // port is not, since every server in this binary runs concurrently.
    config.port = 0;
    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(
        handle.command_rx,
        event_tx,
        Some(FAKE_SIDECAR.to_string()),
    ));

    let request = ws_request_with_origin(port, "http://localhost:4200");
    let (ws_stream, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let (mut tx, mut rx) = ws_stream.split();

    tx.send(Message::Text(r#"{"type":"list_scanners","id":"ls-c"}"#.into()))
        .await
        .unwrap();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), rx.next())
        .await
        .expect("timeout")
        .expect("stream ended")
        .expect("ws error");

    tx.send(Message::Text(
        r#"{"type":"start_scan","id":"scan-c","options":{"scanner_id":"Fake Scanner","format":"pdf"}}"#.into(),
    ))
    .await
    .unwrap();

    // Let a few pages land, then cancel. The scan_id comes from the first
    // scan_progress message.
    let mut scan_id = String::new();
    for _ in 0..200 {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(30), rx.next())
            .await
            .expect("timeout")
            .expect("stream ended")
            .expect("ws error");
        if let Message::Text(text) = msg {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            if v["type"] == "scan_progress" {
                scan_id = v["scan_id"].as_str().unwrap_or_default().to_string();
                if v["page"].as_u64().unwrap_or(0) >= 3 {
                    break;
                }
            }
        }
    }
    assert!(!scan_id.is_empty(), "never saw a scan_progress message");

    tx.send(Message::Text(
        format!(r#"{{"type":"cancel_scan","id":"c-1","scan_id":"{scan_id}"}}"#),
    ))
    .await
    .unwrap();

    // The scan has to actually unwind as cancelled: a run that quietly
    // finished would also leave no temp file, and would prove nothing.
    let mut cancelled = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        let remaining = deadline - std::time::Instant::now();
        let Ok(Some(Ok(msg))) = tokio::time::timeout(remaining, rx.next()).await else {
            break;
        };
        if let Message::Text(text) = msg {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            match v["type"].as_str() {
                Some("error") if v["id"] == "scan-c" => {
                    assert_eq!(v["code"], "SCAN_CANCELLED", "expected a cancellation: {v}");
                    cancelled = true;
                    break;
                }
                Some("scan_complete") => panic!("the batch finished instead of cancelling: {v}"),
                _ => {}
            }
        }
    }

    handler.abort();

    assert!(cancelled, "the scan never reported cancellation");

    let after = leaked_scan_files();
    let leaked: Vec<_> = after.difference(&before).collect();
    assert!(
        leaked.is_empty(),
        "cancelled scan left temp files behind: {leaked:?}"
    );
}

/// A browser connecting to a release-shaped agent, with a valid Origin and no
/// token, must be served. This is the configuration the MSI ships, and it
/// rejected every connection until the token was made opt-in.
#[tokio::test]
async fn default_config_accepts_a_browser_connection_without_a_token() {
    let agent_config = scan_agent_lib::config::AgentConfig::default();
    let config: WsServerConfig = (&agent_config).into();

    assert!(
        config.auth_token.is_none(),
        "a default config must not demand a token"
    );

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    // No ?token= in the URL, and an Origin a browser would really send.
    let request = ws_request_with_origin(port, "http://localhost:4200");
    let (ws_stream, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("a default install must accept a localhost browser origin");
    let (mut tx, mut rx) = ws_stream.split();

    tx.send(Message::Text(r#"{"type":"ping","id":"no-token"}"#.into()))
        .await
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), rx.next())
        .await
        .expect("Timeout waiting for pong")
        .expect("Stream ended")
        .expect("WS error");
    let v: serde_json::Value = serde_json::from_str(&response.into_text().unwrap()).unwrap();
    assert_eq!(v["type"], "pong");
    assert_eq!(v["id"], "no-token");

    handler.abort();
}

/// A page that sends a token to an agent with none configured must still be
/// served — an extra query parameter is not an error.
#[tokio::test]
async fn unexpected_token_is_ignored_when_none_is_configured() {
    let agent_config = scan_agent_lib::config::AgentConfig::default();
    let config: WsServerConfig = (&agent_config).into();

    let handle = ws_server::start_server(config).await.unwrap();
    let port = handle.port;
    let event_tx = handle.event_tx.clone();
    let handler = tokio::spawn(scan_agent_lib::command_handler(handle.command_rx, event_tx, None));

    let request = tungstenite::http::Request::builder()
        .uri(format!("ws://127.0.0.1:{}/?token=unnecessary", port))
        .header("Host", format!("127.0.0.1:{}", port))
        .header("Origin", "http://localhost:4200")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key())
        .body(())
        .unwrap();

    let (ws_stream, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("an unexpected token must not be rejected");
    let (mut tx, mut rx) = ws_stream.split();

    tx.send(Message::Text(r#"{"type":"ping","id":"extra-token"}"#.into()))
        .await
        .unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(5), rx.next())
        .await
        .expect("Timeout waiting for pong")
        .expect("Stream ended")
        .expect("WS error");
    let v: serde_json::Value = serde_json::from_str(&response.into_text().unwrap()).unwrap();
    assert_eq!(v["type"], "pong");

    handler.abort();
}
