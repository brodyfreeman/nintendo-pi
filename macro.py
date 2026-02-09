"""Macro recording, playback, and storage management.

Binary format:
  Header (16 bytes):
    Bytes 0-3:   Magic "MACO" (0x4D41434F)
    Bytes 4-5:   Format version (uint16 LE) = 1
    Bytes 6-7:   Report size (uint16 LE) = 64
    Bytes 8-11:  Frame count (uint32 LE)
    Bytes 12-15: Duration in microseconds (uint32 LE)

  Per frame (72 bytes):
    Bytes 0-7:   Timestamp in microseconds since recording start (uint64 LE)
    Bytes 8-71:  Raw 64-byte HID report

Storage layout:
  ~/macros/
    index.json          # metadata for all macros
    001_macro.bin
    002_macro.bin
    ...
"""
import json
import mmap
import os
import struct
import time
from pathlib import Path

MAGIC = b"MACO"
FORMAT_VERSION = 1
REPORT_SIZE = 64
HEADER_SIZE = 16
FRAME_SIZE = 8 + REPORT_SIZE  # 72 bytes
HEADER_STRUCT = struct.Struct("<4sHHII")  # magic, version, report_size, frame_count, duration_us
FRAME_TS_STRUCT = struct.Struct("<Q")  # timestamp in microseconds

MACROS_DIR = Path.home() / "macros"


def _ensure_macros_dir():
    MACROS_DIR.mkdir(parents=True, exist_ok=True)


def _index_path():
    return MACROS_DIR / "index.json"


def _load_index():
    path = _index_path()
    if path.exists():
        with open(path, "r") as f:
            return json.load(f)
    return []


def _save_index(index):
    _ensure_macros_dir()
    with open(_index_path(), "w") as f:
        json.dump(index, f, indent=2)


def _next_id(index):
    if not index:
        return 1
    return max(entry["id"] for entry in index) + 1


class MacroRecorder:
    """Records timestamped HID reports to an in-memory buffer."""

    def __init__(self):
        self.frames = []
        self._start_ns = None
        self.recording = False

    def start(self):
        self.frames = []
        self._start_ns = time.monotonic_ns()
        self.recording = True

    def add_frame(self, raw_report):
        """Add a 64-byte raw HID report to the recording."""
        if not self.recording:
            return
        elapsed_us = (time.monotonic_ns() - self._start_ns) // 1000
        self.frames.append((elapsed_us, bytes(raw_report[:REPORT_SIZE])))

    def stop(self):
        """Stop recording and return (frame_count, duration_us)."""
        self.recording = False
        if not self.frames:
            return 0, 0
        return len(self.frames), self.frames[-1][0]

    def save(self, name=None):
        """Flush recorded frames to a .bin file and update index.json.

        Returns the macro ID, or None if no frames were recorded.
        """
        if not self.frames:
            return None

        _ensure_macros_dir()
        index = _load_index()
        macro_id = _next_id(index)

        if name is None:
            name = f"macro_{macro_id}"

        frame_count = len(self.frames)
        duration_us = self.frames[-1][0] if self.frames else 0

        filename = f"{macro_id:03d}_{name}.bin"
        filepath = MACROS_DIR / filename

        with open(filepath, "wb") as f:
            header = HEADER_STRUCT.pack(
                MAGIC, FORMAT_VERSION, REPORT_SIZE, frame_count, duration_us
            )
            f.write(header)
            for ts_us, report in self.frames:
                f.write(FRAME_TS_STRUCT.pack(ts_us))
                # Pad or truncate to exactly REPORT_SIZE
                padded = report[:REPORT_SIZE].ljust(REPORT_SIZE, b"\x00")
                f.write(padded)

        entry = {
            "id": macro_id,
            "name": name,
            "filename": filename,
            "frame_count": frame_count,
            "duration_ms": duration_us // 1000,
            "created": time.strftime("%Y-%m-%d %H:%M:%S"),
        }
        index.append(entry)
        _save_index(index)

        self.frames = []
        return macro_id


class MacroPlayer:
    """Replays a recorded macro from a .bin file using memory mapping."""

    def __init__(self):
        self.playing = False
        self.looping = False
        self._mmap = None
        self._file = None
        self._frame_count = 0
        self._frame_index = 0
        self._start_ns = None
        self._last_report = None

    def load(self, macro_id):
        """Load a macro by ID. Returns True on success."""
        index = _load_index()
        entry = None
        for e in index:
            if e["id"] == macro_id:
                entry = e
                break
        if entry is None:
            return False

        filepath = MACROS_DIR / entry["filename"]
        if not filepath.exists():
            return False

        self._close_mmap()

        self._file = open(filepath, "rb")
        self._mmap = mmap.mmap(self._file.fileno(), 0, access=mmap.ACCESS_READ)

        # Parse header
        header_data = self._mmap[:HEADER_SIZE]
        magic, version, report_size, frame_count, duration_us = HEADER_STRUCT.unpack(header_data)
        if magic != MAGIC or version != FORMAT_VERSION:
            self._close_mmap()
            return False

        self._frame_count = frame_count
        self._frame_index = 0
        self._last_report = None
        return True

    def start(self, loop=False):
        """Start playback. Must call load() first."""
        if self._mmap is None or self._frame_count == 0:
            return False
        self.playing = True
        self.looping = loop
        self._frame_index = 0
        self._start_ns = time.monotonic_ns()
        self._last_report = None
        return True

    def stop(self):
        """Stop playback."""
        self.playing = False
        self.looping = False

    def get_frame(self):
        """Get the current macro frame if its timestamp has been reached.

        Returns:
            bytes: 64-byte raw HID report to send, or None if playback is done.
                   Returns the last frame's report if between frames (hold state).
        """
        if not self.playing or self._mmap is None:
            return None

        elapsed_us = (time.monotonic_ns() - self._start_ns) // 1000

        # Advance through frames whose timestamps have passed
        while self._frame_index < self._frame_count:
            offset = HEADER_SIZE + self._frame_index * FRAME_SIZE
            ts_us = FRAME_TS_STRUCT.unpack(self._mmap[offset:offset + 8])[0]

            if ts_us <= elapsed_us:
                report_offset = offset + 8
                self._last_report = bytes(self._mmap[report_offset:report_offset + REPORT_SIZE])
                self._frame_index += 1
            else:
                break

        # Check if playback is complete
        if self._frame_index >= self._frame_count:
            if self.looping:
                self._frame_index = 0
                self._start_ns = time.monotonic_ns()
            else:
                self.playing = False
                report = self._last_report
                self._last_report = None
                return report

        return self._last_report

    def _close_mmap(self):
        if self._mmap is not None:
            self._mmap.close()
            self._mmap = None
        if self._file is not None:
            self._file.close()
            self._file = None

    def close(self):
        """Release resources."""
        self.stop()
        self._close_mmap()

    def __del__(self):
        self._close_mmap()


# --- Utility functions for macrotool.py ---

def list_macros():
    """Return the macro index (list of dicts)."""
    return _load_index()


def get_macro_info(macro_id):
    """Return the index entry for a macro, or None."""
    for entry in _load_index():
        if entry["id"] == macro_id:
            return entry
    return None


def rename_macro(macro_id, new_name):
    """Rename a macro. Returns True on success."""
    index = _load_index()
    for entry in index:
        if entry["id"] == macro_id:
            old_path = MACROS_DIR / entry["filename"]
            new_filename = f"{macro_id:03d}_{new_name}.bin"
            new_path = MACROS_DIR / new_filename
            if old_path.exists():
                old_path.rename(new_path)
            entry["name"] = new_name
            entry["filename"] = new_filename
            _save_index(index)
            return True
    return False


def delete_macro(macro_id):
    """Delete a macro and its .bin file. Returns True on success."""
    index = _load_index()
    new_index = []
    deleted = False
    for entry in index:
        if entry["id"] == macro_id:
            filepath = MACROS_DIR / entry["filename"]
            if filepath.exists():
                filepath.unlink()
            deleted = True
        else:
            new_index.append(entry)
    if deleted:
        _save_index(new_index)
    return deleted


def get_slot_count():
    """Return the number of recorded macros."""
    return len(_load_index())


def get_macro_id_by_slot(slot_index):
    """Return the macro ID at a given slot index (0-based), or None."""
    index = _load_index()
    if 0 <= slot_index < len(index):
        return index[slot_index]["id"]
    return None
