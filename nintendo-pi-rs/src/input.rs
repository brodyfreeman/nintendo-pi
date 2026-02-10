//! HID report parsing and USB-to-BT button remapping.
//!
//! USB HID report format (64 bytes):
//!   [0]     = Report ID (0x09)
//!   [1]     = Counter
//!   [2]     = Mode byte (0x23 after init)
//!   [3..6]  = Button bitfields (3 bytes)
//!   [6..9]  = Left stick (12-bit packed X/Y)
//!   [9..12] = Right stick (12-bit packed X/Y)
//!   [12]    = Unknown
//!   [13]    = Left trigger
//!   [14]    = Right trigger

/// Parsed input state from a USB HID report.
#[derive(Clone, Debug, Default)]
pub struct InputState {
    pub buttons: ButtonState,
    /// Left stick raw 12-bit values.
    pub left_stick_raw: (u16, u16),
    /// Right stick raw 12-bit values.
    pub right_stick_raw: (u16, u16),
}

/// All button states packed as 3 bytes (USB HID bit layout).
///
/// Use [`Button`] with `get()`/`set()` to access individual buttons.
/// Byte layout matches USB HID report button bytes directly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ButtonState {
    bytes: [u8; 3],
}

/// Unpack two 12-bit values from 3 bytes (little-endian nibble packing).
/// Byte layout: [lo8_a] [hi4_a | lo4_b] [hi8_b]
fn unpack_12bit_triplet(data: &[u8]) -> (u16, u16) {
    let a = (data[0] as u16) | (((data[1] & 0x0F) as u16) << 8);
    let b = ((data[1] >> 4) as u16) | ((data[2] as u16) << 4);
    (a, b)
}

/// Parse a 64-byte USB HID report into an InputState.
pub fn parse_hid_report(report: &[u8; 64]) -> InputState {
    // payload starts at report[1]
    let buttons_bytes = &report[3..6]; // payload[0x2..0x5]
    let stick1 = &report[6..9]; // payload[0x5..0x8]
    let stick2 = &report[9..12]; // payload[0x8..0xB]

    let buttons = ButtonState::from_bytes([buttons_bytes[0], buttons_bytes[1], buttons_bytes[2]]);

    let (lx, ly) = unpack_12bit_triplet(stick1);
    let (rx, ry) = unpack_12bit_triplet(stick2);

    InputState {
        buttons,
        left_stick_raw: (lx, ly),
        right_stick_raw: (rx, ry),
    }
}

/// Button name enum for combo detection (matches Python button names).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    B,
    A,
    Y,
    X,
    R,
    ZR,
    Plus,
    R3,
    DpadDown,
    DpadRight,
    DpadLeft,
    DpadUp,
    L,
    ZL,
    Minus,
    L3,
    Home,
    Capture,
}

impl Button {
    /// (byte_index_in_button_field, bitmask) for raw report filtering.
    pub fn position(self) -> (usize, u8) {
        match self {
            Button::B => (0, 0x01),
            Button::A => (0, 0x02),
            Button::Y => (0, 0x04),
            Button::X => (0, 0x08),
            Button::R => (0, 0x10),
            Button::ZR => (0, 0x20),
            Button::Plus => (0, 0x40),
            Button::R3 => (0, 0x80),
            Button::DpadDown => (1, 0x01),
            Button::DpadRight => (1, 0x02),
            Button::DpadLeft => (1, 0x04),
            Button::DpadUp => (1, 0x08),
            Button::L => (1, 0x10),
            Button::ZL => (1, 0x20),
            Button::Minus => (1, 0x40),
            Button::L3 => (1, 0x80),
            Button::Home => (2, 0x01),
            Button::Capture => (2, 0x02),
        }
    }
}

impl ButtonState {
    pub fn from_bytes(bytes: [u8; 3]) -> Self {
        Self { bytes }
    }

    pub fn get(&self, btn: Button) -> bool {
        let (byte_idx, mask) = btn.position();
        self.bytes[byte_idx] & mask != 0
    }

    pub fn set(&mut self, btn: Button, val: bool) {
        let (byte_idx, mask) = btn.position();
        if val {
            self.bytes[byte_idx] |= mask;
        } else {
            self.bytes[byte_idx] &= !mask;
        }
    }
}

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

    /// Build a minimal 64-byte report with specified button bytes and stick data.
    fn make_report(btn: [u8; 3], stick1: [u8; 3], stick2: [u8; 3]) -> [u8; 64] {
        let mut r = [0u8; 64];
        r[3] = btn[0];
        r[4] = btn[1];
        r[5] = btn[2];
        r[6] = stick1[0];
        r[7] = stick1[1];
        r[8] = stick1[2];
        r[9] = stick2[0];
        r[10] = stick2[1];
        r[11] = stick2[2];
        r
    }

    #[test]
    fn test_parse_no_buttons() {
        let report = make_report([0, 0, 0], [0, 0, 0], [0, 0, 0]);
        let state = parse_hid_report(&report);
        assert_eq!(state.buttons, ButtonState::default());
    }

    #[test]
    fn test_parse_individual_buttons() {
        // B = byte0 bit0
        let r = make_report([0x01, 0, 0], [0; 3], [0; 3]);
        assert!(parse_hid_report(&r).buttons.get(Button::B));

        // A = byte0 bit1
        let r = make_report([0x02, 0, 0], [0; 3], [0; 3]);
        assert!(parse_hid_report(&r).buttons.get(Button::A));

        // R3 = byte0 bit7
        let r = make_report([0x80, 0, 0], [0; 3], [0; 3]);
        assert!(parse_hid_report(&r).buttons.get(Button::R3));

        // DpadDown = byte1 bit0
        let r = make_report([0, 0x01, 0], [0; 3], [0; 3]);
        assert!(parse_hid_report(&r).buttons.get(Button::DpadDown));

        // L3 = byte1 bit7
        let r = make_report([0, 0x80, 0], [0; 3], [0; 3]);
        assert!(parse_hid_report(&r).buttons.get(Button::L3));

        // Home = byte2 bit0
        let r = make_report([0, 0, 0x01], [0; 3], [0; 3]);
        assert!(parse_hid_report(&r).buttons.get(Button::Home));

        // Capture = byte2 bit1
        let r = make_report([0, 0, 0x02], [0; 3], [0; 3]);
        assert!(parse_hid_report(&r).buttons.get(Button::Capture));
    }

    #[test]
    fn test_parse_multiple_buttons() {
        // A + B + L3 + R3
        let r = make_report([0x03 | 0x80, 0x80, 0], [0; 3], [0; 3]);
        let s = parse_hid_report(&r);
        assert!(s.buttons.get(Button::A));
        assert!(s.buttons.get(Button::B));
        assert!(s.buttons.get(Button::R3));
        assert!(s.buttons.get(Button::L3));
        assert!(!s.buttons.get(Button::X));
    }

    #[test]
    fn test_unpack_12bit_sticks() {
        // Pack known values: X=2048 (0x800), Y=2048 (0x800)
        // Unpacking: a = data[0] | (data[1] & 0x0F) << 8
        //            b = (data[1] >> 4) | data[2] << 4
        // X=0x800: data[0]=0x00, data[1] low nibble = 0x8
        // Y=0x800: data[1] high nibble = 0x0, data[2] = 0x80
        // data[1] = 0x08 (low=8, high=0)
        let stick = [0x00, 0x08, 0x80];
        let r = make_report([0; 3], stick, [0; 3]);
        let s = parse_hid_report(&r);
        assert_eq!(s.left_stick_raw, (0x800, 0x800));
    }

    #[test]
    fn test_unpack_12bit_extremes() {
        // X=0, Y=0
        let r = make_report([0; 3], [0, 0, 0], [0; 3]);
        assert_eq!(parse_hid_report(&r).left_stick_raw, (0, 0));

        // X=0xFFF (4095), Y=0xFFF
        let r = make_report([0; 3], [0xFF, 0xFF, 0xFF], [0; 3]);
        assert_eq!(parse_hid_report(&r).left_stick_raw, (0xFFF, 0xFFF));
    }

    #[test]
    fn test_button_position_matches_parse() {
        // For each button, set only its bit, parse, and verify get() returns true
        let all_buttons = [
            Button::B,
            Button::A,
            Button::Y,
            Button::X,
            Button::R,
            Button::ZR,
            Button::Plus,
            Button::R3,
            Button::DpadDown,
            Button::DpadRight,
            Button::DpadLeft,
            Button::DpadUp,
            Button::L,
            Button::ZL,
            Button::Minus,
            Button::L3,
            Button::Home,
            Button::Capture,
        ];

        for btn in all_buttons {
            let (byte_idx, mask) = btn.position();
            let mut btn_bytes = [0u8; 3];
            btn_bytes[byte_idx] = mask;

            let r = make_report(btn_bytes, [0; 3], [0; 3]);
            let state = parse_hid_report(&r);
            assert!(
                state.buttons.get(btn),
                "{btn:?}: position ({byte_idx}, {mask:#04x}) didn't parse correctly"
            );

            // Also verify no other buttons are set
            for other in all_buttons {
                if other != btn {
                    assert!(
                        !state.buttons.get(other),
                        "Setting {btn:?} also set {other:?}"
                    );
                }
            }
        }
    }

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

    #[test]
    fn test_button_set_get_roundtrip() {
        let mut bs = ButtonState::default();
        let all = [
            Button::B,
            Button::A,
            Button::Y,
            Button::X,
            Button::R,
            Button::ZR,
            Button::Plus,
            Button::R3,
            Button::DpadDown,
            Button::DpadRight,
            Button::DpadLeft,
            Button::DpadUp,
            Button::L,
            Button::ZL,
            Button::Minus,
            Button::L3,
            Button::Home,
            Button::Capture,
        ];

        for btn in all {
            assert!(!bs.get(btn));
            bs.set(btn, true);
            assert!(bs.get(btn));
            bs.set(btn, false);
            assert!(!bs.get(btn));
        }
    }
}
