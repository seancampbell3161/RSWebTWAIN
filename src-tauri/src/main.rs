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

use scan_agent_lib::ws_server::{self, WsServerConfig};
use tauri::Manager;
use tauri::tray::TrayIconBuilder;
use tracing::{error, info};

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

            let _tray = TrayIconBuilder::new()
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
                        info!("Autostart toggle requested");
                        // TODO: Toggle autostart via tauri_plugin_autostart
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

            // --- WebSocket Server ---
            let _app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let config = WsServerConfig {
                    port: ws_server::DEFAULT_WS_PORT,
                    allowed_origins: Vec::new(), // Allow all in dev; configure for production
                    auth_token,
                };

                match ws_server::start_server(config).await {
                    Ok(handle) => {
                        info!(
                            "WebSocket server started on port {}",
                            ws_server::DEFAULT_WS_PORT
                        );

                        // Start the command handler
                        scan_agent_lib::command_handler(handle.command_rx, handle.event_tx).await;
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
