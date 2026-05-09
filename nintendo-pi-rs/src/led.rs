//! LED patterns and USB writes for the physical controller.
//!
//! Owns both the 16-byte LED patterns and the USB bulk write to send them.

use tracing::debug;

use crate::usb::init::{PRODUCT_ID, VENDOR_ID};

const USB_INTERFACE: u8 = 1;

/// Player 1 pattern (normal): LED 1 on.
pub const LED_NORMAL: [u8; 16] = [
    0x09, 0x91, 0x00, 0x07, 0x00, 0x08, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Recording pattern: all LEDs blinking.
pub const LED_RECORDING: [u8; 16] = [
    0x09, 0x91, 0x00, 0x07, 0x00, 0x08, 0x00, 0x00, 0x0F, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Playback pattern: LEDs 1+3 on.
pub const LED_PLAYBACK: [u8; 16] = [
    0x09, 0x91, 0x00, 0x07, 0x00, 0x08, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Send an LED pattern to the physical controller (best-effort, non-blocking).
pub fn set_led(pattern: &[u8; 16]) {
    let pattern = *pattern;
    std::thread::spawn(move || {
        send_led_command(&pattern);
    });
}

/// Send an LED command via USB bulk write.
/// Opens a fresh USB connection, sends the command, and closes.
/// Best-effort: errors are logged but not propagated.
fn send_led_command(pattern: &[u8]) {
    let Some(device) = find_device() else {
        debug!("[LED] Device not found for LED write");
        return;
    };

    let _ = device.detach_kernel_driver(USB_INTERFACE);

    let Ok(interface) = device.claim_interface(USB_INTERFACE) else {
        debug!("[LED] Could not claim interface for LED write");
        return;
    };

    let Ok(config) = device.active_configuration() else {
        return;
    };
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
    drop(interface.bulk_out(ep_out, pattern.to_vec()));

    // Reattach kernel driver
    drop(interface);
    let _ = device.attach_kernel_driver(USB_INTERFACE);
}

fn find_device() -> Option<nusb::Device> {
    for dev_info in nusb::list_devices().ok()? {
        if dev_info.vendor_id() == VENDOR_ID && dev_info.product_id() == PRODUCT_ID {
            return dev_info.open().ok();
        }
    }
    None
}
