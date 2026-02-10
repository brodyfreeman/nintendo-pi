pub mod state;

use std::{convert::Infallible, sync::Arc};

use axum::{
    extract::State,
    response::{
        sse::{Event, Sse},
        Html, Json,
    },
    routing::{get, post},
    Router,
};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::{wrappers::BroadcastStream, Stream, StreamExt};
use tracing::{debug, error, info, warn};

use self::state::{MitmState, WebCommand};
use crate::macro_engine::storage::{self, MacroEntry};

/// Shared state for the web server.
pub struct WebState {
    pub mitm_state: Arc<MitmState>,
    pub cmd_tx: mpsc::Sender<WebCommand>,
    pub state_rx: broadcast::Sender<String>,
    pub macros_dir: std::path::PathBuf,
}

/// Start the web server on the given port.
pub async fn start_server(
    port: u16,
    mitm_state: Arc<MitmState>,
    cmd_tx: mpsc::Sender<WebCommand>,
    state_broadcast: broadcast::Sender<String>,
    macros_dir: std::path::PathBuf,
) -> anyhow::Result<()> {
    let shared = Arc::new(WebState {
        mitm_state,
        cmd_tx,
        state_rx: state_broadcast,
        macros_dir,
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/api/state", get(api_state))
        .route("/api/macros", get(api_macros))
        .route("/events", get(sse_handler))
        .route("/api/cmd", post(cmd_handler))
        .with_state(shared);

    let addr = format!("0.0.0.0:{port}");
    info!("[WEB] Server starting on http://{addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Serve the embedded index.html.
async fn index_handler() -> Html<&'static str> {
    Html(include_str!("../../static/index.html"))
}

/// GET /api/state
async fn api_state(State(state): State<Arc<WebState>>) -> Json<serde_json::Value> {
    Json(state.mitm_state.snapshot_json())
}

/// GET /api/macros
async fn api_macros(State(state): State<Arc<WebState>>) -> Json<Vec<MacroEntry>> {
    Json(storage::list_macros(&state.macros_dir))
}

/// GET /events — SSE endpoint for real-time state updates.
async fn sse_handler(
    State(state): State<Arc<WebState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let initial_state = state.mitm_state.snapshot_json();
    let initial_macros = storage::list_macros(&state.macros_dir);
    let init_msg = serde_json::json!({
        "type": "init",
        "state": initial_state,
        "macros": initial_macros,
    });

    info!(
        "[WEB] SSE client connected (macros: {})",
        initial_macros.len()
    );

    let rx = state.state_rx.subscribe();
    let broadcast_stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(msg) => Some(Ok(Event::default().data(msg))),
        Err(e) => {
            debug!("[WEB] SSE broadcast lag, dropping message: {e}");
            None
        }
    });

    let init_event = tokio_stream::once(Ok(Event::default().data(init_msg.to_string())));

    Sse::new(init_event.chain(broadcast_stream))
}

/// POST /api/cmd — receive commands from the web UI.
async fn cmd_handler(
    State(state): State<Arc<WebState>>,
    axum::Json(val): axum::Json<serde_json::Value>,
) -> axum::http::StatusCode {
    match parse_web_command(&val, &state.macros_dir) {
        Some(cmd) => {
            debug!("[WEB] Command received: {cmd:?}");
            if let Err(e) = state.cmd_tx.send(cmd).await {
                error!("[WEB] Failed to send command: {e}");
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            } else {
                axum::http::StatusCode::OK
            }
        }
        None => {
            warn!("[WEB] Invalid command payload: {val}");
            axum::http::StatusCode::BAD_REQUEST
        }
    }
}

fn parse_web_command(val: &serde_json::Value, _macros_dir: &std::path::Path) -> Option<WebCommand> {
    let cmd = val.get("cmd")?.as_str()?;
    match cmd {
        "TOGGLE_MACRO_MODE" => Some(WebCommand::ToggleMacroMode),
        "TOGGLE_RECORDING" => Some(WebCommand::ToggleRecording),
        "PREV_SLOT" => Some(WebCommand::PrevSlot),
        "NEXT_SLOT" => Some(WebCommand::NextSlot),
        "PLAY_MACRO" => Some(WebCommand::PlayMacro),
        "STOP_PLAYBACK" => Some(WebCommand::StopPlayback),
        "SELECT_SLOT" => {
            let slot = val.get("data")?.as_u64()? as usize;
            debug!("[WEB] Select slot {slot}");
            Some(WebCommand::SelectSlot(slot))
        }
        "RENAME_MACRO" => {
            let data = val.get("data")?;
            let arr = data.as_array()?;
            if arr.len() >= 2 {
                let id = arr[0].as_u64()? as u32;
                let name = arr[1].as_str()?.to_string();
                debug!("[WEB] Rename macro {id} -> \"{name}\"");
                Some(WebCommand::RenameMacro(id, name))
            } else {
                warn!("[WEB] RENAME_MACRO: expected [id, name], got {data}");
                None
            }
        }
        "DELETE_MACRO" => {
            let id = val.get("data")?.as_u64()? as u32;
            debug!("[WEB] Delete macro {id}");
            Some(WebCommand::DeleteMacro(id))
        }
        "CYCLE_SPEED" => Some(WebCommand::CycleSpeed),
        "TOGGLE_LOOP" => Some(WebCommand::ToggleLoop),
        "SET_PLAYBACK_SPEED" => {
            let speed = val.get("data")?.as_f64()?;
            debug!("[WEB] Set playback speed: {speed}x");
            Some(WebCommand::SetPlaybackSpeed(speed))
        }
        "SET_STICK_DEADZONE" => {
            let dz = val.get("data")?.as_f64()?;
            debug!("[WEB] Set stick deadzone: {dz}");
            Some(WebCommand::SetStickDeadzone(dz))
        }
        "SET_COMBO_HOLD_TIME" => {
            let t = val.get("data")?.as_f64()?;
            debug!("[WEB] Set combo hold time: {t}s");
            Some(WebCommand::SetComboHoldTime(t))
        }
        "SET_AUTO_LOOP_DEFAULT" => {
            let v = val.get("data")?.as_bool()?;
            debug!("[WEB] Set auto-loop default: {v}");
            Some(WebCommand::SetAutoLoopDefault(v))
        }
        "SET_PLAYBACK_SPEED_DEFAULT" => {
            let s = val.get("data")?.as_f64()?;
            debug!("[WEB] Set playback speed default: {s}x");
            Some(WebCommand::SetPlaybackSpeedDefault(s))
        }
        "SET_PLAYBACK_START_DELAY" => {
            let d = val.get("data")?.as_f64()?;
            debug!("[WEB] Set playback start delay: {d}s");
            Some(WebCommand::SetPlaybackStartDelay(d))
        }
        "SET_LOOP_RESTART_DELAY" => {
            let d = val.get("data")?.as_f64()?;
            debug!("[WEB] Set loop restart delay: {d}s");
            Some(WebCommand::SetLoopRestartDelay(d))
        }
        "SET_RECORDING_TRIM_END" => {
            let d = val.get("data")?.as_f64()?;
            debug!("[WEB] Set recording trim end: {d}s");
            Some(WebCommand::SetRecordingTrimEnd(d))
        }
        "SET_RECORDING_START_DELAY" => {
            let d = val.get("data")?.as_f64()?;
            debug!("[WEB] Set recording start delay: {d}s");
            Some(WebCommand::SetRecordingStartDelay(d))
        }
        "SET_UI_UPDATE_INTERVAL" => {
            let ms = val.get("data")?.as_u64()?;
            debug!("[WEB] Set UI update interval: {ms}ms");
            Some(WebCommand::SetUiUpdateInterval(ms))
        }
        "SET_CALIBRATION_SAMPLES" => {
            let n = val.get("data")?.as_u64()? as u32;
            debug!("[WEB] Set calibration samples: {n}");
            Some(WebCommand::SetCalibrationSamples(n))
        }
        "SET_PLAY_MACRO_BUTTON" => {
            let s = val.get("data")?.as_str()?.to_string();
            debug!("[WEB] Set play macro button: {s}");
            Some(WebCommand::SetPlayMacroButton(s))
        }
        "SET_STOP_PLAYBACK_BUTTON" => {
            let s = val.get("data")?.as_str()?.to_string();
            debug!("[WEB] Set stop playback button: {s}");
            Some(WebCommand::SetStopPlaybackButton(s))
        }
        "SET_TOGGLE_MACRO_MODE_BUTTON" => {
            let s = val.get("data")?.as_str()?.to_string();
            debug!("[WEB] Set toggle macro mode button: {s}");
            Some(WebCommand::SetToggleMacroModeButton(s))
        }
        "SET_TOGGLE_LOOP_BUTTON" => {
            let s = val.get("data")?.as_str()?.to_string();
            debug!("[WEB] Set toggle loop button: {s}");
            Some(WebCommand::SetToggleLoopButton(s))
        }
        "SET_CYCLE_SPEED_BUTTON" => {
            let s = val.get("data")?.as_str()?.to_string();
            debug!("[WEB] Set cycle speed button: {s}");
            Some(WebCommand::SetCycleSpeedButton(s))
        }
        "SET_PREV_SLOT_BUTTON" => {
            let s = val.get("data")?.as_str()?.to_string();
            debug!("[WEB] Set prev slot button: {s}");
            Some(WebCommand::SetPrevSlotButton(s))
        }
        "SET_NEXT_SLOT_BUTTON" => {
            let s = val.get("data")?.as_str()?.to_string();
            debug!("[WEB] Set next slot button: {s}");
            Some(WebCommand::SetNextSlotButton(s))
        }
        "SET_TOGGLE_RECORDING_BUTTON" => {
            let s = val.get("data")?.as_str()?.to_string();
            debug!("[WEB] Set toggle recording button: {s}");
            Some(WebCommand::SetToggleRecordingButton(s))
        }
        _ => {
            warn!("[WEB] Unknown command: {cmd}");
            None
        }
    }
}
