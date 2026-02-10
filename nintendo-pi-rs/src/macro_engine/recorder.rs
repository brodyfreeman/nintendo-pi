//! Macro recorder: accumulates timestamped HID frames in memory.

use std::path::Path;
use std::time::Instant;

use tracing::{debug, info, warn};

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
        let count = self.frames.len();
        if count == 1 {
            debug!("[MACRO] First frame captured");
        } else if count.is_multiple_of(1000) {
            debug!(
                "[MACRO] Recording: {count} frames ({}s)",
                elapsed_us / 1_000_000
            );
        }
    }

    /// Stop recording. Returns (frame_count, duration_us).
    pub fn stop(&mut self, trim_end_secs: f64) -> (usize, u64) {
        self.recording = false;
        let trim_us = (trim_end_secs.max(0.0) * 1_000_000.0) as u64;
        if trim_us > 0 {
            if let Some(&(last_ts, _)) = self.frames.last() {
                let cutoff = last_ts.saturating_sub(trim_us);
                let orig = self.frames.len();
                self.frames.retain(|&(ts, _)| ts <= cutoff);
                let trimmed = orig - self.frames.len();
                if trimmed > 0 {
                    info!(
                        "[MACRO] Trimmed {trimmed} frames ({:.1}s) from end",
                        trim_end_secs
                    );
                }
            }
        }
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
        let frame_count = self.frames.len();
        let result = storage::save_macro(macros_dir, &self.frames, name);
        self.frames.clear();
        if result.is_none() && frame_count > 0 {
            warn!("[MACRO] Save failed for {frame_count} recorded frames");
        }
        result
    }
}
