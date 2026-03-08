//! WebSocket integration tests.
//!
//! These tests spin up the real WS server and connect a client,
//! verifying the full message round-trip without needing scanner hardware.

use futures_util::{SinkExt, StreamExt};
use scan_agent_lib::ws_server::{self, WsServerConfig};
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::Message;

/// Find an available port by binding to port 0
async fn get_free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn ping_pong() {
    let port = get_free_port().await;
    let config = WsServerConfig {
        port,
        allowed_origins: Vec::new(),
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();

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
    let port = get_free_port().await;
    let config = WsServerConfig {
        port,
        allowed_origins: Vec::new(),
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
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
    let port = get_free_port().await;
    let config = WsServerConfig {
        port,
        allowed_origins: Vec::new(),
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
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
    let port = get_free_port().await;
    let config = WsServerConfig {
        port,
        allowed_origins: Vec::new(),
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
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

// ============================================================
// Auth token tests
// ============================================================

#[tokio::test]
async fn auth_token_valid_connects() {
    let port = get_free_port().await;
    let config = WsServerConfig {
        port,
        allowed_origins: Vec::new(),
        auth_token: Some("secret".to_string()),
    };

    let handle = ws_server::start_server(config).await.unwrap();
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
    let port = get_free_port().await;
    let config = WsServerConfig {
        port,
        allowed_origins: Vec::new(),
        auth_token: Some("secret".to_string()),
    };

    let handle = ws_server::start_server(config).await.unwrap();
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
    let port = get_free_port().await;
    let config = WsServerConfig {
        port,
        allowed_origins: Vec::new(),
        auth_token: Some("secret".to_string()),
    };

    let handle = ws_server::start_server(config).await.unwrap();
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

// ============================================================
// Origin validation tests
// ============================================================

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
    let port = get_free_port().await;
    let config = WsServerConfig {
        port,
        allowed_origins: vec!["https://app.example.com".to_string()],
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
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
    let port = get_free_port().await;
    let config = WsServerConfig {
        port,
        allowed_origins: vec!["https://app.example.com".to_string()],
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
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
    let port = get_free_port().await;
    let config = WsServerConfig {
        port,
        allowed_origins: vec!["https://app.example.com".to_string()],
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
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

// ============================================================
// Cancel scan error path
// ============================================================

#[tokio::test]
async fn cancel_unknown_scan_returns_error() {
    let port = get_free_port().await;
    let config = WsServerConfig {
        port,
        allowed_origins: Vec::new(),
        auth_token: None,
    };

    let handle = ws_server::start_server(config).await.unwrap();
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
