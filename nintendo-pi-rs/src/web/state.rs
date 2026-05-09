//! Shared MITM state and web command types.

use serde::Serialize;
use tokio::sync::watch;

use crate::config::Config;
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

        let normalize_to_unit_range = |raw: u16| ((raw as f64 - 2048.0) / 2048.0).clamp(-1.0, 1.0);

        Self {
            buttons,
            left_stick: (
                normalize_to_unit_range(input.left_stick_raw.0),
                normalize_to_unit_range(input.left_stick_raw.1),
            ),
            right_stick: (
                normalize_to_unit_range(input.right_stick_raw.0),
                normalize_to_unit_range(input.right_stick_raw.1),
            ),
        }
    }
}

/// Thread/task-safe MITM state snapshot for the web UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateSnapshot {
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
    // Config is flattened into the JSON so the web UI sees the same field names
    #[serde(flatten)]
    pub config: Config,
}

impl Default for StateSnapshot {
    fn default() -> Self {
        Self {
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
            config: Config::default(),
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
