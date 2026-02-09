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

/// Thread/task-safe MITM state snapshot for the web UI.
#[derive(Debug, Clone, Serialize)]
pub struct StateSnapshot {
    pub macro_mode: bool,
    pub recording: bool,
    pub playing: bool,
    pub current_slot: usize,
    pub slot_count: usize,
    pub current_macro_name: Option<String>,
    pub connected: bool,
}

pub struct MitmState {
    inner: Mutex<StateSnapshot>,
    changed: Mutex<bool>,
}

impl MitmState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(StateSnapshot {
                macro_mode: false,
                recording: false,
                playing: false,
                current_slot: 0,
                slot_count: 0,
                current_macro_name: None,
                connected: false,
            }),
            changed: Mutex::new(false),
        }
    }

    pub fn update(&self, snapshot: StateSnapshot) {
        let mut inner = self.inner.lock().unwrap();
        // Only mark changed if values actually differ
        let changed = inner.macro_mode != snapshot.macro_mode
            || inner.recording != snapshot.recording
            || inner.playing != snapshot.playing
            || inner.current_slot != snapshot.current_slot
            || inner.slot_count != snapshot.slot_count
            || inner.current_macro_name != snapshot.current_macro_name
            || inner.connected != snapshot.connected;

        if changed {
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
