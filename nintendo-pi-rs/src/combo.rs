//! Combo detection state machine.
//!
//! Direct port of combo.py. Detects L3+R3+button combos and reports
//! which buttons to suppress from forwarding to the Switch.

use std::time::Instant;

use tracing::{debug, info};

use crate::config::Config;
use crate::input::{Button, ButtonState};

/// Whether the toggle-macro-mode combo is hold-triggered or edge-triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerMode {
    /// Must hold the button for `hold_duration` seconds (default).
    Hold,
    /// Instant press (rising edge), like all other combos.
    Edge,
}

impl TriggerMode {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerMode::Hold => "hold",
            TriggerMode::Edge => "edge",
        }
    }

    pub fn from_str_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "hold" => Some(TriggerMode::Hold),
            "edge" => Some(TriggerMode::Edge),
            _ => None,
        }
    }
}

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
    CycleSpeed,
    ToggleLoop,
}

impl std::fmt::Display for ComboAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComboAction::None => write!(f, "None"),
            ComboAction::ToggleMacroMode => write!(f, "ToggleMacroMode"),
            ComboAction::ToggleRecording => write!(f, "ToggleRecording"),
            ComboAction::PrevSlot => write!(f, "PrevSlot"),
            ComboAction::NextSlot => write!(f, "NextSlot"),
            ComboAction::PlayMacro => write!(f, "PlayMacro"),
            ComboAction::StopPlayback => write!(f, "StopPlayback"),
            ComboAction::CycleSpeed => write!(f, "CycleSpeed"),
            ComboAction::ToggleLoop => write!(f, "ToggleLoop"),
        }
    }
}

impl From<ComboAction> for Option<crate::macro_engine::controller::MacroCommand> {
    fn from(action: ComboAction) -> Self {
        use crate::macro_engine::controller::MacroCommand;
        match action {
            ComboAction::None => None,
            ComboAction::ToggleMacroMode => Some(MacroCommand::ToggleMacroMode),
            ComboAction::ToggleRecording => Some(MacroCommand::ToggleRecording),
            ComboAction::PrevSlot => Some(MacroCommand::PrevSlot),
            ComboAction::NextSlot => Some(MacroCommand::NextSlot),
            ComboAction::PlayMacro => Some(MacroCommand::PlayMacro),
            ComboAction::StopPlayback => Some(MacroCommand::StopPlayback),
            ComboAction::CycleSpeed => Some(MacroCommand::CycleSpeed),
            ComboAction::ToggleLoop => Some(MacroCommand::ToggleLoop),
        }
    }
}

/// Default hold duration for macro mode toggle (seconds).
pub const DEFAULT_HOLD_DURATION: f64 = 0.5;

/// Default play macro button.
pub const DEFAULT_PLAY_MACRO_BUTTON: Button = Button::A;

/// Default stop playback button.
pub const DEFAULT_STOP_PLAYBACK_BUTTON: Button = Button::B;

/// Default toggle macro mode button (hold-triggered).
pub const DEFAULT_TOGGLE_MACRO_MODE_BUTTON: Button = Button::DpadDown;

/// Default toggle loop button.
pub const DEFAULT_TOGGLE_LOOP_BUTTON: Button = Button::Y;

/// Default cycle speed button.
pub const DEFAULT_CYCLE_SPEED_BUTTON: Button = Button::DpadUp;

/// Default prev slot button.
pub const DEFAULT_PREV_SLOT_BUTTON: Button = Button::DpadLeft;

/// Default next slot button.
pub const DEFAULT_NEXT_SLOT_BUTTON: Button = Button::DpadRight;

/// Default toggle recording button.
pub const DEFAULT_TOGGLE_RECORDING_BUTTON: Button = Button::Minus;

/// Default base combo button 1.
pub const DEFAULT_BASE_COMBO_BUTTON_1: Button = Button::L3;

/// Default base combo button 2.
pub const DEFAULT_BASE_COMBO_BUTTON_2: Button = Button::R3;

/// Default trigger mode for toggle macro mode combo.
pub const DEFAULT_TOGGLE_MACRO_MODE_TRIGGER: TriggerMode = TriggerMode::Hold;

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

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Filter button state: set suppressed buttons to false.
    pub fn filter_buttons(&self, buttons: &mut ButtonState) {
        for btn in self.buttons[..self.count].iter().flatten() {
            buttons.set(*btn, false);
        }
    }

    /// Filter raw HID report: zero out suppressed button bits.
    /// Button bytes are at report[3..6] (payload offset 0x2).
    pub fn filter_raw_report(&self, report: &mut [u8; 64]) {
        const BTN_BASE: usize = 3;
        for btn in self.buttons[..self.count].iter().flatten() {
            let (byte_idx, mask) = btn.position();
            report[BTN_BASE + byte_idx] &= !mask;
        }
    }
}

/// Combo detector state machine.
///
/// Button bindings and timing come from `Config`, passed to `update()`.
/// The detector only tracks its own internal state (macro mode, hold timer,
/// previous buttons).
pub struct ComboDetector {
    pub macro_mode: bool,
    hold_button_start: Option<Instant>,
    prev_buttons: ButtonState,
    prev_base_held: bool,
}

impl ComboDetector {
    pub fn new() -> Self {
        Self {
            macro_mode: false,
            hold_button_start: None,
            prev_buttons: ButtonState::default(),
            prev_base_held: false,
        }
    }

    /// Process button state. Returns (action, suppressed_buttons).
    ///
    /// Reads button bindings and timing from `config`.
    pub fn update(
        &mut self,
        buttons: &ButtonState,
        config: &Config,
    ) -> (ComboAction, SuppressedButtons) {
        let base_held =
            buttons.get(config.base_combo_button_1) && buttons.get(config.base_combo_button_2);
        let mut action = ComboAction::None;
        let mut suppressed = SuppressedButtons::default();

        // Log base combo rising edge
        if base_held && !self.prev_base_held {
            debug!(
                "[COMBO] {}+{} held",
                config.base_combo_button_1.display_name(),
                config.base_combo_button_2.display_name()
            );
        }

        if base_held {
            // Always suppress base combo buttons when both held
            suppressed.add(config.base_combo_button_1);
            suppressed.add(config.base_combo_button_2);

            // Check configurable button for macro mode toggle
            let toggle_btn_pressed = buttons.get(config.toggle_macro_mode_button);
            match config.toggle_macro_mode_trigger {
                TriggerMode::Hold => {
                    if toggle_btn_pressed {
                        suppressed.add(config.toggle_macro_mode_button);
                        match self.hold_button_start {
                            None => {
                                debug!(
                                    "[COMBO] {} hold started (need {}s for macro mode toggle)",
                                    config.toggle_macro_mode_button.display_name(),
                                    config.combo_hold_time
                                );
                                self.hold_button_start = Some(Instant::now());
                            }
                            Some(start) => {
                                if start.elapsed().as_secs_f64() >= config.combo_hold_time {
                                    action = ComboAction::ToggleMacroMode;
                                    self.hold_button_start = None;
                                }
                            }
                        }
                    } else {
                        if self.hold_button_start.is_some() {
                            debug!(
                                "[COMBO] {} released before hold threshold",
                                config.toggle_macro_mode_button.display_name()
                            );
                        }
                        self.hold_button_start = None;
                    }
                }
                TriggerMode::Edge => {
                    let was_pressed = self.prev_buttons.get(config.toggle_macro_mode_button);
                    if toggle_btn_pressed {
                        suppressed.add(config.toggle_macro_mode_button);
                    }
                    if toggle_btn_pressed && !was_pressed {
                        action = ComboAction::ToggleMacroMode;
                    }
                }
            }

            // Edge-triggered combo buttons: suppress while held, fire on rising edge.
            let edge_combos: [(Button, ComboAction, bool); 7] = [
                (config.prev_slot_button, ComboAction::PrevSlot, false),
                (config.next_slot_button, ComboAction::NextSlot, false),
                (config.play_macro_button, ComboAction::PlayMacro, false),
                (
                    config.stop_playback_button,
                    ComboAction::StopPlayback,
                    false,
                ),
                (config.toggle_loop_button, ComboAction::ToggleLoop, false),
                (config.cycle_speed_button, ComboAction::CycleSpeed, false),
                (
                    config.toggle_recording_button,
                    ComboAction::ToggleRecording,
                    true,
                ),
            ];

            for (btn, combo_action, requires_macro_mode) in edge_combos {
                if requires_macro_mode && !self.macro_mode {
                    continue;
                }
                let pressed = buttons.get(btn);
                if pressed {
                    suppressed.add(btn);
                    if !self.prev_buttons.get(btn) {
                        action = combo_action;
                    }
                }
            }
        } else {
            self.hold_button_start = None;
        }

        if action != ComboAction::None {
            info!("[COMBO] {action}");
        }

        self.prev_buttons = buttons.clone();
        self.prev_base_held = base_held;

        (action, suppressed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> Config {
        Config::default()
    }

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
        let cfg = default_config();

        // Pressing A alone does nothing
        let (action, sup) = cd.update(&buttons_with(&[Button::A]), &cfg);
        assert_eq!(action, ComboAction::None);
        assert!(sup.is_empty());

        // DpadDown alone does nothing
        let (action, sup) = cd.update(&buttons_with(&[Button::DpadDown]), &cfg);
        assert_eq!(action, ComboAction::None);
        assert!(sup.is_empty());
    }

    #[test]
    fn test_l3r3_suppressed() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();
        let (_, sup) = cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);
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
        let cfg = default_config();

        // First frame: L3+R3 (rising edge, but no combo button)
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        // Second frame: L3+R3+A (A rising edge → PlayMacro)
        let (action, sup) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::A]), &cfg);
        assert_eq!(action, ComboAction::PlayMacro);
        assert!(sup.buttons[..sup.count]
            .iter()
            .any(|b| *b == Some(Button::A)));
    }

    #[test]
    fn test_instant_combo_stop_playback() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::B]), &cfg);
        assert_eq!(action, ComboAction::StopPlayback);
    }

    #[test]
    fn test_instant_combo_prev_next_slot() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        let (action, _) = cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::DpadLeft]),
            &cfg,
        );
        assert_eq!(action, ComboAction::PrevSlot);

        // Release DpadLeft
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        let (action, _) = cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::DpadRight]),
            &cfg,
        );
        assert_eq!(action, ComboAction::NextSlot);
    }

    #[test]
    fn test_combo_not_retriggered_on_hold() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        // First press: triggers
        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::A]), &cfg);
        assert_eq!(action, ComboAction::PlayMacro);

        // Held: doesn't retrigger
        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::A]), &cfg);
        assert_eq!(action, ComboAction::None);
    }

    #[test]
    fn test_toggle_recording_in_macro_mode() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();
        cd.macro_mode = true;

        // L3+R3 first (no combo button)
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        // L3+R3+Minus rising edge in macro mode → ToggleRecording
        let (action, sup) = cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::Minus]),
            &cfg,
        );
        assert_eq!(action, ComboAction::ToggleRecording);
        assert!(sup.buttons[..sup.count]
            .iter()
            .any(|b| *b == Some(Button::Minus)));
    }

    #[test]
    fn test_no_recording_without_macro_mode() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();
        assert!(!cd.macro_mode);

        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        // L3+R3+Minus without macro mode → no recording (button suppressed but no action)
        let (action, _) = cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::Minus]),
            &cfg,
        );
        assert_eq!(action, ComboAction::None);
    }

    #[test]
    fn test_dpad_down_hold_toggle() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();

        // Hold L3+R3+DpadDown for > 0.5s
        cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::DpadDown]),
            &cfg,
        );

        // Sleep just over the hold duration
        std::thread::sleep(std::time::Duration::from_millis(550));

        let (action, _) = cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::DpadDown]),
            &cfg,
        );
        assert_eq!(action, ComboAction::ToggleMacroMode);
    }

    #[test]
    fn test_dpad_down_short_press_no_toggle() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();

        // Press briefly
        cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::DpadDown]),
            &cfg,
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        let (action, _) = cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::DpadDown]),
            &cfg,
        );
        assert_eq!(action, ComboAction::None);
    }

    #[test]
    fn test_edge_trigger_mode_instant_toggle() {
        let mut cd = ComboDetector::new();
        let mut cfg = default_config();
        cfg.toggle_macro_mode_trigger = TriggerMode::Edge;

        // First frame: L3+R3 (no combo button)
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        // Second frame: L3+R3+DpadDown rising edge → instant ToggleMacroMode
        let (action, sup) = cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::DpadDown]),
            &cfg,
        );
        assert_eq!(action, ComboAction::ToggleMacroMode);
        assert!(sup.buttons[..sup.count]
            .iter()
            .any(|b| *b == Some(Button::DpadDown)));
    }

    #[test]
    fn test_edge_trigger_mode_no_retrigger_on_hold() {
        let mut cd = ComboDetector::new();
        let mut cfg = default_config();
        cfg.toggle_macro_mode_trigger = TriggerMode::Edge;

        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        // First press: triggers
        let (action, _) = cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::DpadDown]),
            &cfg,
        );
        assert_eq!(action, ComboAction::ToggleMacroMode);

        // Held: doesn't retrigger
        let (action, _) = cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::DpadDown]),
            &cfg,
        );
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
        let cfg = default_config();
        cd.macro_mode = true;

        // L3+R3+A: should NOT trigger recording (A takes priority)
        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::A]), &cfg);
        assert_eq!(action, ComboAction::PlayMacro);
    }

    #[test]
    fn test_instant_combo_toggle_loop() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        let (action, sup) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::Y]), &cfg);
        assert_eq!(action, ComboAction::ToggleLoop);
        assert!(sup.buttons[..sup.count]
            .iter()
            .any(|b| *b == Some(Button::Y)));
    }

    #[test]
    fn test_instant_combo_cycle_speed() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        let (action, sup) = cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::DpadUp]),
            &cfg,
        );
        assert_eq!(action, ComboAction::CycleSpeed);
        assert!(sup.buttons[..sup.count]
            .iter()
            .any(|b| *b == Some(Button::DpadUp)));
    }
}
