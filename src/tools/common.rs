use serde_json::Value;
use std::collections::HashMap;

use crate::api_helpers::{flatten_rails_nested, ApiErrorContext};
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
        match v {
               // Nested objects/arrays become Rails bracket params (e.g. relation[issue_to_id]=...).
               // A JSON-string encoding of a nested value would 500 on the server side.
            Value::Object(_) | Value::Array(_) => flatten_rails_nested(v, Some(k), &mut map),
            other => {
                let s = match other {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Null => continue,
                        _ => continue,
                   };
                map.insert(k.clone(), s);
                 }
             }
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

/// Policy message for identity profile selection (tool-arg override / default fallback).
pub const PROFILE_POLICY_MSG: &str = "プロフィールをユーザーの許可なく変更することは原則的に禁止されています。通常プロフィール引数は不要です。接続ヘッダ X-Redmine-Profile を使ってください。default フォールバックは無効です。 / Changing the profile without the user's permission is principally forbidden. Normally omit the profile argument; use the X-Redmine-Profile connection header. The default fallback is disabled.";

/// Resolve the session identity profile.
///
/// - Tool argument `profile` is rejected (principally forbidden).
/// - Header `X-Redmine-Profile` is required.
/// - Resolved name `default` is rejected.
pub(crate) fn resolve_session_profile(
    args: &Value,
    header_profile: Option<&str>,
) -> Result<String, McpError> {
    let arg = args
        .get(PROFILE_PROP)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if arg.is_some() {
        return Err(McpError::InvalidArgs(PROFILE_POLICY_MSG.into()));
    }

    let header = header_profile
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| McpError::InvalidArgs(PROFILE_POLICY_MSG.into()))?;

    if header.eq_ignore_ascii_case("default") {
        return Err(McpError::InvalidArgs(PROFILE_POLICY_MSG.into()));
    }

    Ok(header.to_string())
}

#[cfg(test)]
mod resolve_session_profile_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn header_only_succeeds() {
        let p = resolve_session_profile(&json!({}), Some("cursor")).unwrap();
        assert_eq!(p, "cursor");
    }

    #[test]
    fn tool_arg_rejected_even_with_header() {
        let err = resolve_session_profile(&json!({"profile": "takahiro"}), Some("cursor")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("原則的に禁止") || msg.contains("principally forbidden"), "{msg}");
    }

    #[test]
    fn missing_header_rejected() {
        let err = resolve_session_profile(&json!({}), None).unwrap_err();
        assert!(matches!(err, McpError::InvalidArgs(_)));
    }

    #[test]
    fn default_header_rejected() {
        let err = resolve_session_profile(&json!({}), Some("default")).unwrap_err();
        assert!(matches!(err, McpError::InvalidArgs(_)));
    }

    #[test]
    fn explicit_default_arg_rejected() {
        let err = resolve_session_profile(&json!({"profile": "default"}), None).unwrap_err();
        assert!(matches!(err, McpError::InvalidArgs(_)));
    }
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

