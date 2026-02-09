//! L2CAP server for Pro Controller emulation.
//!
//! Listens on PSM 17 (control) and PSM 19 (interrupt) for the Switch to connect.
//! Handles the pairing subcommand sequence, then switches to forwarding
//! 0x30 input reports from the main loop.

use std::time::Duration;

use bluer::l2cap::{SocketAddr, Stream, StreamListener};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info, warn};

use super::protocol;

/// PSM for HID Control channel.
const PSM_CONTROL: u16 = 17;
/// PSM for HID Interrupt channel.
const PSM_INTERRUPT: u16 = 19;

/// A connected BT session with the Switch.
pub struct BtSession {
    control: Stream,
    interrupt: Stream,
}

/// Accept a connection from the Switch on both L2CAP channels.
///
/// Blocks until both channels are connected.
pub async fn accept_connection() -> anyhow::Result<BtSession> {
    info!("[BT] Starting L2CAP listeners on PSM {PSM_CONTROL} (control) and {PSM_INTERRUPT} (interrupt)...");

    // Bind control channel
    let control_listener = StreamListener::bind(SocketAddr::new(
        bluer::Address::any(),
        bluer::AddressType::BrEdr,
        PSM_CONTROL,
    ))
    .await?;

    // Bind interrupt channel
    let interrupt_listener = StreamListener::bind(SocketAddr::new(
        bluer::Address::any(),
        bluer::AddressType::BrEdr,
        PSM_INTERRUPT,
    ))
    .await?;

    info!("[BT] Waiting for Switch to connect...");
    info!("[BT] >> Open 'Change Grip/Order' on the Switch <<");

    // Accept both channels (control first, then interrupt)
    let (control, control_addr) = control_listener.accept().await?;
    info!("[BT] Control channel connected from {}", control_addr.addr);

    let (interrupt, interrupt_addr) = interrupt_listener.accept().await?;
    info!("[BT] Interrupt channel connected from {}", interrupt_addr.addr);

    Ok(BtSession { control, interrupt })
}

/// Run the pairing handshake on the control channel.
///
/// The Switch sends subcommands (report type 0x01 or 0x80) and we reply with
/// the appropriate data. This continues until the Switch switches to 0x30 mode.
pub async fn run_pairing(session: &mut BtSession) -> anyhow::Result<()> {
    info!("[BT] Starting pairing handshake...");

    let mut timer: u8 = 0;
    let mut buf = [0u8; 512];

    loop {
        // Read from control channel with timeout
        let n = match tokio::time::timeout(Duration::from_secs(10), session.control.read(&mut buf)).await {
            Ok(Ok(0)) => {
                warn!("[BT] Control channel closed during pairing");
                return Err(anyhow::anyhow!("Control channel closed during pairing"));
            }
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                return Err(anyhow::anyhow!("Control channel read error: {e}"));
            }
            Err(_) => {
                debug!("[BT] Control read timeout during pairing, continuing...");
                continue;
            }
        };

        let data = &buf[..n];
        debug!("[BT] Pairing recv ({n} bytes): {:02X?}", &data[..n.min(20)]);

        if data.is_empty() {
            continue;
        }

        match data[0] {
            // 0xA1 prefix + 0x01: Subcommand
            0x01 | 0x11 => {
                // Subcommand is at byte 10 (after header + rumble data)
                if n >= 11 {
                    let subcmd_id = data[10];
                    let subcmd_data = if n > 11 { &data[11..] } else { &[] };

                    let (ack, reply_data) = protocol::handle_subcommand(subcmd_id, subcmd_data);
                    let reply = protocol::build_subcommand_reply(timer, subcmd_id, ack, &reply_data);

                    timer = timer.wrapping_add(1);

                    debug!("[BT] Replying to subcmd 0x{subcmd_id:02X} with ACK 0x{ack:02X}");
                    session.interrupt.write_all(&reply).await?;

                    // If the Switch requested input mode 0x30, we're done pairing
                    if subcmd_id == 0x03 {
                        info!("[BT] Switch requested standard input mode -- pairing complete!");
                        // Send a few empty 0x30 reports to keep the connection alive
                        for _ in 0..3 {
                            let empty_report = protocol::build_subcommand_reply(timer, 0, 0, &[]);
                            let mut input_report = [0u8; 50];
                            input_report[0] = 0x30;
                            input_report[1] = timer;
                            input_report[2] = 0x8E;
                            // Center sticks
                            input_report[5] = 0x00;
                            input_report[6] = 0x08;
                            input_report[7] = 0x80;
                            input_report[8] = 0x00;
                            input_report[9] = 0x08;
                            input_report[10] = 0x80;
                            session.interrupt.write_all(&input_report).await?;
                            timer = timer.wrapping_add(1);
                            tokio::time::sleep(Duration::from_millis(15)).await;
                        }
                        return Ok(());
                    }
                }
            }

            // 0x80: Subcommand without rumble data
            0x80 => {
                if n >= 2 {
                    match data[1] {
                        // 0x01: Request connection status
                        0x01 => {
                            let reply = [0x81u8, 0x01, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00];
                            session.control.write_all(&reply).await?;
                            debug!("[BT] Replied to 0x80/0x01 connection status request");
                        }
                        // 0x02: Handshake
                        0x02 => {
                            let reply = [0x81u8, 0x02];
                            session.control.write_all(&reply).await?;
                            debug!("[BT] Replied to 0x80/0x02 handshake");
                        }
                        // 0x03: Set baudrate for 3Mbit (we ACK but don't change anything)
                        0x03 => {
                            let reply = [0x81u8, 0x03];
                            session.control.write_all(&reply).await?;
                            debug!("[BT] Replied to 0x80/0x03 baudrate");
                        }
                        // 0x04: Force USB
                        0x04 => {
                            debug!("[BT] Received 0x80/0x04 (ignore)");
                        }
                        // 0x05: Disable USB timeout
                        0x05 => {
                            let reply = [0x81u8, 0x05];
                            session.control.write_all(&reply).await?;
                            debug!("[BT] Replied to 0x80/0x05 USB timeout");
                        }
                        sub => {
                            debug!("[BT] Unknown 0x80 sub-type: 0x{sub:02X}");
                        }
                    }
                }
            }

            other => {
                debug!("[BT] Unknown report type during pairing: 0x{other:02X}");
            }
        }
    }
}

/// Send a 0x30 input report on the interrupt channel.
pub async fn send_input_report(session: &mut BtSession, report: &[u8]) -> anyhow::Result<()> {
    session.interrupt.write_all(report).await?;
    Ok(())
}

/// Check for and handle any incoming subcommands on the control channel (non-blocking).
/// Returns true if a disconnect was detected.
pub async fn poll_control(session: &mut BtSession, timer: &mut u8) -> anyhow::Result<bool> {
    let mut buf = [0u8; 512];

    // Non-blocking read on control channel
    match tokio::time::timeout(Duration::from_millis(1), session.control.read(&mut buf)).await {
        Ok(Ok(0)) => {
            info!("[BT] Control channel closed by Switch");
            return Ok(true);
        }
        Ok(Ok(n)) => {
            let data = &buf[..n];
            debug!("[BT] Control recv ({n} bytes): {:02X?}", &data[..n.min(20)]);

            // Handle any subcommands that come in during normal operation
            if !data.is_empty() && (data[0] == 0x01 || data[0] == 0x11) && n >= 11 {
                let subcmd_id = data[10];
                let subcmd_data = if n > 11 { &data[11..] } else { &[] };
                let (ack, reply_data) = protocol::handle_subcommand(subcmd_id, subcmd_data);
                let reply = protocol::build_subcommand_reply(*timer, subcmd_id, ack, &reply_data);
                *timer = timer.wrapping_add(1);
                let _ = session.interrupt.write_all(&reply).await;
            }
        }
        Ok(Err(_)) => {
            // Read error -- possible disconnect
            return Ok(true);
        }
        Err(_) => {
            // Timeout -- no data available, that's fine
        }
    }

    Ok(false)
}
