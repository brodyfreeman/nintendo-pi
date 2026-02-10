//! Nintendo Pi MITM Bridge - Rust implementation.
//!
//! USB controller input -> Bluetooth Pro Controller output with macro support.
//! Single binary with embedded web UI.

mod bt;
mod calibration;
mod combo;
mod input;
mod led;
mod macro_engine;
mod usb;
mod web;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use calibration::{auto_calibrate_centers, StickCalibrator, C_STICK_CAL, MAIN_STICK_CAL};
use combo::{ComboAction, ComboDetector};
use input::{build_bt_report, parse_hid_report};
use macro_engine::{player::MacroPlayer, recorder::MacroRecorder, storage};
use web::state::{MitmState, StateSnapshot, WebCommand};

#[derive(Parser)]
#[command(name = "nintendo-pi", about = "MITM bridge: USB controller -> BT Pro Controller")]
struct Args {
    /// Macros directory path
    #[arg(long, default_value = "/root/macros")]
    macros_dir: PathBuf,

    /// Web UI port
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Initialize tracing
    let filter = if args.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .init();

    info!("=== Nintendo Pi MITM Bridge (Rust) ===");
    info!("USB-in, Bluetooth-out");
    info!("Macros dir: {}", args.macros_dir.display());
    info!("Web UI port: {}", args.port);

    // Ensure macros directory exists
    std::fs::create_dir_all(&args.macros_dir).ok();

    // --- Web UI setup (start early so it's available during hardware init) ---
    let mitm_state = Arc::new(MitmState::new());
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<WebCommand>(32);
    let (state_broadcast, _) = broadcast::channel::<String>(16);

    // Spawn web server
    let web_state = mitm_state.clone();
    let web_broadcast = state_broadcast.clone();
    let web_macros_dir = args.macros_dir.clone();
    let web_port = args.port;
    let web_cmd_tx = cmd_tx;
    tokio::spawn(async move {
        if let Err(e) = web::start_server(web_port, web_state, web_cmd_tx, web_broadcast, web_macros_dir).await {
            error!("[WEB] Server error: {e}");
        }
    });

    // Give the web server a moment to bind
    tokio::time::sleep(Duration::from_millis(100)).await;

    // --- Bluetooth setup (one-time, retry until adapter is ready) ---
    // Order matters: agent first (for pairing), adapter config, SDP profile,
    // then device class LAST (D-Bus calls can reset the HCI class).
    let _dbus_conn = loop {
        match async {
            let conn = zbus::Connection::system().await?;
            bt::sdp::register_agent(&conn).await?;
            bt::sdp::configure_adapter(&conn).await?;
            bt::sdp::register_sdp_profile(&conn).await?;
            bt::sdp::set_device_class().await?;
            anyhow::Ok(conn)
        }
        .await
        {
            Ok(conn) => break conn,
            Err(e) => {
                warn!("[BT] Setup failed: {e} — retrying in 3s...");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    };

    // --- State emitter task (5Hz broadcast when changed) ---
    let emitter_state = mitm_state.clone();
    let emitter_broadcast = state_broadcast.clone();
    tokio::spawn(async move {
        loop {
            if let Some(snapshot) = emitter_state.pop_if_changed() {
                let msg = serde_json::json!({
                    "type": "state_update",
                    "state": snapshot,
                });
                let _ = emitter_broadcast.send(msg.to_string());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });

    // Shared flag: BT forwarding side sets this so USB processing knows BT status
    let bt_connected = Arc::new(AtomicBool::new(false));

    // === Hardware lifecycle loop ===
    // Outer loop handles USB controller disconnect/reconnect.
    // Inner loop handles BT (Switch) disconnect/reconnect.
    loop {
        // Drain stale web commands from previous session
        while cmd_rx.try_recv().is_ok() {}

        // --- Phase 0: USB Init (retry until controller is plugged in) ---
        mitm_state.update(StateSnapshot {
            macro_mode: false, recording: false, playing: false,
            current_slot: 0, slot_count: 0, current_macro_name: None,
            usb_connected: false, bt_connected: false,
        });
        loop {
            match usb::init::initialize_controller().await {
                Ok(()) => break,
                Err(e) => {
                    warn!("[USB] {e} — retrying in 5s...");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
        // USB controller found — update state
        mitm_state.update(StateSnapshot {
            macro_mode: false, recording: false, playing: false,
            current_slot: 0, slot_count: 0, current_macro_name: None,
            usb_connected: true, bt_connected: false,
        });

        // Wait for HID device to appear after init
        info!("[USB] Waiting for HID device to appear...");
        tokio::time::sleep(Duration::from_secs(2)).await;

        // --- Spawn HID reader thread ---
        let hid_rx = usb::hid::spawn_reader(2);

        // --- Auto-calibrate stick centers ---
        info!("[USB] Calibrating stick centers (don't touch the sticks)...");
        let mut cal_reports = Vec::with_capacity(20);
        for _ in 0..20 {
            match hid_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(report) => cal_reports.push(report),
                Err(_) => break,
            }
        }
        let (left_center, right_center) = auto_calibrate_centers(&cal_reports);
        info!(
            "[USB] Left stick center: ({}, {}), Right: ({}, {})",
            left_center.0, left_center.1, right_center.0, right_center.1
        );

        let main_cal = StickCalibrator::new(MAIN_STICK_CAL, 10.0);
        let c_cal = StickCalibrator::new(C_STICK_CAL, 10.0);

        // --- Spawn USB processing on a blocking thread ---
        let (report_tx, mut report_rx) = mpsc::channel::<[u8; 50]>(4);

        let usb_mitm_state = mitm_state.clone();
        let usb_state_broadcast = state_broadcast.clone();
        let usb_bt_connected = bt_connected.clone();
        let usb_macros_dir = args.macros_dir.clone();

        let usb_handle = tokio::task::spawn_blocking(move || {
            usb_processing_loop(
                hid_rx,
                cmd_rx,
                report_tx,
                usb_mitm_state,
                usb_state_broadcast,
                usb_bt_connected,
                usb_macros_dir,
                main_cal,
                c_cal,
                left_center,
                right_center,
            )
        });

        // --- BT connection loop (async, on main task) ---
        'bt_loop: loop {
            info!("[BT] Waiting for Switch to connect...");
            bt_connected.store(false, Ordering::Relaxed);
            mitm_state.update(StateSnapshot {
                macro_mode: false, recording: false, playing: false,
                current_slot: 0, slot_count: 0, current_macro_name: None,
                usb_connected: true, bt_connected: false,
            });

            // Wait for BT connection, but also check if USB has disconnected.
            // Important: accept_connection() must NOT be cancelled by a timer,
            // because dropping the future tears down the L2CAP listeners and
            // prevents the Switch from completing its connection.
            let accept_fut = bt::emulator::accept_connection();
            tokio::pin!(accept_fut);

            let mut bt_session = loop {
                tokio::select! {
                    result = &mut accept_fut => {
                        match result {
                            Ok(session) => break session,
                            Err(e) => {
                                error!("[BT] Connection error: {e}");
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                // Recreate accept future after an error
                                accept_fut.set(bt::emulator::accept_connection());
                            }
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {
                        if usb_handle.is_finished() {
                            warn!("[USB] Controller disconnected. Waiting for reconnection...");
                            mitm_state.update(StateSnapshot {
                                macro_mode: false, recording: false, playing: false,
                                current_slot: 0, slot_count: 0, current_macro_name: None,
                                usb_connected: false, bt_connected: false,
                            });
                            break 'bt_loop;
                        }
                        // Don't recreate accept_fut — keep the listeners alive
                    }
                }
            };

            // Run pairing
            if let Err(e) = bt::emulator::run_pairing(&mut bt_session).await {
                error!("[BT] Pairing error: {e}");
                continue;
            }

            info!("[BT] Connected to Switch!");
            bt_connected.store(true, Ordering::Relaxed);
            led::set_led(&led::LED_NORMAL);

            // --- BT forwarding loop ---
            let mut bt_timer: u8 = 0;
            loop {
                match report_rx.recv().await {
                    Some(mut report) => {
                        // Overwrite timer byte with the real BT timer
                        // Timer is at byte [2] (after 0xA1 header and report ID)
                        report[2] = bt_timer;
                        bt_timer = bt_timer.wrapping_add(1);

                        if let Err(e) = bt::emulator::send_input_report(&mut bt_session, &report).await {
                            warn!("[BT] Send error: {e}");
                            break; // BT disconnected
                        }

                        // Poll BT control channel for subcommands
                        match bt::emulator::poll_control(&mut bt_session, &mut bt_timer).await {
                            Ok(true) | Err(_) => break, // BT disconnected
                            _ => {}
                        }
                    }
                    None => {
                        // USB processing ended (sender dropped)
                        break 'bt_loop;
                    }
                }
            }

            // BT disconnected — continue bt_loop to wait for reconnection
            warn!("[BT] Switch disconnected. Waiting for reconnection...");
            bt_connected.store(false, Ordering::Relaxed);
            led::set_led(&led::LED_NORMAL);
        }

        // USB processing thread ended — get cmd_rx back for the next USB cycle
        bt_connected.store(false, Ordering::Relaxed);
        mitm_state.update(StateSnapshot {
            macro_mode: false, recording: false, playing: false,
            current_slot: 0, slot_count: 0, current_macro_name: None,
            usb_connected: false, bt_connected: false,
        });
        cmd_rx = usb_handle.await?;
    }
}

/// USB processing loop — runs on a blocking thread via `spawn_blocking`.
///
/// Reads HID reports, runs combo detection, macro recording/playback, and web
/// commands. Sends built BT reports over `report_tx`. Returns `cmd_rx` so it
/// can be reused across USB reconnection cycles.
#[allow(clippy::too_many_arguments)]
fn usb_processing_loop(
    hid_rx: std::sync::mpsc::Receiver<usb::hid::HidReport>,
    mut cmd_rx: mpsc::Receiver<WebCommand>,
    report_tx: mpsc::Sender<[u8; 50]>,
    mitm_state: Arc<MitmState>,
    state_broadcast: broadcast::Sender<String>,
    bt_connected: Arc<AtomicBool>,
    macros_dir: PathBuf,
    main_cal: StickCalibrator,
    c_cal: StickCalibrator,
    left_center: (u16, u16),
    right_center: (u16, u16),
) -> mpsc::Receiver<WebCommand> {
    let mut combo = ComboDetector::new();
    let mut recorder = MacroRecorder::new();
    let mut player = MacroPlayer::new();
    let mut current_slot: usize = 0;
    let mut cached_slot_count = storage::get_slot_count(&macros_dir);
    let mut cached_macro_name: Option<String> = None;
    let mut usb_check_counter: u32 = 0;

    let refresh_cache = |slot: usize, macros_dir: &std::path::Path| -> (usize, Option<String>) {
        let count = storage::get_slot_count(macros_dir);
        let name = storage::get_macro_id_by_slot(macros_dir, slot)
            .and_then(|id| storage::get_macro_info(macros_dir, id))
            .map(|e| e.name);
        (count, name)
    };

    let broadcast_macros = |broadcast: &broadcast::Sender<String>, macros_dir: &std::path::Path| {
        let macros = storage::list_macros(macros_dir);
        let msg = serde_json::json!({ "type": "macro_list", "macros": macros });
        let _ = broadcast.send(msg.to_string());
    };

    // Initial cache
    let (sc, mn) = refresh_cache(current_slot, &macros_dir);
    cached_slot_count = sc;
    cached_macro_name = mn;

    info!("[MITM] USB processing active.");

    loop {
        // --- Drain web command queue ---
        while let Ok(web_cmd) = cmd_rx.try_recv() {
            match web_cmd {
                WebCommand::ToggleMacroMode => {
                    combo.macro_mode = !combo.macro_mode;
                    if combo.macro_mode {
                        led::set_led(&led::LED_MACRO_MODE);
                        let (sc, mn) = refresh_cache(current_slot, &macros_dir);
                        cached_slot_count = sc;
                        cached_macro_name = mn;
                        info!("[WEB] Macro mode ON. {} macro(s). Slot: {}", cached_slot_count, current_slot);
                    } else {
                        if recorder.recording {
                            recorder.stop();
                            recorder.save(&macros_dir, None);
                            broadcast_macros(&state_broadcast, &macros_dir);
                        }
                        led::set_led(&led::LED_NORMAL);
                        info!("[WEB] Macro mode OFF.");
                    }
                }
                WebCommand::ToggleRecording => {
                    if recorder.recording {
                        recorder.stop();
                        recorder.save(&macros_dir, None);
                        led::set_led(&led::LED_MACRO_MODE);
                        broadcast_macros(&state_broadcast, &macros_dir);
                        let (sc, mn) = refresh_cache(current_slot, &macros_dir);
                        cached_slot_count = sc;
                        cached_macro_name = mn;
                    } else {
                        recorder.start();
                        led::set_led(&led::LED_RECORDING);
                    }
                }
                WebCommand::PrevSlot => {
                    if cached_slot_count > 0 {
                        current_slot = if current_slot == 0 { cached_slot_count - 1 } else { current_slot - 1 };
                        let (sc, mn) = refresh_cache(current_slot, &macros_dir);
                        cached_slot_count = sc;
                        cached_macro_name = mn;
                    }
                }
                WebCommand::NextSlot => {
                    if cached_slot_count > 0 {
                        current_slot = (current_slot + 1) % cached_slot_count;
                        let (sc, mn) = refresh_cache(current_slot, &macros_dir);
                        cached_slot_count = sc;
                        cached_macro_name = mn;
                    }
                }
                WebCommand::SelectSlot(slot) => {
                    if slot < cached_slot_count {
                        current_slot = slot;
                        let (sc, mn) = refresh_cache(current_slot, &macros_dir);
                        cached_slot_count = sc;
                        cached_macro_name = mn;
                    }
                }
                WebCommand::PlayMacro => {
                    if let Some(macro_id) = storage::get_macro_id_by_slot(&macros_dir, current_slot) {
                        if player.load(&macros_dir, macro_id) {
                            player.start(false);
                            led::set_led(&led::LED_PLAYBACK);
                        }
                    }
                }
                WebCommand::StopPlayback => {
                    if player.playing {
                        player.stop();
                        led::set_led(if combo.macro_mode { &led::LED_MACRO_MODE } else { &led::LED_NORMAL });
                    }
                }
                WebCommand::RenameMacro(id, name) => {
                    if storage::rename_macro(&macros_dir, id, &name) {
                        broadcast_macros(&state_broadcast, &macros_dir);
                        let (sc, mn) = refresh_cache(current_slot, &macros_dir);
                        cached_slot_count = sc;
                        cached_macro_name = mn;
                    }
                }
                WebCommand::DeleteMacro(id) => {
                    if storage::delete_macro(&macros_dir, id) {
                        broadcast_macros(&state_broadcast, &macros_dir);
                        let new_count = storage::get_slot_count(&macros_dir);
                        cached_slot_count = new_count;
                        if new_count == 0 {
                            current_slot = 0;
                        } else if current_slot >= new_count {
                            current_slot = new_count - 1;
                        }
                        let (sc, mn) = refresh_cache(current_slot, &macros_dir);
                        cached_slot_count = sc;
                        cached_macro_name = mn;
                    }
                }
            }
        }

        // --- Read HID report (non-blocking from channel) ---
        let raw_report = match hid_rx.recv_timeout(Duration::from_millis(8)) {
            Ok(report) => report,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Periodically check if USB device is still present (~every 2s)
                usb_check_counter += 1;
                if usb_check_counter >= 250 {
                    usb_check_counter = 0;
                    if !usb::init::is_device_present() {
                        return cmd_rx; // USB disconnected
                    }
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return cmd_rx; // USB disconnected
            }
        };

        // --- Macro playback override ---
        if player.playing {
            if let Some(macro_frame) = player.get_frame() {
                // Use macro frame for BT output
                let parsed = parse_hid_report(&macro_frame);
                let left_cal = calibrate_stick(&main_cal, parsed.left_stick_raw, left_center);
                let right_cal = calibrate_stick(&c_cal, parsed.right_stick_raw, right_center);
                // Build with timer=0; BT side overwrites with real timer
                let bt_report = build_bt_report(&parsed, left_cal, right_cal, 0);
                let _ = report_tx.try_send(bt_report);

                // Check for abort combo on live input
                let live_parsed = parse_hid_report(&raw_report);
                let (action, _) = combo.update(&live_parsed.buttons);
                if action == ComboAction::StopPlayback {
                    player.stop();
                    led::set_led(if combo.macro_mode { &led::LED_MACRO_MODE } else { &led::LED_NORMAL });
                }

                update_state(
                    &mitm_state, &combo, &recorder, &player,
                    current_slot, cached_slot_count, &cached_macro_name,
                    bt_connected.load(Ordering::Relaxed),
                );
                continue;
            } else {
                // Playback finished
                player.stop();
                led::set_led(if combo.macro_mode { &led::LED_MACRO_MODE } else { &led::LED_NORMAL });
                info!("[MACRO] Playback finished.");
            }
        }

        // --- Parse live input ---
        let mut parsed = parse_hid_report(&raw_report);

        // --- Combo detection ---
        let (action, suppressed) = combo.update(&parsed.buttons);

        // --- Handle combo actions ---
        match action {
            ComboAction::ToggleMacroMode => {
                combo.macro_mode = !combo.macro_mode;
                if combo.macro_mode {
                    led::set_led(&led::LED_MACRO_MODE);
                    let (sc, mn) = refresh_cache(current_slot, &macros_dir);
                    cached_slot_count = sc;
                    cached_macro_name = mn;
                    info!("[MACRO] Macro mode ON. {} macro(s). Slot: {}", cached_slot_count, current_slot);
                } else {
                    if recorder.recording {
                        recorder.stop();
                        recorder.save(&macros_dir, None);
                        broadcast_macros(&state_broadcast, &macros_dir);
                    }
                    led::set_led(&led::LED_NORMAL);
                    info!("[MACRO] Macro mode OFF.");
                }
            }
            ComboAction::ToggleRecording => {
                if recorder.recording {
                    recorder.stop();
                    recorder.save(&macros_dir, None);
                    led::set_led(&led::LED_MACRO_MODE);
                    broadcast_macros(&state_broadcast, &macros_dir);
                    let (sc, mn) = refresh_cache(current_slot, &macros_dir);
                    cached_slot_count = sc;
                    cached_macro_name = mn;
                } else {
                    recorder.start();
                    led::set_led(&led::LED_RECORDING);
                }
            }
            ComboAction::PrevSlot => {
                if cached_slot_count > 0 {
                    current_slot = if current_slot == 0 { cached_slot_count - 1 } else { current_slot - 1 };
                    let (sc, mn) = refresh_cache(current_slot, &macros_dir);
                    cached_slot_count = sc;
                    cached_macro_name = mn;
                    info!("[MACRO] Slot {} selected.", current_slot);
                }
            }
            ComboAction::NextSlot => {
                if cached_slot_count > 0 {
                    current_slot = (current_slot + 1) % cached_slot_count;
                    let (sc, mn) = refresh_cache(current_slot, &macros_dir);
                    cached_slot_count = sc;
                    cached_macro_name = mn;
                    info!("[MACRO] Slot {} selected.", current_slot);
                }
            }
            ComboAction::PlayMacro => {
                if let Some(macro_id) = storage::get_macro_id_by_slot(&macros_dir, current_slot) {
                    if player.load(&macros_dir, macro_id) {
                        player.start(false);
                        led::set_led(&led::LED_PLAYBACK);
                        info!("[MACRO] Playing macro {} (slot {}).", macro_id, current_slot);
                    }
                }
            }
            ComboAction::StopPlayback => {
                if player.playing {
                    player.stop();
                    led::set_led(if combo.macro_mode { &led::LED_MACRO_MODE } else { &led::LED_NORMAL });
                }
            }
            ComboAction::None => {}
        }

        // --- Filter suppressed buttons ---
        let mut filtered_report = raw_report;
        if !suppressed.is_empty() {
            suppressed.filter_buttons(&mut parsed.buttons);
            suppressed.filter_raw_report(&mut filtered_report);
        }

        // --- Record if active ---
        if recorder.recording {
            recorder.add_frame(&filtered_report);
        }

        // --- Build BT report and send to forwarding channel ---
        // Timer=0 placeholder; BT forwarding side overwrites with real timer
        let left_cal = calibrate_stick(&main_cal, parsed.left_stick_raw, left_center);
        let right_cal = calibrate_stick(&c_cal, parsed.right_stick_raw, right_center);
        let bt_report = build_bt_report(&parsed, left_cal, right_cal, 0);
        let _ = report_tx.try_send(bt_report);

        // --- Update web UI state ---
        update_state(
            &mitm_state, &combo, &recorder, &player,
            current_slot, cached_slot_count, &cached_macro_name,
            bt_connected.load(Ordering::Relaxed),
        );
    }
}

fn calibrate_stick(
    cal: &StickCalibrator,
    raw: (u16, u16),
    center: (u16, u16),
) -> (f64, f64) {
    let x_c = raw.0 as f64 - center.0 as f64;
    let y_c = raw.1 as f64 - center.1 as f64;
    let (x_cal, y_cal) = cal.calibrate(x_c, y_c);
    // Calibrator outputs ~[-2600, 2600] at full tilt — scale to [-100, 100]
    // matching Python: max(-100, min(100, int(cal * 100 / 2048)))
    (
        (x_cal * 100.0 / 2048.0).clamp(-100.0, 100.0),
        (y_cal * 100.0 / 2048.0).clamp(-100.0, 100.0),
    )
}

fn update_state(
    mitm_state: &MitmState,
    combo: &ComboDetector,
    recorder: &MacroRecorder,
    player: &MacroPlayer,
    current_slot: usize,
    slot_count: usize,
    macro_name: &Option<String>,
    bt_connected: bool,
) {
    mitm_state.update(StateSnapshot {
        macro_mode: combo.macro_mode,
        recording: recorder.recording,
        playing: player.playing,
        current_slot,
        slot_count,
        current_macro_name: macro_name.clone(),
        usb_connected: true,
        bt_connected,
    });
}
