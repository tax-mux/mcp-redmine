//! Legacy MCP HTTP+SSE transport (protocol 2024-11-05):
//! - GET  /sse                    → SSE stream; first event is `endpoint`
//! - POST /message?sessionId=...  → JSON-RPC; responses pushed on the SSE stream

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream;
use serde::Deserialize;
use serde_json::json;
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use uuid::Uuid;

use crate::handler::handle_request;
use crate::keys::KeyStore;
use crate::mcp::JsonRpcRequest;

type SessionMap = Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>;

pub const PROFILE_HEADER: &str = "x-redmine-profile";

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<KeyStore>>,
    pub sessions: SessionMap,
}

#[derive(Debug, Deserialize)]
pub struct MessageQuery {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/sse", get(sse_handler))
        .route("/message", post(message_handler))
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let store = state.store.lock().await;
    let profiles = store.profile_names();
    let sessions = state.sessions.lock().await.len();
    let body: Value = json!({
        "status": "ok",
        "service": "mcp-redmine",
        "version": env!("CARGO_PKG_VERSION"),
        "profiles_loaded": profiles.len(),
        "profile_names": profiles,
        "default_profile": store.default_profile(),
        "active_sse_sessions": sessions,
    });
    (StatusCode::OK, Json(body))
}

fn header_profile(headers: &HeaderMap) -> Option<String> {
    headers
        .get(PROFILE_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

async fn sse_handler(
    State(state): State<AppState>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let session_id = Uuid::new_v4().to_string();
    let (tx, rx) = mpsc::channel::<String>(64);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), tx);
    }

    let endpoint = format!("/message?sessionId={session_id}");
    tracing::info!(%session_id, "SSE session opened");

    let first = stream::once(async move {
        Ok::<Event, Infallible>(Event::default().event("endpoint").data(endpoint))
    });

    let rest = ReceiverStream::new(rx).map(|payload| {
        Ok::<Event, Infallible>(Event::default().event("message").data(payload))
    });

    Sse::new(first.chain(rest)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

async fn message_handler(
    State(state): State<AppState>,
    Query(query): Query<MessageQuery>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let tx = {
        let sessions = state.sessions.lock().await;
        sessions.get(&query.session_id).cloned()
    };

    let Some(tx) = tx else {
        return (StatusCode::NOT_FOUND, "unknown sessionId").into_response();
    };

    let req: JsonRpcRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid JSON-RPC: {e}")).into_response();
        }
    };

    let profile = header_profile(&headers);
    if let Some(response) =
        handle_request(req, state.store.clone(), profile.as_deref()).await
    {
        match serde_json::to_string(&response) {
            Ok(payload) => {
                if tx.send(payload).await.is_err() {
                    tracing::warn!(%query.session_id, "SSE session closed while delivering MCP response");
                    let mut sessions = state.sessions.lock().await;
                    sessions.remove(&query.session_id);
                    return (StatusCode::GONE, "SSE session closed").into_response();
                }
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("serialize error: {e}"),
                )
                    .into_response();
            }
        }
    }

    StatusCode::ACCEPTED.into_response()
}
