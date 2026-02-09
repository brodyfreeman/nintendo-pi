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
    /// Raw 3 button bytes from the USB report (for combo filtering).
    pub buttons_raw: [u8; 3],
    /// Left stick raw 12-bit values.
    pub left_stick_raw: (u16, u16),
    /// Right stick raw 12-bit values.
    pub right_stick_raw: (u16, u16),
    /// Left trigger (0-255 after remap).
    pub left_trigger: u8,
    /// Right trigger (0-255 after remap).
    pub right_trigger: u8,
}

/// All button states as booleans.
#[derive(Clone, Debug, Default)]
pub struct ButtonState {
    pub b: bool,
    pub a: bool,
    pub y: bool,
    pub x: bool,
    pub r: bool,
    pub zr: bool,
    pub plus: bool,
    pub r3: bool,
    pub dpad_down: bool,
    pub dpad_right: bool,
    pub dpad_left: bool,
    pub dpad_up: bool,
    pub l: bool,
    pub zl: bool,
    pub minus: bool,
    pub l3: bool,
    pub home: bool,
    pub capture: bool,
}

/// Unpack two 12-bit values from 3 bytes (little-endian nibble packing).
/// Byte layout: [lo8_a] [hi4_a | lo4_b] [hi8_b]
fn unpack_12bit_triplet(data: &[u8]) -> (u16, u16) {
    let a = (data[0] as u16) | (((data[1] & 0x0F) as u16) << 8);
    let b = ((data[1] >> 4) as u16) | ((data[2] as u16) << 4);
    (a, b)
}

/// Remap trigger value from raw range [36..240] to [0..255].
fn remap_trigger_value(value: u8) -> u8 {
    const MIN_IN: u16 = 36;
    const MAX_IN: u16 = 240;
    let clamped = (value as u16).clamp(MIN_IN, MAX_IN);
    let percentage = (clamped - MIN_IN) as f32 / (MAX_IN - MIN_IN) as f32;
    (percentage * 255.0) as u8
}

/// Parse a 64-byte USB HID report into an InputState.
pub fn parse_hid_report(report: &[u8; 64]) -> InputState {
    // payload starts at report[1]
    let buttons_bytes = &report[3..6]; // payload[0x2..0x5]
    let stick1 = &report[6..9]; // payload[0x5..0x8]
    let stick2 = &report[9..12]; // payload[0x8..0xB]
    let left_trigger_raw = report[13]; // payload[0x0C]
    let right_trigger_raw = report[14]; // payload[0x0D]

    let buttons = ButtonState {
        b: buttons_bytes[0] & 0x01 != 0,
        a: buttons_bytes[0] & 0x02 != 0,
        y: buttons_bytes[0] & 0x04 != 0,
        x: buttons_bytes[0] & 0x08 != 0,
        r: buttons_bytes[0] & 0x10 != 0,
        zr: buttons_bytes[0] & 0x20 != 0,
        plus: buttons_bytes[0] & 0x40 != 0,
        r3: buttons_bytes[0] & 0x80 != 0,
        dpad_down: buttons_bytes[1] & 0x01 != 0,
        dpad_right: buttons_bytes[1] & 0x02 != 0,
        dpad_left: buttons_bytes[1] & 0x04 != 0,
        dpad_up: buttons_bytes[1] & 0x08 != 0,
        l: buttons_bytes[1] & 0x10 != 0,
        zl: buttons_bytes[1] & 0x20 != 0,
        minus: buttons_bytes[1] & 0x40 != 0,
        l3: buttons_bytes[1] & 0x80 != 0,
        home: buttons_bytes[2] & 0x01 != 0,
        capture: buttons_bytes[2] & 0x02 != 0,
    };

    let (lx, ly) = unpack_12bit_triplet(stick1);
    let (rx, ry) = unpack_12bit_triplet(stick2);

    InputState {
        buttons,
        buttons_raw: [buttons_bytes[0], buttons_bytes[1], buttons_bytes[2]],
        left_stick_raw: (lx, ly),
        right_stick_raw: (rx, ry),
        left_trigger: remap_trigger_value(left_trigger_raw),
        right_trigger: remap_trigger_value(right_trigger_raw),
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
    pub fn get(&self, btn: Button) -> bool {
        match btn {
            Button::B => self.b,
            Button::A => self.a,
            Button::Y => self.y,
            Button::X => self.x,
            Button::R => self.r,
            Button::ZR => self.zr,
            Button::Plus => self.plus,
            Button::R3 => self.r3,
            Button::DpadDown => self.dpad_down,
            Button::DpadRight => self.dpad_right,
            Button::DpadLeft => self.dpad_left,
            Button::DpadUp => self.dpad_up,
            Button::L => self.l,
            Button::ZL => self.zl,
            Button::Minus => self.minus,
            Button::L3 => self.l3,
            Button::Home => self.home,
            Button::Capture => self.capture,
        }
    }

    pub fn set(&mut self, btn: Button, val: bool) {
        match btn {
            Button::B => self.b = val,
            Button::A => self.a = val,
            Button::Y => self.y = val,
            Button::X => self.x = val,
            Button::R => self.r = val,
            Button::ZR => self.zr = val,
            Button::Plus => self.plus = val,
            Button::R3 => self.r3 = val,
            Button::DpadDown => self.dpad_down = val,
            Button::DpadRight => self.dpad_right = val,
            Button::DpadLeft => self.dpad_left = val,
            Button::DpadUp => self.dpad_up = val,
            Button::L => self.l = val,
            Button::ZL => self.zl = val,
            Button::Minus => self.minus = val,
            Button::L3 => self.l3 = val,
            Button::Home => self.home = val,
            Button::Capture => self.capture = val,
        }
    }
}

/// Build BT 0x30 report bytes from InputState + calibrated sticks.
///
/// BT button byte layout is different from USB:
///   BT byte 0 (right): Y=01 X=02 B=04 A=08 R=40 ZR=80
///   BT byte 1 (shared): MINUS=01 PLUS=02 RSTICK=04 LSTICK=08 HOME=10 CAP=20
///   BT byte 2 (left):   DD=01 DU=02 DR=04 DL=08 L=40 ZL=80
///
/// Stick encoding: 12-bit packed, center = 0x800 (2048), range 0-4095.
pub fn build_bt_report(
    input: &InputState,
    left_cal: (f64, f64),
    right_cal: (f64, f64),
    timer: u8,
) -> [u8; 50] {
    let mut report = [0u8; 50];

    report[0] = 0xA1; // HID report header
    report[1] = 0x30; // Standard full report
    report[2] = timer;
    report[3] = 0x8E; // Battery level + connection info

    // --- BT button encoding ---
    let b = &input.buttons;

    // Byte 4: right-side buttons
    let mut bt0: u8 = 0;
    if b.y { bt0 |= 0x01; }
    if b.x { bt0 |= 0x02; }
    if b.b { bt0 |= 0x04; }
    if b.a { bt0 |= 0x08; }
    if b.r { bt0 |= 0x40; }
    if b.zr { bt0 |= 0x80; }
    report[4] = bt0;

    // Byte 5: shared buttons
    let mut bt1: u8 = 0;
    if b.minus { bt1 |= 0x01; }
    if b.plus { bt1 |= 0x02; }
    if b.r3 { bt1 |= 0x04; }
    if b.l3 { bt1 |= 0x08; }
    if b.home { bt1 |= 0x10; }
    if b.capture { bt1 |= 0x20; }
    report[5] = bt1;

    // Byte 6: left-side buttons
    let mut bt2: u8 = 0;
    if b.dpad_down { bt2 |= 0x01; }
    if b.dpad_up { bt2 |= 0x02; }
    if b.dpad_right { bt2 |= 0x04; }
    if b.dpad_left { bt2 |= 0x08; }
    if b.l { bt2 |= 0x40; }
    if b.zl { bt2 |= 0x80; }
    report[6] = bt2;

    // --- Stick encoding ---
    // Calibrated values are in range ~[-100, 100], map to 12-bit [0, 4095] with center 2048
    let lx = ((left_cal.0 * 2048.0 / 100.0) + 2048.0).clamp(0.0, 4095.0) as u16;
    let ly = ((left_cal.1 * 2048.0 / 100.0) + 2048.0).clamp(0.0, 4095.0) as u16;

    // Pack left stick: bytes 7-9
    report[7] = (lx & 0xFF) as u8;
    report[8] = ((lx >> 8) & 0x0F) as u8 | (((ly & 0x0F) as u8) << 4);
    report[9] = ((ly >> 4) & 0xFF) as u8;

    let rx = ((right_cal.0 * 2048.0 / 100.0) + 2048.0).clamp(0.0, 4095.0) as u16;
    let ry = ((right_cal.1 * 2048.0 / 100.0) + 2048.0).clamp(0.0, 4095.0) as u16;

    // Pack right stick: bytes 10-12
    report[10] = (rx & 0xFF) as u8;
    report[11] = ((rx >> 8) & 0x0F) as u8 | (((ry & 0x0F) as u8) << 4);
    report[12] = ((ry >> 4) & 0xFF) as u8;

    // report[13] = vibrator byte (0x00)
    // report[14..50] = IMU data (zeroed)

    report
}
