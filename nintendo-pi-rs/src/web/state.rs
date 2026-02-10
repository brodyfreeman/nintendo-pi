//! Shared MITM state and web command types.

use std::sync::Mutex;

use serde::Serialize;

use crate::input::{Button, InputState};

/// Commands the web UI can send to the MITM main loop.
#[derive(Debug, Clone)]
pub enum WebCommand {
    ToggleMacroMode,
    ToggleRecording,
    PrevSlot,
    NextSlot,
    PlayMacro,
    StopPlayback,
    SelectSlot(usize),
    RenameMacro(u32, String),
    DeleteMacro(u32),
    CycleSpeed,
    SetPlaybackSpeed(f64),
    ToggleLoop,
}

impl From<WebCommand> for crate::macro_engine::controller::MacroCommand {
    fn from(cmd: WebCommand) -> Self {
        match cmd {
            WebCommand::ToggleMacroMode => Self::ToggleMacroMode,
            WebCommand::ToggleRecording => Self::ToggleRecording,
            WebCommand::PrevSlot => Self::PrevSlot,
            WebCommand::NextSlot => Self::NextSlot,
            WebCommand::SelectSlot(s) => Self::SelectSlot(s),
            WebCommand::PlayMacro => Self::PlayMacro,
            WebCommand::StopPlayback => Self::StopPlayback,
            WebCommand::RenameMacro(id, name) => Self::RenameMacro(id, name),
            WebCommand::DeleteMacro(id) => Self::DeleteMacro(id),
            WebCommand::CycleSpeed => Self::CycleSpeed,
            WebCommand::SetPlaybackSpeed(speed) => Self::SetPlaybackSpeed(speed),
            WebCommand::ToggleLoop => Self::ToggleLoop,
        }
    }
}

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
}

impl Default for StateSnapshot {
    fn default() -> Self {
        Self {
            macro_mode: false,
            recording: false,
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
        }
    }
}

pub struct MitmState {
    inner: Mutex<StateSnapshot>,
    changed: Mutex<bool>,
}

impl MitmState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(StateSnapshot::default()),
            changed: Mutex::new(false),
        }
    }

    pub fn update(&self, snapshot: StateSnapshot) {
        let mut inner = self.inner.lock().unwrap();
        if *inner != snapshot {
            *inner = snapshot;
            *self.changed.lock().unwrap() = true;
        }
    }

    pub fn snapshot(&self) -> StateSnapshot {
        self.inner.lock().unwrap().clone()
    }

    pub fn snapshot_json(&self) -> serde_json::Value {
        serde_json::to_value(self.snapshot()).unwrap_or_default()
    }

    /// Return snapshot if changed since last pop, else None.
    pub fn pop_if_changed(&self) -> Option<StateSnapshot> {
        let mut changed = self.changed.lock().unwrap();
        if *changed {
            *changed = false;
            Some(self.inner.lock().unwrap().clone())
        } else {
            None
        }
    }
}
