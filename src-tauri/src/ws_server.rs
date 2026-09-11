//! WebSocket server for communication between the Angular app and the scan agent.
//!
//! Binds to `127.0.0.1` on a configurable port and validates the `Origin` header
//! to prevent unauthorized connections from malicious web pages.

use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::protocol::{AgentMessage, ClientMessage};

/// Default WebSocket port
pub const DEFAULT_WS_PORT: u16 = 47115;

/// Origin acceptance policy for the WebSocket handshake.
#[derive(Debug, Clone)]
pub enum OriginPolicy {
    /// Accept any origin (and missing Origin headers). Dev-mode only.
    AllowAll,
    /// Accept localhost (when `allow_localhost`) plus exact matches in `extra`.
    /// Production builds always use this variant.
    Restricted {
        /// Accept http(s)://localhost(:any-port), 127.0.0.1, and [::1].
        allow_localhost: bool,
        /// Additional exact-match origin strings (e.g., "https://app.example.com").
        extra: Vec<String>,
    },
}

/// Configuration for the WebSocket server
#[derive(Debug, Clone)]
pub struct WsServerConfig {
    pub port: u16,
    /// Origin acceptance policy. `AllowAll` is dev-only.
    pub origin_policy: OriginPolicy,
    /// Auth token for WebSocket connections. If `Some`, clients must include `?token=<value>`
    /// in the connection URL. If `None`, no authentication is required (development mode).
    pub auth_token: Option<String>,
}

impl Default for WsServerConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_WS_PORT,
            origin_policy: OriginPolicy::AllowAll,
            auth_token: None,
        }
    }
}

/// Channel type for sending commands from WS connections to the scanner orchestrator
pub type CommandSender = mpsc::UnboundedSender<(ClientMessage, ResponseSender)>;
pub type CommandReceiver = mpsc::UnboundedReceiver<(ClientMessage, ResponseSender)>;

/// Chunk size for binary transfers. Comfortably below any frame limit, and
/// small enough that a bounded channel throttles a slow client promptly.
pub const CHUNK_BYTES: usize = 256 * 1024;

/// Depth of the per-connection response queue. Deliberately small: this is
/// the backpressure that keeps a large transfer from accumulating in memory.
///
/// The writer task is the only consumer and never sends on this channel, so
/// bounding it cannot deadlock. Any future code that sends from the writer
/// path would stall the pipeline.
pub const RESPONSE_CHANNEL_CAPACITY: usize = 8;

/// Something to write to one client's socket.
#[derive(Debug)]
pub enum OutgoingMessage {
    /// A protocol message, serialised to a text frame.
    Json(Box<AgentMessage>),
    /// One chunk of a binary transfer announced by a preceding `BinaryStart`.
    Binary(Vec<u8>),
}

/// Channel for sending responses back to a specific WS connection
pub type ResponseSender = mpsc::Sender<OutgoingMessage>;
pub type ResponseReceiver = mpsc::Receiver<OutgoingMessage>;

/// Queue a protocol message. Errors are ignored: a disconnected client is
/// normal, and every caller previously discarded the send result too.
pub async fn send_json(tx: &ResponseSender, msg: AgentMessage) {
    let _ = tx.send(OutgoingMessage::Json(Box::new(msg))).await;
}

/// Queue a payload as a sequence of `CHUNK_BYTES` frames.
///
/// Awaits on each frame, so a client that reads slowly slows the scan rather
/// than causing the agent to buffer the whole transfer.
pub async fn send_binary(
    tx: &ResponseSender,
    payload: &[u8],
) -> Result<(), mpsc::error::SendError<OutgoingMessage>> {
    for chunk in payload.chunks(CHUNK_BYTES) {
        tx.send(OutgoingMessage::Binary(chunk.to_vec())).await?;
    }
    Ok(())
}

/// Broadcast channel for scanner events that should go to all connected clients
pub type EventSender = broadcast::Sender<AgentMessage>;

/// Handle to the running WebSocket server
pub struct WsServerHandle {
    shutdown_tx: broadcast::Sender<()>,
    pub command_rx: CommandReceiver,
    pub event_tx: EventSender,
    /// The port actually bound. Equals the configured port, except when the
    /// config asked for 0, where the OS chooses and this reports the choice.
    pub port: u16,
}

impl WsServerHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

/// Start the WebSocket server
pub async fn start_server(config: WsServerConfig) -> Result<WsServerHandle, Box<dyn std::error::Error + Send + Sync>> {
    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    let listener = TcpListener::bind(addr).await?;
    // With port 0 the OS assigns one, so read back what we actually got.
    let bound_port = listener.local_addr()?.port();
    info!("WebSocket server listening on ws://127.0.0.1:{}", bound_port);

    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, _) = broadcast::channel::<AgentMessage>(64);
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    let config = Arc::new(config);
    let event_tx_clone = event_tx.clone();
    let mut shutdown_rx = shutdown_tx.subscribe();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, peer_addr)) => {
                            debug!("New connection from: {}", peer_addr);
                            let cmd_tx = command_tx.clone();
                            let evt_rx = event_tx_clone.subscribe();
                            let cfg = config.clone();
                            tokio::spawn(handle_connection(stream, peer_addr, cmd_tx, evt_rx, cfg));
                        }
                        Err(e) => {
                            error!("Failed to accept connection: {}", e);
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("WebSocket server shutting down");
                    // Notify connected clients before exiting
                    let _ = event_tx_clone.send(AgentMessage::ServerShutdown);
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    break;
                }
            }
        }
    });

    Ok(WsServerHandle {
        shutdown_tx,
        command_rx,
        event_tx,
        port: bound_port,
    })
}

// tungstenite's handshake callback returns a Result whose Err carries a full
// HTTP Response (~136 bytes) — outside our control, so accept the large variant.
#[allow(clippy::result_large_err)]
async fn handle_connection(
    stream: TcpStream,
    peer_addr: SocketAddr,
    command_tx: CommandSender,
    mut event_rx: broadcast::Receiver<AgentMessage>,
    config: Arc<WsServerConfig>,
) {
    // Accept WebSocket upgrade with origin validation
    let config_clone = config.clone();
    let ws_stream = tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, resp: Response| {
        validate_handshake(req, &config_clone, resp)
    })
    .await;

    let ws_stream = match ws_stream {
        Ok(ws) => ws,
        Err(e) => {
            warn!("WebSocket handshake failed for {}: {}", peer_addr, e);
            return;
        }
    };

    info!("WebSocket connection established: {}", peer_addr);

    let (ws_tx, mut ws_rx) = ws_stream.split();
    let (response_tx, mut response_rx): (ResponseSender, ResponseReceiver) =
        mpsc::channel(RESPONSE_CHANNEL_CAPACITY);

    // Task: Forward responses and events to this client
    let ws_tx = Arc::new(Mutex::new(ws_tx));
    let ws_tx_events = ws_tx.clone();
    let ws_tx_responses = ws_tx.clone();

    // Watch channel to signal per-connection tasks to shut down cooperatively
    let (close_tx, _) = watch::channel(false);
    let mut close_rx_resp = close_tx.subscribe();
    let mut close_rx_evt = close_tx.subscribe();
    let mut close_rx_ping = close_tx.subscribe();

    // Forward direct responses to this client
    let response_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = response_rx.recv() => {
                    match msg {
                        Some(OutgoingMessage::Json(m)) => {
                            if let Ok(json) = serde_json::to_string(&*m) {
                                let mut tx = ws_tx_responses.lock().await;
                                if tx.send(Message::Text(json)).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Some(OutgoingMessage::Binary(bytes)) => {
                            let mut tx = ws_tx_responses.lock().await;
                            if tx.send(Message::Binary(bytes)).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = close_rx_resp.changed() => break,
            }
        }
    });

    // Forward broadcast events to this client
    let event_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                result = event_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let mut tx = ws_tx_events.lock().await;
                                if tx.send(Message::Text(json)).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("Client {} lagged {} events", peer_addr, n);
                        }
                    }
                }
                _ = close_rx_evt.changed() => break,
            }
        }
    });

    // Server-side heartbeat: send WebSocket pings every 30s to detect dead connections
    let ws_tx_ping = ws_tx.clone();
    let ping_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        interval.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let mut tx = ws_tx_ping.lock().await;
                    if tx.send(Message::Ping(vec![])).await.is_err() {
                        break;
                    }
                }
                _ = close_rx_ping.changed() => break,
            }
        }
    });

    // Read incoming messages from this client
    while let Some(msg_result) = ws_rx.next().await {
        match msg_result {
            Ok(Message::Text(text)) => {
                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(client_msg) => {
                        debug!("Received from {}: {:?}", peer_addr, client_msg);
                        if command_tx.send((client_msg, response_tx.clone())).is_err() {
                            error!("Command channel closed");
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("Invalid message from {}: {}", peer_addr, e);
                        let error_msg = AgentMessage::Error {
                            id: String::new(),
                            code: crate::protocol::ErrorCode::InvalidRequest,
                            message: format!("Invalid message format: {}", e),
                        };
                        if let Ok(json) = serde_json::to_string(&error_msg) {
                            let mut tx = ws_tx.lock().await;
                            let _ = tx.send(Message::Text(json)).await;
                        }
                    }
                }
            }
            Ok(Message::Close(_)) => {
                info!("Client {} disconnected", peer_addr);
                break;
            }
            Ok(Message::Ping(data)) => {
                let mut tx = ws_tx.lock().await;
                let _ = tx.send(Message::Pong(data)).await;
            }
            Ok(_) => {} // Ignore binary frames for now
            Err(e) => {
                error!("WebSocket error for {}: {}", peer_addr, e);
                break;
            }
        }
    }

    // Signal per-connection tasks to shut down cooperatively
    let _ = close_tx.send(true);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let _ = response_task.await;
        let _ = event_task.await;
        let _ = ping_task.await;
    })
    .await;

    info!("Connection handler ended for {}", peer_addr);
}

/// Validate the WebSocket handshake: origin + auth token.
#[allow(clippy::result_large_err)]
fn validate_handshake(
    req: &Request,
    config: &WsServerConfig,
    resp: Response,
) -> Result<Response, tokio_tungstenite::tungstenite::http::Response<Option<String>>> {
    // --- Origin validation ---
    match &config.origin_policy {
        OriginPolicy::AllowAll => {} // dev only — accept anything
        OriginPolicy::Restricted { allow_localhost, extra } => {
            let Some(origin_str) = req
                .headers()
                .get("Origin")
                .and_then(|v| v.to_str().ok())
            else {
                warn!("Rejected connection: missing Origin header");
                return Err(reject_response(403, "Forbidden: Origin header required"));
            };

            let parsed = match url::Url::parse(origin_str) {
                Ok(u) => u,
                Err(_) => {
                    warn!("Rejected connection: malformed Origin '{}'", origin_str);
                    return Err(reject_response(403, "Forbidden: Malformed Origin"));
                }
            };

            // url::Url::host_str returns IPv6 addresses with brackets (e.g. "[::1]").
            let host = parsed.host_str().unwrap_or("");
            let scheme_ok = matches!(parsed.scheme(), "http" | "https");
            let is_localhost = scheme_ok
                && matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]");

            let accepted = (*allow_localhost && is_localhost)
                || extra.iter().any(|o| o == origin_str);

            if !accepted {
                warn!("Rejected connection from unauthorized origin: {}", origin_str);
                return Err(reject_response(403, "Forbidden: Origin not allowed"));
            }
        }
    }

    // --- Auth token validation ---
    if let Some(expected) = &config.auth_token {
        let provided = req.uri().query().and_then(parse_token_from_query);

        match provided {
            Some(ref token) if token == expected => {}
            _ => {
                warn!("Rejected connection: invalid or missing auth token");
                return Err(reject_response(401, "Unauthorized: Invalid or missing token"));
            }
        }
    }

    Ok(resp)
}

/// Build a rejection response for the WebSocket handshake.
fn reject_response(
    status: u16,
    body: &str,
) -> tokio_tungstenite::tungstenite::http::Response<Option<String>> {
    tokio_tungstenite::tungstenite::http::Response::builder()
        .status(status)
        .body(Some(body.to_string()))
        .unwrap_or_else(|_| {
            // Fallback: if the builder somehow fails, return a bare 500
            let mut resp = tokio_tungstenite::tungstenite::http::Response::new(Some(
                "Internal Server Error".to_string(),
            ));
            *resp.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::INTERNAL_SERVER_ERROR;
            resp
        })
}

/// Extract and percent-decode the `token` value from a URI query string.
fn parse_token_from_query(query: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key == "token" {
            Some(percent_decode(value))
        } else {
            None
        }
    })
}

/// Decode percent-encoded characters in a string (e.g., "%2F" -> "/").
fn percent_decode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(b) = from_hex(bytes[i + 1], bytes[i + 2]) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decode a pair of hex digits into a byte.
fn from_hex(hi: u8, lo: u8) -> Option<u8> {
    let h = match hi {
        b'0'..=b'9' => hi - b'0',
        b'a'..=b'f' => hi - b'a' + 10,
        b'A'..=b'F' => hi - b'A' + 10,
        _ => return None,
    };
    let l = match lo {
        b'0'..=b'9' => lo - b'0',
        b'a'..=b'f' => lo - b'a' + 10,
        b'A'..=b'F' => lo - b'A' + 10,
        _ => return None,
    };
    Some(h << 4 | l)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Binding port 0 lets the OS pick a free port. The handle must report
    /// which one, so callers never have to probe for a port and then race
    /// another process to bind it.
    #[tokio::test]
    async fn port_zero_binds_and_reports_the_actual_port() {
        let config = WsServerConfig {
            port: 0,
            origin_policy: OriginPolicy::AllowAll,
            auth_token: None,
        };

        let handle = start_server(config).await.expect("server starts on port 0");

        assert_ne!(handle.port, 0, "handle must report the OS-assigned port");
        tokio::net::TcpStream::connect(("127.0.0.1", handle.port))
            .await
            .expect("reported port should be connectable");
    }

    #[test]
    fn parse_token_basic() {
        assert_eq!(
            parse_token_from_query("token=abc123"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn parse_token_among_params() {
        assert_eq!(
            parse_token_from_query("foo=bar&token=secret&baz=1"),
            Some("secret".to_string())
        );
    }

    #[test]
    fn parse_token_missing() {
        assert_eq!(parse_token_from_query("foo=bar"), None);
    }

    #[test]
    fn parse_token_percent_encoded() {
        assert_eq!(
            parse_token_from_query("token=hello%20world"),
            Some("hello world".to_string())
        );
    }

    #[test]
    fn percent_decode_special_chars() {
        assert_eq!(percent_decode("a%2Fb%3Dc"), "a/b=c");
    }

    #[test]
    fn percent_decode_passthrough() {
        assert_eq!(percent_decode("no-encoding"), "no-encoding");
    }

    /// A bounded channel is what makes streaming real: without it, chunks pile
    /// up in the queue and the memory simply moves rather than shrinking.
    #[tokio::test]
    async fn send_binary_splits_payloads_into_chunks_and_applies_backpressure() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<OutgoingMessage>(RESPONSE_CHANNEL_CAPACITY);

        // Two and a half chunks.
        let payload = vec![7u8; CHUNK_BYTES * 2 + 5];
        let sender = tokio::spawn(async move {
            send_binary(&tx, &payload).await.unwrap();
        });

        let mut received = Vec::new();
        let mut frames = 0;
        while let Some(msg) = rx.recv().await {
            match msg {
                OutgoingMessage::Binary(bytes) => {
                    frames += 1;
                    assert!(bytes.len() <= CHUNK_BYTES, "frame exceeded the chunk size");
                    received.extend_from_slice(&bytes);
                }
                OutgoingMessage::Json(_) => panic!("send_binary must not emit JSON"),
            }
        }

        sender.await.unwrap();
        assert_eq!(frames, 3, "expected two full chunks and one remainder");
        assert_eq!(received.len(), CHUNK_BYTES * 2 + 5);
        assert!(received.iter().all(|&b| b == 7));
    }

    #[tokio::test]
    async fn send_binary_of_an_exact_multiple_emits_no_empty_trailing_frame() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<OutgoingMessage>(RESPONSE_CHANNEL_CAPACITY);
        let payload = vec![1u8; CHUNK_BYTES * 2];
        let sender = tokio::spawn(async move { send_binary(&tx, &payload).await.unwrap() });

        let mut frames = 0;
        while let Some(msg) = rx.recv().await {
            if let OutgoingMessage::Binary(b) = msg {
                assert!(!b.is_empty(), "empty frame emitted");
                frames += 1;
            }
        }
        sender.await.unwrap();
        assert_eq!(frames, 2);
    }

    #[tokio::test]
    async fn send_binary_reports_failure_when_the_client_is_gone() {
        let (tx, rx) = tokio::sync::mpsc::channel::<OutgoingMessage>(RESPONSE_CHANNEL_CAPACITY);
        drop(rx);
        assert!(send_binary(&tx, &[1, 2, 3]).await.is_err());
    }
}
