use serde_json::Value;
use std::collections::HashMap;

use crate::api_helpers::ApiErrorContext;
use crate::error::{McpError, RedmineError};
use crate::keys::KeyStore;
use crate::redmine::RedmineClient;
use crate::secret::args_attempt_credential_injection;
use crate::tools::definitions::PROFILE_PROP;

pub(crate) async fn upload_attachment_files(client: &RedmineClient, paths: &[std::string::String]) -> Result<Vec<Value>, McpError> {
    let mut out = Vec::new();
    for p in paths {
        out.push(client.upload_attachment(p).await?);
    }
    Ok(out)
}

pub(crate) fn query_map(value: Option<&Value>) -> Option<HashMap<String, String>> {
    let obj = value?.as_object()?;
    let mut map = HashMap::new();
    for (k, v) in obj {
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            other => other.to_string(),
        };
        map.insert(k.clone(), s);
    }
    Some(map)
}

pub(crate) fn reject_credential_args(args: &Value) -> Result<(), McpError> {
    if args_attempt_credential_injection(args) {
        return Err(McpError::InvalidArgs(
            "Credential fields (api_key / REDMINE_API_KEY / password) must not be passed as tool arguments; the API key stays inside the container".into(),
        ));
    }
    Ok(())
}

pub(crate) fn resolve_profile_arg<'a>(args: &'a Value, header_profile: Option<&'a str>) -> Option<&'a str> {
    args.get(PROFILE_PROP)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| header_profile.map(str::trim).filter(|s| !s.is_empty()))
}

pub(crate) fn api_ctx(profile: Option<&str>, method: &str, path: &str) -> ApiErrorContext {
    ApiErrorContext {
        profile: profile.map(str::to_string),
        method: Some(method.to_string()),
        path: Some(path.to_string()),
    }
}

pub(crate) fn map_api_err(e: RedmineError, ctx: ApiErrorContext) -> McpError {
    match e {
        RedmineError::ApiError { status, body } => McpError::Api {
            status,
            body,
            ctx,
        },
        other => McpError::Redmine(other),
    }
}

pub(crate) async fn client_request(
    client: &RedmineClient,
    profile: Option<&str>,
    method: &str,
    path: &str,
    query: Option<&HashMap<String, String>>,
    body: Option<&Value>,
) -> Result<Value, McpError> {
    client
        .request(method, path, query, body)
        .await
        .map_err(|e| map_api_err(e, api_ctx(profile, method, path)))
}

pub(crate) async fn client_request_with_status(
    client: &RedmineClient,
    profile: Option<&str>,
    method: &str,
    path: &str,
    query: Option<&HashMap<String, String>>,
    body: Option<&Value>,
) -> Result<(u16, Value), McpError> {
    client
        .request_with_status(method, path, query, body)
        .await
        .map_err(|e| map_api_err(e, api_ctx(profile, method, path)))
}

pub fn safe_error_text(err: &McpError, store: &KeyStore) -> String {
    let structured = err.to_structured_json(&ApiErrorContext::default(), store);
    serde_json::to_string_pretty(&structured).unwrap_or_else(|_| store.redact_all(&err.to_string()))
}

