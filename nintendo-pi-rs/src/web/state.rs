//! Shared MITM state and web command types.

use std::sync::Mutex;

use serde::Serialize;

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
        }
    }
}

/// Thread/task-safe MITM state snapshot for the web UI.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StateSnapshot {
    pub macro_mode: bool,
    pub recording: bool,
    pub playing: bool,
    pub current_slot: usize,
    pub slot_count: usize,
    pub current_macro_name: Option<String>,
    pub usb_connected: bool,
    pub bt_connected: bool,
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
