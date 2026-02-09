# Nintendo Switch 2 Pro Controller on Raspberry Pi Zero 2 W

USB driver for using a Nintendo Switch 2 Pro Controller (vendor `057e`, product `2069`) as a standard Linux gamepad on a Raspberry Pi Zero 2 W.

Bluetooth is not currently supported — USB only.

## How it works

The `hid-generic` kernel driver can detect the controller but doesn't understand its HID report format, so buttons don't work natively. The `enable_procon2.py` script handles this in two phases:

1. **USB initialization** — Detaches the kernel driver, sends a 17-command init sequence over raw USB to put the controller into its full input reporting mode (without this, buttons are not reported), then reattaches the kernel driver.

2. **HID reading + virtual gamepad** — Opens the controller via HID, parses the raw reports (buttons, sticks, triggers), applies stick calibration, and emits events through a `uinput` virtual device (`Nintendo Switch 2 Pro Controller` on `/dev/input/js0`).

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
- Left and right analog sticks (calibrated)
- Left and right analog triggers

## Setup

### Prerequisites

```bash
sudo apt install python3-pip
pip3 install hidapi python-uinput pyusb
```

### Files on the Pi

| Path | Purpose |
|------|---------|
| `~/enable_procon2.py` | Main driver script |
| `/etc/modules-load.d/uinput.conf` | Loads `uinput` kernel module at boot |
| `/etc/udev/rules.d/99-switch2-procon.rules` | Triggers service on controller plug/unplug |
| `/etc/systemd/system/switch2-procon.service` | Runs the driver script as a system service |

### Auto-start on plug-in

The controller driver starts automatically when the controller is plugged in and stops when unplugged. This is handled by a udev rule that triggers a systemd service.

To check status:

```bash
sudo systemctl status switch2-procon.service
```

To view logs:

```bash
sudo journalctl -u switch2-procon.service
```

### Manual usage

```bash
sudo python3 ~/enable_procon2.py
```

### Testing

Verify the virtual gamepad is working:

```bash
# Check the device exists
sudo cat /proc/bus/input/devices | grep -A4 'Switch 2'

# Test button input
sudo evtest /dev/input/event0  # event number may vary

# Test joystick input
jstest /dev/input/js0
```

## Known issues

- Bluetooth is not supported; USB connection only.
- The `uinput` kernel module can get unloaded between sessions. The `/etc/modules-load.d/uinput.conf` file ensures it loads at boot.
- The controller must be unplugged and replugged if a previous driver session was killed uncleanly (stale `usbfs` claim).
- Stick calibration values are hardcoded in the script for a specific controller. Other units may need recalibration.
