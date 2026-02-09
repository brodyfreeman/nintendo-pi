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
        self.buttons[..self.count]
            .iter()
            .any(|b| *b == Some(btn))
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
        let base_held = buttons.l3 && buttons.r3;
        let mut action = ComboAction::None;
        let mut suppressed = SuppressedButtons::default();

        if base_held {
            // Always suppress L3+R3 when both held
            suppressed.add(Button::L3);
            suppressed.add(Button::R3);

            // Check D-pad Down hold for macro mode toggle
            let dpad_down = buttons.dpad_down;
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
                let any_combo_btn = dpad_down
                    || INSTANT_COMBOS
                        .iter()
                        .any(|&(btn, _)| buttons.get(btn));
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
