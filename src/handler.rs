use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::api_helpers::{ApiErrorContext, KnownProjectsConfig};
use crate::error::McpError;
use crate::keys::KeyStore;
use crate::mcp::{JsonRpcId, JsonRpcRequest, JsonRpcResponse};
use crate::tools::{all_tool_definitions, dispatch_tool, safe_error_text, PROFILE_POLICY_MSG};

pub fn initialize_result() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "mcp-redmine",
            "version": "0.2.0"
        },
        "instructions": PROFILE_POLICY_MSG
    })
}

pub async fn handle_request(
    req: JsonRpcRequest,
    store: Arc<Mutex<KeyStore>>,
    header_profile: Option<&str>,
    known_projects: Arc<KnownProjectsConfig>,
) -> Option<JsonRpcResponse> {
    if !req.is_valid_version() {
        let id = req.id.clone().unwrap_or(JsonRpcId::Null);
        return Some(JsonRpcResponse::err(id, -32600, "Invalid JSON-RPC version"));
    }

    if req.is_notification() {
        match req.method.as_str() {
            "notifications/initialized" => {
                tracing::info!("MCP client initialized");
            }
            "notifications/cancelled" => {
                tracing::debug!("Request cancelled");
            }
            other => {
                tracing::debug!("Ignoring notification: {other}");
            }
        }
        return None;
    }

    let id = req.id.clone().unwrap_or(JsonRpcId::Null);
    Some(match req.method.as_str() {
        "initialize" => JsonRpcResponse::ok(id, initialize_result()),
        "ping" => JsonRpcResponse::ok(id, json!({})),
        "tools/list" => JsonRpcResponse::ok(id, json!({ "tools": all_tool_definitions() })),
        "tools/call" => {
            handle_tool_call(id, req.params, store, header_profile, known_projects).await
        }
        _ => JsonRpcResponse::err(id, -32601, &format!("Method not found: {}", req.method)),
    })
}

async fn handle_tool_call(
    id: JsonRpcId,
    params: Value,
    store: Arc<Mutex<KeyStore>>,
    header_profile: Option<&str>,
    known_projects: Arc<KnownProjectsConfig>,
) -> JsonRpcResponse {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match dispatch_tool(
        tool_name,
        args,
        store.clone(),
        header_profile,
        known_projects,
    )
    .await
    {
        Ok(content) => json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&content).unwrap_or_default()
            }]
        }),
        Err(e) => {
            let guard = store.lock().await;
            let ctx = ApiErrorContext {
                profile: header_profile.map(str::to_string),
                ..Default::default()
            };
            let structured = e.to_structured_json(&ctx, &guard);
            json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&structured).unwrap_or_else(|_| safe_error_text(&e, &guard))
                }],
                "isError": true
            })
        }
    };

    JsonRpcResponse::ok(id, result)
}

/// Backward-compatible loader used by older call sites; prefer [`KeyStore::from_env`].
pub fn load_config() -> Result<(String, String), McpError> {
    let store = KeyStore::from_env()?;
    let key = store.resolve_key(None)?.to_string();
    let url = std::env::var("REDMINE_URL").map_err(|_| {
        McpError::Internal("REDMINE_URL is not set (inject via container env_file)".into())
    })?;
    Ok((url, key))
}
