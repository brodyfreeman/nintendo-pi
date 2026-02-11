//! Centralized configuration for button bindings and timing values.
//!
//! Shared by `MacroController`, `ComboDetector`, and the web UI state.
//! One struct, one source of truth — no more field-by-field syncing.

use serde::Serialize;
use tracing::{info, warn};

use crate::combo::{self, TriggerMode};
use crate::input::Button;

/// All user-configurable settings in one place.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Config {
    // Timing / thresholds
    pub stick_deadzone: f64,
    pub combo_hold_time: f64,
    pub auto_loop_default: bool,
    pub playback_speed_default: f64,
    pub playback_start_delay: f64,
    pub loop_restart_delay: f64,
    pub recording_trim_end: f64,
    pub recording_start_delay: f64,
    pub ui_update_interval_ms: u64,
    pub calibration_samples: u32,

    // Button bindings (serialized as display names for the web UI)
    #[serde(serialize_with = "ser_button")]
    pub play_macro_button: Button,
    #[serde(serialize_with = "ser_button")]
    pub stop_playback_button: Button,
    #[serde(serialize_with = "ser_button")]
    pub toggle_macro_mode_button: Button,
    #[serde(serialize_with = "ser_button")]
    pub toggle_loop_button: Button,
    #[serde(serialize_with = "ser_button")]
    pub cycle_speed_button: Button,
    #[serde(serialize_with = "ser_button")]
    pub prev_slot_button: Button,
    #[serde(serialize_with = "ser_button")]
    pub next_slot_button: Button,
    #[serde(serialize_with = "ser_button")]
    pub toggle_recording_button: Button,
    #[serde(serialize_with = "ser_button")]
    pub base_combo_button_1: Button,
    #[serde(serialize_with = "ser_button")]
    pub base_combo_button_2: Button,
    #[serde(serialize_with = "ser_trigger")]
    pub toggle_macro_mode_trigger: TriggerMode,
}

fn ser_button<S: serde::Serializer>(btn: &Button, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(btn.display_name())
}

fn ser_trigger<S: serde::Serializer>(mode: &TriggerMode, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(mode.as_str())
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stick_deadzone: crate::calibration::DEFAULT_DEADZONE,
            combo_hold_time: combo::DEFAULT_HOLD_DURATION,
            auto_loop_default: false,
            playback_speed_default: 1.0,
            playback_start_delay: 0.0,
            loop_restart_delay: 0.0,
            recording_trim_end: 0.0,
            recording_start_delay: 0.0,
            ui_update_interval_ms: 200,
            calibration_samples: 20,
            play_macro_button: combo::DEFAULT_PLAY_MACRO_BUTTON,
            stop_playback_button: combo::DEFAULT_STOP_PLAYBACK_BUTTON,
            toggle_macro_mode_button: combo::DEFAULT_TOGGLE_MACRO_MODE_BUTTON,
            toggle_loop_button: combo::DEFAULT_TOGGLE_LOOP_BUTTON,
            cycle_speed_button: combo::DEFAULT_CYCLE_SPEED_BUTTON,
            prev_slot_button: combo::DEFAULT_PREV_SLOT_BUTTON,
            next_slot_button: combo::DEFAULT_NEXT_SLOT_BUTTON,
            toggle_recording_button: combo::DEFAULT_TOGGLE_RECORDING_BUTTON,
            base_combo_button_1: combo::DEFAULT_BASE_COMBO_BUTTON_1,
            base_combo_button_2: combo::DEFAULT_BASE_COMBO_BUTTON_2,
            toggle_macro_mode_trigger: combo::DEFAULT_TOGGLE_MACRO_MODE_TRIGGER,
        }
    }
}

/// A single config field update from the web UI.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigUpdate {
    StickDeadzone(f64),
    ComboHoldTime(f64),
    AutoLoopDefault(bool),
    PlaybackSpeedDefault(f64),
    PlaybackStartDelay(f64),
    LoopRestartDelay(f64),
    RecordingTrimEnd(f64),
    RecordingStartDelay(f64),
    UiUpdateInterval(u64),
    CalibrationSamples(u32),
    PlayMacroButton(String),
    StopPlaybackButton(String),
    ToggleMacroModeButton(String),
    ToggleLoopButton(String),
    CycleSpeedButton(String),
    PrevSlotButton(String),
    NextSlotButton(String),
    ToggleRecordingButton(String),
    BaseComboButton1(String),
    BaseComboButton2(String),
    ToggleMacroModeTrigger(String),
}

/// Try to parse a button name, logging a warning on failure.
fn parse_button(name: &str, label: &str) -> Option<Button> {
    match Button::from_str_name(name) {
        Some(btn) => {
            info!("[SETTINGS] {label} set to {}", btn.display_name());
            Some(btn)
        }
        None => {
            warn!("[SETTINGS] Unknown button name: {name}");
            None
        }
    }
}

impl Config {
    /// Apply a config update, clamping values and validating names.
    pub fn apply(&mut self, update: ConfigUpdate) {
        use crate::macro_engine::player::SPEED_PRESETS;

        match update {
            ConfigUpdate::StickDeadzone(v) => {
                self.stick_deadzone = v.clamp(0.0, 50.0);
                info!(
                    "[SETTINGS] Stick deadzone set to {:.1}",
                    self.stick_deadzone
                );
            }
            ConfigUpdate::ComboHoldTime(v) => {
                self.combo_hold_time = v.clamp(0.1, 2.0);
                info!(
                    "[SETTINGS] Combo hold time set to {:.1}s",
                    self.combo_hold_time
                );
            }
            ConfigUpdate::AutoLoopDefault(v) => {
                self.auto_loop_default = v;
                info!(
                    "[SETTINGS] Auto-loop default: {}",
                    if v { "ON" } else { "OFF" }
                );
            }
            ConfigUpdate::PlaybackSpeedDefault(v) => {
                self.playback_speed_default =
                    v.clamp(SPEED_PRESETS[0], SPEED_PRESETS[SPEED_PRESETS.len() - 1]);
                info!(
                    "[SETTINGS] Playback speed default set to {:.2}x",
                    self.playback_speed_default
                );
            }
            ConfigUpdate::PlaybackStartDelay(v) => {
                self.playback_start_delay = v.clamp(0.0, 5.0);
                info!(
                    "[SETTINGS] Playback start delay set to {:.1}s",
                    self.playback_start_delay
                );
            }
            ConfigUpdate::LoopRestartDelay(v) => {
                self.loop_restart_delay = v.clamp(0.0, 5.0);
                info!(
                    "[SETTINGS] Loop restart delay set to {:.1}s",
                    self.loop_restart_delay
                );
            }
            ConfigUpdate::RecordingTrimEnd(v) => {
                self.recording_trim_end = v.clamp(0.0, 2.0);
                info!(
                    "[SETTINGS] Recording trim end set to {:.1}s",
                    self.recording_trim_end
                );
            }
            ConfigUpdate::RecordingStartDelay(v) => {
                self.recording_start_delay = v.clamp(0.0, 5.0);
                info!(
                    "[SETTINGS] Recording start delay set to {:.1}s",
                    self.recording_start_delay
                );
            }
            ConfigUpdate::UiUpdateInterval(v) => {
                self.ui_update_interval_ms = v.clamp(50, 500);
                info!(
                    "[SETTINGS] UI update interval set to {}ms",
                    self.ui_update_interval_ms
                );
            }
            ConfigUpdate::CalibrationSamples(v) => {
                self.calibration_samples = v.clamp(5, 100);
                info!(
                    "[SETTINGS] Calibration samples set to {}",
                    self.calibration_samples
                );
            }
            ConfigUpdate::PlayMacroButton(ref s) => {
                if let Some(btn) = parse_button(s, "Play macro button") {
                    self.play_macro_button = btn;
                }
            }
            ConfigUpdate::StopPlaybackButton(ref s) => {
                if let Some(btn) = parse_button(s, "Stop playback button") {
                    self.stop_playback_button = btn;
                }
            }
            ConfigUpdate::ToggleMacroModeButton(ref s) => {
                if let Some(btn) = parse_button(s, "Toggle macro mode button") {
                    self.toggle_macro_mode_button = btn;
                }
            }
            ConfigUpdate::ToggleLoopButton(ref s) => {
                if let Some(btn) = parse_button(s, "Toggle loop button") {
                    self.toggle_loop_button = btn;
                }
            }
            ConfigUpdate::CycleSpeedButton(ref s) => {
                if let Some(btn) = parse_button(s, "Cycle speed button") {
                    self.cycle_speed_button = btn;
                }
            }
            ConfigUpdate::PrevSlotButton(ref s) => {
                if let Some(btn) = parse_button(s, "Prev slot button") {
                    self.prev_slot_button = btn;
                }
            }
            ConfigUpdate::NextSlotButton(ref s) => {
                if let Some(btn) = parse_button(s, "Next slot button") {
                    self.next_slot_button = btn;
                }
            }
            ConfigUpdate::ToggleRecordingButton(ref s) => {
                if let Some(btn) = parse_button(s, "Toggle recording button") {
                    self.toggle_recording_button = btn;
                }
            }
            ConfigUpdate::BaseComboButton1(ref s) => {
                if let Some(btn) = parse_button(s, "Base combo button 1") {
                    self.base_combo_button_1 = btn;
                }
            }
            ConfigUpdate::BaseComboButton2(ref s) => {
                if let Some(btn) = parse_button(s, "Base combo button 2") {
                    self.base_combo_button_2 = btn;
                }
            }
            ConfigUpdate::ToggleMacroModeTrigger(ref s) => {
                if let Some(mode) = TriggerMode::from_str_name(s) {
                    self.toggle_macro_mode_trigger = mode;
                    info!(
                        "[SETTINGS] Toggle macro mode trigger set to {}",
                        mode.as_str()
                    );
                } else {
                    warn!("[SETTINGS] Unknown trigger mode: {s}");
                }
            }
        }
    }
}
