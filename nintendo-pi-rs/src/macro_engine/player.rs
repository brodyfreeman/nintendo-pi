//! Macro player: memory-mapped playback with timestamp chasing.

use std::fs::File;
use std::path::Path;
use std::time::Instant;

use memmap2::Mmap;
use tracing::{debug, error, info, warn};

use super::storage::{self, FRAME_SIZE, HEADER_SIZE, MAGIC, MAGIC_V1};
use crate::config::SPEED_PRESETS;

pub struct MacroPlayer {
    pub playing: bool,
    pub looping: bool,
    pub speed: f64,
    pub loop_restart_delay: f64,
    mmap: Option<Mmap>,
    _file: Option<File>,
    frame_count: usize,
    frame_index: usize,
    start: Option<Instant>,
    last_report: Option<[u8; 64]>,
    delay_us: u64,
}

impl MacroPlayer {
    pub fn new() -> Self {
        Self {
            playing: false,
            looping: false,
            speed: 1.0,
            loop_restart_delay: 0.0,
            mmap: None,
            _file: None,
            frame_count: 0,
            frame_index: 0,
            start: None,
            last_report: None,
            delay_us: 0,
        }
    }

    /// Load a macro by ID from the index. Returns true on success.
    pub fn load(&mut self, macros_dir: &Path, macro_id: u32) -> bool {
        let entry = match storage::get_macro_info(macros_dir, macro_id) {
            Some(e) => e,
            None => {
                warn!("[MACRO] Macro {macro_id} not found in index");
                return false;
            }
        };

        let filepath = macros_dir.join(&entry.filename);
        if !filepath.exists() {
            warn!("[MACRO] Macro file {} not found", entry.filename);
            return false;
        }

        self.close_mmap();

        let file = match File::open(&filepath) {
            Ok(f) => f,
            Err(e) => {
                error!("[MACRO] Failed to open {}: {e}", entry.filename);
                return false;
            }
        };

        let mmap = match unsafe { Mmap::map(&file) } {
            Ok(m) => m,
            Err(e) => {
                error!("[MACRO] Failed to mmap {}: {e}", entry.filename);
                return false;
            }
        };

        if mmap.len() < HEADER_SIZE {
            warn!("[MACRO] File too small for header");
            return false;
        }

        // Validate magic (accept both MAC2 and MACO)
        let magic = &mmap[0..4];
        if magic != MAGIC && magic != MAGIC_V1 {
            warn!("[MACRO] Invalid magic: {:?}", magic);
            return false;
        }

        let report_size = u16::from_le_bytes([mmap[6], mmap[7]]);
        if report_size != storage::REPORT_SIZE {
            warn!("[MACRO] Unsupported report size in macro file: {report_size}");
            return false;
        }

        let frame_count = u32::from_le_bytes([mmap[8], mmap[9], mmap[10], mmap[11]]) as usize;
        let Some(expected_len) = frame_count
            .checked_mul(FRAME_SIZE)
            .and_then(|frame_bytes| HEADER_SIZE.checked_add(frame_bytes))
        else {
            warn!("[MACRO] Macro frame count is too large: {frame_count}");
            return false;
        };
        if mmap.len() < expected_len {
            warn!(
                "[MACRO] Truncated macro file {}: expected at least {} bytes, got {}",
                entry.filename,
                expected_len,
                mmap.len()
            );
            return false;
        }

        self.mmap = Some(mmap);
        self._file = Some(file);
        self.frame_count = frame_count;
        self.frame_index = 0;
        self.last_report = None;

        info!("[MACRO] Loaded macro {macro_id} ({frame_count} frames)");
        true
    }

    /// Start playback with an optional delay (in seconds) before the first frame plays.
    /// Must call load() first.
    pub fn start_with_delay(&mut self, looping: bool, delay_secs: f64) -> bool {
        if self.mmap.is_none() || self.frame_count == 0 {
            return false;
        }
        self.playing = true;
        self.looping = looping;
        self.frame_index = 0;
        self.start = Some(Instant::now());
        self.last_report = None;
        self.delay_us = (delay_secs.max(0.0) * 1_000_000.0) as u64;
        if self.delay_us > 0 {
            info!("[MACRO] Playback starting in {delay_secs:.1}s (loop={looping})");
        } else {
            info!("[MACRO] Playback started (loop={looping})");
        }
        true
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.looping = false;
        info!("[MACRO] Playback stopped");
    }

    /// Set playback speed (clamped to valid range).
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(SPEED_PRESETS[0], SPEED_PRESETS[SPEED_PRESETS.len() - 1]);
        info!("[MACRO] Playback speed set to {:.2}x", self.speed);
    }

    /// Cycle to the next speed preset. Wraps around.
    pub fn cycle_speed(&mut self) {
        let current_idx = SPEED_PRESETS
            .iter()
            .position(|&s| (s - self.speed).abs() < 0.01)
            .unwrap_or(2); // default to 1.0x index
        let next_idx = (current_idx + 1) % SPEED_PRESETS.len();
        self.set_speed(SPEED_PRESETS[next_idx]);
    }

    /// Get the current frame if its timestamp has been reached.
    ///
    /// Returns Some(report) with the current 64-byte report, or None if done.
    pub fn get_frame(&mut self) -> Option<[u8; 64]> {
        if !self.playing {
            return None;
        }
        let mmap = self.mmap.as_ref()?;
        let total_elapsed_us = self.start.as_ref()?.elapsed().as_micros() as u64;
        if total_elapsed_us < self.delay_us {
            return None; // Still in countdown — pass through live input
        }
        let elapsed_us = ((total_elapsed_us - self.delay_us) as f64 * self.speed) as u64;

        // Advance through frames whose timestamps have passed
        while self.frame_index < self.frame_count {
            let offset = HEADER_SIZE + self.frame_index * FRAME_SIZE;
            if offset + FRAME_SIZE > mmap.len() {
                break;
            }

            let ts_us = u64::from_le_bytes(mmap[offset..offset + 8].try_into().unwrap());

            if ts_us <= elapsed_us {
                let report_offset = offset + 8;
                let mut report = [0u8; 64];
                report.copy_from_slice(&mmap[report_offset..report_offset + 64]);
                self.last_report = Some(report);
                self.frame_index += 1;
            } else {
                break;
            }
        }

        // Check if playback is complete
        if self.frame_index >= self.frame_count {
            if self.looping {
                self.frame_index = 0;
                self.start = Some(Instant::now());
                self.delay_us = (self.loop_restart_delay.max(0.0) * 1_000_000.0) as u64;
                debug!(
                    "[MACRO] Playback loop restarted ({} frames, delay={}ms)",
                    self.frame_count,
                    self.delay_us / 1000,
                );
            } else {
                self.playing = false;
                info!("[MACRO] Playback finished ({} frames)", self.frame_count);
                let report = self.last_report.take();
                return report;
            }
        }

        self.last_report
    }

    pub fn frame_index(&self) -> usize {
        self.frame_index
    }

    pub fn frame_count(&self) -> usize {
        self.frame_count
    }

    fn close_mmap(&mut self) {
        self.mmap = None;
        self._file = None;
    }
}

impl Drop for MacroPlayer {
    fn drop(&mut self) {
        self.close_mmap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use tempfile::TempDir;

    #[test]
    fn test_new_defaults_to_1x_speed() {
        let player = MacroPlayer::new();
        assert!((player.speed - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_set_speed_clamps_to_range() {
        let mut player = MacroPlayer::new();

        player.set_speed(10.0);
        assert!((player.speed - 4.0).abs() < f64::EPSILON);

        player.set_speed(0.01);
        assert!((player.speed - 0.25).abs() < f64::EPSILON);

        player.set_speed(2.0);
        assert!((player.speed - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_cycle_speed_wraps() {
        let mut player = MacroPlayer::new();
        // Start at 1.0x (index 2)
        assert!((player.speed - 1.0).abs() < f64::EPSILON);

        player.cycle_speed(); // -> 2.0
        assert!((player.speed - 2.0).abs() < f64::EPSILON);

        player.cycle_speed(); // -> 4.0
        assert!((player.speed - 4.0).abs() < f64::EPSILON);

        player.cycle_speed(); // -> 0.25 (wrap)
        assert!((player.speed - 0.25).abs() < f64::EPSILON);

        player.cycle_speed(); // -> 0.5
        assert!((player.speed - 0.5).abs() < f64::EPSILON);

        player.cycle_speed(); // -> 1.0
        assert!((player.speed - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_cycle_speed_from_unknown_defaults_to_after_1x() {
        let mut player = MacroPlayer::new();
        // Set to a non-preset value
        player.speed = 1.5;
        // Should default to index 2 (1.0x), then advance to index 3 (2.0x)
        player.cycle_speed();
        assert!((player.speed - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_load_rejects_truncated_macro_file() {
        let dir = TempDir::new().unwrap();
        let frame: [u8; 64] = [0; 64];
        let id = storage::save_macro(dir.path(), &[(0, frame), (1000, frame)], None).unwrap();
        let entry = storage::get_macro_info(dir.path(), id).unwrap();
        let path = dir.path().join(entry.filename);

        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len((HEADER_SIZE + FRAME_SIZE) as u64).unwrap();

        let mut player = MacroPlayer::new();
        assert!(!player.load(dir.path(), id));
        assert_eq!(player.frame_count(), 0);
    }
}
