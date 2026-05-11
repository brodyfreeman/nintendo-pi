//! Input and macro runtime loops.
//!
//! The input pipeline is intentionally small: USB reports become calibrated
//! logical input and, in live mode, current BT reports. Macro/web work runs in
//! a separate control loop so UI and file I/O do not sit on the pass-through
//! path.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, watch};
use tracing::{info, warn};

use crate::bt::report::build_bt_report_from_logical;
use crate::calibration::StickPair;
use crate::config::Config;
use crate::input::{parse_hid_report, LogicalInput};
use crate::led;
use crate::macro_engine::command::{MacroCommand, MacroEffect};
use crate::macro_engine::controller::MacroController;
use crate::macro_engine::storage;
use crate::usb;
use crate::web::state::{MitmState, PlaybackInput, StateSnapshot};

/// USB adapter events consumed by the input pipeline.
pub enum UsbEvent {
    Connected(Box<StickPair>),
    Report(usb::hid::HidReport),
    Disconnected,
}

/// Which loop currently owns BT output reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Live,
    Playback,
}

/// Low-rate status produced by the input pipeline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PipelineStatus {
    pub usb_connected: bool,
}

/// Channels and shared state for the input pipeline.
pub struct InputPipelineIO {
    pub usb_rx: mpsc::Receiver<UsbEvent>,
    pub report_tx: watch::Sender<[u8; 50]>,
    pub live_input_tx: watch::Sender<Option<LogicalInput>>,
    pub status_tx: watch::Sender<PipelineStatus>,
    pub output_mode_rx: watch::Receiver<OutputMode>,
    pub config_rx: watch::Receiver<Config>,
}

/// Fast path for live controller input and pass-through output.
pub struct InputPipeline {
    io: InputPipelineIO,
    current_sticks: Option<StickPair>,
    latest_live_input: Option<LogicalInput>,
    usb_connected: bool,
}

impl InputPipeline {
    pub fn new(io: InputPipelineIO) -> Self {
        Self {
            io,
            current_sticks: None,
            latest_live_input: None,
            usb_connected: false,
        }
    }

    pub async fn run(mut self) {
        info!("[MITM] Input pipeline active.");
        self.publish_status();

        let mut output_tick = runtime_interval(Duration::from_millis(8));

        loop {
            tokio::select! {
                Some(event) = self.io.usb_rx.recv() => {
                    self.handle_usb_event_batch(event);
                }
                result = self.io.output_mode_rx.changed() => {
                    if result.is_err() {
                        break;
                    }
                    if self.output_mode() == OutputMode::Live {
                        self.emit_live_output();
                    }
                }
                result = self.io.config_rx.changed() => {
                    if result.is_err() {
                        break;
                    }
                    self.apply_config();
                }
                _ = output_tick.tick() => {
                    if self.output_mode() == OutputMode::Live {
                        self.emit_live_output();
                    }
                }
                else => break,
            }
        }
    }

    fn handle_usb_event_batch(&mut self, event: UsbEvent) {
        match event {
            UsbEvent::Report(report) => self.handle_usb_report_batch(report),
            other => self.handle_usb_event(other),
        }
    }

    fn handle_usb_report_batch(&mut self, mut report: usb::hid::HidReport) {
        while let Ok(event) = self.io.usb_rx.try_recv() {
            match event {
                UsbEvent::Report(newer_report) => report = newer_report,
                other => {
                    self.handle_usb_event(other);
                    return;
                }
            }
        }

        self.handle_usb_report(&report);
    }

    fn handle_usb_event(&mut self, event: UsbEvent) {
        match event {
            UsbEvent::Connected(mut sticks) => {
                sticks.set_deadzone(self.io.config_rx.borrow().stick_deadzone);
                self.current_sticks = Some(*sticks);
                self.latest_live_input = None;
                self.usb_connected = true;
                let _ = self.io.live_input_tx.send(None);
                self.publish_status();
                led::set_led(&led::LED_NORMAL);
                info!("[USB] Controller input available.");
            }
            UsbEvent::Report(report) => self.handle_usb_report(&report),
            UsbEvent::Disconnected => {
                warn!("[USB] Controller disconnected.");
                self.usb_connected = false;
                self.current_sticks = None;
                self.latest_live_input = None;
                let _ = self.io.live_input_tx.send(None);
                self.publish_status();
            }
        }
    }

    fn handle_usb_report(&mut self, raw_report: &usb::hid::HidReport) {
        let Some(sticks) = self.current_sticks.as_ref() else {
            return;
        };

        let parsed = parse_hid_report(raw_report);
        let (left_stick, right_stick) =
            sticks.calibrate(parsed.left_stick_raw, parsed.right_stick_raw);
        let logical = LogicalInput::from_parts(parsed.buttons, left_stick, right_stick);

        self.latest_live_input = Some(logical.clone());
        let _ = self.io.live_input_tx.send(Some(logical.clone()));

        if self.output_mode() == OutputMode::Live {
            self.emit_output(&logical);
        }
    }

    fn emit_live_output(&self) {
        let input = self
            .latest_live_input
            .as_ref()
            .cloned()
            .unwrap_or_else(LogicalInput::neutral);
        self.emit_output(&input);
    }

    fn emit_output(&self, input: &LogicalInput) {
        let report = build_bt_report_from_logical(input, 0);
        let _ = self.io.report_tx.send(report);
    }

    fn apply_config(&mut self) {
        if let Some(sticks) = self.current_sticks.as_mut() {
            sticks.set_deadzone(self.io.config_rx.borrow().stick_deadzone);
        }
    }

    fn output_mode(&self) -> OutputMode {
        *self.io.output_mode_rx.borrow()
    }

    fn publish_status(&self) {
        let _ = self.io.status_tx.send(PipelineStatus {
            usb_connected: self.usb_connected,
        });
    }
}

/// Channels and shared state for the macro/web control loop.
pub struct MacroRuntimeIO {
    pub cmd_rx: mpsc::Receiver<MacroCommand>,
    pub live_input_rx: watch::Receiver<Option<LogicalInput>>,
    pub pipeline_status_rx: watch::Receiver<PipelineStatus>,
    pub output_mode_tx: watch::Sender<OutputMode>,
    pub config_tx: watch::Sender<Config>,
    pub report_tx: watch::Sender<[u8; 50]>,
    pub mitm_state: Arc<MitmState>,
    pub state_broadcast: broadcast::Sender<String>,
    pub bt_connected: Arc<AtomicBool>,
    pub calibration_samples: Arc<AtomicU32>,
}

/// Owns web commands, macro state, recording/playback, and UI snapshots.
pub struct MacroRuntime {
    io: MacroRuntimeIO,
    macro_ctrl: MacroController,
    latest_live_input: Option<LogicalInput>,
    latest_playback_input: Option<LogicalInput>,
    usb_connected: bool,
}

impl MacroRuntime {
    pub fn new(io: MacroRuntimeIO, macros_dir: PathBuf) -> Self {
        let mut macro_ctrl = MacroController::new(macros_dir);
        macro_ctrl.config.calibration_samples = io.calibration_samples.load(Ordering::Relaxed);

        Self {
            io,
            macro_ctrl,
            latest_live_input: None,
            latest_playback_input: None,
            usb_connected: false,
        }
    }

    pub async fn run(mut self) {
        info!("[MITM] Macro control runtime active.");
        self.sync_config_to_pipeline();
        self.update_state();

        let mut playback_tick = runtime_interval(Duration::from_millis(8));
        let mut state_tick = runtime_interval(self.ui_update_interval());

        loop {
            tokio::select! {
                Some(cmd) = self.io.cmd_rx.recv() => {
                    self.handle_web_command(cmd);
                    self.update_state();
                    state_tick = runtime_interval(self.ui_update_interval());
                }
                result = self.io.live_input_rx.changed() => {
                    if result.is_err() {
                        break;
                    }
                    self.handle_live_input();
                }
                result = self.io.pipeline_status_rx.changed() => {
                    if result.is_err() {
                        break;
                    }
                    self.handle_pipeline_status();
                    self.update_state();
                }
                _ = playback_tick.tick() => {
                    self.emit_playback_output();
                }
                _ = state_tick.tick() => {
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
        self.sync_output_mode();
        if self.macro_ctrl.is_playing() {
            self.emit_playback_output();
        }
    }

    fn handle_live_input(&mut self) {
        self.latest_live_input = self.io.live_input_rx.borrow_and_update().clone();
        if let Some(input) = self.latest_live_input.as_ref() {
            if self.macro_ctrl.is_recording() {
                self.macro_ctrl.add_recording_frame(input);
            }
        }
    }

    fn handle_pipeline_status(&mut self) {
        let status = self.io.pipeline_status_rx.borrow_and_update().clone();
        self.usb_connected = status.usb_connected;
        if !self.usb_connected {
            self.latest_live_input = None;
            if self.macro_ctrl.is_recording() {
                info!("[MACRO] USB disconnected during recording; saving captured frames.");
                self.execute_command(MacroCommand::ToggleRecording);
            }
        }
    }

    fn execute_command(&mut self, cmd: MacroCommand) {
        let effect = self.macro_ctrl.execute(cmd);
        self.sync_config_to_pipeline();
        self.apply_effect(effect);
    }

    fn sync_config_to_pipeline(&self) {
        self.io.calibration_samples.store(
            self.macro_ctrl.config.calibration_samples,
            Ordering::Relaxed,
        );
        let _ = self.io.config_tx.send(self.macro_ctrl.config.clone());
    }

    fn sync_output_mode(&self) {
        let mode = if self.macro_ctrl.is_playing() {
            OutputMode::Playback
        } else {
            OutputMode::Live
        };
        let _ = self.io.output_mode_tx.send(mode);
    }

    fn emit_playback_output(&mut self) {
        if !self.macro_ctrl.is_playing() {
            self.latest_playback_input = None;
            return;
        }

        let output = match self.macro_ctrl.get_playback_frame() {
            Some(input) => {
                self.latest_playback_input = Some(input.clone());
                input
            }
            None => self
                .latest_live_input
                .clone()
                .unwrap_or_else(LogicalInput::neutral),
        };

        if !self.macro_ctrl.is_playing() {
            self.apply_effect(MacroEffect {
                led: Some(&led::LED_NORMAL),
                broadcast_macros: false,
            });
            self.sync_output_mode();
            info!("[MACRO] Playback finished.");
        }

        let report = build_bt_report_from_logical(&output, 0);
        let _ = self.io.report_tx.send(report);
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

    fn ui_update_interval(&self) -> Duration {
        Duration::from_millis(self.macro_ctrl.config.ui_update_interval_ms)
    }
}

fn runtime_interval(duration: Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(duration);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval
}
