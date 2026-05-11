# Nintendo Switch 2 Pro Controller on Raspberry Pi Zero 2 W

MITM bridge for using a Nintendo Switch 2 Pro Controller (vendor `057e`, product `2069`) through a Raspberry Pi Zero 2 W. The Pi sits between the physical controller (USB) and the Switch console (Bluetooth), enabling macro recording/playback while forwarding all normal input transparently.

Single Rust binary with an embedded web UI. Cross-compiled for aarch64.

## How it works

1. **USB initialization** — Detaches the kernel HID driver, sends a 17-command init sequence over raw USB bulk transfer to put the controller into its full input reporting mode, then reattaches the kernel driver so hidapi can read reports.

2. **HID reading and pass-through** — Opens the controller via hidapi on a dedicated OS thread. A small input pipeline parses the raw 64-byte reports (buttons, sticks, triggers), applies stick calibration (auto-centered on startup with radial correction and deadzone), and forwards live input without waiting on web UI or macro file I/O.

3. **Bluetooth emulation** — Registers as a Pro Controller over L2CAP (HID interrupt + control channels), handles the Switch's pairing handshake and subcommand protocol, and forwards calibrated input reports in the NXBT-compatible 0x30 format.

4. **Macro engine** — Records timestamped logical input frames to a binary format (MAC3), plays them back with memory-mapped I/O and timestamp chasing. Supports looping, speed control (0.25x–4x), configurable start delays, and end trimming.

5. **Web UI** — Axum HTTP server with SSE for real-time state updates. Provides recording, playback controls, slot management, macro library (rename/delete), and timing/calibration settings.

### HID report format

After initialization, the controller sends 64-byte HID reports:

| Byte(s) | Content |
|---------|---------|
| 0 | Report ID (`0x09`) |
| 1 | Incrementing counter |
| 2 | Mode byte (`0x23` after full init) |
| 3-5 | Button bitfields |
| 6-8 | Left stick (12-bit packed X/Y) |
| 9-11 | Right stick (12-bit packed X/Y) |
| 12 | Unknown |
| 13 | Left trigger |
| 14 | Right trigger |

The partial init (3 commands) results in mode byte `0x20` where buttons and triggers are always zero. The full 17-command sequence switches the controller to mode `0x23` with full input reporting.

## Supported inputs

- Face buttons (A, B, X, Y)
- D-pad (up, down, left, right)
- Shoulder buttons (L, R, ZL, ZR)
- Stick clicks (L3, R3)
- Start, Select, Home
- Left and right analog sticks (auto-calibrated with radial correction)
- Left and right analog triggers

## Macro control

Macro recording and playback are controlled from the web UI. Controller input is passed through normally; there are no controller button combos reserved for macro actions.

Controller LEDs change to indicate state (recording, playback).

## Web UI

Available at `http://Nintendo-Pi:8080` when the service is running. Provides:
- Real-time state display (USB/BT connection, recording, playback, current slot)
- Buttons to start/stop recording, play/stop macros, switch slots
- Macro library with rename and delete
- Configuration panel for timing values and calibration settings

The web server starts before hardware init, so it's available even when the controller isn't plugged in.

## Setup

### Build

Requires `cross` for cross-compilation to aarch64:

```bash
cargo install cross --git https://github.com/cross-rs/cross
```

First-time setup — build the Docker image (includes libudev-dev for arm64):

```bash
cd nintendo-pi-rs && docker build -t cross-aarch64-libudev -f Dockerfile.aarch64 .
```

Build release binary:

```bash
cd nintendo-pi-rs && cross build --release --target aarch64-unknown-linux-gnu
```

The binary is at `nintendo-pi-rs/target/aarch64-unknown-linux-gnu/release/nintendo-pi`.

### Deployment

```bash
rsync -avz nintendo-pi-rs/target/aarch64-unknown-linux-gnu/release/nintendo-pi brody@Nintendo-Pi:~/nintendo-pi/
```

### Service

The binary runs as a systemd service that auto-starts when the controller is plugged in via udev.

| Path | Purpose |
|------|---------|
| `/etc/systemd/system/switch2-procon.service` | Runs the binary as a system service |
| `/etc/udev/rules.d/` | Triggers service on USB plug/unplug of 057e:2069 |

```bash
# View logs
sudo journalctl -u switch2-procon.service -f

# Restart
sudo systemctl restart switch2-procon.service

# Stop (to run manually)
sudo systemctl stop switch2-procon.service
```

### CLI options

```
nintendo-pi [OPTIONS]
  --macros-dir <PATH>   Macros directory path [default: /root/macros]
  --port <PORT>         Web UI port [default: 8080]
  -v, --verbose         Verbose logging
```

## Known issues

- On first BT connection, the Switch must be on the "Change Grip/Order" screen to pair with the Pi's virtual Pro Controller.
- Don't touch the sticks during the first ~1s after USB init — stick centers are auto-calibrated from initial samples.
- Bluetooth must have `AutoEnable=true` in `/etc/bluetooth/main.conf` (uncommented). Without this, BT stays off after reboot and the service will crash at BT init.
- The controller must be unplugged and replugged if a previous session was killed uncleanly (stale `usbfs` claim).
