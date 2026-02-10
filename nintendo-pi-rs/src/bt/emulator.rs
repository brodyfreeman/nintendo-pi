//! L2CAP server for Pro Controller emulation.
//!
//! Uses raw AF_BLUETOOTH L2CAP sockets (not bluer) for compatibility
//! with the Nintendo Switch 2. Listens on PSM 17 (control) and PSM 19
//! (interrupt) for the Switch to connect, then handles the pairing
//! subcommand sequence before forwarding 0x30 input reports.

use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Duration;

use tokio::io::unix::AsyncFd;
use tokio::io::Interest;
use tracing::{debug, info, warn};

use super::protocol;

/// PSM for HID Control channel.
const PSM_CONTROL: u16 = 17;
/// PSM for HID Interrupt channel.
const PSM_INTERRUPT: u16 = 19;

// Bluetooth socket constants
const AF_BLUETOOTH: i32 = 31;
const BTPROTO_L2CAP: i32 = 0;
const BDADDR_ANY: [u8; 6] = [0; 6];

/// sockaddr_l2 structure for L2CAP sockets.
#[repr(C)]
struct SockAddrL2 {
    l2_family: u16,
    l2_psm: u16, // little-endian
    l2_bdaddr: [u8; 6],
    l2_cid: u16,
    l2_bdaddr_type: u8,
}

/// An async wrapper around a raw L2CAP socket file descriptor.
struct L2capSocket {
    inner: AsyncFd<RawFdWrapper>,
}

/// Wrapper to impl AsRawFd for a raw fd.
struct RawFdWrapper(RawFd);

impl AsRawFd for RawFdWrapper {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Drop for RawFdWrapper {
    fn drop(&mut self) {
        unsafe { libc::close(self.0); }
    }
}

impl L2capSocket {
    fn from_raw_fd(fd: RawFd) -> io::Result<Self> {
        // Set non-blocking for tokio
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            inner: AsyncFd::new(RawFdWrapper(fd))?,
        })
    }

    async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.inner.readable().await?;
            match guard.try_io(|inner| {
                let n = unsafe {
                    libc::recv(inner.as_raw_fd(), buf.as_mut_ptr() as *mut _, buf.len(), 0)
                };
                if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
            }) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }

    async fn write_all(&self, data: &[u8]) -> io::Result<()> {
        let mut written = 0;
        while written < data.len() {
            let mut guard = self.inner.writable().await?;
            match guard.try_io(|inner| {
                let n = unsafe {
                    libc::send(
                        inner.as_raw_fd(),
                        data[written..].as_ptr() as *const _,
                        data.len() - written,
                        0,
                    )
                };
                if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
            }) {
                Ok(Ok(n)) => written += n,
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
        Ok(())
    }
}

/// A connected BT session with the Switch.
pub struct BtSession {
    control: L2capSocket,
    interrupt: L2capSocket,
}

/// Create and bind a raw L2CAP listener socket.
fn bind_l2cap(psm: u16) -> io::Result<RawFd> {
    let fd = unsafe {
        libc::socket(AF_BLUETOOTH, libc::SOCK_SEQPACKET, BTPROTO_L2CAP)
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    let addr = SockAddrL2 {
        l2_family: AF_BLUETOOTH as u16,
        l2_psm: psm.to_le(),
        l2_bdaddr: BDADDR_ANY,
        l2_cid: 0,
        l2_bdaddr_type: 0, // BREDR
    };

    let ret = unsafe {
        libc::bind(
            fd,
            &addr as *const SockAddrL2 as *const libc::sockaddr,
            std::mem::size_of::<SockAddrL2>() as u32,
        )
    };
    if ret < 0 {
        let err = io::Error::last_os_error();
        unsafe { libc::close(fd); }
        if err.kind() == io::ErrorKind::AddrInUse {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "L2CAP PSM {psm} already in use — \
                     ensure bluetoothd runs with --noplugin=input \
                     (edit /lib/systemd/system/bluetooth.service, add --noplugin=input to ExecStart)"
                ),
            ));
        }
        return Err(err);
    }

    let ret = unsafe { libc::listen(fd, 1) };
    if ret < 0 {
        let err = io::Error::last_os_error();
        unsafe { libc::close(fd); }
        return Err(err);
    }

    Ok(fd)
}

/// Async accept on a raw listening socket.
async fn async_accept(listener_fd: RawFd) -> io::Result<RawFd> {
    // Set listener non-blocking for async accept
    let flags = unsafe { libc::fcntl(listener_fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { libc::fcntl(listener_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };

    let async_fd = AsyncFd::with_interest(
        RawFdWrapper(listener_fd),
        Interest::READABLE,
    )?;

    loop {
        let mut guard = async_fd.readable().await?;
        match guard.try_io(|inner| {
            let mut peer_addr: SockAddrL2 = unsafe { std::mem::zeroed() };
            let mut addr_len = std::mem::size_of::<SockAddrL2>() as u32;
            let client_fd = unsafe {
                libc::accept(
                    inner.as_raw_fd(),
                    &mut peer_addr as *mut SockAddrL2 as *mut libc::sockaddr,
                    &mut addr_len,
                )
            };
            if client_fd < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(client_fd)
            }
        }) {
            Ok(result) => {
                // Prevent the AsyncFd from closing the listener fd on drop
                let _ = std::mem::ManuallyDrop::new(async_fd);
                return result;
            }
            Err(_would_block) => continue,
        }
    }
}

/// Accept a connection from the Switch on both L2CAP channels.
///
/// Binds listeners, then accepts both channels concurrently.
/// The BT HID spec requires control (PSM 17) before interrupt (PSM 19),
/// but the Switch may connect them in either order, so we accept both
/// concurrently to avoid deadlocking on a sequential accept.
pub async fn accept_connection() -> anyhow::Result<BtSession> {
    info!("[BT] Starting L2CAP listeners on PSM {PSM_CONTROL} (control) and {PSM_INTERRUPT} (interrupt)...");

    let ctrl_listener = bind_l2cap(PSM_CONTROL)?;
    let itr_listener = bind_l2cap(PSM_INTERRUPT)?;

    info!("[BT] Waiting for Switch to connect...");
    info!("[BT] >> Open 'Change Grip/Order' on the Switch <<");

    // Accept both channels concurrently — the Switch may connect them in either order
    let (ctrl_result, itr_result) = tokio::join!(
        async_accept(ctrl_listener),
        async_accept(itr_listener),
    );

    // Close listeners regardless of result
    unsafe { libc::close(ctrl_listener); }
    unsafe { libc::close(itr_listener); }

    let ctrl_fd = ctrl_result?;
    info!("[BT] Control channel connected");
    let itr_fd = itr_result?;
    info!("[BT] Interrupt channel connected");

    let control = L2capSocket::from_raw_fd(ctrl_fd)?;
    let interrupt = L2capSocket::from_raw_fd(itr_fd)?;

    Ok(BtSession { control, interrupt })
}

/// Run the pairing handshake on the interrupt channel (matches NXBT approach).
///
/// NXBT only reads from the interrupt channel during pairing and sends a
/// report every cycle. The Switch sends 0xA2-prefixed subcommand data on
/// the interrupt channel. We respond with 0xA1-prefixed replies.
///
/// Pairing is complete when both vibration is enabled AND player lights are set.
pub async fn run_pairing(session: &mut BtSession) -> anyhow::Result<()> {
    info!("[BT] Starting pairing handshake...");

    let mut timer: u8 = 0;
    let mut itr_buf = [0u8; 512];
    let mut vibration_enabled = false;
    let mut player_set = false;
    let mut device_info_queried = false;
    let mut received_first_message = false;

    // Send an initial empty report to prompt the Switch (like NXBT)
    let initial_report = build_empty_input_report(timer, device_info_queried);
    session.interrupt.write_all(&initial_report).await?;
    timer = timer.wrapping_add(1);

    loop {
        // Non-blocking read from interrupt channel only (like NXBT)
        let reply_data = tokio::select! {
            result = session.interrupt.read(&mut itr_buf) => {
                match result {
                    Ok(0) => {
                        warn!("[BT] Interrupt channel closed during pairing");
                        return Err(anyhow::anyhow!("Interrupt channel closed"));
                    }
                    Ok(n) => Some(n),
                    Err(e) => {
                        return Err(anyhow::anyhow!("Interrupt read error: {e}"));
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(if received_first_message { 66 } else { 1000 })) => {
                None
            }
        };

        if let Some(n) = reply_data {
            let data = &itr_buf[..n];
            debug!("[BT] Pairing recv ({n} bytes): {:02X?}", &data[..n.min(30)]);

            if !received_first_message {
                received_first_message = true;
            }

            // Parse incoming data — handle both with and without 0xA2 prefix
            let (report_type, subcmd_offset) = if n > 0 && data[0] == 0xA2 {
                // NXBT-style: 0xA2 prefix, subcommand at data[11]
                if n >= 2 { (data[1], 11usize) } else { continue; }
            } else if n > 0 {
                // Direct report type (no HID header)
                (data[0], 10usize)
            } else {
                continue;
            };

            match report_type {
                // Subcommand with rumble data
                0x01 | 0x11 => {
                    if n > subcmd_offset {
                        let subcmd_id = data[subcmd_offset];
                        let subcmd_data = if n > subcmd_offset + 1 { &data[subcmd_offset + 1..] } else { &[] };

                        let (ack, reply_data) = protocol::handle_subcommand(subcmd_id, subcmd_data);
                        let reply = protocol::build_subcommand_reply(timer, subcmd_id, ack, &reply_data);
                        timer = timer.wrapping_add(1);

                        info!("[BT] Pairing: subcmd 0x{subcmd_id:02X} -> ACK 0x{ack:02X}");
                        session.interrupt.write_all(&reply).await?;

                        // Track pairing progress
                        if subcmd_id == 0x02 {
                            device_info_queried = true;
                        } else if subcmd_id == 0x48 {
                            vibration_enabled = true;
                        } else if subcmd_id == 0x30 {
                            player_set = true;
                        }

                        // Check if pairing is complete (like NXBT)
                        if vibration_enabled && player_set {
                            info!("[BT] Pairing complete! (vibration enabled + player lights set)");
                            return Ok(());
                        }

                        continue; // Already sent a reply, skip the default report below
                    }
                }

                _ => {
                    debug!("[BT] Pairing: unknown report type 0x{report_type:02X}");
                }
            }
        }

        // Send a standard input report every cycle (like NXBT)
        let report = build_empty_input_report(timer, device_info_queried);
        timer = timer.wrapping_add(1);
        if let Err(e) = session.interrupt.write_all(&report).await {
            debug!("[BT] Pairing send error: {e}");
        }
    }
}

/// Build an empty 0x30 input report for pairing (NXBT-compatible).
fn build_empty_input_report(timer: u8, include_state: bool) -> [u8; 50] {
    let mut report = [0u8; 50];
    report[0] = 0xA1; // HID transaction header
    report[1] = 0x30; // Standard full input report
    report[2] = timer;

    if include_state {
        report[3] = 0x90; // Battery level (full) + connection info

        // Buttons at neutral (zeros) — [4..6]

        // Left stick at center — NXBT uses [0x6F, 0xC8, 0x77]
        report[7] = 0x6F;
        report[8] = 0xC8;
        report[9] = 0x77;
        // Right stick at center — NXBT uses [0x16, 0xD8, 0x7D]
        report[10] = 0x16;
        report[11] = 0xD8;
        report[12] = 0x7D;

        // Vibrator byte
        report[13] = 0xB0;
    }

    report
}

/// Send a 0x30 input report on the interrupt channel.
pub async fn send_input_report(session: &mut BtSession, report: &[u8]) -> anyhow::Result<()> {
    session.interrupt.write_all(report).await?;
    Ok(())
}

/// Check for and handle any incoming subcommands on the interrupt channel (non-blocking).
/// Returns true if a disconnect was detected.
pub async fn poll_control(session: &mut BtSession, timer: &mut u8) -> anyhow::Result<bool> {
    let mut itr_buf = [0u8; 512];

    // Non-blocking read on interrupt channel (like NXBT)
    tokio::select! {
        result = session.interrupt.read(&mut itr_buf) => {
            match result {
                Ok(0) => {
                    info!("[BT] Interrupt channel closed by Switch");
                    return Ok(true);
                }
                Ok(n) => {
                    let data = &itr_buf[..n];
                    debug!("[BT] Interrupt recv ({n} bytes): {:02X?}", &data[..n.min(20)]);
                    handle_incoming_subcommand(session, data, n, timer).await;
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::ConnectionReset {
                        return Ok(true);
                    }
                    // Other errors may be transient
                    debug!("[BT] Interrupt read error: {e}");
                }
            }
        }
        _ = tokio::time::sleep(Duration::from_millis(1)) => {
            // Timeout -- no data available, that's fine
        }
    }

    Ok(false)
}

/// Handle an incoming subcommand during normal operation.
/// Handles both 0xA2-prefixed (NXBT-style) and raw report data.
async fn handle_incoming_subcommand(session: &mut BtSession, data: &[u8], n: usize, timer: &mut u8) {
    if data.is_empty() {
        return;
    }

    // Determine report type and subcmd offset, handling optional 0xA2 prefix
    let (report_type, subcmd_offset) = if data[0] == 0xA2 && n >= 2 {
        (data[1], 11usize)
    } else {
        (data[0], 10usize)
    };

    if (report_type == 0x01 || report_type == 0x11) && n > subcmd_offset {
        let subcmd_id = data[subcmd_offset];
        let subcmd_data = if n > subcmd_offset + 1 { &data[subcmd_offset + 1..] } else { &[] };
        let (ack, reply_data) = protocol::handle_subcommand(subcmd_id, subcmd_data);
        let reply = protocol::build_subcommand_reply(*timer, subcmd_id, ack, &reply_data);
        *timer = timer.wrapping_add(1);
        debug!("[BT] Subcmd 0x{subcmd_id:02X} -> ACK 0x{ack:02X}");
        let _ = session.interrupt.write_all(&reply).await;
    }
}
