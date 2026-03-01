//! Scan Agent — Tauri v2 headless system tray application.
//!
//! Runs as a background process with a system tray icon, providing a WebSocket
//! server on localhost for the Angular frontend to communicate with TWAIN scanners.
//!
//! Features:
//! - System tray icon with context menu
//! - Deep link protocol (`scan-agent://`) for wake-on-demand
//! - Autostart with Windows
//! - WebSocket server for scanner control

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use scan_agent_lib::ws_server::{self, WsServerConfig, DEFAULT_WS_PORT};
use tauri::Manager;
use tauri::tray::TrayIconBuilder;
use tauri_plugin_autostart::ManagerExt;
use tracing::{error, info, warn};

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "scan_agent=info".into()),
        )
        .init();

    info!("Scan Agent starting");

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_deep_link::init())
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
                std::fs::write(&token_path, &token)?;
                info!("Auth token written to {}", token_path.display());
                Some(token)
            };

            let token_cleanup_path = if !cfg!(debug_assertions) {
                app.path().app_data_dir().ok().map(|d| d.join("ws-token"))
            } else {
                None
            };

            // --- System Tray ---
            let quit = tauri::menu::MenuItem::with_id(app, "quit", "Quit Scan Agent", true, None::<&str>)?;
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
                "About Scan Agent",
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
                .tooltip("Scan Agent - Ready")
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
                        } else {
                            if let Err(e) = manager.enable() {
                                error!("Failed to enable autostart: {}", e);
                            } else {
                                info!("Autostart enabled");
                                let _ = autostart_item.set_text("✓ Start with Windows");
                            }
                        }
                    }
                    "about" => {
                        info!("About requested");
                        // Could show a small dialog, but we're headless
                    }
                    _ => {}
                })
                .build(app)?;

            // --- Deep Links ---
            #[cfg(desktop)]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                if let Err(e) = app.deep_link().register("scan-agent") {
                    error!("Failed to register deep link: {}", e);
                }

                app.deep_link().on_open_url(|event| {
                    info!("Deep link received: {:?}", event.urls());
                    // Deep link URLs can carry scan parameters:
                    // scan-agent://scan?scanner=default&format=pdf
                    // For now, we just log — the WS server handles actual commands
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
            let port: u16 = match std::env::var("SCAN_AGENT_PORT") {
                Ok(val) => match val.parse() {
                    Ok(p) => {
                        info!("Using custom port from SCAN_AGENT_PORT: {}", p);
                        p
                    }
                    Err(_) => {
                        warn!("Invalid SCAN_AGENT_PORT '{}', using default {}", val, DEFAULT_WS_PORT);
                        DEFAULT_WS_PORT
                    }
                },
                Err(_) => DEFAULT_WS_PORT,
            };

            // --- Allowed Origins ---
            let allowed_origins: Vec<String> = if cfg!(debug_assertions) {
                // In debug mode, allow all origins for easier development
                Vec::new()
            } else {
                match std::env::var("SCAN_AGENT_ALLOWED_ORIGINS") {
                    Ok(val) => {
                        let origins: Vec<String> = val
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        info!("Allowed origins from SCAN_AGENT_ALLOWED_ORIGINS: {:?}", origins);
                        origins
                    }
                    Err(_) => {
                        // Production defaults — update these to match your deployment
                        let defaults = vec![
                            "https://your-app.example.com".to_string(),
                            "https://localhost:4200".to_string(),
                        ];
                        info!("Using default allowed origins: {:?}", defaults);
                        defaults
                    }
                }
            };

            let _app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let config = WsServerConfig {
                    port,
                    allowed_origins,
                    auth_token,
                };

                match ws_server::start_server(config).await {
                    Ok(handle) => {
                        info!("WebSocket server started on port {}", port);

                        // Start the command handler
                        scan_agent_lib::command_handler(handle.command_rx, handle.event_tx, sidecar_path).await;
                    }
                    Err(e) => {
                        error!("Failed to start WebSocket server: {}", e);
                    }
                }
            });

            info!("Scan Agent setup complete");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running scan agent");
}
