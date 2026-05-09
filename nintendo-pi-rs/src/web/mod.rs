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

use self::state::MitmState;
use crate::macro_engine::command::MacroCommand;
use crate::macro_engine::storage::{self, MacroEntry};

/// Shared state for the web server.
pub struct WebState {
    pub mitm_state: Arc<MitmState>,
    pub cmd_tx: mpsc::Sender<MacroCommand>,
    pub state_rx: broadcast::Sender<String>,
    pub macros_dir: std::path::PathBuf,
}

/// Start the web server on the given port.
pub async fn start_server(
    port: u16,
    mitm_state: Arc<MitmState>,
    cmd_tx: mpsc::Sender<MacroCommand>,
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
    match parse_web_command(&val) {
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

fn parse_web_command(val: &serde_json::Value) -> Option<MacroCommand> {
    use crate::config::ConfigUpdate;

    let cmd = val.get("cmd")?.as_str()?;
    match cmd {
        "TOGGLE_RECORDING" => Some(MacroCommand::ToggleRecording),
        "PREV_SLOT" => Some(MacroCommand::PrevSlot),
        "NEXT_SLOT" => Some(MacroCommand::NextSlot),
        "PLAY_MACRO" => Some(MacroCommand::PlayMacro),
        "STOP_PLAYBACK" => Some(MacroCommand::StopPlayback),
        "CYCLE_SPEED" => Some(MacroCommand::CycleSpeed),
        "TOGGLE_LOOP" => Some(MacroCommand::ToggleLoop),
        "SELECT_SLOT" => {
            let slot = val.get("data")?.as_u64()? as usize;
            Some(MacroCommand::SelectSlot(slot))
        }
        "RENAME_MACRO" => {
            let data = val.get("data")?;
            let arr = data.as_array()?;
            if arr.len() >= 2 {
                let id = arr[0].as_u64()? as u32;
                let name = arr[1].as_str()?.to_string();
                Some(MacroCommand::RenameMacro(id, name))
            } else {
                warn!("[WEB] RENAME_MACRO: expected [id, name], got {data}");
                None
            }
        }
        "DELETE_MACRO" => {
            let id = val.get("data")?.as_u64()? as u32;
            Some(MacroCommand::DeleteMacro(id))
        }
        "SET_PLAYBACK_SPEED" => {
            let speed = val.get("data")?.as_f64()?;
            Some(MacroCommand::SetPlaybackSpeed(speed))
        }
        "SET_UI_UPDATE_INTERVAL" => Some(MacroCommand::UpdateConfig(ConfigUpdate {
            field_name: "ui_update_interval_ms".into(),
            value: val.get("data")?.clone(),
        })),
        // All config updates: strip "SET_" prefix, lowercase to field name
        _ if cmd.starts_with("SET_") => {
            let field_name = cmd[4..].to_ascii_lowercase();
            let value = val.get("data")?.clone();
            Some(MacroCommand::UpdateConfig(ConfigUpdate {
                field_name,
                value,
            }))
        }
        _ => {
            warn!("[WEB] Unknown command: {cmd}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_web_command_preserves_zero_config_value() {
        let val = serde_json::json!({
            "cmd": "SET_RECORDING_TRIM_END",
            "data": 0.0,
        });

        let Some(MacroCommand::UpdateConfig(update)) = parse_web_command(&val) else {
            panic!("expected config update");
        };

        assert_eq!(update.field_name, "recording_trim_end");
        assert_eq!(update.value, serde_json::json!(0.0));
    }

    #[test]
    fn test_parse_web_command_accepts_ui_interval_alias() {
        let val = serde_json::json!({
            "cmd": "SET_UI_UPDATE_INTERVAL",
            "data": 50,
        });

        let Some(MacroCommand::UpdateConfig(update)) = parse_web_command(&val) else {
            panic!("expected config update");
        };

        assert_eq!(update.field_name, "ui_update_interval_ms");
        assert_eq!(update.value, serde_json::json!(50));
    }
}
