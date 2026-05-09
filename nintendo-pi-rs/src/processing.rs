//! Always-running macro runtime.
//!
//! Owns macro state, web/command handling, playback timing, combo detection,
//! and state updates. USB and Bluetooth are adapters around this runtime:
//! USB sends optional live input events, and BT consumes output reports.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::bt::report::build_bt_report_from_logical;
use crate::calibration::StickPair;
use crate::combo::ComboDetector;
use crate::input::{parse_hid_report, LogicalInput};
use crate::led;
use crate::macro_engine::command::{MacroCommand, MacroEffect};
use crate::macro_engine::controller::MacroController;
use crate::macro_engine::storage;
use crate::usb;
use crate::web::state::{MitmState, PlaybackInput, StateSnapshot};

/// USB adapter events consumed by the macro runtime.
pub enum UsbEvent {
    Connected(Box<StickPair>),
    Report(usb::hid::HidReport),
    Disconnected,
}

/// Channels and shared state passed in from main.
pub struct RuntimeIO {
    pub cmd_rx: mpsc::Receiver<MacroCommand>,
    pub usb_rx: mpsc::Receiver<UsbEvent>,
    pub report_tx: mpsc::Sender<[u8; 50]>,
    pub mitm_state: Arc<MitmState>,
    pub state_broadcast: broadcast::Sender<String>,
    pub bt_connected: Arc<AtomicBool>,
    pub calibration_samples: Arc<AtomicU32>,
}

/// The process-wide owner for macro behavior.
pub struct MacroRuntime {
    io: RuntimeIO,
    combo: ComboDetector,
    macro_ctrl: MacroController,
    current_sticks: Option<StickPair>,
    latest_live_input: Option<LogicalInput>,
    latest_playback_input: Option<LogicalInput>,
    usb_connected: bool,
}

impl MacroRuntime {
    pub fn new(io: RuntimeIO, macros_dir: PathBuf) -> Self {
        let mut macro_ctrl = MacroController::new(macros_dir);
        macro_ctrl.config.calibration_samples = io.calibration_samples.load(Ordering::Relaxed);

        Self {
            io,
            combo: ComboDetector::new(),
            macro_ctrl,
            current_sticks: None,
            latest_live_input: None,
            latest_playback_input: None,
            usb_connected: false,
        }
    }

    pub async fn run(mut self) {
        info!("[MITM] Macro runtime active.");
        self.update_state();

        let mut tick = tokio::time::interval(Duration::from_millis(8));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                Some(cmd) = self.io.cmd_rx.recv() => {
                    self.handle_web_command(cmd);
                    self.update_state();
                }
                Some(event) = self.io.usb_rx.recv() => {
                    self.handle_usb_event(event);
                    self.update_state();
                }
                _ = tick.tick() => {
                    self.emit_current_output();
                    self.update_state();
                }
                else => break,
            }
        }
    }

    fn handle_web_command(&mut self, cmd: MacroCommand) {
        if matches!(cmd, MacroCommand::ToggleRecording)
            && !self.usb_connected
            && !self.macro_ctrl.is_recording()
        {
            warn!("[MACRO] Cannot start recording without a connected USB controller");
            return;
        }

        self.execute_command(cmd);
        self.emit_current_output();
    }

    fn handle_usb_event(&mut self, event: UsbEvent) {
        match event {
            UsbEvent::Connected(mut sticks) => {
                sticks.set_deadzone(self.macro_ctrl.config.stick_deadzone);
                self.current_sticks = Some(*sticks);
                self.latest_live_input = None;
                self.combo = ComboDetector::new();
                self.usb_connected = true;
                led::set_led(&led::LED_NORMAL);
                info!("[USB] Controller input available.");
            }
            UsbEvent::Report(raw_report) => self.handle_usb_report(&raw_report),
            UsbEvent::Disconnected => {
                warn!("[USB] Controller disconnected.");
                self.usb_connected = false;
                self.current_sticks = None;
                self.latest_live_input = None;
                self.combo = ComboDetector::new();

                if self.macro_ctrl.is_recording() {
                    info!("[MACRO] USB disconnected during recording; saving captured frames.");
                    self.execute_command(MacroCommand::ToggleRecording);
                }
            }
        }
    }

    fn handle_usb_report(&mut self, raw_report: &usb::hid::HidReport) {
        if self.current_sticks.is_none() {
            return;
        }

        let mut parsed = parse_hid_report(raw_report);
        let (command, suppressed) = self.combo.update(&parsed.buttons, &self.macro_ctrl.config);

        if let Some(cmd) = command {
            self.execute_command(cmd);
        }

        let Some(sticks) = self.current_sticks.as_ref() else {
            return;
        };
        let (left_stick, right_stick) =
            sticks.calibrate(parsed.left_stick_raw, parsed.right_stick_raw);

        if !suppressed.is_empty() {
            suppressed.filter_buttons(&mut parsed.buttons);
        }

        let logical = LogicalInput::from_parts(parsed.buttons, left_stick, right_stick);
        if self.macro_ctrl.is_recording() {
            self.macro_ctrl.add_recording_frame(&logical);
        }
        self.latest_live_input = Some(logical);

        self.emit_current_output();
    }

    fn execute_command(&mut self, cmd: MacroCommand) {
        let effect = self.macro_ctrl.execute(cmd);
        self.sync_config_to_hardware();
        self.apply_effect(effect);
    }

    fn sync_config_to_hardware(&mut self) {
        if let Some(sticks) = self.current_sticks.as_mut() {
            sticks.set_deadzone(self.macro_ctrl.config.stick_deadzone);
        }
        self.io.calibration_samples.store(
            self.macro_ctrl.config.calibration_samples,
            Ordering::Relaxed,
        );
    }

    fn emit_current_output(&mut self) {
        let was_playing = self.macro_ctrl.is_playing();
        let output = if was_playing {
            match self.macro_ctrl.get_playback_frame() {
                Some(input) => {
                    self.latest_playback_input = Some(input.clone());
                    input
                }
                None => self
                    .latest_live_input
                    .clone()
                    .unwrap_or_else(LogicalInput::neutral),
            }
        } else {
            self.latest_playback_input = None;
            self.latest_live_input
                .clone()
                .unwrap_or_else(LogicalInput::neutral)
        };

        if was_playing && !self.macro_ctrl.is_playing() {
            self.apply_effect(MacroEffect {
                led: Some(&led::LED_NORMAL),
                broadcast_macros: false,
            });
            info!("[MACRO] Playback finished.");
        }

        let report = build_bt_report_from_logical(&output, 0);
        let _ = self.io.report_tx.try_send(report);
    }

    fn apply_effect(&self, effect: MacroEffect) {
        if let Some(pattern) = effect.led {
            led::set_led(pattern);
        }
        if effect.broadcast_macros {
            let macros = storage::list_macros(self.macro_ctrl.macros_dir());
            let msg = serde_json::json!({ "type": "macro_list", "macros": macros });
            let _ = self.io.state_broadcast.send(msg.to_string());
        }
    }

    fn update_state(&self) {
        self.io.mitm_state.update(StateSnapshot {
            recording: self.macro_ctrl.is_recording(),
            recording_countdown: self.macro_ctrl.recording_in_countdown(),
            playing: self.macro_ctrl.is_playing(),
            current_slot: self.macro_ctrl.current_slot,
            slot_count: self.macro_ctrl.cached_slot_count,
            current_macro_name: self.macro_ctrl.cached_macro_name.clone(),
            usb_connected: self.usb_connected,
            bt_connected: self.io.bt_connected.load(Ordering::Relaxed),
            playback_speed: self.macro_ctrl.playback_speed(),
            looping: self.macro_ctrl.looping(),
            playback_frame: self.macro_ctrl.playback_frame(),
            playback_frame_count: self.macro_ctrl.playback_frame_count(),
            playback_input: self
                .latest_playback_input
                .as_ref()
                .map(PlaybackInput::from_logical_input),
            live_input: self
                .latest_live_input
                .as_ref()
                .map(PlaybackInput::from_logical_input),
            config: self.macro_ctrl.config.clone(),
        });
    }
}
