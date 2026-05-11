//! USB HID report parsing.
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

/// Controller input in the macro runtime's device-independent format.
///
/// Stick values are normalized percentages in the range `[-100.0, 100.0]`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogicalInput {
    pub buttons: ButtonState,
    pub left_stick: (f64, f64),
    pub right_stick: (f64, f64),
}

impl LogicalInput {
    pub fn neutral() -> Self {
        Self::default()
    }

    pub fn from_parts(
        buttons: ButtonState,
        left_stick: (f64, f64),
        right_stick: (f64, f64),
    ) -> Self {
        Self {
            buttons,
            left_stick,
            right_stick,
        }
    }
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
fn unpack_12bit_triplet(packed_bytes: &[u8]) -> (u16, u16) {
    let a = (packed_bytes[0] as u16) | (((packed_bytes[1] & 0x0F) as u16) << 8);
    let b = ((packed_bytes[1] >> 4) as u16) | ((packed_bytes[2] as u16) << 4);
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

/// Button name enum for parsed controller input.
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
    pub const ALL: [Button; 18] = [
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
}

impl Button {
    /// (byte_index_in_button_field, bitmask) in the USB HID button field.
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

    pub fn to_mask(&self) -> u32 {
        Button::ALL
            .iter()
            .enumerate()
            .fold(0u32, |mask, (idx, &button)| {
                if self.get(button) {
                    mask | (1 << idx)
                } else {
                    mask
                }
            })
    }

    pub fn from_mask(mask: u32) -> Self {
        let mut state = Self::default();
        for (idx, &button) in Button::ALL.iter().enumerate() {
            state.set(button, mask & (1 << idx) != 0);
        }
        state
    }

    pub fn get(&self, btn: Button) -> bool {
        let (byte_idx, mask) = btn.position();
        self.bytes[byte_idx] & mask != 0
    }

    pub fn set(&mut self, btn: Button, pressed: bool) {
        let (byte_idx, mask) = btn.position();
        if pressed {
            self.bytes[byte_idx] |= mask;
        } else {
            self.bytes[byte_idx] &= !mask;
        }
    }
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
