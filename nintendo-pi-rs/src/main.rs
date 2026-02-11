//! Nintendo Pi MITM Bridge - Rust implementation.
//!
//! USB controller input -> Bluetooth Pro Controller output with macro support.
//! Single binary with embedded web UI.

mod bt;
mod calibration;
mod combo;
mod config;
mod input;
mod led;
mod lifecycle;
mod macro_engine;
mod processing;
mod usb;
mod web;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use lifecycle::LifecycleIO;
use macro_engine::command::MacroCommand;
use web::state::MitmState;

#[derive(Parser)]
#[command(
    name = "nintendo-pi",
    about = "MITM bridge: USB controller -> BT Pro Controller"
)]
struct Args {
    /// Macros directory path
    #[arg(long, default_value = "/root/macros")]
    macros_dir: PathBuf,

    /// Web UI port
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Initialize tracing
    let filter = if args.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .init();

    info!("=== Nintendo Pi MITM Bridge (Rust) ===");
    info!("USB-in, Bluetooth-out");
    info!("Macros dir: {}", args.macros_dir.display());
    info!("Web UI port: {}", args.port);

    // Ensure macros directory exists
    std::fs::create_dir_all(&args.macros_dir).ok();

    // --- Web UI setup (start early so it's available during hardware init) ---
    let mitm_state = Arc::new(MitmState::new());
    let (cmd_tx, cmd_rx) = mpsc::channel::<MacroCommand>(32);
    let (state_broadcast, _) = broadcast::channel::<String>(16);

    // Spawn web server
    let web_state = mitm_state.clone();
    let web_broadcast = state_broadcast.clone();
    let web_macros_dir = args.macros_dir.clone();
    let web_port = args.port;
    tokio::spawn(async move {
        if let Err(e) =
            web::start_server(web_port, web_state, cmd_tx, web_broadcast, web_macros_dir).await
        {
            error!("[WEB] Server error: {e}");
        }
    });

    // Give the web server a moment to bind
    tokio::time::sleep(Duration::from_millis(100)).await;

    // --- Bluetooth setup (one-time, retry until adapter is ready) ---
    let _dbus_conn = loop {
        match async {
            let conn = zbus::Connection::system().await?;
            bt::agent::register_agent(&conn).await?;
            bt::adapter::configure_adapter(&conn).await?;
            bt::sdp::register_sdp_profile(&conn).await?;
            bt::adapter::set_device_class().await?;
            anyhow::Ok(conn)
        }
        .await
        {
            Ok(conn) => break conn,
            Err(e) => {
                warn!("[BT] Setup failed: {e} — retrying in 3s...");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    };

    // --- State emitter task (throttled broadcast on change) ---
    let mut state_rx = mitm_state.subscribe();
    let emitter_broadcast = state_broadcast.clone();
    tokio::spawn(async move {
        loop {
            if state_rx.changed().await.is_err() {
                break; // sender dropped
            }
            let snapshot = state_rx.borrow_and_update().clone();
            let interval_ms = snapshot.config.ui_update_interval_ms;
            let msg = serde_json::json!({
                "type": "state_update",
                "state": snapshot,
            });
            let _ = emitter_broadcast.send(msg.to_string());
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        }
    });

    // --- Hardware lifecycle loop ---
    lifecycle::run(LifecycleIO {
        macros_dir: args.macros_dir,
        cmd_rx,
        mitm_state,
        state_broadcast,
    })
    .await
}
