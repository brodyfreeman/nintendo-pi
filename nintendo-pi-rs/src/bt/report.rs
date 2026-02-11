//! Bluetooth 0x30 input report building.
//!
//! Converts parsed USB input state into the BT Pro Controller wire format
//! (NXBT-compatible 50-byte reports).

use crate::input::{Button, ButtonState, InputState};

/// BT button mapping: (Button, bt_byte_index, bt_mask).
///
/// The BT wire format uses a different bit layout than USB HID.
const BT_BUTTON_MAP: [(Button, usize, u8); 18] = [
    // Byte 0 (right-side): Y X B A _ _ R ZR
    (Button::Y, 0, 0x01),
    (Button::X, 0, 0x02),
    (Button::B, 0, 0x04),
    (Button::A, 0, 0x08),
    (Button::R, 0, 0x40),
    (Button::ZR, 0, 0x80),
    // Byte 1 (shared): MINUS PLUS RSTICK LSTICK HOME CAP _ _
    (Button::Minus, 1, 0x01),
    (Button::Plus, 1, 0x02),
    (Button::R3, 1, 0x04),
    (Button::L3, 1, 0x08),
    (Button::Home, 1, 0x10),
    (Button::Capture, 1, 0x20),
    // Byte 2 (left-side): DD DU DR DL _ _ L ZL
    (Button::DpadDown, 2, 0x01),
    (Button::DpadUp, 2, 0x02),
    (Button::DpadRight, 2, 0x04),
    (Button::DpadLeft, 2, 0x08),
    (Button::L, 2, 0x40),
    (Button::ZL, 2, 0x80),
];

/// Encode button state into 3 BT report bytes.
fn encode_bt_buttons(buttons: &ButtonState) -> [u8; 3] {
    let mut bt = [0u8; 3];
    for &(btn, byte_idx, mask) in &BT_BUTTON_MAP {
        if buttons.get(btn) {
            bt[byte_idx] |= mask;
        }
    }
    bt
}

/// Pack a calibrated stick (x, y in ~[-100, 100]) into 3 bytes of 12-bit packed format.
fn pack_stick_12bit(out: &mut [u8], cal: (f64, f64)) {
    let x = ((cal.0 * 2048.0 / 100.0) + 2048.0).clamp(0.0, 4095.0) as u16;
    let y = ((cal.1 * 2048.0 / 100.0) + 2048.0).clamp(0.0, 4095.0) as u16;
    out[0] = (x & 0xFF) as u8;
    out[1] = ((x >> 8) & 0x0F) as u8 | (((y & 0x0F) as u8) << 4);
    out[2] = ((y >> 4) & 0xFF) as u8;
}

/// Build BT 0x30 report bytes from InputState + calibrated sticks.
///
/// NXBT-compatible layout (50 bytes):
///   [0]  = 0xA1 (HID transaction header)
///   [1]  = 0x30 (standard full input report ID)
///   [2]  = timer
///   [3]  = battery/connection info (0x90)
///   [4]  = button byte 0 (right): Y=01 X=02 B=04 A=08 R=40 ZR=80
///   [5]  = button byte 1 (shared): MINUS=01 PLUS=02 RSTICK=04 LSTICK=08 HOME=10 CAP=20
///   [6]  = button byte 2 (left): DD=01 DU=02 DR=04 DL=08 L=40 ZL=80
///   [7..9]   = left stick (12-bit packed, center = 0x800)
///   [10..12] = right stick
///   [13] = vibrator byte
///
/// Stick encoding: 12-bit packed, center = 0x800 (2048), range 0-4095.
pub fn build_bt_report(
    input: &InputState,
    left_cal: (f64, f64),
    right_cal: (f64, f64),
    timer: u8,
) -> [u8; 50] {
    let mut report = [0u8; 50];

    report[0] = 0xA1; // HID transaction header
    report[1] = 0x30; // Standard full input report
    report[2] = timer;
    report[3] = 0x90; // Battery level (full) + connection info

    // --- BT button encoding ---
    // Each entry: (Button, bt_byte_offset, bt_mask)
    let [bt0, bt1, bt2] = encode_bt_buttons(&input.buttons);
    report[4] = bt0;
    report[5] = bt1;
    report[6] = bt2;

    // --- Stick encoding ---
    // Calibrated values are in range ~[-100, 100], map to 12-bit [0, 4095] with center 2048
    pack_stick_12bit(&mut report[7..10], left_cal);
    pack_stick_12bit(&mut report[10..13], right_cal);

    // Vibrator byte
    report[13] = 0xB0;

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_bt_report_header() {
        let input = InputState::default();
        let report = build_bt_report(&input, (0.0, 0.0), (0.0, 0.0), 42);
        assert_eq!(report[0], 0xA1);
        assert_eq!(report[1], 0x30);
        assert_eq!(report[2], 42); // timer
        assert_eq!(report[3], 0x90); // battery
        assert_eq!(report[13], 0xB0); // vibrator
    }

    #[test]
    fn test_build_bt_report_buttons() {
        let mut input = InputState::default();
        input.buttons.set(Button::A, true);
        input.buttons.set(Button::B, true);
        input.buttons.set(Button::Y, true);
        input.buttons.set(Button::Plus, true);
        input.buttons.set(Button::L3, true);
        input.buttons.set(Button::DpadDown, true);
        input.buttons.set(Button::ZL, true);

        let report = build_bt_report(&input, (0.0, 0.0), (0.0, 0.0), 0);

        // Byte 4: Y=0x01, B=0x04, A=0x08
        assert_eq!(report[4] & 0x01, 0x01); // Y
        assert_eq!(report[4] & 0x04, 0x04); // B
        assert_eq!(report[4] & 0x08, 0x08); // A

        // Byte 5: PLUS=0x02, LSTICK=0x08
        assert_eq!(report[5] & 0x02, 0x02); // Plus
        assert_eq!(report[5] & 0x08, 0x08); // L3

        // Byte 6: DD=0x01, ZL=0x80
        assert_eq!(report[6] & 0x01, 0x01); // DpadDown
        assert_eq!(report[6] & 0x80, 0x80); // ZL
    }

    #[test]
    fn test_build_bt_report_sticks_center() {
        let input = InputState::default();
        let report = build_bt_report(&input, (0.0, 0.0), (0.0, 0.0), 0);

        // Center = 2048 = 0x800
        // Byte 7: lx & 0xFF = 0x00
        // Byte 8: (lx >> 8) & 0x0F = 0x8, (ly & 0x0F) << 4 = 0x00 → 0x80
        // Byte 9: (ly >> 4) & 0xFF = 0x80
        assert_eq!(report[7], 0x00);
        assert_eq!(report[8], 0x08);
        assert_eq!(report[9], 0x80);
    }

    #[test]
    fn test_build_bt_report_sticks_full_tilt() {
        let input = InputState::default();
        // Full right: x=100 → lx = (100 * 2048/100 + 2048) = 4096 → clamped to 4095
        let report = build_bt_report(&input, (100.0, 100.0), (-100.0, -100.0), 0);

        // Left stick full positive: 4095 = 0xFFF
        let lx = report[7] as u16 | (((report[8] & 0x0F) as u16) << 8);
        let ly = ((report[8] >> 4) as u16) | ((report[9] as u16) << 4);
        assert_eq!(lx, 4095);
        assert_eq!(ly, 4095);

        // Right stick full negative: 0 = 0x000
        let rx = report[10] as u16 | (((report[11] & 0x0F) as u16) << 8);
        let ry = ((report[11] >> 4) as u16) | ((report[12] as u16) << 4);
        assert_eq!(rx, 0);
        assert_eq!(ry, 0);
    }
}
