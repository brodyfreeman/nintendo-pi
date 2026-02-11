//! Hardware lifecycle loop: USB and Bluetooth reconnection.
//!
//! Owns the outer USB reconnect loop and inner BT reconnect loop.
//! Each USB cycle: init controller, calibrate sticks, spawn HID reader
//! and processor, then run the BT connection loop until USB disconnects.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use crate::bt;
use crate::calibration::{
    auto_calibrate_centers, StickCalibrator, StickPair, C_STICK_CAL, MAIN_STICK_CAL,
};
use crate::led;
use crate::macro_engine::command::MacroCommand;
use crate::processing::{Processor, ProcessorIO};
use crate::usb;
use crate::web::state::{MitmState, StateSnapshot};

/// Everything the lifecycle loop needs from the caller.
pub struct LifecycleIO {
    pub macros_dir: PathBuf,
    pub cmd_rx: mpsc::Receiver<MacroCommand>,
    pub mitm_state: Arc<MitmState>,
    pub state_broadcast: broadcast::Sender<String>,
}

/// Run the hardware lifecycle loop (never returns under normal operation).
///
/// Outer loop: wait for USB controller, init, calibrate, process.
/// Inner loop: wait for BT (Switch) connection, forward reports.
pub async fn run(io: LifecycleIO) -> anyhow::Result<()> {
    let bt_connected = Arc::new(AtomicBool::new(false));
    let calibration_samples = Arc::new(AtomicU32::new(20));
    let mut cmd_rx = io.cmd_rx;

    loop {
        // Drain stale web commands from previous session
        while cmd_rx.try_recv().is_ok() {}

        // --- USB init (retry until controller is plugged in) ---
        io.mitm_state.update(StateSnapshot::default());
        wait_for_usb().await;
        io.mitm_state.update(StateSnapshot {
            usb_connected: true,
            ..Default::default()
        });

        // Wait for HID device to appear after init
        info!("[USB] Waiting for HID device to appear...");
        tokio::time::sleep(Duration::from_secs(2)).await;

        // --- Spawn HID reader and calibrate sticks ---
        let hid_rx = usb::hid::spawn_reader(2);
        let sticks = calibrate_sticks(&hid_rx, &calibration_samples);

        // --- Spawn USB processing on a blocking thread ---
        let (report_tx, mut report_rx) = mpsc::channel::<[u8; 50]>(4);

        let processor_io = ProcessorIO {
            hid_rx,
            cmd_rx,
            report_tx,
            mitm_state: io.mitm_state.clone(),
            state_broadcast: io.state_broadcast.clone(),
            bt_connected: bt_connected.clone(),
            calibration_samples: calibration_samples.clone(),
        };
        let processor = Processor::new(processor_io, sticks, io.macros_dir.clone());
        let usb_handle = tokio::task::spawn_blocking(move || processor.run());

        // --- BT connection loop ---
        run_bt_loop(&mut report_rx, &usb_handle, &bt_connected, &io.mitm_state).await;

        // USB processing thread ended — get cmd_rx back for the next cycle
        bt_connected.store(false, Ordering::Relaxed);
        io.mitm_state.update(StateSnapshot::default());
        cmd_rx = usb_handle.await?;
    }
}

/// Retry USB initialization until the controller is found.
async fn wait_for_usb() {
    loop {
        match usb::init::initialize_controller().await {
            Ok(()) => return,
            Err(e) => {
                warn!("[USB] {e} — retrying in 5s...");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// Auto-calibrate stick centers from initial HID reports.
fn calibrate_sticks(
    hid_rx: &std::sync::mpsc::Receiver<usb::hid::HidReport>,
    calibration_samples: &AtomicU32,
) -> StickPair {
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

    StickPair::new(
        StickCalibrator::new(MAIN_STICK_CAL, 10.0),
        StickCalibrator::new(C_STICK_CAL, 10.0),
        left_center,
        right_center,
    )
}

/// Run the inner BT connection loop until USB disconnects.
async fn run_bt_loop(
    report_rx: &mut mpsc::Receiver<[u8; 50]>,
    usb_handle: &tokio::task::JoinHandle<mpsc::Receiver<MacroCommand>>,
    bt_connected: &Arc<AtomicBool>,
    mitm_state: &Arc<MitmState>,
) {
    loop {
        info!("[BT] Waiting for Switch to connect...");
        bt_connected.store(false, Ordering::Relaxed);
        mitm_state.update(StateSnapshot {
            usb_connected: true,
            ..Default::default()
        });

        let mut bt_session = match accept_bt_session(usb_handle, mitm_state).await {
            Some(session) => session,
            None => return, // USB disconnected
        };

        if let Err(e) = bt_session.run_pairing().await {
            error!("[BT] Pairing error: {e}");
            continue;
        }

        info!("[BT] Connected to Switch!");
        bt_connected.store(true, Ordering::Relaxed);
        led::set_led(&led::LED_NORMAL);

        let usb_alive = bt_session.forward_reports(report_rx).await;
        if !usb_alive {
            return;
        }

        warn!("[BT] Switch disconnected. Waiting for reconnection...");
        bt_connected.store(false, Ordering::Relaxed);
        led::set_led(&led::LED_NORMAL);
    }
}

/// Accept a BT session, checking periodically if USB has disconnected.
///
/// Returns `None` if the USB handle has finished (controller unplugged).
async fn accept_bt_session(
    usb_handle: &tokio::task::JoinHandle<mpsc::Receiver<MacroCommand>>,
    mitm_state: &Arc<MitmState>,
) -> Option<bt::emulator::BtSession> {
    let accept_fut = bt::emulator::BtSession::accept();
    tokio::pin!(accept_fut);

    loop {
        tokio::select! {
            result = &mut accept_fut => {
                match result {
                    Ok(session) => return Some(session),
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
                    return None;
                }
            }
        }
    }
}
