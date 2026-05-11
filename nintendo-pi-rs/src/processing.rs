//! Input and macro runtime loops.
//!
//! The input pipeline is intentionally small: USB reports become calibrated
//! logical input and, in live mode, current BT reports. Macro/web work runs in
//! a separate control loop so UI and file I/O do not sit on the pass-through
//! path.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    /// USB reports have passed calibration and are safe for live passthrough.
    InputReady(Box<StickPair>),
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
    /// True only when USB input has passed calibration and is safe to use.
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
    stats: InputStats,
}

impl InputPipeline {
    pub fn new(io: InputPipelineIO) -> Self {
        Self {
            io,
            current_sticks: None,
            latest_live_input: None,
            usb_connected: false,
            stats: InputStats::new(),
        }
    }

    pub async fn run(mut self) {
        info!("[MITM] Input pipeline active.");
        self.publish_status();

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
        let mut coalesced_reports = 0;
        while let Ok(event) = self.io.usb_rx.try_recv() {
            match event {
                UsbEvent::Report(newer_report) => {
                    report = newer_report;
                    coalesced_reports += 1;
                }
                other => {
                    self.handle_usb_event(other);
                    return;
                }
            }
        }

        self.stats.coalesced_reports += coalesced_reports;
        self.handle_usb_report(&report);
    }

    fn handle_usb_event(&mut self, event: UsbEvent) {
        match event {
            UsbEvent::InputReady(mut sticks) => {
                sticks.set_deadzone(self.io.config_rx.borrow().stick_deadzone);
                self.current_sticks = Some(*sticks);
                self.latest_live_input = None;
                self.usb_connected = true;
                let _ = self.io.live_input_tx.send(None);
                self.publish_status();
                led::set_led(&led::LED_NORMAL);
                info!("[USB] Controller input validated and available.");
            }
            UsbEvent::Report(report) => self.handle_usb_report(&report),
            UsbEvent::Disconnected => {
                warn!("[USB] Controller disconnected.");
                self.usb_connected = false;
                self.current_sticks = None;
                self.latest_live_input = None;
                let _ = self.io.live_input_tx.send(None);
                if self.output_mode() == OutputMode::Live {
                    self.emit_output(&LogicalInput::neutral());
                }
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

        self.stats.usb_reports += 1;
        self.latest_live_input = Some(logical.clone());
        let _ = self.io.live_input_tx.send(Some(logical.clone()));

        if self.output_mode() == OutputMode::Live {
            self.emit_output(&logical);
            self.stats.live_outputs += 1;
        }
        self.log_stats_if_due();
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

    fn log_stats_if_due(&mut self) {
        if self.stats.window_start.elapsed() < Duration::from_secs(1) {
            return;
        }
        info!(
            "[PERF] input: usb_reports={} live_outputs={} coalesced_reports={} mode={:?}",
            self.stats.usb_reports,
            self.stats.live_outputs,
            self.stats.coalesced_reports,
            self.output_mode(),
        );
        self.stats = InputStats::new();
    }
}

struct InputStats {
    window_start: Instant,
    usb_reports: u32,
    live_outputs: u32,
    coalesced_reports: u32,
}

impl InputStats {
    fn new() -> Self {
        Self {
            window_start: Instant::now(),
            usb_reports: 0,
            live_outputs: 0,
            coalesced_reports: 0,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::{StickCalibrator, C_STICK_CAL, MAIN_STICK_CAL};

    fn input_pipeline_for_test() -> (
        InputPipeline,
        watch::Receiver<[u8; 50]>,
        watch::Receiver<Option<LogicalInput>>,
    ) {
        let (_usb_tx, usb_rx) = mpsc::channel(1);
        let (report_tx, report_rx) = watch::channel([0; 50]);
        let (live_input_tx, live_input_rx) = watch::channel(None);
        let (status_tx, _status_rx) = watch::channel(PipelineStatus::default());
        let (_output_mode_tx, output_mode_rx) = watch::channel(OutputMode::Live);
        let (_config_tx, config_rx) = watch::channel(Config::default());

        (
            InputPipeline::new(InputPipelineIO {
                usb_rx,
                report_tx,
                live_input_tx,
                status_tx,
                output_mode_rx,
                config_rx,
            }),
            report_rx,
            live_input_rx,
        )
    }

    fn default_sticks() -> StickPair {
        StickPair::new(
            StickCalibrator::new(MAIN_STICK_CAL, 10.0),
            StickCalibrator::new(C_STICK_CAL, 10.0),
            (2048, 2048),
            (2048, 2048),
        )
    }

    fn pack_12bit_pair(x: u16, y: u16) -> [u8; 3] {
        [
            (x & 0xFF) as u8,
            (((x >> 8) & 0x0F) | ((y & 0x0F) << 4)) as u8,
            ((y >> 4) & 0xFF) as u8,
        ]
    }

    fn report_with_sticks(left: (u16, u16), right: (u16, u16)) -> usb::hid::HidReport {
        let mut report = [0u8; 64];
        let left_bytes = pack_12bit_pair(left.0, left.1);
        let right_bytes = pack_12bit_pair(right.0, right.1);
        report[6..9].copy_from_slice(&left_bytes);
        report[9..12].copy_from_slice(&right_bytes);
        report
    }

    #[test]
    fn report_is_ignored_until_usb_input_is_ready() {
        let (mut pipeline, report_rx, live_input_rx) = input_pipeline_for_test();
        let initial_report = *report_rx.borrow();

        pipeline.handle_usb_event(UsbEvent::Report(report_with_sticks(
            (4095, 2048),
            (2048, 2048),
        )));

        assert_eq!(*report_rx.borrow(), initial_report);
        assert!(live_input_rx.borrow().is_none());
    }

    #[test]
    fn disconnect_emits_neutral_report_in_live_mode() {
        let (mut pipeline, report_rx, _live_input_rx) = input_pipeline_for_test();
        let non_neutral = LogicalInput::from_parts(Default::default(), (60.0, 0.0), (-25.0, 10.0));
        let neutral_report = build_bt_report_from_logical(&LogicalInput::neutral(), 0);

        pipeline.current_sticks = Some(default_sticks());
        pipeline.usb_connected = true;
        pipeline.latest_live_input = Some(non_neutral.clone());
        pipeline.emit_output(&non_neutral);
        assert_ne!(*report_rx.borrow(), neutral_report);

        pipeline.handle_usb_event(UsbEvent::Disconnected);

        assert_eq!(*report_rx.borrow(), neutral_report);
        assert!(!pipeline.usb_connected);
        assert!(pipeline.current_sticks.is_none());
        assert!(pipeline.latest_live_input.is_none());
    }
}
