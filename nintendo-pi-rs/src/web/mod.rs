pub mod state;

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{Html, IntoResponse, Json},
    routing::get,
    Router,
};
use futures::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc};
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
        .route("/ws", get(ws_handler))
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

/// WebSocket handler for real-time state updates and commands.
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<WebState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: Arc<WebState>) {
    let (mut sender, mut receiver) = socket.split();

    // Send initial state
    let initial_state = state.mitm_state.snapshot_json();
    let initial_macros = storage::list_macros(&state.macros_dir);
    let init_msg = serde_json::json!({
        "type": "init",
        "state": initial_state,
        "macros": initial_macros,
    });
    if let Err(e) = sender.send(Message::Text(init_msg.to_string())).await {
        debug!("[WEB] Failed to send init: {e}");
        return;
    }

    let mut state_rx = state.state_rx.subscribe();
    let cmd_tx = state.cmd_tx.clone();
    let macros_dir = state.macros_dir.clone();

    // Task to forward state broadcasts to the WebSocket
    let send_task = tokio::spawn(async move {
        while let Ok(msg) = state_rx.recv().await {
            if sender.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    // Task to receive commands from the WebSocket
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(val) => {
                    if let Some(cmd) = parse_web_command(&val, &macros_dir) {
                        if let Err(e) = cmd_tx.send(cmd).await {
                            error!("[WEB] Failed to send command: {e}");
                        }
                    }
                }
                Err(e) => {
                    warn!("[WEB] Invalid JSON from WebSocket: {e}");
                }
            },
            Ok(Message::Close(_)) => break,
            Err(e) => {
                debug!("[WEB] WebSocket error: {e}");
                break;
            }
            _ => {}
        }
    }

    send_task.abort();
    debug!("[WEB] WebSocket connection closed");
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
            Some(WebCommand::SelectSlot(slot))
        }
        "RENAME_MACRO" => {
            let data = val.get("data")?;
            let arr = data.as_array()?;
            if arr.len() >= 2 {
                let id = arr[0].as_u64()? as u32;
                let name = arr[1].as_str()?.to_string();
                Some(WebCommand::RenameMacro(id, name))
            } else {
                None
            }
        }
        "DELETE_MACRO" => {
            let id = val.get("data")?.as_u64()? as u32;
            Some(WebCommand::DeleteMacro(id))
        }
        _ => {
            warn!("[WEB] Unknown command: {cmd}");
            None
        }
    }
}
