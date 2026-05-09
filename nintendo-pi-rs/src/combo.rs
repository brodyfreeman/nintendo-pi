//! Combo detection state machine.
//!
//! Detects base-button action combos and reports which buttons to suppress
//! from forwarding to the Switch. Returns `MacroCommand` directly —
//! no intermediate action enum needed.

use tracing::{debug, info};

use crate::config::Config;
use crate::input::{Button, ButtonState};
use crate::macro_engine::command::MacroCommand;

/// Default play macro button.
pub const DEFAULT_PLAY_MACRO_BUTTON: Button = Button::A;

/// Default stop playback button.
pub const DEFAULT_STOP_PLAYBACK_BUTTON: Button = Button::B;

/// Default toggle loop button.
pub const DEFAULT_TOGGLE_LOOP_BUTTON: Button = Button::Y;

/// Default cycle speed button.
pub const DEFAULT_CYCLE_SPEED_BUTTON: Button = Button::DpadUp;

/// Default prev slot button.
pub const DEFAULT_PREV_SLOT_BUTTON: Button = Button::DpadLeft;

/// Default next slot button.
pub const DEFAULT_NEXT_SLOT_BUTTON: Button = Button::DpadRight;

/// Default base combo button 1.
pub const DEFAULT_BASE_COMBO_BUTTON_1: Button = Button::L3;

/// Default base combo button 2.
pub const DEFAULT_BASE_COMBO_BUTTON_2: Button = Button::R3;

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
}

/// Combo detector state machine.
///
/// Button bindings and timing come from `Config`, passed to `update()`.
/// The detector only tracks its own internal state for previous buttons.
pub struct ComboDetector {
    prev_buttons: ButtonState,
    prev_base_held: bool,
}

impl ComboDetector {
    pub fn new() -> Self {
        Self {
            prev_buttons: ButtonState::default(),
            prev_base_held: false,
        }
    }

    /// Process button state. Returns (command, suppressed_buttons).
    pub fn update(
        &mut self,
        buttons: &ButtonState,
        config: &Config,
    ) -> (Option<MacroCommand>, SuppressedButtons) {
        let base_held =
            buttons.get(config.base_combo_button_1) && buttons.get(config.base_combo_button_2);
        let mut command = None;
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

            // Edge-triggered combo buttons: suppress while held, fire on rising edge.
            let edge_combos: [(Button, MacroCommand); 6] = [
                (config.prev_slot_button, MacroCommand::PrevSlot),
                (config.next_slot_button, MacroCommand::NextSlot),
                (config.play_macro_button, MacroCommand::PlayMacro),
                (config.stop_playback_button, MacroCommand::StopPlayback),
                (config.toggle_loop_button, MacroCommand::ToggleLoop),
                (config.cycle_speed_button, MacroCommand::CycleSpeed),
            ];

            for (btn, combo_cmd) in edge_combos {
                let pressed = buttons.get(btn);
                if pressed {
                    suppressed.add(btn);
                    if !self.prev_buttons.get(btn) {
                        command = Some(combo_cmd);
                    }
                }
            }
        }

        if let Some(ref cmd) = command {
            info!("[COMBO] {cmd:?}");
        }

        self.prev_buttons = buttons.clone();
        self.prev_base_held = base_held;

        (command, suppressed)
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
        assert_eq!(action, None);
        assert!(sup.is_empty());

        // DpadDown alone does nothing
        let (action, sup) = cd.update(&buttons_with(&[Button::DpadDown]), &cfg);
        assert_eq!(action, None);
        assert!(sup.is_empty());
    }

    #[test]
    fn test_l3r3_suppressed() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();
        let (_, sup) = cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);
        assert!(!sup.is_empty());
        assert!(sup.buttons[..sup.count].contains(&Some(Button::L3)));
        assert!(sup.buttons[..sup.count].contains(&Some(Button::R3)));
    }

    #[test]
    fn test_instant_combo_play_macro() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();

        // First frame: L3+R3 (rising edge, but no combo button)
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        // Second frame: L3+R3+A (A rising edge → PlayMacro)
        let (action, sup) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::A]), &cfg);
        assert_eq!(action, Some(MacroCommand::PlayMacro));
        assert!(sup.buttons[..sup.count].contains(&Some(Button::A)));
    }

    #[test]
    fn test_instant_combo_stop_playback() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::B]), &cfg);
        assert_eq!(action, Some(MacroCommand::StopPlayback));
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
        assert_eq!(action, Some(MacroCommand::PrevSlot));

        // Release DpadLeft
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        let (action, _) = cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::DpadRight]),
            &cfg,
        );
        assert_eq!(action, Some(MacroCommand::NextSlot));
    }

    #[test]
    fn test_combo_not_retriggered_on_hold() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        // First press: triggers
        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::A]), &cfg);
        assert_eq!(action, Some(MacroCommand::PlayMacro));

        // Held: doesn't retrigger
        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::A]), &cfg);
        assert_eq!(action, None);
    }

    #[test]
    fn test_recording_combo_is_not_handled_by_controller() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();

        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        let (action, sup) = cd.update(
            &buttons_with(&[Button::L3, Button::R3, Button::Minus]),
            &cfg,
        );

        assert_eq!(action, None);
        assert!(!sup.buttons[..sup.count].contains(&Some(Button::Minus)));
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
    fn test_play_combo_takes_priority_over_non_combo_buttons() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();

        let (action, _) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::A]), &cfg);
        assert_eq!(action, Some(MacroCommand::PlayMacro));
    }

    #[test]
    fn test_instant_combo_toggle_loop() {
        let mut cd = ComboDetector::new();
        let cfg = default_config();
        cd.update(&buttons_with(&[Button::L3, Button::R3]), &cfg);

        let (action, sup) = cd.update(&buttons_with(&[Button::L3, Button::R3, Button::Y]), &cfg);
        assert_eq!(action, Some(MacroCommand::ToggleLoop));
        assert!(sup.buttons[..sup.count].contains(&Some(Button::Y)));
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
        assert_eq!(action, Some(MacroCommand::CycleSpeed));
        assert!(sup.buttons[..sup.count].contains(&Some(Button::DpadUp)));
    }
}
