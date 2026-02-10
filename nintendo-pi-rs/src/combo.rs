//! Combo detection state machine.
//!
//! Direct port of combo.py. Detects L3+R3+button combos and reports
//! which buttons to suppress from forwarding to the Switch.

use std::time::Instant;

use crate::input::{Button, ButtonState};

/// Action triggered by a combo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComboAction {
    None,
    ToggleMacroMode,
    ToggleRecording,
    PrevSlot,
    NextSlot,
    PlayMacro,
    StopPlayback,
}

/// Hold duration for macro mode toggle (seconds).
const HOLD_DURATION: f64 = 0.5;

/// Instant combos: button -> action (edge-triggered when L3+R3 held).
const INSTANT_COMBOS: &[(Button, ComboAction)] = &[
    (Button::DpadLeft, ComboAction::PrevSlot),
    (Button::DpadRight, ComboAction::NextSlot),
    (Button::A, ComboAction::PlayMacro),
    (Button::B, ComboAction::StopPlayback),
];

/// Set of buttons to suppress (smallvec would be overkill, just use a fixed array).
#[derive(Debug, Clone, Default)]
pub struct SuppressedButtons {
    buttons: [Option<Button>; 8],
    count: usize,
}

impl SuppressedButtons {
    pub fn add(&mut self, btn: Button) {
        if self.count < self.buttons.len() {
            self.buttons[self.count] = Some(btn);
            self.count += 1;
        }
    }

    pub fn contains(&self, btn: Button) -> bool {
        self.buttons[..self.count].iter().any(|b| *b == Some(btn))
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Filter button state: set suppressed buttons to false.
    pub fn filter_buttons(&self, buttons: &mut ButtonState) {
        for b in &self.buttons[..self.count] {
            if let Some(btn) = b {
                buttons.set(*btn, false);
            }
        }
    }

    /// Filter raw HID report: zero out suppressed button bits.
    /// Button bytes are at report[3..6] (payload offset 0x2).
    pub fn filter_raw_report(&self, report: &mut [u8; 64]) {
        const BTN_BASE: usize = 3;
        for b in &self.buttons[..self.count] {
            if let Some(btn) = b {
                let (byte_idx, mask) = btn.position();
                report[BTN_BASE + byte_idx] &= !mask;
            }
        }
    }
}

/// Combo detector state machine.
pub struct ComboDetector {
    pub macro_mode: bool,
    dpad_down_start: Option<Instant>,
    prev_buttons: ButtonState,
    prev_base_held: bool,
}

impl ComboDetector {
    pub fn new() -> Self {
        Self {
            macro_mode: false,
            dpad_down_start: None,
            prev_buttons: ButtonState::default(),
            prev_base_held: false,
        }
    }

    /// Process button state. Returns (action, suppressed_buttons).
    pub fn update(&mut self, buttons: &ButtonState) -> (ComboAction, SuppressedButtons) {
        let base_held = buttons.get(Button::L3) && buttons.get(Button::R3);
        let mut action = ComboAction::None;
        let mut suppressed = SuppressedButtons::default();

        if base_held {
            // Always suppress L3+R3 when both held
            suppressed.add(Button::L3);
            suppressed.add(Button::R3);

            // Check D-pad Down hold for macro mode toggle
            let dpad_down = buttons.get(Button::DpadDown);
            if dpad_down {
                suppressed.add(Button::DpadDown);
                match self.dpad_down_start {
                    None => {
                        self.dpad_down_start = Some(Instant::now());
                    }
                    Some(start) => {
                        if start.elapsed().as_secs_f64() >= HOLD_DURATION {
                            action = ComboAction::ToggleMacroMode;
                            self.dpad_down_start = None;
                        }
                    }
                }
            } else {
                self.dpad_down_start = None;
            }

            // Check instant combos (edge-triggered)
            for &(btn, combo_action) in INSTANT_COMBOS {
                let pressed = buttons.get(btn);
                let was_pressed = self.prev_buttons.get(btn);
                if pressed {
                    suppressed.add(btn);
                }
                if pressed && !was_pressed {
                    action = combo_action;
                }
            }

            // In macro mode, L3+R3 alone toggles recording (rising edge)
            if self.macro_mode && !self.prev_base_held {
                let any_combo_btn =
                    dpad_down || INSTANT_COMBOS.iter().any(|&(btn, _)| buttons.get(btn));
                if !any_combo_btn {
                    action = ComboAction::ToggleRecording;
                }
            }
        } else {
            self.dpad_down_start = None;
        }

        self.prev_buttons = buttons.clone();
        self.prev_base_held = base_held;

        (action, suppressed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buttons_with(set: &[Button]) -> ButtonState {
        let mut bs = ButtonState::default();
        for &btn in set {
            bs.set(btn, true);
        }
        bs
    }

    #[test]
    fn test_no_combo_without_l3r3() {
        let mut cd = ComboDetector::new();

        // Pressing A alone does nothing
        let (action, sup) = cd.update(&buttons_with(&[Button::A]));
        assert_eq!(action, ComboAction::None);
        assert!(sup.is_empty());

        // DpadDown alone does nothing
        let (action, sup) = cd.update(&buttons_with(&[Button::DpadDown]));
        assert_eq!(action, ComboAction::None);
        assert!(sup.is_empty());
    }

    #[test]
    fn test_l3r3_suppressed() {
        let mut cd = ComboDetector::new();
        let (_, sup) = cd.update(&buttons_with(&[Button::L3, Button::R3]));
        assert!(!sup.is_empty());
        assert!(sup.buttons[..sup.count]
            .iter()
            .any(|b| *b == Some(Button::L3)));
        assert!(sup.buttons[..sup.count]
            .iter()
            .any(|b| *b == Some(Button::R3)));
    }

    #[test]
    fn test_instant_combo_play_macro() {
        let mut cd = ComboDetector::new();

        // First frame: L3+R3 (rising edge, but no combo button)
        cd.update(&buttons_with(&[Button::L3, Button::R3]));

        // Second frame: L3+R3+A (A rising edge → PlayMacro)
        let (action, sup) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::A]));
        assert_eq!(action, ComboAction::PlayMacro);
        assert!(sup.buttons[..sup.count]
            .iter()
            .any(|b| *b == Some(Button::A)));
    }

    #[test]
    fn test_instant_combo_stop_playback() {
        let mut cd = ComboDetector::new();
        cd.update(&buttons_with(&[Button::L3, Button::R3]));

        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::B]));
        assert_eq!(action, ComboAction::StopPlayback);
    }

    #[test]
    fn test_instant_combo_prev_next_slot() {
        let mut cd = ComboDetector::new();
        cd.update(&buttons_with(&[Button::L3, Button::R3]));

        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::DpadLeft]));
        assert_eq!(action, ComboAction::PrevSlot);

        // Release DpadLeft
        cd.update(&buttons_with(&[Button::L3, Button::R3]));

        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::DpadRight]));
        assert_eq!(action, ComboAction::NextSlot);
    }

    #[test]
    fn test_combo_not_retriggered_on_hold() {
        let mut cd = ComboDetector::new();
        cd.update(&buttons_with(&[Button::L3, Button::R3]));

        // First press: triggers
        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::A]));
        assert_eq!(action, ComboAction::PlayMacro);

        // Held: doesn't retrigger
        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::A]));
        assert_eq!(action, ComboAction::None);
    }

    #[test]
    fn test_toggle_recording_in_macro_mode() {
        let mut cd = ComboDetector::new();
        cd.macro_mode = true;

        // L3+R3 rising edge in macro mode → ToggleRecording
        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3]));
        assert_eq!(action, ComboAction::ToggleRecording);
    }

    #[test]
    fn test_no_recording_without_macro_mode() {
        let mut cd = ComboDetector::new();
        assert!(!cd.macro_mode);

        // L3+R3 rising edge without macro mode → no recording
        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3]));
        assert_eq!(action, ComboAction::None);
    }

    #[test]
    fn test_dpad_down_hold_toggle() {
        let mut cd = ComboDetector::new();

        // Hold L3+R3+DpadDown for > 0.5s
        cd.update(&buttons_with(&[Button::L3, Button::R3, Button::DpadDown]));

        // Sleep just over the hold duration
        std::thread::sleep(std::time::Duration::from_millis(550));

        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::DpadDown]));
        assert_eq!(action, ComboAction::ToggleMacroMode);
    }

    #[test]
    fn test_dpad_down_short_press_no_toggle() {
        let mut cd = ComboDetector::new();

        // Press briefly
        cd.update(&buttons_with(&[Button::L3, Button::R3, Button::DpadDown]));
        std::thread::sleep(std::time::Duration::from_millis(100));
        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::DpadDown]));
        assert_eq!(action, ComboAction::None);
    }

    #[test]
    fn test_suppressed_filter_buttons() {
        let mut sup = SuppressedButtons::default();
        sup.add(Button::L3);
        sup.add(Button::R3);
        sup.add(Button::A);

        let mut bs = buttons_with(&[Button::L3, Button::R3, Button::A, Button::B]);
        sup.filter_buttons(&mut bs);

        assert!(!bs.get(Button::L3));
        assert!(!bs.get(Button::R3));
        assert!(!bs.get(Button::A));
        assert!(bs.get(Button::B)); // not suppressed
    }

    #[test]
    fn test_suppressed_filter_raw_report() {
        let mut sup = SuppressedButtons::default();
        sup.add(Button::B); // byte0, 0x01
        sup.add(Button::L3); // byte1, 0x80
        sup.add(Button::Home); // byte2, 0x01

        let mut report = [0u8; 64];
        report[3] = 0xFF; // all byte0 buttons
        report[4] = 0xFF; // all byte1 buttons
        report[5] = 0xFF; // all byte2 buttons

        sup.filter_raw_report(&mut report);

        assert_eq!(report[3], 0xFE); // B (0x01) cleared
        assert_eq!(report[4], 0x7F); // L3 (0x80) cleared
        assert_eq!(report[5], 0xFE); // Home (0x01) cleared
    }

    #[test]
    fn test_recording_not_triggered_with_combo_button() {
        let mut cd = ComboDetector::new();
        cd.macro_mode = true;

        // L3+R3+A: should NOT trigger recording (A takes priority)
        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::A]));
        assert_eq!(action, ComboAction::PlayMacro);
    }
}
