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

/// Configuration for the WebSocket server
#[derive(Debug, Clone)]
pub struct WsServerConfig {
    pub port: u16,
    /// Allowed origins for CORS-like validation (e.g., "https://your-app.example.com")
    /// If empty, all origins are allowed (development mode).
    pub allowed_origins: Vec<String>,
    /// Auth token for WebSocket connections. If `Some`, clients must include `?token=<value>`
    /// in the connection URL. If `None`, no authentication is required (development mode).
    pub auth_token: Option<String>,
}

impl Default for WsServerConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_WS_PORT,
            allowed_origins: Vec::new(),
            auth_token: None,
        }
    }
}

/// Channel type for sending commands from WS connections to the scanner orchestrator
pub type CommandSender = mpsc::UnboundedSender<(ClientMessage, ResponseSender)>;
pub type CommandReceiver = mpsc::UnboundedReceiver<(ClientMessage, ResponseSender)>;

/// Channel for sending responses back to a specific WS connection
pub type ResponseSender = mpsc::UnboundedSender<AgentMessage>;
pub type ResponseReceiver = mpsc::UnboundedReceiver<AgentMessage>;

/// Broadcast channel for scanner events that should go to all connected clients
pub type EventSender = broadcast::Sender<AgentMessage>;

/// Handle to the running WebSocket server
pub struct WsServerHandle {
    shutdown_tx: broadcast::Sender<()>,
    pub command_rx: CommandReceiver,
    pub event_tx: EventSender,
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
    info!("WebSocket server listening on ws://{}", addr);

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
                    break;
                }
            }
        }
    });

    Ok(WsServerHandle {
        shutdown_tx,
        command_rx,
        event_tx,
    })
}

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
        mpsc::unbounded_channel();

    // Task: Forward responses and events to this client
    let ws_tx = Arc::new(Mutex::new(ws_tx));
    let ws_tx_events = ws_tx.clone();
    let ws_tx_responses = ws_tx.clone();

    // Watch channel to signal per-connection tasks to shut down cooperatively
    let (close_tx, _) = watch::channel(false);
    let mut close_rx_resp = close_tx.subscribe();
    let mut close_rx_evt = close_tx.subscribe();

    // Forward direct responses to this client
    let response_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = response_rx.recv() => {
                    match msg {
                        Some(msg) => {
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let mut tx = ws_tx_responses.lock().await;
                                if tx.send(Message::Text(json.into())).await.is_err() {
                                    break;
                                }
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
                                if tx.send(Message::Text(json.into())).await.is_err() {
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
                            let _ = tx.send(Message::Text(json.into())).await;
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
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let _ = response_task.await;
        let _ = event_task.await;
    })
    .await;

    info!("Connection handler ended for {}", peer_addr);
}

/// Validate the WebSocket handshake: origin + auth token.
fn validate_handshake(
    req: &Request,
    config: &WsServerConfig,
    resp: Response,
) -> Result<Response, tokio_tungstenite::tungstenite::http::Response<Option<String>>> {
    // --- Origin validation ---
    if !config.allowed_origins.is_empty() {
        let origin = req
            .headers()
            .get("Origin")
            .and_then(|v| v.to_str().ok());

        match origin {
            Some(origin) if config.allowed_origins.iter().any(|ao| ao == origin) => {}
            Some(origin) => {
                warn!("Rejected connection from unauthorized origin: {}", origin);
                let reject = tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(403)
                    .body(Some("Forbidden: Origin not allowed".to_string()))
                    .unwrap();
                return Err(reject);
            }
            None => {
                // No origin header — could be a non-browser client, allow it
            }
        }
    }

    // --- Auth token validation ---
    if let Some(expected) = &config.auth_token {
        let provided = req.uri().query().and_then(parse_token_from_query);

        match provided {
            Some(token) if token == expected => {}
            _ => {
                warn!("Rejected connection: invalid or missing auth token");
                let reject = tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(401)
                    .body(Some("Unauthorized: Invalid or missing token".to_string()))
                    .unwrap();
                return Err(reject);
            }
        }
    }

    Ok(resp)
}

/// Extract `token` value from a URI query string (e.g., "token=abc&foo=bar" -> "abc").
fn parse_token_from_query(query: &str) -> Option<&str> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key == "token" {
            Some(value)
        } else {
            None
        }
    })
}
