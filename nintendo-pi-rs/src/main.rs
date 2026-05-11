//! Nintendo Pi MITM Bridge - Rust implementation.
//!
//! USB controller input -> Bluetooth Pro Controller output with macro support.
//! Single binary with embedded web UI.

mod bt;
mod calibration;
mod config;
mod input;
mod led;
mod lifecycle;
mod macro_engine;
mod processing;
mod usb;
mod web;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{error, info, warn};

use config::Config;
use macro_engine::command::MacroCommand;
use processing::{
    InputPipeline, InputPipelineIO, MacroRuntime, MacroRuntimeIO, OutputMode, PipelineStatus,
};
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
    if let Some(backup_dir) = macro_engine::storage::migrate_old_macros(&args.macros_dir)? {
        info!(
            "[MACRO] Old macro format migrated. Backup kept at {}",
            backup_dir.display()
        );
    }

    // --- Shared runtime setup ---
    let mitm_state = Arc::new(MitmState::new());
    let (cmd_tx, cmd_rx) = mpsc::channel::<MacroCommand>(32);
    let (usb_tx, usb_rx) = mpsc::channel::<processing::UsbEvent>(8);
    let (report_tx, mut report_rx) = watch::channel::<[u8; 50]>([0; 50]);
    let (live_input_tx, live_input_rx) = watch::channel(None);
    let (pipeline_status_tx, pipeline_status_rx) = watch::channel(PipelineStatus::default());
    let (output_mode_tx, output_mode_rx) = watch::channel(OutputMode::Live);
    let (config_tx, config_rx) = watch::channel(Config::default());
    let (state_broadcast, _) = broadcast::channel::<String>(16);
    let bt_connected = Arc::new(AtomicBool::new(false));
    let calibration_samples = Arc::new(AtomicU32::new(20));

    let runtime = MacroRuntime::new(
        MacroRuntimeIO {
            cmd_rx,
            live_input_rx,
            pipeline_status_rx,
            output_mode_tx,
            config_tx,
            report_tx: report_tx.clone(),
            mitm_state: mitm_state.clone(),
            state_broadcast: state_broadcast.clone(),
            bt_connected: bt_connected.clone(),
            calibration_samples: calibration_samples.clone(),
        },
        args.macros_dir.clone(),
    );
    tokio::spawn(runtime.run());

    let input_pipeline = InputPipeline::new(InputPipelineIO {
        usb_rx,
        report_tx,
        live_input_tx,
        status_tx: pipeline_status_tx,
        output_mode_rx,
        config_rx,
    });
    tokio::spawn(input_pipeline.run());

    // --- Web UI setup (start early so it's available during hardware init) ---
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

    // --- USB lifecycle task ---
    tokio::spawn(lifecycle::run_usb(lifecycle::UsbLifecycleIO {
        usb_tx,
        calibration_samples,
    }));

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

    lifecycle::run_bt_loop(&mut report_rx, bt_connected).await
}
