//! WebSocket integration tests.
//!
//! These tests spin up the real WS server and connect a client,
//! verifying the full message round-trip without needing scanner hardware.

use futures_util::{SinkExt, StreamExt};
use scan_agent_lib::ws_server::{self, WsServerConfig};
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
