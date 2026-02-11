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
mod macro_engine;
mod processing;
mod usb;
mod web;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use calibration::{
    auto_calibrate_centers, StickCalibrator, StickPair, C_STICK_CAL, MAIN_STICK_CAL,
};
use macro_engine::controller::MacroCommand;
use processing::{Processor, ProcessorIO};
use web::state::{MitmState, StateSnapshot};

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
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<MacroCommand>(32);
    let (state_broadcast, _) = broadcast::channel::<String>(16);

    // Spawn web server
    let web_state = mitm_state.clone();
    let web_broadcast = state_broadcast.clone();
    let web_macros_dir = args.macros_dir.clone();
    let web_port = args.port;
    let web_cmd_tx = cmd_tx;
    tokio::spawn(async move {
        if let Err(e) = web::start_server(
            web_port,
            web_state,
            web_cmd_tx,
            web_broadcast,
            web_macros_dir,
        )
        .await
        {
            error!("[WEB] Server error: {e}");
        }
    });

    // Give the web server a moment to bind
    tokio::time::sleep(Duration::from_millis(100)).await;

    // --- Bluetooth setup (one-time, retry until adapter is ready) ---
    // Order matters: agent first (for pairing), adapter config, SDP profile,
    // then device class LAST (D-Bus calls can reset the HCI class).
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

    // Shared flag: BT forwarding side sets this so USB processing knows BT status
    let bt_connected = Arc::new(AtomicBool::new(false));
    // Shared calibration samples count — persists across USB reconnects
    let calibration_samples = Arc::new(AtomicU32::new(20));

    // === Hardware lifecycle loop ===
    // Outer loop handles USB controller disconnect/reconnect.
    // Inner loop handles BT (Switch) disconnect/reconnect.
    loop {
        // Drain stale web commands from previous session
        while cmd_rx.try_recv().is_ok() {}

        // --- Phase 0: USB Init (retry until controller is plugged in) ---
        mitm_state.update(StateSnapshot::default());
        loop {
            match usb::init::initialize_controller().await {
                Ok(()) => break,
                Err(e) => {
                    warn!("[USB] {e} — retrying in 5s...");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
        // USB controller found — update state
        mitm_state.update(StateSnapshot {
            usb_connected: true,
            ..Default::default()
        });

        // Wait for HID device to appear after init
        info!("[USB] Waiting for HID device to appear...");
        tokio::time::sleep(Duration::from_secs(2)).await;

        // --- Spawn HID reader thread ---
        let hid_rx = usb::hid::spawn_reader(2);

        // --- Auto-calibrate stick centers ---
        let cal_count = calibration_samples.load(Ordering::Relaxed) as usize;
        info!("[USB] Calibrating stick centers ({cal_count} samples, don't touch the sticks)...");
        let mut cal_reports = Vec::with_capacity(cal_count);
        for _ in 0..cal_count {
            match hid_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(report) => cal_reports.push(report),
                Err(_) => break,
            }
        }
        let (left_center, right_center) = auto_calibrate_centers(&cal_reports);
        info!(
            "[USB] Left stick center: ({}, {}), Right: ({}, {})",
            left_center.0, left_center.1, right_center.0, right_center.1
        );

        let sticks = StickPair::new(
            StickCalibrator::new(MAIN_STICK_CAL, 10.0),
            StickCalibrator::new(C_STICK_CAL, 10.0),
            left_center,
            right_center,
        );

        // --- Spawn USB processing on a blocking thread ---
        let (report_tx, mut report_rx) = mpsc::channel::<[u8; 50]>(4);

        let io = ProcessorIO {
            hid_rx,
            cmd_rx,
            report_tx,
            mitm_state: mitm_state.clone(),
            state_broadcast: state_broadcast.clone(),
            bt_connected: bt_connected.clone(),
            calibration_samples: calibration_samples.clone(),
        };
        let processor = Processor::new(io, sticks, args.macros_dir.clone());

        let usb_handle = tokio::task::spawn_blocking(move || processor.run());

        // --- BT connection loop (async, on main task) ---
        'bt_loop: loop {
            info!("[BT] Waiting for Switch to connect...");
            bt_connected.store(false, Ordering::Relaxed);
            mitm_state.update(StateSnapshot {
                usb_connected: true,
                ..Default::default()
            });

            // Wait for BT connection, but also check if USB has disconnected.
            // Important: accept() must NOT be cancelled by a timer, because
            // dropping the future tears down the L2CAP listeners and prevents
            // the Switch from completing its connection.
            let accept_fut = bt::emulator::BtSession::accept();
            tokio::pin!(accept_fut);

            let mut bt_session = loop {
                tokio::select! {
                    result = &mut accept_fut => {
                        match result {
                            Ok(session) => break session,
                            Err(e) => {
                                error!("[BT] Connection error: {e}");
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                accept_fut.set(bt::emulator::BtSession::accept());
                            }
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {
                        if usb_handle.is_finished() {
                            warn!("[USB] Controller disconnected. Waiting for reconnection...");
                            mitm_state.update(StateSnapshot::default());
                            break 'bt_loop;
                        }
                    }
                }
            };

            if let Err(e) = bt_session.run_pairing().await {
                error!("[BT] Pairing error: {e}");
                continue;
            }

            info!("[BT] Connected to Switch!");
            bt_connected.store(true, Ordering::Relaxed);
            led::set_led(&led::LED_NORMAL);

            let usb_alive = bt_session.forward_reports(&mut report_rx).await;
            if !usb_alive {
                break 'bt_loop;
            }
            warn!("[BT] Switch disconnected. Waiting for reconnection...");
            bt_connected.store(false, Ordering::Relaxed);
            led::set_led(&led::LED_NORMAL);
        }

        // USB processing thread ended — get cmd_rx back for the next USB cycle
        bt_connected.store(false, Ordering::Relaxed);
        mitm_state.update(StateSnapshot::default());
        cmd_rx = usb_handle.await?;
    }
}
