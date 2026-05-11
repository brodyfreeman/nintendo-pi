//! Pro Controller emulation over L2CAP.
//!
//! `BtSession` owns a connection to the Switch and handles the full
//! lifecycle: accept, pair, forward input reports, respond to subcommands.

use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracing::{debug, info, warn};

use super::l2cap::L2capSocket;
use super::protocol;

/// Human-readable name for a BT subcommand ID.
fn subcmd_name(id: u8) -> &'static str {
    match id {
        0x02 => "RequestDeviceInfo",
        0x03 => "SetInputReportMode",
        0x04 => "TriggerButtonsElapsed",
        0x08 => "SetShipmentState",
        0x10 => "SPIFlashRead",
        0x21 => "SetNfcIrMcuConfig",
        0x22 => "SetNfcIrState",
        0x30 => "SetPlayerLights",
        0x38 => "SetHomeLed",
        0x40 => "EnableIMU",
        0x41 => "SetIMUSensitivity",
        0x48 => "EnableVibration",
        _ => "Unknown",
    }
}

/// PSM for HID Control channel.
const PSM_CONTROL: u16 = 17;
/// PSM for HID Interrupt channel.
const PSM_INTERRUPT: u16 = 19;

/// A connected BT session with the Switch.
pub struct BtSession {
    /// Held open to keep the L2CAP control channel alive (PSM 17).
    _control: L2capSocket,
    interrupt: L2capSocket,
    report_timer: u8,
}

impl BtSession {
    /// Accept a connection from the Switch on both L2CAP channels.
    ///
    /// The Switch may connect control and interrupt in either order,
    /// so we accept both concurrently.
    pub async fn accept() -> anyhow::Result<Self> {
        info!("[BT] Starting L2CAP listeners on PSM {PSM_CONTROL} (control) and {PSM_INTERRUPT} (interrupt)...");

        let ctrl_listener = super::l2cap::bind_and_listen(PSM_CONTROL)?;
        let itr_listener = super::l2cap::bind_and_listen(PSM_INTERRUPT)?;

        info!("[BT] Waiting for Switch to connect...");
        info!("[BT] >> Open 'Change Grip/Order' on the Switch <<");

        let wait_start = Instant::now();

        let (ctrl_result, itr_result) = tokio::join!(
            super::l2cap::accept(ctrl_listener),
            super::l2cap::accept(itr_listener),
        );

        // Close listeners regardless of result
        unsafe {
            libc::close(ctrl_listener);
        }
        unsafe {
            libc::close(itr_listener);
        }

        let control = ctrl_result?;
        info!("[BT] Control channel connected");
        let interrupt = itr_result?;
        info!(
            "[BT] Interrupt channel connected (both channels up in {:.1}s)",
            wait_start.elapsed().as_secs_f64()
        );

        Ok(Self {
            _control: control,
            interrupt,
            report_timer: 0,
        })
    }

    /// Run the pairing handshake with the Switch.
    ///
    /// Sends keepalive reports and responds to subcommands until both
    /// vibration is enabled and player lights are set.
    pub async fn run_pairing(&mut self) -> anyhow::Result<()> {
        info!("[BT] Starting pairing handshake...");

        let pairing_start = Instant::now();
        let mut interrupt_buffer = [0u8; 512];
        let mut progress = PairingProgress::default();
        let mut subcmd_count: u32 = 0;

        // Send an initial empty report to prompt the Switch (like NXBT)
        let initial_report =
            build_empty_input_report(self.report_timer, progress.device_info_queried);
        self.interrupt.write_all(&initial_report).await?;
        self.report_timer = self.report_timer.wrapping_add(1);
        debug!("[BT] Sent initial empty report to prompt Switch");

        loop {
            let reply_data = tokio::select! {
                result = self.interrupt.read(&mut interrupt_buffer) => {
                    match result {
                        Ok(0) => {
                            warn!(
                                "[BT] Interrupt channel closed during pairing after {subcmd_count} subcommands ({:.1}s)",
                                pairing_start.elapsed().as_secs_f64()
                            );
                            return Err(anyhow::anyhow!("Interrupt channel closed"));
                        }
                        Ok(n) => Some(n),
                        Err(e) => {
                            warn!(
                                "[BT] Interrupt read error during pairing after {subcmd_count} subcommands: {e}"
                            );
                            return Err(anyhow::anyhow!("Interrupt read error: {e}"));
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(if progress.received_first_message { 66 } else { 1000 })) => {
                    None
                }
            };

            if let Some(n) = reply_data {
                let data = &interrupt_buffer[..n];
                debug!("[BT] Pairing recv ({n} bytes): {:02X?}", &data[..n.min(30)]);

                if !progress.received_first_message {
                    info!(
                        "[BT] First message from Switch after {:.1}s",
                        pairing_start.elapsed().as_secs_f64()
                    );
                }
                progress.received_first_message = true;

                if let Some((subcmd_id, subcmd_data)) = extract_subcommand(data) {
                    subcmd_count += 1;
                    let (ack, reply_data) = protocol::handle_subcommand(subcmd_id, subcmd_data);
                    let reply = protocol::build_subcommand_reply(
                        self.report_timer,
                        subcmd_id,
                        ack,
                        &reply_data,
                    );
                    self.report_timer = self.report_timer.wrapping_add(1);

                    let name = subcmd_name(subcmd_id);
                    info!(
                        "[BT] Pairing [{subcmd_count}]: {name} (0x{subcmd_id:02X}) -> ACK 0x{ack:02X}, reply {} bytes",
                        reply_data.len()
                    );
                    self.interrupt.write_all(&reply).await?;

                    progress.track(subcmd_id);

                    if progress.is_complete() {
                        info!(
                            "[BT] Pairing complete in {:.1}s ({subcmd_count} subcommands handled)",
                            pairing_start.elapsed().as_secs_f64()
                        );
                        return Ok(());
                    }

                    continue;
                }
            }

            // Send a standard input report every cycle (like NXBT)
            let report = build_empty_input_report(self.report_timer, progress.device_info_queried);
            self.report_timer = self.report_timer.wrapping_add(1);
            if let Err(e) = self.interrupt.write_all(&report).await {
                warn!("[BT] Pairing: failed to send keepalive report: {e}");
            }
        }
    }

    /// Forward input reports from USB processing to the Switch.
    ///
    /// Receives calibrated BT reports via `report_rx`, stamps each with a
    /// monotonic timer byte, sends them to the Switch, and handles any
    /// incoming subcommands.
    ///
    /// Returns `true` if the report sender is still alive (BT disconnected),
    /// or `false` if the report sender dropped.
    pub async fn forward_reports(&mut self, report_rx: &mut watch::Receiver<[u8; 50]>) -> bool {
        let _ = report_rx.borrow_and_update();

        loop {
            if report_rx.changed().await.is_err() {
                return false;
            }

            let mut report = *report_rx.borrow_and_update();
            report[2] = self.report_timer;
            self.report_timer = self.report_timer.wrapping_add(1);

            if let Err(e) = self.interrupt.write_all(&report).await {
                warn!("[BT] Send error: {e}");
                return true;
            }

            if self.poll_and_handle_subcommands().await {
                return true;
            }
        }
    }

    /// Non-blocking check for incoming subcommands. Returns `true` on disconnect.
    async fn poll_and_handle_subcommands(&mut self) -> bool {
        let mut interrupt_buffer = [0u8; 512];

        tokio::select! {
            result = self.interrupt.read(&mut interrupt_buffer) => {
                match result {
                    Ok(0) => {
                        info!("[BT] Interrupt channel closed by Switch");
                        return true;
                    }
                    Ok(n) => {
                        let data = &interrupt_buffer[..n];
                        debug!("[BT] Interrupt recv ({n} bytes): {:02X?}", &data[..n.min(20)]);
                        self.reply_to_subcommand(data).await;
                    }
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::ConnectionReset {
                            info!("[BT] Interrupt channel reset by Switch (ConnectionReset)");
                            return true;
                        }
                        debug!("[BT] Interrupt read error: {e}");
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(1)) => {}
        }

        false
    }

    /// Parse and reply to a subcommand embedded in a received packet.
    async fn reply_to_subcommand(&mut self, data: &[u8]) {
        if let Some((subcmd_id, subcmd_data)) = extract_subcommand(data) {
            let (ack, reply_data) = protocol::handle_subcommand(subcmd_id, subcmd_data);
            let reply =
                protocol::build_subcommand_reply(self.report_timer, subcmd_id, ack, &reply_data);
            self.report_timer = self.report_timer.wrapping_add(1);
            debug!(
                "[BT] Subcmd {} (0x{subcmd_id:02X}) -> ACK 0x{ack:02X}",
                subcmd_name(subcmd_id)
            );
            if let Err(e) = self.interrupt.write_all(&reply).await {
                warn!(
                    "[BT] Failed to send reply for {} (0x{subcmd_id:02X}): {e}",
                    subcmd_name(subcmd_id)
                );
            }
        }
    }
}

/// Tracks which pairing subcommands the Switch has sent.
#[derive(Default)]
struct PairingProgress {
    device_info_queried: bool,
    vibration_enabled: bool,
    player_set: bool,
    received_first_message: bool,
}

impl PairingProgress {
    fn is_complete(&self) -> bool {
        self.vibration_enabled && self.player_set
    }

    fn track(&mut self, subcmd_id: u8) {
        match subcmd_id {
            0x02 => self.device_info_queried = true,
            0x48 => self.vibration_enabled = true,
            0x30 => self.player_set = true,
            _ => {}
        }
    }
}

/// Build an empty 0x30 input report for pairing (NXBT-compatible).
fn build_empty_input_report(timer: u8, include_state: bool) -> [u8; 50] {
    let mut report = [0u8; 50];
    report[0] = 0xA1;
    report[1] = 0x30;
    report[2] = timer;
    if include_state {
        report[3] = 0x90; // Battery level (full) + connection info
        report[7..10].copy_from_slice(&[0x6F, 0xC8, 0x77]); // Left stick center (NXBT)
        report[10..13].copy_from_slice(&[0x16, 0xD8, 0x7D]); // Right stick center (NXBT)
        report[13] = 0xB0; // Vibrator byte
    }
    report
}

/// Parse report type and subcommand offset, handling optional 0xA2 prefix.
fn parse_report_header(data: &[u8]) -> Option<(u8, usize)> {
    match data {
        [0xA2, report_type, ..] => Some((*report_type, 11)),
        [report_type, ..] => Some((*report_type, 10)),
        [] => None,
    }
}

/// Extract subcommand ID and payload from a report, if present.
fn extract_subcommand(data: &[u8]) -> Option<(u8, &[u8])> {
    let (report_type, subcmd_offset) = parse_report_header(data)?;
    if (report_type == 0x01 || report_type == 0x11) && data.len() > subcmd_offset {
        let subcmd_id = data[subcmd_offset];
        let subcmd_data = data.get(subcmd_offset + 1..).unwrap_or(&[]);
        Some((subcmd_id, subcmd_data))
    } else {
        None
    }
}
