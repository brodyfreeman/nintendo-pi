//! Command and effect types for the macro engine.
//!
//! These are the "vocabulary" shared between combo detection, web UI,
//! and the controller that executes them. Kept separate so that producers
//! of commands don't need to depend on the controller implementation.

use crate::config::ConfigUpdate;

/// Unified command enum — covers both combo actions and web commands.
#[derive(Debug, Clone, PartialEq)]
pub enum MacroCommand {
    ToggleRecording,
    PrevSlot,
    NextSlot,
    SelectSlot(usize),
    PlayMacro,
    StopPlayback,
    RenameMacro(u32, String),
    DeleteMacro(u32),
    RefreshMacros,
    CycleSpeed,
    SetPlaybackSpeed(f64),
    ToggleLoop,
    UpdateConfig(ConfigUpdate),
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
    pub fn none() -> Self {
        Self {
            led: None,
            broadcast_macros: false,
        }
    }
}
