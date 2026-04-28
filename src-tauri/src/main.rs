//! RSWebTWAIN — Tauri v2 headless system tray application.
//!
//! Runs as a background process with a system tray icon, providing a WebSocket
//! server on localhost for the Angular frontend to communicate with TWAIN scanners.
//!
//! Features:
//! - System tray icon with context menu
//! - Deep link protocol (`rswebtwain://`) for wake-on-demand
//! - Autostart with Windows
//! - WebSocket server for scanner control

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use scan_agent_lib::protocol::AgentMessage;
use scan_agent_lib::ws_server::{self, EventSender, WsServerConfig, DEFAULT_WS_PORT};
use tauri::Manager;
use tauri::tray::TrayIconBuilder;
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::DialogExt;
use tracing::{error, info, warn};

/// Encrypt data using Windows DPAPI (current-user scope).
#[cfg(windows)]
fn dpapi_encrypt(plaintext: &[u8]) -> std::io::Result<Vec<u8>> {
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let input = CRYPT_INTEGER_BLOB {
        cbData: plaintext.len() as u32,
        pbData: plaintext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    unsafe {
        CryptProtectData(
            &input,
            None,
            None,
            None,
            None,
            0,
            &mut output,
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;

        let encrypted = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        windows::Win32::Foundation::LocalFree(windows::Win32::Foundation::HLOCAL(
            output.pbData as *mut std::ffi::c_void,
        ));
        Ok(encrypted)
    }
}

/// Decrypt data using Windows DPAPI (current-user scope).
#[cfg(windows)]
#[allow(dead_code)]
fn dpapi_decrypt(ciphertext: &[u8]) -> std::io::Result<Vec<u8>> {
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let input = CRYPT_INTEGER_BLOB {
        cbData: ciphertext.len() as u32,
        pbData: ciphertext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            0,
            &mut output,
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;

        let decrypted = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        windows::Win32::Foundation::LocalFree(windows::Win32::Foundation::HLOCAL(
            output.pbData as *mut std::ffi::c_void,
        ));
        Ok(decrypted)
    }
}

#[cfg(not(windows))]
fn dpapi_encrypt(plaintext: &[u8]) -> std::io::Result<Vec<u8>> {
    Ok(plaintext.to_vec())
}

#[cfg(not(windows))]
#[allow(dead_code)]
fn dpapi_decrypt(ciphertext: &[u8]) -> std::io::Result<Vec<u8>> {
    Ok(ciphertext.to_vec())
}

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "scan_agent=info".into()),
        )
        .init();

    info!("RSWebTWAIN starting");

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // --- Auth Token ---
            let auth_token = if cfg!(debug_assertions) {
                None
            } else {
                let token = uuid::Uuid::new_v4().to_string();
                let data_dir = app.path().app_data_dir()?;
                std::fs::create_dir_all(&data_dir)?;
                let token_path = data_dir.join("ws-token");
                let encrypted = dpapi_encrypt(token.as_bytes())?;
                std::fs::write(&token_path, &encrypted)?;
                info!("Auth token written to {}", token_path.display());
                Some(token)
            };

            let token_cleanup_path = if !cfg!(debug_assertions) {
                app.path().app_data_dir().ok().map(|d| d.join("ws-token"))
            } else {
                None
            };

            // --- System Tray ---
            let quit = tauri::menu::MenuItem::with_id(app, "quit", "Quit RSWebTWAIN", true, None::<&str>)?;
            let status = tauri::menu::MenuItem::with_id(app, "status", "Status: Ready", false, None::<&str>)?;
            let autostart_toggle = tauri::menu::MenuItem::with_id(
                app,
                "autostart",
                "Start with Windows",
                true,
                None::<&str>,
            )?;
            let separator = tauri::menu::PredefinedMenuItem::separator(app)?;
            let about = tauri::menu::MenuItem::with_id(
                app,
                "about",
                "About RSWebTWAIN",
                true,
                None::<&str>,
            )?;

            let menu = tauri::menu::Menu::with_items(
                app,
                &[&status, &separator, &autostart_toggle, &about, &separator, &quit],
            )?;

            // Set initial autostart label to reflect current state
            let autostart_enabled = app.autolaunch().is_enabled().unwrap_or(false);
            if autostart_enabled {
                let _ = autostart_toggle.set_text("✓ Start with Windows");
            }

            let autostart_item = autostart_toggle.clone();
            let _tray = TrayIconBuilder::with_id("main")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .tooltip("RSWebTWAIN - Ready")
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "quit" => {
                        info!("Quit requested via tray menu");
                        if let Some(ref path) = token_cleanup_path {
                            let _ = std::fs::remove_file(path);
                        }
                        app.exit(0);
                    }
                    "autostart" => {
                        let manager = app.autolaunch();
                        let currently_enabled = manager.is_enabled().unwrap_or(false);
                        if currently_enabled {
                            if let Err(e) = manager.disable() {
                                error!("Failed to disable autostart: {}", e);
                            } else {
                                info!("Autostart disabled");
                                let _ = autostart_item.set_text("Start with Windows");
                            }
                        } else if let Err(e) = manager.enable() {
                            error!("Failed to enable autostart: {}", e);
                        } else {
                            info!("Autostart enabled");
                            let _ = autostart_item.set_text("✓ Start with Windows");
                        }
                    }
                    "about" => {
                        let version = app.package_info().version.to_string();
                        app.dialog()
                            .message(format!(
                                "RSWebTWAIN v{}\n\n\
                                 A background scanning service that bridges\n\
                                 your web application to local TWAIN scanners.\n\n\
                                 WebSocket: ws://127.0.0.1:{}\n\
                                 Protocol: rswebtwain://",
                                version, DEFAULT_WS_PORT
                            ))
                            .title("About RSWebTWAIN")
                            .kind(tauri_plugin_dialog::MessageDialogKind::Info)
                            .blocking_show();
                    }
                    _ => {}
                })
                .build(app)?;

            // --- Deep Links ---
            // Shared sender filled once the WS server starts; deep link handler reads it.
            let deep_link_tx: Arc<Mutex<Option<EventSender>>> = Arc::new(Mutex::new(None));

            #[cfg(desktop)]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                if let Err(e) = app.deep_link().register("rswebtwain") {
                    error!("Failed to register deep link: {}", e);
                }

                let dl_tx = deep_link_tx.clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        info!("Deep link received: {}", url);

                        // Parse the URL: rswebtwain://action?key=value&...
                        let action = if url.host_str().is_some() {
                            url.host_str().map(|s| s.to_string())
                        } else {
                            // Some URL parsers put the path as the action
                            let path = url.path().trim_start_matches('/');
                            if path.is_empty() { None } else { Some(path.to_string()) }
                        };

                        let params: HashMap<String, String> = url
                            .query_pairs()
                            .map(|(k, v)| (k.into_owned(), v.into_owned()))
                            .collect();

                        let msg = AgentMessage::DeepLink {
                            url: url.to_string(),
                            action,
                            params,
                        };

                        // Broadcast to all connected WS clients (if server is up)
                        if let Ok(guard) = dl_tx.lock() {
                            if let Some(ref tx) = *guard {
                                match tx.send(msg) {
                                    Ok(n) => info!("Deep link broadcast to {} client(s)", n),
                                    Err(_) => info!("Deep link received but no clients connected"),
                                }
                            } else {
                                warn!("Deep link received before WS server started");
                            }
                        }
                    }
                });
            }

            // --- Sidecar Path ---
            let sidecar_path = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|p| p.join("twain-scanner-32bit.exe")))
                .filter(|p| p.exists())
                .map(|p| p.to_string_lossy().into_owned());

            if let Some(ref path) = sidecar_path {
                info!("32-bit sidecar found: {}", path);
            } else {
                info!("32-bit sidecar not found (32-bit-only scanners will be unavailable)");
            }

            // --- WebSocket Server ---
            let port: u16 = match std::env::var("RSWEBTWAIN_PORT") {
                Ok(val) => match val.parse() {
                    Ok(p) => {
                        info!("Using custom port from RSWEBTWAIN_PORT: {}", p);
                        p
                    }
                    Err(_) => {
                        warn!("Invalid RSWEBTWAIN_PORT '{}', using default {}", val, DEFAULT_WS_PORT);
                        DEFAULT_WS_PORT
                    }
                },
                Err(_) => DEFAULT_WS_PORT,
            };

            // TODO(Task 10): wire origin_policy from AgentConfig loaded by config module.
            // For now use AllowAll in debug and a localhost-only Restricted in release.
            let origin_policy = if cfg!(debug_assertions) {
                ws_server::OriginPolicy::AllowAll
            } else {
                ws_server::OriginPolicy::Restricted {
                    allow_localhost: true,
                    extra: Vec::new(),
                }
            };

            let _app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let config = WsServerConfig {
                    port,
                    origin_policy,
                    auth_token,
                };

                match ws_server::start_server(config).await {
                    Ok(handle) => {
                        info!("WebSocket server started on port {}", port);

                        // Share the event sender with the deep link handler
                        if let Ok(mut guard) = deep_link_tx.lock() {
                            *guard = Some(handle.event_tx.clone());
                        }

                        // Start the command handler
                        scan_agent_lib::command_handler(handle.command_rx, handle.event_tx, sidecar_path).await;
                    }
                    Err(e) => {
                        error!("Failed to start WebSocket server: {}", e);
                    }
                }
            });

            info!("RSWebTWAIN setup complete");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running RSWebTWAIN");
}
