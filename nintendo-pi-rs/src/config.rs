//! Centralized configuration for button bindings and timing values.
//!
//! Shared by `MacroController`, `ComboDetector`, and the web UI state.
//! One struct, one source of truth — no more field-by-field syncing.

use serde::Serialize;
use tracing::{info, warn};

use crate::combo;
use crate::input::Button;

/// Available playback speed presets.
pub const SPEED_PRESETS: &[f64] = &[0.25, 0.5, 1.0, 2.0, 4.0];

/// All user-configurable settings in one place.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Config {
    // Timing / thresholds
    pub stick_deadzone: f64,
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
    pub toggle_loop_button: Button,
    #[serde(serialize_with = "ser_button")]
    pub cycle_speed_button: Button,
    #[serde(serialize_with = "ser_button")]
    pub prev_slot_button: Button,
    #[serde(serialize_with = "ser_button")]
    pub next_slot_button: Button,
    #[serde(serialize_with = "ser_button")]
    pub base_combo_button_1: Button,
    #[serde(serialize_with = "ser_button")]
    pub base_combo_button_2: Button,
}

fn ser_button<S: serde::Serializer>(btn: &Button, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(btn.display_name())
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stick_deadzone: crate::calibration::DEFAULT_DEADZONE,
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
            toggle_loop_button: combo::DEFAULT_TOGGLE_LOOP_BUTTON,
            cycle_speed_button: combo::DEFAULT_CYCLE_SPEED_BUTTON,
            prev_slot_button: combo::DEFAULT_PREV_SLOT_BUTTON,
            next_slot_button: combo::DEFAULT_NEXT_SLOT_BUTTON,
            base_combo_button_1: combo::DEFAULT_BASE_COMBO_BUTTON_1,
            base_combo_button_2: combo::DEFAULT_BASE_COMBO_BUTTON_2,
        }
    }
}

/// A single config field update from the web UI.
///
/// The field name matches the Config struct field (e.g. "stick_deadzone"),
/// and value is the JSON value to apply.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigUpdate {
    pub field_name: String,
    pub value: serde_json::Value,
}

impl Config {
    /// Apply a config update by field name, clamping values and validating names.
    /// Returns false if the field name is unknown or the value is invalid.
    pub fn apply(&mut self, update: &ConfigUpdate) -> bool {
        let field = update.field_name.as_str();
        let val = &update.value;

        // f64 fields with (min, max) clamping
        let f64_result = match field {
            "stick_deadzone" => Some((&mut self.stick_deadzone, 0.0_f64, 50.0)),
            "playback_speed_default" => Some((
                &mut self.playback_speed_default,
                SPEED_PRESETS[0],
                SPEED_PRESETS[SPEED_PRESETS.len() - 1],
            )),
            "playback_start_delay" => Some((&mut self.playback_start_delay, 0.0, 5.0)),
            "loop_restart_delay" => Some((&mut self.loop_restart_delay, 0.0, 5.0)),
            "recording_trim_end" => Some((&mut self.recording_trim_end, 0.0, 2.0)),
            "recording_start_delay" => Some((&mut self.recording_start_delay, 0.0, 5.0)),
            _ => None,
        };
        if let Some((target, min, max)) = f64_result {
            if let Some(v) = val.as_f64() {
                *target = v.clamp(min, max);
                info!("[SETTINGS] {field} set to {target}");
                return true;
            }
            warn!("[SETTINGS] {field}: expected number, got {val}");
            return false;
        }

        // Button binding fields
        let button_result = match field {
            "play_macro_button" => Some(&mut self.play_macro_button),
            "stop_playback_button" => Some(&mut self.stop_playback_button),
            "toggle_loop_button" => Some(&mut self.toggle_loop_button),
            "cycle_speed_button" => Some(&mut self.cycle_speed_button),
            "prev_slot_button" => Some(&mut self.prev_slot_button),
            "next_slot_button" => Some(&mut self.next_slot_button),
            "base_combo_button_1" => Some(&mut self.base_combo_button_1),
            "base_combo_button_2" => Some(&mut self.base_combo_button_2),
            _ => None,
        };
        if let Some(target) = button_result {
            if let Some(name) = val.as_str() {
                if let Some(btn) = Button::from_str_name(name) {
                    *target = btn;
                    info!("[SETTINGS] {field} set to {}", btn.display_name());
                    return true;
                }
                warn!("[SETTINGS] Unknown button name: {name}");
            }
            return false;
        }

        // Remaining one-off fields
        match field {
            "auto_loop_default" => {
                if let Some(v) = val.as_bool() {
                    self.auto_loop_default = v;
                    info!(
                        "[SETTINGS] auto_loop_default: {}",
                        if v { "ON" } else { "OFF" }
                    );
                    return true;
                }
            }
            "ui_update_interval_ms" => {
                if let Some(v) = val.as_u64() {
                    self.ui_update_interval_ms = v.clamp(50, 500);
                    info!(
                        "[SETTINGS] ui_update_interval_ms set to {}",
                        self.ui_update_interval_ms
                    );
                    return true;
                }
            }
            "calibration_samples" => {
                if let Some(v) = val.as_u64() {
                    self.calibration_samples = (v as u32).clamp(5, 100);
                    info!(
                        "[SETTINGS] calibration_samples set to {}",
                        self.calibration_samples
                    );
                    return true;
                }
            }
            _ => {
                warn!("[SETTINGS] Unknown config field: {field}");
            }
        }
        false
    }
}
