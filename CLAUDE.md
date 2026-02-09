# Nintendo Pi - MITM Macro Device

## Deployment

Pi Zero 2 W access:
```
ssh brody@Nintendo-Pi
```

Deploy files:
```
rsync -avz --exclude='.claude' --exclude='__pycache__' --exclude='.venv' ./ brody@Nintendo-Pi:~/nintendo-pi/
```

Install dependencies on Pi (uses uv + system dbus):
```
ssh brody@Nintendo-Pi 'cd ~/nintendo-pi && ~/.local/bin/uv venv --system-site-packages && ~/.local/bin/uv pip install --no-deps nxbt@git+http://github.com/Brikwerk/nxbt.git@abb966d438be79678b1b23579b06517995246618 && ~/.local/bin/uv pip install hidapi pyusb python-uinput flask flask-socketio simple-websocket eventlet pynput psutil'
```

## Service

`mitm.py` runs as a systemd service that auto-starts when the controller is plugged in via udev.

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

## Macro Combos

All combos use the physical controller's stick clicks (L3/R3).

| Combo | Action |
|-------|--------|
| L3+R3+D-pad Down (hold 0.5s) | Toggle macro mode on/off |
| L3+R3 (in macro mode) | Toggle recording start/stop |
| L3+R3+D-pad Left/Right | Switch macro slot |
| L3+R3+A | Play selected macro |
| L3+R3+B | Stop playback |

Controller LEDs change to indicate state (macro mode, recording, playback).

## Macro Management

Macros are stored in `/root/macros/` (root because the service runs as root). Use `sudo` with macrotool:

```
ssh brody@Nintendo-Pi 'cd ~/nintendo-pi && sudo .venv/bin/python3 macrotool.py list'
ssh brody@Nintendo-Pi 'cd ~/nintendo-pi && sudo .venv/bin/python3 macrotool.py info <id>'
ssh brody@Nintendo-Pi 'cd ~/nintendo-pi && sudo .venv/bin/python3 macrotool.py rename <id> <name>'
ssh brody@Nintendo-Pi 'cd ~/nintendo-pi && sudo .venv/bin/python3 macrotool.py delete -f <id>'
ssh brody@Nintendo-Pi 'cd ~/nintendo-pi && sudo .venv/bin/python3 macrotool.py export <id> <path>'
```

## Web UI

A phone-friendly web interface is available at `http://Nintendo-Pi:8080` when the MITM service is running. It provides:
- Real-time state display (macro mode, recording, playback, current slot)
- Buttons to toggle macro mode, start/stop recording, play/stop macros, switch slots
- Macro library with rename and delete

The web server (Flask-SocketIO) runs in a daemon thread alongside the MITM main loop using `threading` async mode with `simple-websocket`. No eventlet monkey-patching is used for the web server.

## Notes

- dbus-python 1.2.16 (pinned by nxbt) doesn't build on Python 3.13 (removed `imp` module). Use system `python3-dbus` package via `--system-site-packages` instead.
- uv is installed at `~/.local/bin/uv` on the Pi.
- On first BT connection, the Switch must be on the "Change Grip/Order" screen to pair with the Pi's virtual Pro Controller.
- Stick centers are auto-calibrated on startup (don't touch the sticks during the first ~1s).
