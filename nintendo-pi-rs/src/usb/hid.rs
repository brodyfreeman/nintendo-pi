//! HID report reader thread.
//!
//! Runs on a dedicated OS thread (not tokio) because hidapi::read() is blocking.
//! Sends raw 64-byte reports via a bounded mpsc channel to the main async task.

use std::sync::mpsc;
use std::time::Duration;

use tracing::{error, info, warn};

use super::init::{PRODUCT_ID, VENDOR_ID};

/// Raw 64-byte HID report.
pub type HidReport = [u8; 64];

/// Spawn the HID reader thread. Returns a receiver for raw reports.
///
/// The thread will run until the device disconnects or the receiver is dropped.
pub fn spawn_reader(channel_cap: usize) -> mpsc::Receiver<HidReport> {
    let (tx, rx) = mpsc::sync_channel::<HidReport>(channel_cap);

    std::thread::Builder::new()
        .name("hid-reader".into())
        .spawn(move || {
            if let Err(e) = reader_loop(&tx) {
                error!("[HID] Reader thread exited with error: {e}");
            }
        })
        .expect("failed to spawn HID reader thread");

    rx
}

fn reader_loop(tx: &mpsc::SyncSender<HidReport>) -> anyhow::Result<()> {
    info!(
        "[HID] Opening HID device {:04X}:{:04X}...",
        VENDOR_ID, PRODUCT_ID
    );

    let api = hidapi::HidApi::new()?;

    // Retry a few times -- kernel driver may take a moment to appear after init
    let device = {
        let mut dev = None;
        for attempt in 1..=10 {
            match api.open(VENDOR_ID, PRODUCT_ID) {
                Ok(d) => {
                    dev = Some(d);
                    break;
                }
                Err(e) => {
                    if attempt == 10 {
                        return Err(anyhow::anyhow!(
                            "Could not open HID device after 10 attempts: {e}"
                        ));
                    }
                    warn!("[HID] Attempt {attempt}/10 failed: {e}, retrying...");
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
        dev.unwrap()
    };

    // Set non-blocking to false (blocking read with timeout)
    device.set_blocking_mode(true)?;

    info!("[HID] HID device connected. Reading reports...");

    let mut buf = [0u8; 64];
    loop {
        match device.read_timeout(&mut buf, 100) {
            Ok(0) => {
                // Timeout, no data -- just loop again
                continue;
            }
            Ok(n) => {
                if n < 64 {
                    warn!("[HID] Short read: {n} bytes");
                    continue;
                }
                let mut report = [0u8; 64];
                report.copy_from_slice(&buf[..64]);
                if tx.send(report).is_err() {
                    info!("[HID] Channel closed, exiting reader thread.");
                    return Ok(());
                }
            }
            Err(e) => {
                error!("[HID] Read error: {e}");
                return Err(anyhow::anyhow!("HID read error: {e}"));
            }
        }
    }
}
