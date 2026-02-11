//! Shared MITM state and web command types.

use serde::Serialize;
use tokio::sync::watch;

use crate::input::{Button, InputState};

const ALL_BUTTONS: [(Button, &str); 18] = [
    (Button::A, "A"),
    (Button::B, "B"),
    (Button::X, "X"),
    (Button::Y, "Y"),
    (Button::L, "L"),
    (Button::R, "R"),
    (Button::ZL, "ZL"),
    (Button::ZR, "ZR"),
    (Button::Plus, "+"),
    (Button::Minus, "-"),
    (Button::L3, "L3"),
    (Button::R3, "R3"),
    (Button::DpadUp, "Up"),
    (Button::DpadDown, "Down"),
    (Button::DpadLeft, "Left"),
    (Button::DpadRight, "Right"),
    (Button::Home, "Home"),
    (Button::Capture, "Cap"),
];

/// Current playback input state for visualization.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlaybackInput {
    pub buttons: Vec<&'static str>,
    pub left_stick: (f64, f64),
    pub right_stick: (f64, f64),
}

impl PlaybackInput {
    pub fn from_input_state(input: &InputState) -> Self {
        let buttons = ALL_BUTTONS
            .iter()
            .filter(|(btn, _)| input.buttons.get(*btn))
            .map(|(_, name)| *name)
            .collect();

        let normalize = |raw: u16| ((raw as f64 - 2048.0) / 2048.0).clamp(-1.0, 1.0);

        Self {
            buttons,
            left_stick: (
                normalize(input.left_stick_raw.0),
                normalize(input.left_stick_raw.1),
            ),
            right_stick: (
                normalize(input.right_stick_raw.0),
                normalize(input.right_stick_raw.1),
            ),
        }
    }
}

/// Thread/task-safe MITM state snapshot for the web UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateSnapshot {
    pub macro_mode: bool,
    pub recording: bool,
    pub recording_countdown: bool,
    pub playing: bool,
    pub current_slot: usize,
    pub slot_count: usize,
    pub current_macro_name: Option<String>,
    pub usb_connected: bool,
    pub bt_connected: bool,
    pub playback_speed: f64,
    pub looping: bool,
    pub playback_frame: usize,
    pub playback_frame_count: usize,
    pub playback_input: Option<PlaybackInput>,
    pub live_input: Option<PlaybackInput>,
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
    pub play_macro_button: String,
    pub stop_playback_button: String,
    pub toggle_macro_mode_button: String,
    pub toggle_loop_button: String,
    pub cycle_speed_button: String,
    pub prev_slot_button: String,
    pub next_slot_button: String,
    pub toggle_recording_button: String,
    pub base_combo_button_1: String,
    pub base_combo_button_2: String,
    pub toggle_macro_mode_trigger: String,
}

impl Default for StateSnapshot {
    fn default() -> Self {
        Self {
            macro_mode: false,
            recording: false,
            recording_countdown: false,
            playing: false,
            current_slot: 0,
            slot_count: 0,
            current_macro_name: None,
            usb_connected: false,
            bt_connected: false,
            playback_speed: 1.0,
            looping: false,
            playback_frame: 0,
            playback_frame_count: 0,
            playback_input: None,
            live_input: None,
            stick_deadzone: crate::calibration::DEFAULT_DEADZONE,
            combo_hold_time: crate::combo::DEFAULT_HOLD_DURATION,
            auto_loop_default: false,
            playback_speed_default: 1.0,
            playback_start_delay: 0.0,
            loop_restart_delay: 0.0,
            recording_trim_end: 0.0,
            recording_start_delay: 0.0,
            ui_update_interval_ms: 200,
            calibration_samples: 20,
            play_macro_button: crate::combo::DEFAULT_PLAY_MACRO_BUTTON
                .display_name()
                .to_string(),
            stop_playback_button: crate::combo::DEFAULT_STOP_PLAYBACK_BUTTON
                .display_name()
                .to_string(),
            toggle_macro_mode_button: crate::combo::DEFAULT_TOGGLE_MACRO_MODE_BUTTON
                .display_name()
                .to_string(),
            toggle_loop_button: crate::combo::DEFAULT_TOGGLE_LOOP_BUTTON
                .display_name()
                .to_string(),
            cycle_speed_button: crate::combo::DEFAULT_CYCLE_SPEED_BUTTON
                .display_name()
                .to_string(),
            prev_slot_button: crate::combo::DEFAULT_PREV_SLOT_BUTTON
                .display_name()
                .to_string(),
            next_slot_button: crate::combo::DEFAULT_NEXT_SLOT_BUTTON
                .display_name()
                .to_string(),
            toggle_recording_button: crate::combo::DEFAULT_TOGGLE_RECORDING_BUTTON
                .display_name()
                .to_string(),
            base_combo_button_1: crate::combo::DEFAULT_BASE_COMBO_BUTTON_1
                .display_name()
                .to_string(),
            base_combo_button_2: crate::combo::DEFAULT_BASE_COMBO_BUTTON_2
                .display_name()
                .to_string(),
            toggle_macro_mode_trigger: crate::combo::DEFAULT_TOGGLE_MACRO_MODE_TRIGGER
                .as_str()
                .to_string(),
        }
    }
}

pub struct MitmState {
    tx: watch::Sender<StateSnapshot>,
}

impl MitmState {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(StateSnapshot::default());
        Self { tx }
    }

    pub fn update(&self, snapshot: StateSnapshot) {
        self.tx.send_if_modified(|current| {
            if *current != snapshot {
                *current = snapshot;
                true
            } else {
                false
            }
        });
    }

    pub fn snapshot(&self) -> StateSnapshot {
        self.tx.borrow().clone()
    }

    pub fn snapshot_json(&self) -> serde_json::Value {
        serde_json::to_value(self.snapshot()).unwrap_or_default()
    }

    pub fn subscribe(&self) -> watch::Receiver<StateSnapshot> {
        self.tx.subscribe()
    }
}
