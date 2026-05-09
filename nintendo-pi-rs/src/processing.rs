//! USB input processing loop.
//!
//! Reads HID reports, runs combo detection, handles macro recording/playback,
//! builds BT reports, and pushes state updates. Runs on a blocking thread
//! via `spawn_blocking`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tracing::info;

use crate::bt::report::build_bt_report;
use crate::calibration::StickPair;
use crate::combo::ComboDetector;
use crate::input::{parse_hid_report, InputState};
use crate::led;
use crate::macro_engine::command::{MacroCommand, MacroEffect};
use crate::macro_engine::controller::MacroController;
use crate::macro_engine::storage;
use crate::usb;
use crate::web::state::{MitmState, PlaybackInput, StateSnapshot};

/// Channels and shared state passed in from main.
pub struct ProcessorIO {
    pub hid_rx: std::sync::mpsc::Receiver<usb::hid::HidReport>,
    pub cmd_rx: mpsc::Receiver<MacroCommand>,
    pub report_tx: mpsc::Sender<[u8; 50]>,
    pub mitm_state: Arc<MitmState>,
    pub state_broadcast: broadcast::Sender<String>,
    pub bt_connected: Arc<AtomicBool>,
    pub calibration_samples: Arc<AtomicU32>,
}

/// Owns all state needed for the USB processing loop.
pub struct Processor {
    io: ProcessorIO,
    sticks: StickPair,
    combo: ComboDetector,
    macro_ctrl: MacroController,
}

impl Processor {
    pub fn new(io: ProcessorIO, sticks: StickPair, macros_dir: PathBuf) -> Self {
        let combo = ComboDetector::new();
        let mut macro_ctrl = MacroController::new(macros_dir);
        macro_ctrl.config.calibration_samples = io.calibration_samples.load(Ordering::Relaxed);

        Self {
            io,
            sticks,
            combo,
            macro_ctrl,
        }
    }

    /// Run the processing loop until USB disconnects. Returns `cmd_rx` so it
    /// can be reused across USB reconnection cycles.
    pub fn run(mut self) -> mpsc::Receiver<MacroCommand> {
        let mut usb_check_counter: u32 = 0;

        info!("[MITM] USB processing active.");

        loop {
            self.drain_web_commands();

            let raw_report = match self.io.hid_rx.recv_timeout(Duration::from_millis(8)) {
                Ok(report) => report,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    usb_check_counter += 1;
                    if usb_check_counter >= 250 {
                        usb_check_counter = 0;
                        if !usb::init::is_device_present() {
                            return self.io.cmd_rx;
                        }
                    }
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return self.io.cmd_rx;
                }
            };

            if self.process_playback(&raw_report) {
                continue;
            }

            self.process_live_input(&raw_report);
        }
    }

    /// Drain and execute all pending web commands.
    fn drain_web_commands(&mut self) {
        while let Ok(web_cmd) = self.io.cmd_rx.try_recv() {
            let effect = self.macro_ctrl.execute(web_cmd);
            self.sync_config_to_hardware();
            self.apply_effect(effect);
        }
    }

    /// Handle macro playback. Returns `true` if a playback frame was sent
    /// (caller should skip normal input processing).
    fn process_playback(&mut self, raw_report: &[u8; 64]) -> bool {
        if !self.macro_ctrl.is_playing() {
            return false;
        }

        if let Some(macro_frame) = self.macro_ctrl.get_playback_frame() {
            let parsed = parse_hid_report(&macro_frame);
            let bt_report = self.build_calibrated_report(&parsed);
            let _ = self.io.report_tx.try_send(bt_report);

            // Check for abort combo on live input
            let live_parsed = parse_hid_report(raw_report);
            let (command, _) = self
                .combo
                .update(&live_parsed.buttons, &self.macro_ctrl.config);
            let mut stopped_by_combo = false;
            if command == Some(MacroCommand::StopPlayback) {
                let effect = self.macro_ctrl.execute(MacroCommand::StopPlayback);
                self.apply_effect(effect);
                stopped_by_combo = true;
            }

            if !stopped_by_combo && !self.macro_ctrl.is_playing() {
                self.apply_effect(MacroEffect {
                    led: Some(&led::LED_NORMAL),
                    broadcast_macros: false,
                });
            }

            self.update_state(Some(&parsed), Some(&live_parsed));
            return true;
        }

        if !self.macro_ctrl.is_playing() {
            // Playback finished naturally
            let effect = self.macro_ctrl.execute(MacroCommand::StopPlayback);
            self.apply_effect(effect);
            info!("[MACRO] Playback finished.");
        }

        // Still playing but no frame ready yet — fall through to live input
        false
    }

    /// Process live controller input: combo detection, recording, BT forwarding.
    fn process_live_input(&mut self, raw_report: &[u8; 64]) {
        let mut parsed = parse_hid_report(raw_report);
        let (command, suppressed) = self.combo.update(&parsed.buttons, &self.macro_ctrl.config);

        if let Some(cmd) = command {
            let effect = self.macro_ctrl.execute(cmd);
            self.apply_effect(effect);
        }

        let mut filtered_report = *raw_report;
        if !suppressed.is_empty() {
            suppressed.filter_buttons(&mut parsed.buttons);
            suppressed.filter_raw_report(&mut filtered_report);
        }

        if self.macro_ctrl.is_recording() {
            self.macro_ctrl.add_recording_frame(&filtered_report);
        }

        let bt_report = self.build_calibrated_report(&parsed);
        let _ = self.io.report_tx.try_send(bt_report);

        self.update_state(None, Some(&parsed));
    }

    /// Calibrate sticks and build a BT report from parsed input.
    fn build_calibrated_report(&self, parsed: &InputState) -> [u8; 50] {
        let (left, right) = self
            .sticks
            .calibrate(parsed.left_stick_raw, parsed.right_stick_raw);
        build_bt_report(parsed, left, right, 0)
    }

    /// Push config changes to hardware-facing state (stick deadzone, calibration).
    fn sync_config_to_hardware(&mut self) {
        self.sticks
            .set_deadzone(self.macro_ctrl.config.stick_deadzone);
        self.io.calibration_samples.store(
            self.macro_ctrl.config.calibration_samples,
            Ordering::Relaxed,
        );
    }

    /// Apply side effects from a macro command (LED, broadcast).
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

    /// Push current state to the web UI.
    fn update_state(&self, playback_input: Option<&InputState>, live_input: Option<&InputState>) {
        self.io.mitm_state.update(StateSnapshot {
            recording: self.macro_ctrl.is_recording(),
            recording_countdown: self.macro_ctrl.recording_in_countdown(),
            playing: self.macro_ctrl.is_playing(),
            current_slot: self.macro_ctrl.current_slot,
            slot_count: self.macro_ctrl.cached_slot_count,
            current_macro_name: self.macro_ctrl.cached_macro_name.clone(),
            usb_connected: true,
            bt_connected: self.io.bt_connected.load(Ordering::Relaxed),
            playback_speed: self.macro_ctrl.playback_speed(),
            looping: self.macro_ctrl.looping(),
            playback_frame: self.macro_ctrl.playback_frame(),
            playback_frame_count: self.macro_ctrl.playback_frame_count(),
            playback_input: playback_input.map(PlaybackInput::from_input_state),
            live_input: live_input.map(PlaybackInput::from_input_state),
            config: self.macro_ctrl.config.clone(),
        });
    }
}
