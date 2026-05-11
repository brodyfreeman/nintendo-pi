# Nintendo Pi - MITM Macro Device (Rust)

## Build

Requires `cross` for cross-compilation to aarch64 (Pi Zero 2 W):
```
cargo install cross --git https://github.com/cross-rs/cross
```

First-time setup — build the cross-compilation Docker image (includes libudev-dev for arm64):
```
cd nintendo-pi-rs && docker build -t cross-aarch64-libudev -f Dockerfile.aarch64 .
```

Build release binary:
```
cd nintendo-pi-rs && cross build --release --target aarch64-unknown-linux-gnu
```

The binary is at `nintendo-pi-rs/target/aarch64-unknown-linux-gnu/release/nintendo-pi`.

## Deployment

Pi Zero 2 W access:
```
ssh brody@Nintendo-Pi
```

Deploy binary (web assets are embedded at compile time):
```
rsync -avz nintendo-pi-rs/target/aarch64-unknown-linux-gnu/release/nintendo-pi brody@Nintendo-Pi:~/nintendo-pi/
```

## Service

The binary runs as a systemd service that auto-starts when the controller is plugged in via udev.

- Service: `switch2-procon.service`
- Service file: `/etc/systemd/system/switch2-procon.service`
- Udev rules: `/etc/udev/rules.d/` (triggers service on USB plug/unplug of 057e:2069)

View logs:
```
ssh brody@Nintendo-Pi 'sudo journalctl -u switch2-procon.service -f'
```

Restart service:
```
ssh brody@Nintendo-Pi 'sudo systemctl restart switch2-procon.service'
```

Stop service (to run manually):
```
ssh brody@Nintendo-Pi 'sudo systemctl stop switch2-procon.service'
```

## CLI Options

```
nintendo-pi [OPTIONS]
  --macros-dir <PATH>   Macros directory path [default: /root/macros]
  --port <PORT>         Web UI port [default: 8080]
  -v, --verbose         Verbose logging
```

## Macro Control

Macro recording and playback are controlled from the web UI. Controller input is passed through normally; no controller button combos are reserved for macro actions.

Controller LEDs change to indicate state (recording, playback).

## Web UI

A phone-friendly web interface is available at `http://Nintendo-Pi:8080` when the service is running. It provides:
- Real-time state display (USB/BT connection, recording, playback, current slot)
- Buttons to start/stop recording, play/stop macros, switch slots
- Macro library with rename and delete
- Configuration panel for timing values and calibration settings

The web server (Axum with SSE) starts before hardware init, so it's available even when the controller isn't plugged in. USB init retries every 5s until the controller appears.

## Notes

- On first BT connection, the Switch must be on the "Change Grip/Order" screen to pair with the Pi's virtual Pro Controller.
- Stick centers are auto-calibrated on startup (don't touch the sticks during the first ~1s).
- Macros are stored in the `--macros-dir` directory (`/root/macros/` by default, root because the service runs as root).
- Bluetooth must be set to auto-power-on: `AutoEnable=true` in `/etc/bluetooth/main.conf` (uncommented). Without this, BT stays off after reboot and the service will crash at BT init.
