//! USB initialization sequence for Switch 2 Pro Controller (057E:2069).
//!
//! Sends 17 commands via bulk transfer to wake the controller and set it
//! into the correct input mode. After init, the kernel HID driver takes
//! over and we read reports via hidapi.

use std::time::Duration;

use nusb::transfer::RequestBuffer;
use tracing::{debug, info, warn};

pub const VENDOR_ID: u16 = 0x057E;
pub const PRODUCT_ID: u16 = 0x2069;
const USB_INTERFACE: u8 = 1;
const INIT_DELAY: Duration = Duration::from_millis(50);
const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// The 17-command initialization sequence, ported from enable_procon2.py.
const INIT_COMMANDS: &[&[u8]] = &[
    // 1. INIT_COMMAND_0x03
    &[0x03, 0x91, 0x00, 0x0D, 0x00, 0x08, 0x00, 0x00, 0x01, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
    // 2. UNKNOWN_COMMAND_0x07
    &[0x07, 0x91, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00],
    // 3. UNKNOWN_COMMAND_0x16
    &[0x16, 0x91, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00],
    // 4. REQUEST_CONTROLLER_MAC
    &[0x15, 0x91, 0x00, 0x01, 0x00, 0x0E, 0x00, 0x00, 0x00, 0x02, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
    // 5. LTK_REQUEST
    &[0x15, 0x91, 0x00, 0x02, 0x00, 0x11, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
    // 6. UNKNOWN_COMMAND_0x15_ARG_0x03
    &[0x15, 0x91, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00],
    // 7. UNKNOWN_COMMAND_0x09
    &[0x09, 0x91, 0x00, 0x07, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    // 8. IMU_COMMAND_0x02
    &[0x0C, 0x91, 0x00, 0x02, 0x00, 0x04, 0x00, 0x00, 0x27, 0x00, 0x00, 0x00],
    // 9. OUT_UNKNOWN_COMMAND_0x11
    &[0x11, 0x91, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00],
    // 10. UNKNOWN_COMMAND_0x0A
    &[0x0A, 0x91, 0x00, 0x08, 0x00, 0x14, 0x00, 0x00, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x35, 0x00, 0x46, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    // 11. IMU_COMMAND_0x04
    &[0x0C, 0x91, 0x00, 0x04, 0x00, 0x04, 0x00, 0x00, 0x27, 0x00, 0x00, 0x00],
    // 12. ENABLE_HAPTICS
    &[0x03, 0x91, 0x00, 0x0A, 0x00, 0x04, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00],
    // 13. OUT_UNKNOWN_COMMAND_0x10
    &[0x10, 0x91, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00],
    // 14. OUT_UNKNOWN_COMMAND_0x01
    &[0x01, 0x91, 0x00, 0x0C, 0x00, 0x00, 0x00, 0x00],
    // 15. OUT_UNKNOWN_COMMAND_0x03
    &[0x03, 0x91, 0x00, 0x01, 0x00, 0x00, 0x00],
    // 16. OUT_UNKNOWN_COMMAND_0x0A_ALT
    &[0x0A, 0x91, 0x00, 0x02, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0x00],
    // 17. SET_PLAYER_LED
    &[0x09, 0x91, 0x00, 0x07, 0x00, 0x08, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
];

/// Check if the Switch 2 Pro Controller is present on the USB bus.
pub fn is_device_present() -> bool {
    let Ok(devices) = nusb::list_devices() else { return false };
    devices
        .into_iter()
        .any(|d| d.vendor_id() == VENDOR_ID && d.product_id() == PRODUCT_ID)
}

/// Find and open the Switch 2 Pro Controller USB device.
fn find_device() -> Option<nusb::Device> {
    for dev_info in nusb::list_devices().ok()? {
        if dev_info.vendor_id() == VENDOR_ID && dev_info.product_id() == PRODUCT_ID {
            return dev_info.open().ok();
        }
    }
    None
}

/// Run the 17-command USB initialization sequence.
///
/// This detaches the kernel driver, sends all init commands via bulk transfer,
/// then re-attaches the kernel driver so hidapi can claim the device.
pub async fn initialize_controller() -> anyhow::Result<()> {
    info!("[USB] Searching for Switch 2 Pro Controller...");

    let device = find_device().ok_or_else(|| anyhow::anyhow!("USB device 057E:2069 not found"))?;

    // Detach kernel driver from interface 1 if active
    let _ = device.detach_kernel_driver(USB_INTERFACE);

    let interface = device.claim_interface(USB_INTERFACE)?;

    // Find bulk endpoints on interface 1
    let config = device.active_configuration()?;
    let iface_desc = config
        .interface_alt_settings()
        .find(|i| i.interface_number() == USB_INTERFACE)
        .ok_or_else(|| anyhow::anyhow!("Interface {USB_INTERFACE} not found"))?;

    let mut ep_out = None;
    let mut ep_in = None;
    for ep in iface_desc.endpoints() {
        match ep.direction() {
            nusb::transfer::Direction::Out => ep_out = Some(ep.address()),
            nusb::transfer::Direction::In => ep_in = Some(ep.address()),
        }
    }
    let ep_out = ep_out.ok_or_else(|| anyhow::anyhow!("No bulk OUT endpoint found"))?;

    info!("[USB] Device connected. Sending initialization sequence ({} commands)...", INIT_COMMANDS.len());

    for (i, cmd) in INIT_COMMANDS.iter().enumerate() {
        debug!("[USB] Sending command {}/{}: 0x{:02X}", i + 1, INIT_COMMANDS.len(), cmd[0]);

        let result = interface.bulk_out(ep_out, cmd.to_vec()).await;
        if let Err(e) = result.status {
            warn!("[USB] Command {} send error: {}", i + 1, e);
        }

        // Try to read response (best effort, may timeout)
        if let Some(ep_in_addr) = ep_in {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let read_fut = interface.bulk_in(ep_in_addr, RequestBuffer::new(64));
            match tokio::time::timeout(READ_TIMEOUT, read_fut).await {
                Ok(completion) => {
                    if let Err(e) = completion.status {
                        debug!("[USB] Command {} read error (ok): {}", i + 1, e);
                    }
                }
                Err(_) => {
                    debug!("[USB] Command {} read timeout (ok)", i + 1);
                }
            }
        }

        tokio::time::sleep(INIT_DELAY).await;
    }

    // Release interface and reattach kernel driver
    drop(interface);
    let _ = device.attach_kernel_driver(USB_INTERFACE);

    info!("[USB] Initialization sequence complete!");
    Ok(())
}

/// Send an LED command to the physical controller.
/// Opens a fresh USB connection, sends the command, and closes.
/// Best-effort: errors are logged but not propagated.
pub fn send_led_command(pattern: &[u8]) {
    let Some(device) = find_device() else {
        debug!("[LED] Device not found for LED write");
        return;
    };

    let _ = device.detach_kernel_driver(USB_INTERFACE);

    let Ok(interface) = device.claim_interface(USB_INTERFACE) else {
        debug!("[LED] Could not claim interface for LED write");
        return;
    };

    // Find OUT endpoint
    let Ok(config) = device.active_configuration() else { return };
    let Some(iface_desc) = config
        .interface_alt_settings()
        .find(|i| i.interface_number() == USB_INTERFACE)
    else {
        return;
    };

    let Some(ep_out) = iface_desc
        .endpoints()
        .find(|ep| ep.direction() == nusb::transfer::Direction::Out)
        .map(|ep| ep.address())
    else {
        return;
    };

    // Fire and forget -- queue the transfer but don't await
    let _ = interface.bulk_out(ep_out, pattern.to_vec());

    // Reattach kernel driver
    drop(interface);
    let _ = device.attach_kernel_driver(USB_INTERFACE);
}
