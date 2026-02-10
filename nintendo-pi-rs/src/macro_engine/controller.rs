//! Unified macro command handler.
//!
//! Owns recorder, player, and slot state. Handles commands from both
//! combo detection and web UI, eliminating the duplication that existed
//! when both paths had their own match arms.

use std::path::{Path, PathBuf};

use tracing::info;

use super::player::MacroPlayer;
use super::recorder::MacroRecorder;
use super::storage;
use crate::led;

/// Unified command enum — covers both combo actions and web commands.
#[derive(Debug, Clone, PartialEq)]
pub enum MacroCommand {
    ToggleMacroMode,
    ToggleRecording,
    PrevSlot,
    NextSlot,
    SelectSlot(usize),
    PlayMacro,
    StopPlayback,
    RenameMacro(u32, String),
    DeleteMacro(u32),
    CycleSpeed,
    SetPlaybackSpeed(f64),
}

/// Side effects produced by executing a command.
///
/// The caller is responsible for applying these (setting LEDs, broadcasting
/// macro list updates) so that `MacroController` stays free of I/O.
pub struct MacroEffect {
    /// LED pattern to set, if any.
    pub led: Option<&'static [u8; 16]>,
    /// Whether the macro list should be broadcast to web clients.
    pub broadcast_macros: bool,
}

impl MacroEffect {
    fn none() -> Self {
        Self {
            led: None,
            broadcast_macros: false,
        }
    }
}

/// Owns all macro state and provides a single `execute()` entry point.
pub struct MacroController {
    pub macro_mode: bool,
    pub recorder: MacroRecorder,
    pub player: MacroPlayer,
    pub current_slot: usize,
    pub cached_slot_count: usize,
    pub cached_macro_name: Option<String>,
    macros_dir: PathBuf,
}

impl MacroController {
    pub fn new(macros_dir: PathBuf) -> Self {
        let slot_count = storage::get_slot_count(&macros_dir);
        let macro_name = storage::get_macro_id_by_slot(&macros_dir, 0)
            .and_then(|id| storage::get_macro_info(&macros_dir, id))
            .map(|e| e.name);

        Self {
            macro_mode: false,
            recorder: MacroRecorder::new(),
            player: MacroPlayer::new(),
            current_slot: 0,
            cached_slot_count: slot_count,
            cached_macro_name: macro_name,
            macros_dir,
        }
    }

    /// Execute a macro command. Returns the side effects to apply.
    pub fn execute(&mut self, cmd: MacroCommand) -> MacroEffect {
        match cmd {
            MacroCommand::ToggleMacroMode => self.toggle_macro_mode(),
            MacroCommand::ToggleRecording => self.toggle_recording(),
            MacroCommand::PrevSlot => self.prev_slot(),
            MacroCommand::NextSlot => self.next_slot(),
            MacroCommand::SelectSlot(slot) => self.select_slot(slot),
            MacroCommand::PlayMacro => self.play_macro(),
            MacroCommand::StopPlayback => self.stop_playback(),
            MacroCommand::RenameMacro(id, name) => self.rename_macro(id, &name),
            MacroCommand::DeleteMacro(id) => self.delete_macro(id),
            MacroCommand::CycleSpeed => self.cycle_speed(),
            MacroCommand::SetPlaybackSpeed(speed) => self.set_playback_speed(speed),
        }
    }

    /// The macros directory path.
    pub fn macros_dir(&self) -> &Path {
        &self.macros_dir
    }

    /// LED pattern for the current mode (macro mode vs normal).
    pub fn mode_led(&self) -> &'static [u8; 16] {
        if self.macro_mode {
            &led::LED_MACRO_MODE
        } else {
            &led::LED_NORMAL
        }
    }

    fn refresh_cache(&mut self) {
        self.cached_slot_count = storage::get_slot_count(&self.macros_dir);
        self.cached_macro_name = storage::get_macro_id_by_slot(&self.macros_dir, self.current_slot)
            .and_then(|id| storage::get_macro_info(&self.macros_dir, id))
            .map(|e| e.name);
    }

    fn toggle_macro_mode(&mut self) -> MacroEffect {
        self.macro_mode = !self.macro_mode;
        if self.macro_mode {
            self.refresh_cache();
            info!(
                "[MACRO] Macro mode ON. {} macro(s). Slot: {}",
                self.cached_slot_count, self.current_slot
            );
            MacroEffect {
                led: Some(&led::LED_MACRO_MODE),
                broadcast_macros: false,
            }
        } else {
            let mut broadcast = false;
            if self.recorder.recording {
                self.recorder.stop();
                self.recorder.save(&self.macros_dir, None);
                broadcast = true;
            }
            info!("[MACRO] Macro mode OFF.");
            MacroEffect {
                led: Some(&led::LED_NORMAL),
                broadcast_macros: broadcast,
            }
        }
    }

    fn toggle_recording(&mut self) -> MacroEffect {
        if self.recorder.recording {
            self.recorder.stop();
            self.recorder.save(&self.macros_dir, None);
            self.refresh_cache();
            MacroEffect {
                led: Some(&led::LED_MACRO_MODE),
                broadcast_macros: true,
            }
        } else {
            self.recorder.start();
            MacroEffect {
                led: Some(&led::LED_RECORDING),
                broadcast_macros: false,
            }
        }
    }

    fn prev_slot(&mut self) -> MacroEffect {
        if self.cached_slot_count > 0 {
            self.current_slot = if self.current_slot == 0 {
                self.cached_slot_count - 1
            } else {
                self.current_slot - 1
            };
            self.refresh_cache();
            info!("[MACRO] Slot {} selected.", self.current_slot);
        }
        MacroEffect::none()
    }

    fn next_slot(&mut self) -> MacroEffect {
        if self.cached_slot_count > 0 {
            self.current_slot = (self.current_slot + 1) % self.cached_slot_count;
            self.refresh_cache();
            info!("[MACRO] Slot {} selected.", self.current_slot);
        }
        MacroEffect::none()
    }

    fn select_slot(&mut self, slot: usize) -> MacroEffect {
        if slot < self.cached_slot_count {
            self.current_slot = slot;
            self.refresh_cache();
        }
        MacroEffect::none()
    }

    fn play_macro(&mut self) -> MacroEffect {
        if let Some(macro_id) = storage::get_macro_id_by_slot(&self.macros_dir, self.current_slot) {
            if self.player.load(&self.macros_dir, macro_id) {
                self.player.start(false);
                info!(
                    "[MACRO] Playing macro {} (slot {}).",
                    macro_id, self.current_slot
                );
                return MacroEffect {
                    led: Some(&led::LED_PLAYBACK),
                    broadcast_macros: false,
                };
            }
        }
        MacroEffect::none()
    }

    fn stop_playback(&mut self) -> MacroEffect {
        if self.player.playing {
            self.player.stop();
            MacroEffect {
                led: Some(self.mode_led()),
                broadcast_macros: false,
            }
        } else {
            MacroEffect::none()
        }
    }

    fn cycle_speed(&mut self) -> MacroEffect {
        self.player.cycle_speed();
        MacroEffect::none()
    }

    fn set_playback_speed(&mut self, speed: f64) -> MacroEffect {
        self.player.set_speed(speed);
        MacroEffect::none()
    }

    fn rename_macro(&mut self, id: u32, name: &str) -> MacroEffect {
        if storage::rename_macro(&self.macros_dir, id, name) {
            self.refresh_cache();
            MacroEffect {
                led: None,
                broadcast_macros: true,
            }
        } else {
            MacroEffect::none()
        }
    }

    fn delete_macro(&mut self, id: u32) -> MacroEffect {
        if storage::delete_macro(&self.macros_dir, id) {
            let new_count = storage::get_slot_count(&self.macros_dir);
            self.cached_slot_count = new_count;
            if new_count == 0 {
                self.current_slot = 0;
            } else if self.current_slot >= new_count {
                self.current_slot = new_count - 1;
            }
            self.refresh_cache();
            MacroEffect {
                led: None,
                broadcast_macros: true,
            }
        } else {
            MacroEffect::none()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_controller() -> (MacroController, TempDir) {
        let dir = TempDir::new().unwrap();
        let ctrl = MacroController::new(dir.path().to_path_buf());
        (ctrl, dir)
    }

    #[test]
    fn test_toggle_macro_mode_on_off() {
        let (mut ctrl, _dir) = make_controller();
        assert!(!ctrl.macro_mode);

        let effect = ctrl.execute(MacroCommand::ToggleMacroMode);
        assert!(ctrl.macro_mode);
        assert_eq!(
            effect.led.unwrap() as *const _,
            &led::LED_MACRO_MODE as *const _
        );
        assert!(!effect.broadcast_macros);

        let effect = ctrl.execute(MacroCommand::ToggleMacroMode);
        assert!(!ctrl.macro_mode);
        assert_eq!(
            effect.led.unwrap() as *const _,
            &led::LED_NORMAL as *const _
        );
    }

    #[test]
    fn test_toggle_macro_mode_off_stops_recording() {
        let (mut ctrl, _dir) = make_controller();
        ctrl.execute(MacroCommand::ToggleMacroMode); // ON
        ctrl.recorder.start();
        assert!(ctrl.recorder.recording);

        let effect = ctrl.execute(MacroCommand::ToggleMacroMode); // OFF
        assert!(!ctrl.recorder.recording);
        assert!(effect.broadcast_macros); // saved macro triggers broadcast
    }

    #[test]
    fn test_slot_navigation_empty() {
        let (mut ctrl, _dir) = make_controller();
        assert_eq!(ctrl.cached_slot_count, 0);

        // PrevSlot/NextSlot do nothing with 0 macros
        ctrl.execute(MacroCommand::PrevSlot);
        assert_eq!(ctrl.current_slot, 0);
        ctrl.execute(MacroCommand::NextSlot);
        assert_eq!(ctrl.current_slot, 0);
    }

    #[test]
    fn test_slot_navigation_wraps() {
        let (mut ctrl, _dir) = make_controller();

        // Create 3 macros by saving frames
        let frame: [u8; 64] = [0; 64];
        for _ in 0..3 {
            storage::save_macro(ctrl.macros_dir(), &[(0, frame), (1000, frame)], None);
        }
        ctrl.cached_slot_count = storage::get_slot_count(ctrl.macros_dir());
        assert_eq!(ctrl.cached_slot_count, 3);

        // Forward wrap
        ctrl.execute(MacroCommand::NextSlot); // 0 -> 1
        assert_eq!(ctrl.current_slot, 1);
        ctrl.execute(MacroCommand::NextSlot); // 1 -> 2
        assert_eq!(ctrl.current_slot, 2);
        ctrl.execute(MacroCommand::NextSlot); // 2 -> 0
        assert_eq!(ctrl.current_slot, 0);

        // Backward wrap
        ctrl.execute(MacroCommand::PrevSlot); // 0 -> 2
        assert_eq!(ctrl.current_slot, 2);
    }

    #[test]
    fn test_toggle_recording() {
        let (mut ctrl, _dir) = make_controller();

        // Start recording
        let effect = ctrl.execute(MacroCommand::ToggleRecording);
        assert!(ctrl.recorder.recording);
        assert_eq!(
            effect.led.unwrap() as *const _,
            &led::LED_RECORDING as *const _
        );
        assert!(!effect.broadcast_macros);

        // Stop recording
        let effect = ctrl.execute(MacroCommand::ToggleRecording);
        assert!(!ctrl.recorder.recording);
        assert_eq!(
            effect.led.unwrap() as *const _,
            &led::LED_MACRO_MODE as *const _
        );
        assert!(effect.broadcast_macros);
    }

    #[test]
    fn test_select_slot_bounds() {
        let (mut ctrl, _dir) = make_controller();

        // Out of bounds does nothing
        ctrl.execute(MacroCommand::SelectSlot(5));
        assert_eq!(ctrl.current_slot, 0);

        // Create a macro
        let frame: [u8; 64] = [0; 64];
        storage::save_macro(ctrl.macros_dir(), &[(0, frame)], None);
        ctrl.cached_slot_count = storage::get_slot_count(ctrl.macros_dir());

        ctrl.execute(MacroCommand::SelectSlot(0));
        assert_eq!(ctrl.current_slot, 0);
    }

    #[test]
    fn test_delete_macro_adjusts_slot() {
        let (mut ctrl, _dir) = make_controller();

        // Create 2 macros
        let frame: [u8; 64] = [0; 64];
        let _id1 = storage::save_macro(ctrl.macros_dir(), &[(0, frame)], None).unwrap();
        let _id2 = storage::save_macro(ctrl.macros_dir(), &[(0, frame)], None).unwrap();
        ctrl.cached_slot_count = storage::get_slot_count(ctrl.macros_dir());
        ctrl.current_slot = 1;

        // Delete second macro — slot should adjust down
        let effect = ctrl.execute(MacroCommand::DeleteMacro(_id2));
        assert!(effect.broadcast_macros);
        assert_eq!(ctrl.current_slot, 0);
        assert_eq!(ctrl.cached_slot_count, 1);
    }

    #[test]
    fn test_rename_macro() {
        let (mut ctrl, _dir) = make_controller();

        let frame: [u8; 64] = [0; 64];
        let id = storage::save_macro(ctrl.macros_dir(), &[(0, frame)], Some("old_name")).unwrap();
        ctrl.cached_slot_count = storage::get_slot_count(ctrl.macros_dir());

        let effect = ctrl.execute(MacroCommand::RenameMacro(id, "new_name".into()));
        assert!(effect.broadcast_macros);

        let info = storage::get_macro_info(ctrl.macros_dir(), id).unwrap();
        assert_eq!(info.name, "new_name");
    }

    #[test]
    fn test_stop_playback_not_playing() {
        let (mut ctrl, _dir) = make_controller();
        let effect = ctrl.execute(MacroCommand::StopPlayback);
        assert!(effect.led.is_none());
    }

    #[test]
    fn test_cycle_speed() {
        let (mut ctrl, _dir) = make_controller();
        assert!((ctrl.player.speed - 1.0).abs() < f64::EPSILON);

        ctrl.execute(MacroCommand::CycleSpeed);
        assert!((ctrl.player.speed - 2.0).abs() < f64::EPSILON);

        ctrl.execute(MacroCommand::CycleSpeed);
        assert!((ctrl.player.speed - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_set_playback_speed() {
        let (mut ctrl, _dir) = make_controller();

        ctrl.execute(MacroCommand::SetPlaybackSpeed(0.5));
        assert!((ctrl.player.speed - 0.5).abs() < f64::EPSILON);

        // Clamped to max
        ctrl.execute(MacroCommand::SetPlaybackSpeed(100.0));
        assert!((ctrl.player.speed - 4.0).abs() < f64::EPSILON);
    }
}
