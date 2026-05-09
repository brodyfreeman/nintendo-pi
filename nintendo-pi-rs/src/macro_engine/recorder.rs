//! Macro recorder: accumulates timestamped HID frames in memory.

use std::path::Path;
use std::time::Instant;

use tracing::{debug, info, warn};

use super::storage;
use crate::input::LogicalInput;

pub struct MacroRecorder {
    pub recording: bool,
    frames: Vec<(u64, LogicalInput)>,
    start: Option<Instant>,
    countdown_deadline: Option<Instant>,
}

impl MacroRecorder {
    pub fn new() -> Self {
        Self {
            recording: false,
            frames: Vec::new(),
            start: None,
            countdown_deadline: None,
        }
    }

    pub fn start_with_delay(&mut self, delay_secs: f64) {
        self.frames.clear();
        self.recording = true;
        let delay_us = (delay_secs.max(0.0) * 1_000_000.0) as u64;
        if delay_us > 0 {
            self.countdown_deadline =
                Some(Instant::now() + std::time::Duration::from_micros(delay_us));
            self.start = None;
            info!("[MACRO] Recording starting in {delay_secs:.1}s");
        } else {
            self.countdown_deadline = None;
            self.start = Some(Instant::now());
            info!("[MACRO] Recording started");
        }
    }

    /// Whether the recorder is in countdown (delay not yet elapsed).
    pub fn in_countdown(&self) -> bool {
        self.countdown_deadline
            .map(|t| Instant::now() < t)
            .unwrap_or(false)
    }

    /// Add a logical input frame to the recording.
    pub fn add_frame(&mut self, input: &LogicalInput) {
        if !self.recording {
            return;
        }
        // Check if still in countdown
        if let Some(deadline) = self.countdown_deadline {
            if Instant::now() < deadline {
                return; // Still waiting
            }
            // Countdown finished — begin actual recording
            self.countdown_deadline = None;
            self.start = Some(Instant::now());
            info!("[MACRO] Recording started (after delay)");
        }
        let elapsed_us = self
            .start
            .map(|s| s.elapsed().as_micros() as u64)
            .unwrap_or(0);
        self.frames.push((elapsed_us, input.clone()));
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
        let trimmed = self.trim_end(trim_us);
        if trimmed > 0 {
            info!(
                "[MACRO] Trimmed {trimmed} frames ({:.1}s) from end",
                trim_end_secs
            );
        }
        let frame_count = self.frames.len();
        let duration_us = self.frames.last().map(|(ts, _)| *ts).unwrap_or(0);
        info!(
            "[MACRO] Recording stopped: {frame_count} frames, {}ms",
            duration_us / 1000
        );
        (frame_count, duration_us)
    }

    fn trim_end(&mut self, trim_us: u64) -> usize {
        if trim_us == 0 {
            return 0;
        }

        if let Some(&(last_ts, _)) = self.frames.last() {
            let count_before_trim = self.frames.len();
            if trim_us >= last_ts {
                self.frames.clear();
            } else {
                let cutoff = last_ts - trim_us;
                self.frames.retain(|&(ts, _)| ts <= cutoff);
            }
            return count_before_trim - self.frames.len();
        }

        0
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

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> LogicalInput {
        LogicalInput::neutral()
    }

    #[test]
    fn stop_without_trim_keeps_recorded_frames() {
        let mut recorder = MacroRecorder::new();
        recorder.recording = true;
        recorder.frames = vec![(0, input()), (250_000, input()), (500_000, input())];

        let (frame_count, duration_us) = recorder.stop(0.0);

        assert_eq!(frame_count, 3);
        assert_eq!(duration_us, 500_000);
        assert_eq!(
            recorder
                .frames
                .iter()
                .map(|(ts, _)| *ts)
                .collect::<Vec<_>>(),
            vec![0, 250_000, 500_000]
        );
    }

    #[test]
    fn stop_trims_frames_after_cutoff() {
        let mut recorder = MacroRecorder::new();
        recorder.recording = true;
        recorder.frames = vec![
            (0, input()),
            (250_000, input()),
            (500_000, input()),
            (750_000, input()),
            (1_000_000, input()),
        ];

        let (frame_count, duration_us) = recorder.stop(0.3);

        assert_eq!(frame_count, 3);
        assert_eq!(duration_us, 500_000);
        assert_eq!(
            recorder
                .frames
                .iter()
                .map(|(ts, _)| *ts)
                .collect::<Vec<_>>(),
            vec![0, 250_000, 500_000]
        );
    }

    #[test]
    fn stop_with_trim_longer_than_recording_discards_recording() {
        let mut recorder = MacroRecorder::new();
        recorder.recording = true;
        recorder.frames = vec![(0, input()), (400_000, input())];

        let (frame_count, duration_us) = recorder.stop(1.0);

        assert_eq!(frame_count, 0);
        assert_eq!(duration_us, 0);
        assert!(recorder.frames.is_empty());
    }
}
