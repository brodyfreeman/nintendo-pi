#!/usr/bin/env python3
"""MITM bridge: USB controller input -> Bluetooth Pro Controller output.

Reads a Switch 2 Pro Controller over USB, detects secret combos for
macro recording/playback, and forwards inputs to a Nintendo Switch
via Bluetooth using NXBT.

Usage:
    sudo python3 mitm.py
"""
import queue
import sys
import time

import hid
import nxbt

from combo import ComboAction, ComboDetector
from enable_procon2 import (
    MAIN_STICK_CAL_STR,
    C_STICK_CAL_STR,
    ControllerInitializer,
    StickCalibrator,
    parse_hid_report,
    remap_trigger_value,
    unpack_12bit_triplet,
)
from macro import (
    MacroPlayer, MacroRecorder, delete_macro, get_macro_info,
    get_slot_count, get_macro_id_by_slot, list_macros, rename_macro,
)
from web_server import MitmState, WebCommand, WebServer

# Map our button names to NXBT DIRECT_INPUT_PACKET keys (flat dict, uppercase)
_BTN_TO_NXBT = {
    "A": "A",
    "B": "B",
    "X": "X",
    "Y": "Y",
    "L": "L",
    "R": "R",
    "ZL": "ZL",
    "ZR": "ZR",
    "PLUS": "PLUS",
    "MINUS": "MINUS",
    "HOME": "HOME",
    "CAPTURE": "CAPTURE",
    "DPAD_UP": "DPAD_UP",
    "DPAD_DOWN": "DPAD_DOWN",
    "DPAD_LEFT": "DPAD_LEFT",
    "DPAD_RIGHT": "DPAD_RIGHT",
    # L3/R3 are inside the stick sub-dicts as "PRESSED"
}

# LED command templates for feedback
# Player 1 pattern (normal): LED 1 on
LED_NORMAL = bytes(
    [0x09, 0x91, 0x00, 0x07, 0x00, 0x08, 0x00, 0x00,
     0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
)
# Recording pattern: all LEDs blinking
LED_RECORDING = bytes(
    [0x09, 0x91, 0x00, 0x07, 0x00, 0x08, 0x00, 0x00,
     0x0F, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
)
# Playback pattern: LEDs 1+3 on
LED_PLAYBACK = bytes(
    [0x09, 0x91, 0x00, 0x07, 0x00, 0x08, 0x00, 0x00,
     0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
)
# Macro mode pattern: LEDs 2+3 on
LED_MACRO_MODE = bytes(
    [0x09, 0x91, 0x00, 0x07, 0x00, 0x08, 0x00, 0x00,
     0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
)


def _send_led(initializer, pattern):
    """Send an LED command to the physical controller."""
    if initializer.usb_device and initializer.usb_endpoint_out:
        try:
            initializer.usb_device.write(
                initializer.usb_endpoint_out, pattern, timeout=100
            )
        except Exception:
            pass


def _apply_to_nxbt_packet(packet, parsed, main_cal, c_cal, left_center, right_center):
    """Map parsed HID report data into an NXBT input packet.

    Args:
        packet: NXBT input packet dict (mutated in place)
        parsed: dict from parse_hid_report()
        main_cal: StickCalibrator for left stick
        c_cal: StickCalibrator for right stick
        left_center: (x, y) resting center for left stick
        right_center: (x, y) resting center for right stick
    """
    # Buttons (flat keys in the packet dict)
    for our_name, nxbt_key in _BTN_TO_NXBT.items():
        packet[nxbt_key] = parsed["buttons"].get(our_name, False)

    # L3/R3 are "PRESSED" inside the stick sub-dicts
    packet["L_STICK"]["PRESSED"] = parsed["buttons"].get("L3", False)
    packet["R_STICK"]["PRESSED"] = parsed["buttons"].get("R3", False)

    # Left stick: calibrate and scale to -100..100
    # The calibrator outputs ~±2048 at full tilt (original code did *16 into ±32768)
    x1_raw, y1_raw = parsed["left_stick_raw"]
    x1_c, y1_c = x1_raw - left_center[0], y1_raw - left_center[1]
    x1_cal, y1_cal = main_cal.calibrate(x1_c, y1_c)
    packet["L_STICK"]["X_VALUE"] = max(-100, min(100, int(x1_cal * 100 / 2048)))
    packet["L_STICK"]["Y_VALUE"] = max(-100, min(100, int(y1_cal * 100 / 2048)))

    # Right stick
    x2_raw, y2_raw = parsed["right_stick_raw"]
    x2_c, y2_c = x2_raw - right_center[0], y2_raw - right_center[1]
    x2_cal, y2_cal = c_cal.calibrate(x2_c, y2_c)
    packet["R_STICK"]["X_VALUE"] = max(-100, min(100, int(x2_cal * 100 / 2048)))
    packet["R_STICK"]["Y_VALUE"] = max(-100, min(100, int(y2_cal * 100 / 2048)))


def main():
    print("=== Switch 2 Pro Controller MITM Bridge ===")
    print("USB-in, Bluetooth-out\n")

    # --- USB init ---
    initializer = ControllerInitializer()
    print("[USB] Initializing controller...")
    if not initializer.connect_and_initialize():
        sys.exit("Failed to initialize USB controller. Is it plugged in?")

    vendor_id = initializer.VENDOR_ID
    product_id = initializer.PRODUCT_ID

    # Keep USB device reference for LED commands
    usb_device_ref = initializer.usb_device
    usb_ep_out = initializer.usb_endpoint_out

    # Disconnect pyusb so hidapi can claim the device
    initializer.disconnect()

    print("[USB] Waiting for HID device to appear...")
    time.sleep(2)

    hid_device = hid.device()
    try:
        hid_device.open(vendor_id, product_id)
    except IOError as e:
        sys.exit(f"Could not open HID device: {e}")
    print("[USB] HID device connected.")

    # --- Auto-calibrate stick centers ---
    print("[USB] Calibrating stick centers (don't touch the sticks)...")
    lx_samples, ly_samples, rx_samples, ry_samples = [], [], [], []
    for _ in range(20):
        report = hid_device.read(64)
        if report:
            p = parse_hid_report(report)
            lx, ly = p["left_stick_raw"]
            rx, ry = p["right_stick_raw"]
            lx_samples.append(lx)
            ly_samples.append(ly)
            rx_samples.append(rx)
            ry_samples.append(ry)
    left_center = (sum(lx_samples) // len(lx_samples), sum(ly_samples) // len(ly_samples))
    right_center = (sum(rx_samples) // len(rx_samples), sum(ry_samples) // len(ry_samples))
    print(f"[USB] Left stick center: {left_center}, Right stick center: {right_center}\n")

    # --- Bluetooth init ---
    print("[BT] Starting NXBT...")
    nx = nxbt.Nxbt()
    controller_id = nx.create_controller(nxbt.PRO_CONTROLLER)
    print("[BT] Virtual Pro Controller created.")
    print("[BT] Waiting for Switch to connect...")
    print("[BT] >> Open 'Change Grip/Order' on the Switch, then press a button. <<")
    nx.wait_for_connection(controller_id)
    print("[BT] Connected to Switch!\n")

    # --- Setup ---
    main_cal = StickCalibrator(MAIN_STICK_CAL_STR)
    c_cal = StickCalibrator(C_STICK_CAL_STR)

    combo = ComboDetector()
    recorder = MacroRecorder()
    player = MacroPlayer()

    current_slot = 0
    packet = nx.create_input_packet()

    # --- Web UI ---
    cmd_queue = queue.Queue()
    mitm_state = MitmState()
    web = WebServer(cmd_queue, mitm_state, port=8080)
    web.start()

    # Cached macro metadata -- refreshed only when macros change
    cached_slot_count = get_slot_count()
    cached_macro_name = None

    def _refresh_macro_cache():
        """Re-read macro index from disk and update cached values."""
        nonlocal cached_slot_count, cached_macro_name
        cached_slot_count = get_slot_count()
        mid = get_macro_id_by_slot(current_slot)
        if mid is not None:
            info = get_macro_info(mid)
            cached_macro_name = info["name"] if info else None
        else:
            cached_macro_name = None

    def _refresh_web_macros():
        """Push updated macro list to all web clients."""
        _refresh_macro_cache()
        web.socketio.emit("macro_list", list_macros())

    print("[MITM] Passthrough active. Press Ctrl+C to exit.")
    print("[MITM] Secret combo: hold L3+R3+D-pad Down for 0.5s to toggle macro mode.\n")

    try:
        # Reconnect USB for LED commands (best effort)
        try:
            import usb.core
            led_dev = usb.core.find(
                idVendor=vendor_id, idProduct=product_id
            )
            if led_dev:
                led_ep_out = usb_ep_out
                initializer.usb_device = led_dev
                initializer.usb_endpoint_out = led_ep_out
        except Exception:
            pass

        while True:
            # --- Drain web command queue ---
            while True:
                try:
                    web_cmd, web_data = cmd_queue.get_nowait()
                except queue.Empty:
                    break

                if web_cmd == WebCommand.TOGGLE_MACRO_MODE:
                    combo.macro_mode = not combo.macro_mode
                    if combo.macro_mode:
                        _send_led(initializer, LED_MACRO_MODE)
                        _refresh_macro_cache()
                        print(f"[WEB] Macro mode ON. {cached_slot_count} macro(s) available. Slot: {current_slot}")
                    else:
                        if recorder.recording:
                            recorder.stop()
                            mid = recorder.save()
                            print(f"[WEB] Recording auto-saved as macro {mid}.")
                            _refresh_web_macros()
                        _send_led(initializer, LED_NORMAL)
                        print("[WEB] Macro mode OFF.")

                elif web_cmd == WebCommand.TOGGLE_RECORDING:
                    if recorder.recording:
                        frame_count, duration_us = recorder.stop()
                        mid = recorder.save()
                        _send_led(initializer, LED_MACRO_MODE)
                        print(f"[WEB] Recording stopped. {frame_count} frames, "
                              f"{duration_us // 1000}ms. Saved as macro {mid}.")
                        _refresh_web_macros()
                    else:
                        recorder.start()
                        _send_led(initializer, LED_RECORDING)
                        print("[WEB] Recording started...")

                elif web_cmd == WebCommand.PREV_SLOT:
                    if cached_slot_count > 0:
                        current_slot = (current_slot - 1) % cached_slot_count
                        _refresh_macro_cache()
                        print(f"[WEB] Slot {current_slot} selected.")

                elif web_cmd == WebCommand.NEXT_SLOT:
                    if cached_slot_count > 0:
                        current_slot = (current_slot + 1) % cached_slot_count
                        _refresh_macro_cache()
                        print(f"[WEB] Slot {current_slot} selected.")

                elif web_cmd == WebCommand.SELECT_SLOT:
                    if isinstance(web_data, int) and 0 <= web_data < cached_slot_count:
                        current_slot = web_data
                        _refresh_macro_cache()
                        print(f"[WEB] Slot {current_slot} selected.")

                elif web_cmd == WebCommand.PLAY_MACRO:
                    macro_id = get_macro_id_by_slot(current_slot)
                    if macro_id is not None:
                        if player.load(macro_id):
                            player.start(loop=False)
                            _send_led(initializer, LED_PLAYBACK)
                            print(f"[WEB] Playing macro {macro_id} (slot {current_slot})...")
                        else:
                            print(f"[WEB] Failed to load macro {macro_id}.")
                    else:
                        print("[WEB] No macro in current slot.")

                elif web_cmd == WebCommand.STOP_PLAYBACK:
                    if player.playing:
                        player.stop()
                        _send_led(initializer, LED_MACRO_MODE if combo.macro_mode else LED_NORMAL)
                        print("[WEB] Playback stopped.")

                elif web_cmd == WebCommand.RENAME_MACRO:
                    if isinstance(web_data, (list, tuple)) and len(web_data) == 2:
                        macro_id, new_name = web_data
                        if rename_macro(macro_id, new_name):
                            print(f"[WEB] Macro {macro_id} renamed to '{new_name}'.")
                            _refresh_web_macros()
                        else:
                            print(f"[WEB] Failed to rename macro {macro_id}.")

                elif web_cmd == WebCommand.DELETE_MACRO:
                    if isinstance(web_data, int):
                        if delete_macro(web_data):
                            print(f"[WEB] Macro {web_data} deleted.")
                            _refresh_web_macros()
                            if cached_slot_count == 0:
                                current_slot = 0
                            elif current_slot >= cached_slot_count:
                                current_slot = cached_slot_count - 1
                            _refresh_macro_cache()
                        else:
                            print(f"[WEB] Failed to delete macro {web_data}.")

            report = hid_device.read(64)
            if not report:
                print("\n[USB] Controller disconnected.")
                break

            raw_report = bytes(report)

            # --- Macro playback override ---
            if player.playing:
                macro_frame = player.get_frame()
                if macro_frame is not None:
                    # Use macro frame instead of live input
                    parsed = parse_hid_report(macro_frame)
                    _apply_to_nxbt_packet(packet, parsed, main_cal, c_cal, left_center, right_center)
                    nx.set_controller_input(controller_id, packet)

                    # Still check for abort combo on live input
                    live_parsed = parse_hid_report(raw_report)
                    action, _ = combo.update(live_parsed["buttons"])
                    if action == ComboAction.STOP_PLAYBACK:
                        player.stop()
                        _send_led(initializer, LED_MACRO_MODE if combo.macro_mode else LED_NORMAL)
                        print("[MACRO] Playback stopped.")
                    mitm_state.update(
                        macro_mode=combo.macro_mode, recording=recorder.recording,
                        playing=player.playing, current_slot=current_slot,
                        slot_count=cached_slot_count, current_macro_name=cached_macro_name,
                        connected=True,
                    )
                    continue
                else:
                    # Playback finished
                    player.stop()
                    _send_led(initializer, LED_MACRO_MODE if combo.macro_mode else LED_NORMAL)
                    print("[MACRO] Playback finished.")

            # --- Parse live input ---
            parsed = parse_hid_report(raw_report)

            # --- Combo detection ---
            action, suppressed = combo.update(parsed["buttons"])

            # --- Handle combo actions ---
            if action == ComboAction.TOGGLE_MACRO_MODE:
                combo.macro_mode = not combo.macro_mode
                if combo.macro_mode:
                    _send_led(initializer, LED_MACRO_MODE)
                    _refresh_macro_cache()
                    print(f"[MACRO] Macro mode ON. {cached_slot_count} macro(s) available. Slot: {current_slot}")
                else:
                    if recorder.recording:
                        recorder.stop()
                        mid = recorder.save()
                        print(f"[MACRO] Recording auto-saved as macro {mid}.")
                        _refresh_macro_cache()
                    _send_led(initializer, LED_NORMAL)
                    print("[MACRO] Macro mode OFF.")

            elif action == ComboAction.TOGGLE_RECORDING:
                if recorder.recording:
                    frame_count, duration_us = recorder.stop()
                    mid = recorder.save()
                    _send_led(initializer, LED_MACRO_MODE)
                    print(f"[MACRO] Recording stopped. {frame_count} frames, "
                          f"{duration_us // 1000}ms. Saved as macro {mid}.")
                    _refresh_macro_cache()
                else:
                    recorder.start()
                    _send_led(initializer, LED_RECORDING)
                    print("[MACRO] Recording started...")

            elif action == ComboAction.PREV_SLOT:
                if cached_slot_count > 0:
                    current_slot = (current_slot - 1) % cached_slot_count
                    _refresh_macro_cache()
                    print(f"[MACRO] Slot {current_slot} selected.")

            elif action == ComboAction.NEXT_SLOT:
                if cached_slot_count > 0:
                    current_slot = (current_slot + 1) % cached_slot_count
                    _refresh_macro_cache()
                    print(f"[MACRO] Slot {current_slot} selected.")

            elif action == ComboAction.PLAY_MACRO:
                macro_id = get_macro_id_by_slot(current_slot)
                if macro_id is not None:
                    if player.load(macro_id):
                        player.start(loop=False)
                        _send_led(initializer, LED_PLAYBACK)
                        print(f"[MACRO] Playing macro {macro_id} (slot {current_slot})...")
                    else:
                        print(f"[MACRO] Failed to load macro {macro_id}.")
                else:
                    print("[MACRO] No macro in current slot.")

            elif action == ComboAction.STOP_PLAYBACK:
                if player.playing:
                    player.stop()
                    _send_led(initializer, LED_MACRO_MODE if combo.macro_mode else LED_NORMAL)
                    print("[MACRO] Playback stopped.")

            # --- Filter suppressed buttons and forward ---
            if suppressed:
                filtered_buttons = combo.filter_buttons(parsed["buttons"], suppressed)
                parsed["buttons"] = filtered_buttons
                raw_report = combo.filter_raw_report(raw_report, suppressed)

            # --- Record if active ---
            if recorder.recording:
                recorder.add_frame(raw_report)

            # --- Send to Switch via NXBT ---
            _apply_to_nxbt_packet(packet, parsed, main_cal, c_cal, left_center, right_center)
            nx.set_controller_input(controller_id, packet)

            # --- Update web UI state ---
            mitm_state.update(
                macro_mode=combo.macro_mode,
                recording=recorder.recording,
                playing=player.playing,
                current_slot=current_slot,
                slot_count=cached_slot_count,
                current_macro_name=cached_macro_name,
                connected=True,
            )

    except KeyboardInterrupt:
        print("\n[MITM] Shutting down...")
    except Exception as e:
        print(f"\n[MITM] Error: {e}", file=sys.stderr)
        import traceback
        traceback.print_exc()
    finally:
        print("\n--- Cleaning Up ---")
        if recorder.recording:
            recorder.stop()
            mid = recorder.save()
            if mid:
                print(f"[MACRO] Emergency save: macro {mid}")
        player.close()
        hid_device.close()
        print("[USB] HID device closed.")
        try:
            nx.remove_controller(controller_id)
        except Exception:
            pass
        print("[BT] NXBT controller removed.")
        print("Done.")


if __name__ == "__main__":
    main()
