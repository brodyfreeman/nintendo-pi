//! Macro player: memory-mapped playback with timestamp chasing.

use std::fs::File;
use std::path::Path;
use std::time::Instant;

use memmap2::Mmap;
use tracing::{error, info, warn};

use super::storage::{self, FRAME_SIZE, HEADER_SIZE, MAGIC, MAGIC_V1};

pub struct MacroPlayer {
    pub playing: bool,
    pub looping: bool,
    mmap: Option<Mmap>,
    _file: Option<File>,
    frame_count: usize,
    frame_index: usize,
    start: Option<Instant>,
    last_report: Option<[u8; 64]>,
}

impl MacroPlayer {
    pub fn new() -> Self {
        Self {
            playing: false,
            looping: false,
            mmap: None,
            _file: None,
            frame_count: 0,
            frame_index: 0,
            start: None,
            last_report: None,
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

        let frame_count = u32::from_le_bytes([mmap[8], mmap[9], mmap[10], mmap[11]]) as usize;

        self.mmap = Some(mmap);
        self._file = Some(file);
        self.frame_count = frame_count;
        self.frame_index = 0;
        self.last_report = None;

        info!("[MACRO] Loaded macro {macro_id} ({frame_count} frames)");
        true
    }

    /// Start playback. Must call load() first.
    pub fn start(&mut self, looping: bool) -> bool {
        if self.mmap.is_none() || self.frame_count == 0 {
            return false;
        }
        self.playing = true;
        self.looping = looping;
        self.frame_index = 0;
        self.start = Some(Instant::now());
        self.last_report = None;
        info!("[MACRO] Playback started (loop={})", looping);
        true
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.looping = false;
        info!("[MACRO] Playback stopped");
    }

    /// Get the current frame if its timestamp has been reached.
    ///
    /// Returns Some(report) with the current 64-byte report, or None if done.
    pub fn get_frame(&mut self) -> Option<[u8; 64]> {
        if !self.playing {
            return None;
        }
        let mmap = self.mmap.as_ref()?;
        let elapsed_us = self.start.as_ref()?.elapsed().as_micros() as u64;

        // Advance through frames whose timestamps have passed
        while self.frame_index < self.frame_count {
            let offset = HEADER_SIZE + self.frame_index * FRAME_SIZE;
            if offset + FRAME_SIZE > mmap.len() {
                break;
            }

            let ts_us = u64::from_le_bytes([
                mmap[offset],
                mmap[offset + 1],
                mmap[offset + 2],
                mmap[offset + 3],
                mmap[offset + 4],
                mmap[offset + 5],
                mmap[offset + 6],
                mmap[offset + 7],
            ]);

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
            } else {
                self.playing = false;
                let report = self.last_report.take();
                return report;
            }
        }

        self.last_report
    }

    fn close_mmap(&mut self) {
        self.mmap = None;
        self._file = None;
    }

    pub fn close(&mut self) {
        self.stop();
        self.close_mmap();
    }
}

impl Drop for MacroPlayer {
    fn drop(&mut self) {
        self.close_mmap();
    }
}
