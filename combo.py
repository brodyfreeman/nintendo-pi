"""Combo detection state machine for MITM macro device.

Detects button combinations (L3+R3+button) in the HID input stream
and suppresses those inputs from being forwarded to the Switch.
"""
from enum import Enum, auto


class ComboAction(Enum):
    NONE = auto()
    PREV_SLOT = auto()          # L3+R3+D-pad Left
    NEXT_SLOT = auto()          # L3+R3+D-pad Right
    PLAY_MACRO = auto()         # L3+R3+A
    STOP_PLAYBACK = auto()      # L3+R3+B


# Buttons that trigger instant actions when L3+R3 held (no hold required)
_INSTANT_COMBOS = {
    "DPAD_LEFT": ComboAction.PREV_SLOT,
    "DPAD_RIGHT": ComboAction.NEXT_SLOT,
    "A": ComboAction.PLAY_MACRO,
    "B": ComboAction.STOP_PLAYBACK,
}

class ComboDetector:
    """Detects L3+R3+button combos and reports which inputs to suppress."""

    def __init__(self):
        self._last_action = ComboAction.NONE
        # Track previous button states for edge detection
        self._prev_buttons = {}
        # Buttons currently being suppressed
        self._suppressed = set()

    def update(self, buttons):
        """Process a parsed button dict. Returns (action, suppressed_buttons).

        Args:
            buttons: dict mapping button name -> bool (from parse_hid_report)

        Returns:
            action: ComboAction indicating what combo was triggered (NONE if nothing)
            suppressed: set of button names that should NOT be forwarded to the Switch
        """
        l3 = buttons.get("L3", False)
        r3 = buttons.get("R3", False)
        base_held = l3 and r3

        action = ComboAction.NONE
        suppressed = set()

        if base_held:
            # Always suppress L3+R3 when both are held
            suppressed.add("L3")
            suppressed.add("R3")

            # Check instant combos (edge-triggered: only on button press, not hold)
            for btn_name, combo_action in _INSTANT_COMBOS.items():
                pressed = buttons.get(btn_name, False)
                was_pressed = self._prev_buttons.get(btn_name, False)
                if pressed:
                    suppressed.add(btn_name)
                if pressed and not was_pressed:
                    # Rising edge -- button just pressed
                    action = combo_action

        self._prev_buttons = dict(buttons)
        self._suppressed = suppressed
        return action, suppressed

    def filter_buttons(self, buttons, suppressed):
        """Return a copy of buttons with suppressed buttons forced to False."""
        filtered = dict(buttons)
        for name in suppressed:
            filtered[name] = False
        return filtered

    def filter_raw_report(self, report, suppressed):
        """Return a copy of the raw 64-byte HID report with suppressed buttons zeroed out.

        This patches the raw button bytes so recorded macros and BT output
        don't contain the combo buttons.
        """
        if not suppressed:
            return bytes(report)

        # Button name -> (byte_index_in_buttons_field, bitmask)
        _BTN_POSITIONS = {
            "B": (0, 0x01), "A": (0, 0x02), "Y": (0, 0x04), "X": (0, 0x08),
            "R": (0, 0x10), "ZR": (0, 0x20), "PLUS": (0, 0x40), "R3": (0, 0x80),
            "DPAD_DOWN": (1, 0x01), "DPAD_RIGHT": (1, 0x02), "DPAD_LEFT": (1, 0x04),
            "DPAD_UP": (1, 0x08), "L": (1, 0x10), "ZL": (1, 0x20), "MINUS": (1, 0x40),
            "L3": (1, 0x80),
            "HOME": (2, 0x01), "CAPTURE": (2, 0x02), "THUMB2": (2, 0x04),
            "THUMB": (2, 0x08), "Z": (2, 0x10),
        }

        data = bytearray(report)
        # Button bytes are at payload offset 0x2-0x4, which is report[3:6]
        # (report[0] is the report ID, payload = report[1:])
        btn_base = 3  # report[3] = buttons[0]

        for name in suppressed:
            if name in _BTN_POSITIONS:
                byte_idx, mask = _BTN_POSITIONS[name]
                data[btn_base + byte_idx] &= ~mask

        return bytes(data)
