//! Macro recorder: accumulates timestamped HID frames in memory.

use std::path::Path;
use std::time::Instant;

use tracing::info;

use super::storage;

pub struct MacroRecorder {
    pub recording: bool,
    frames: Vec<(u64, [u8; 64])>,
    start: Option<Instant>,
}

impl MacroRecorder {
    pub fn new() -> Self {
        Self {
            recording: false,
            frames: Vec::new(),
            start: None,
        }
    }

    pub fn start(&mut self) {
        self.frames.clear();
        self.start = Some(Instant::now());
        self.recording = true;
        info!("[MACRO] Recording started");
    }

    /// Add a 64-byte raw HID report to the recording.
    pub fn add_frame(&mut self, raw_report: &[u8; 64]) {
        if !self.recording {
            return;
        }
        let elapsed_us = self
            .start
            .map(|s| s.elapsed().as_micros() as u64)
            .unwrap_or(0);
        self.frames.push((elapsed_us, *raw_report));
    }

    /// Stop recording. Returns (frame_count, duration_us).
    pub fn stop(&mut self) -> (usize, u64) {
        self.recording = false;
        let frame_count = self.frames.len();
        let duration_us = self.frames.last().map(|(ts, _)| *ts).unwrap_or(0);
        info!(
            "[MACRO] Recording stopped: {frame_count} frames, {}ms",
            duration_us / 1000
        );
        (frame_count, duration_us)
    }

    /// Save recorded frames to disk. Returns macro ID or None.
    pub fn save(&mut self, macros_dir: &Path, name: Option<&str>) -> Option<u32> {
        let result = storage::save_macro(macros_dir, &self.frames, name);
        self.frames.clear();
        result
    }
}
