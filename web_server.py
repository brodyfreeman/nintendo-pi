"""Web UI server for Nintendo Pi macro controls.

Runs Flask-SocketIO in a daemon thread alongside the MITM main loop.
Uses threading async mode with simple-websocket for native WebSocket support.

Communication:
  Web -> MITM: thread-safe queue.Queue of (WebCommand, data) tuples
  MITM -> Web: shared MitmState object (lock-protected); background task
               emits SocketIO state_update events at ~5 Hz on change
"""
import queue
import threading
import time
from enum import Enum, auto

from flask import Flask, jsonify, render_template, request
from flask_socketio import SocketIO, emit

from macro import list_macros


class WebCommand(Enum):
    """Commands the web UI can send to the MITM main loop."""
    TOGGLE_MACRO_MODE = auto()
    TOGGLE_RECORDING = auto()
    PREV_SLOT = auto()
    NEXT_SLOT = auto()
    PLAY_MACRO = auto()
    STOP_PLAYBACK = auto()
    SELECT_SLOT = auto()     # data: slot index (int)
    RENAME_MACRO = auto()    # data: (macro_id, new_name)
    DELETE_MACRO = auto()    # data: macro_id


class MitmState:
    """Thread-safe snapshot of the MITM state for the web UI."""

    def __init__(self):
        self._lock = threading.Lock()
        self._state = {
            "macro_mode": False,
            "recording": False,
            "playing": False,
            "current_slot": 0,
            "slot_count": 0,
            "current_macro_name": None,
            "connected": False,
        }
        self._changed = False

    def update(self, **kwargs):
        """Update state fields. Only sets changed flag if values differ."""
        with self._lock:
            for key, value in kwargs.items():
                if key in self._state and self._state[key] != value:
                    self._state[key] = value
                    self._changed = True

    def snapshot(self):
        """Return a copy of the current state."""
        with self._lock:
            return dict(self._state)

    def pop_if_changed(self):
        """Return snapshot if state changed since last pop, else None."""
        with self._lock:
            if self._changed:
                self._changed = False
                return dict(self._state)
            return None


class WebServer:
    """Flask-SocketIO web server running on a daemon thread."""

    def __init__(self, command_queue, mitm_state, port=8080):
        self.command_queue = command_queue
        self.mitm_state = mitm_state
        self.port = port

        self.app = Flask(__name__)
        self.app.config["SECRET_KEY"] = "nintendo-pi"
        self.socketio = SocketIO(
            self.app,
            async_mode="threading",
            cors_allowed_origins="*",
        )

        self._register_routes()
        self._register_socketio_events()

    def _register_routes(self):
        @self.app.route("/")
        def index():
            return render_template("index.html")

        @self.app.route("/api/state")
        def api_state():
            return jsonify(self.mitm_state.snapshot())

        @self.app.route("/api/macros")
        def api_macros():
            return jsonify(list_macros())

    def _register_socketio_events(self):
        @self.socketio.on("connect")
        def handle_connect():
            emit("state_update", self.mitm_state.snapshot())
            emit("macro_list", list_macros())

        @self.socketio.on("command")
        def handle_command(data):
            cmd_name = data.get("cmd")
            cmd_data = data.get("data")
            try:
                cmd = WebCommand[cmd_name]
            except (KeyError, TypeError):
                emit("error", {"message": f"Unknown command: {cmd_name}"})
                return
            self.command_queue.put((cmd, cmd_data))
            emit("ack", {"cmd": cmd_name})

        @self.socketio.on("request_state")
        def handle_request_state():
            emit("state_update", self.mitm_state.snapshot())
            emit("macro_list", list_macros())

    def _state_emitter(self):
        """Background loop that emits state updates at ~5 Hz when changed."""
        while True:
            snapshot = self.mitm_state.pop_if_changed()
            if snapshot is not None:
                self.socketio.emit("state_update", snapshot)
            time.sleep(0.2)

    def start(self):
        """Start the web server and state emitter on daemon threads."""
        # State emitter thread
        emitter = threading.Thread(target=self._state_emitter, daemon=True)
        emitter.start()

        # Flask-SocketIO server thread
        server = threading.Thread(
            target=lambda: self.socketio.run(
                self.app,
                host="0.0.0.0",
                port=self.port,
                use_reloader=False,
                log_output=False,
                allow_unsafe_werkzeug=True,
            ),
            daemon=True,
        )
        server.start()
        print(f"[WEB] Server started on http://0.0.0.0:{self.port}")
