//! Hardware lifecycle adapters.
//!
//! USB and Bluetooth are independent loops. USB publishes optional live input
//! into the macro runtime; Bluetooth consumes whatever report stream the
//! runtime is producing.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::bt;
use crate::calibration::{
    validate_auto_calibrate_centers, CalibrationRequirements, StickCalibrator, StickPair,
    C_STICK_CAL, DEFAULT_MAX_CENTER_SPREAD, MAIN_STICK_CAL, MIN_TRUSTED_CALIBRATION_SAMPLES,
};
use crate::led;
use crate::processing::UsbEvent;
use crate::usb;

const CALIBRATION_RETRY_DELAY: Duration = Duration::from_millis(250);

/// Everything the USB lifecycle loop needs from the caller.
pub struct UsbLifecycleIO {
    pub usb_tx: mpsc::Sender<UsbEvent>,
    pub calibration_samples: Arc<AtomicU32>,
}

/// Retry USB initialization forever and publish input events while connected.
pub async fn run_usb(io: UsbLifecycleIO) -> anyhow::Result<()> {
    loop {
        wait_for_usb().await;

        // Wait for HID device to appear after init.
        info!("[USB] Waiting for HID device to appear...");
        tokio::time::sleep(Duration::from_secs(2)).await;

        let hid_rx = usb::hid::spawn_reader(2);
        let sticks = match calibrate_sticks(&hid_rx, &io.calibration_samples) {
            Ok(sticks) => sticks,
            Err(e) => {
                warn!("[USB] Calibration failed before input became ready: {e}");
                let _ = io.usb_tx.send(UsbEvent::Disconnected).await;
                continue;
            }
        };

        if io
            .usb_tx
            .send(UsbEvent::InputReady(Box::new(sticks)))
            .await
            .is_err()
        {
            return Ok(());
        }

        let usb_tx = io.usb_tx.clone();
        tokio::task::spawn_blocking(move || forward_hid_reports(hid_rx, usb_tx)).await??;
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

fn forward_hid_reports(
    hid_rx: std::sync::mpsc::Receiver<usb::hid::HidReport>,
    usb_tx: mpsc::Sender<UsbEvent>,
) -> anyhow::Result<()> {
    let mut usb_check_counter: u32 = 0;

    loop {
        match hid_rx.recv_timeout(Duration::from_millis(8)) {
            Ok(report) => {
                if usb_tx.blocking_send(UsbEvent::Report(report)).is_err() {
                    return Ok(());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                usb_check_counter += 1;
                if usb_check_counter >= 250 {
                    usb_check_counter = 0;
                    if !usb::init::is_device_present() {
                        let _ = usb_tx.blocking_send(UsbEvent::Disconnected);
                        return Ok(());
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = usb_tx.blocking_send(UsbEvent::Disconnected);
                return Ok(());
            }
        }
    }
}

/// Auto-calibrate stick centers from initial HID reports.
fn calibrate_sticks(
    hid_rx: &std::sync::mpsc::Receiver<usb::hid::HidReport>,
    calibration_samples: &AtomicU32,
) -> anyhow::Result<StickPair> {
    let cal_count =
        (calibration_samples.load(Ordering::Relaxed) as usize).max(MIN_TRUSTED_CALIBRATION_SAMPLES);
    let min_samples = (cal_count * 3 / 4).max(MIN_TRUSTED_CALIBRATION_SAMPLES);
    let requirements = CalibrationRequirements::new(min_samples, DEFAULT_MAX_CENTER_SPREAD);

    info!(
        "[USB] Calibrating stick centers ({cal_count} samples, need {min_samples} stable; don't touch the sticks)..."
    );

    let mut attempt = 1u32;
    loop {
        let cal_reports = collect_calibration_reports(hid_rx, cal_count)?;

        match validate_auto_calibrate_centers(&cal_reports, requirements) {
            Ok(calibration) => {
                info!(
                    "[USB] Left stick center: ({}, {}), Right: ({}, {})",
                    calibration.left.0,
                    calibration.left.1,
                    calibration.right.0,
                    calibration.right.1
                );

                return Ok(StickPair::new(
                    StickCalibrator::new(MAIN_STICK_CAL, 10.0),
                    StickCalibrator::new(C_STICK_CAL, 10.0),
                    calibration.left,
                    calibration.right,
                ));
            }
            Err(e) => {
                warn!(
                    "[CAL] Calibration attempt {attempt} rejected: {e}; keeping live output neutral and retrying"
                );
                attempt += 1;
                std::thread::sleep(CALIBRATION_RETRY_DELAY);
            }
        }
    }
}

fn collect_calibration_reports(
    hid_rx: &std::sync::mpsc::Receiver<usb::hid::HidReport>,
    cal_count: usize,
) -> anyhow::Result<Vec<usb::hid::HidReport>> {
    let mut cal_reports = Vec::with_capacity(cal_count);

    for _ in 0..cal_count {
        match hid_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(report) => cal_reports.push(report),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("HID reader stopped during calibration");
            }
        }
    }

    Ok(cal_reports)
}

/// Run the BT connection loop forever, consuming reports from the macro runtime.
pub async fn run_bt_loop(
    report_rx: &mut watch::Receiver<[u8; 50]>,
    bt_connected: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    loop {
        let mut bt_session = accept_bt_session().await;

        if let Err(e) = bt_session.run_pairing().await {
            error!("[BT] Pairing error: {e}");
            continue;
        }

        info!("[BT] Connected to Switch!");
        bt_connected.store(true, Ordering::Relaxed);
        led::set_led(&led::LED_NORMAL);

        let sender_alive = bt_session.forward_reports(report_rx).await;
        bt_connected.store(false, Ordering::Relaxed);
        led::set_led(&led::LED_NORMAL);

        if !sender_alive {
            warn!("[BT] Report sender dropped. Stopping BT loop.");
            return Ok(());
        }

        warn!("[BT] Switch disconnected. Waiting for reconnection...");
    }
}

async fn accept_bt_session() -> bt::emulator::BtSession {
    loop {
        info!("[BT] Waiting for Switch to connect...");
        match bt::emulator::BtSession::accept().await {
            Ok(session) => return session,
            Err(e) => {
                error!("[BT] Connection error: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}
