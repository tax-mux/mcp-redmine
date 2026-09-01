use serde_json::{json, Value};

use crate::error::McpError;

/// Static fallback when Reporter profile gets an empty `/projects.json`.
pub fn known_projects_fallback() -> Value {
    json!({
        "projects": [
            {"id": 1, "identifier": "macbook-pro-workenv", "name": "macbook pro 作業環境"},
            {"id": 2, "identifier": "brave-search-mcp", "name": "brave-search-mcp"},
            {"id": 4, "identifier": "harness-seed", "name": "HarnessSeed"},
            {"id": 7, "identifier": "terminal-emulator", "name": "terminal-emulator"},
            {"id": 9, "identifier": "mcp-redmine", "name": "mcp-redmine"},
            {"id": 10, "identifier": "telospvl", "name": "TelosPVL"}
        ],
        "total_count": 6,
        "_fallback": true,
        "_hint": "Reporter profile returned no projects; showing operator-configured known projects. Verify membership in Redmine or use a profile with broader access."
    })
}

/// Reject filesystem / config paths mistakenly passed to `redmine_api_request`.
pub fn validate_redmine_path(path: &str) -> Result<String, McpError> {
    let path = path.split('?').next().unwrap_or(path).trim();
    if path.is_empty() {
        return Err(McpError::InvalidArgs("path must not be empty".into()));
    }
    if path.starts_with("http://") || path.starts_with("https://") {
        return Err(McpError::InvalidArgs(
            "absolute URLs are not allowed; use a Redmine REST path such as /issues.json".into(),
        ));
    }
    if !path.starts_with('/') {
        return Err(McpError::InvalidArgs(
            "path must start with / (Redmine REST path, e.g. /issues.json)".into(),
        ));
    }
    if path.contains("..") {
        return Err(McpError::InvalidArgs("path must not contain ..".into()));
    }

    const BLOCKED: &[&str] = &[
        "/home/",
        "/.config",
        "/.cursor",
        "/.hermes",
        "/opencode",
        "mcp.json",
        "openclaw.json",
        "trajectory",
        "provider_models_cache",
    ];
    for needle in BLOCKED {
        if path.contains(needle) {
            return Err(McpError::InvalidArgs(format!(
                "path `{path}` is not a Redmine REST endpoint (blocked pattern `{needle}`). Use paths like /issues.json or /projects/example.json"
            )));
        }
    }

    let looks_like_rest = path.ends_with(".json")
        || path.contains("/wiki/")
        || path.starts_with("/issues")
        || path.starts_with("/projects")
        || path.starts_with("/users")
        || path.starts_with("/trackers")
        || path.starts_with("/issue_statuses")
        || path.starts_with("/enumerations");
    if !looks_like_rest {
        return Err(McpError::InvalidArgs(format!(
            "path `{path}` does not look like a Redmine REST path. Examples: /issues.json, /projects.json, /issues/42.json"
        )));
    }

    Ok(path.to_string())
}

fn is_issue_update_path(path: &str) -> bool {
    let bare = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    if bare == "/issues.json" || bare == "issues.json" {
        return false;
    }
    if bare.contains("/relations") {
        return false;
    }
    bare.starts_with("/issues/") && bare.ends_with(".json")
}

fn parse_issue_object(value: &Value) -> Option<serde_json::Map<String, Value>> {
    match value {
        Value::Object(map) => Some(map.clone()),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|v| v.as_object().cloned()),
        _ => None,
    }
}

fn wrap_flat_issue_fields(body: &Value) -> Option<Value> {
    if body.get("issue").is_some() {
        if let Some(issue) = body.get("issue") {
            if issue.is_string() {
                if let Some(obj) = parse_issue_object(issue) {
                    return Some(json!({ "issue": Value::Object(obj) }));
                }
            }
        }
        return Some(body.clone());
    }
    if body.is_object()
        && (body.get("subject").is_some()
            || body.get("project_id").is_some()
            || body.get("tracker_id").is_some()
            || body.get("notes").is_some()
            || body.get("status_id").is_some()
            || body.get("done_ratio").is_some())
    {
        return Some(json!({ "issue": body }));
    }
    Some(body.clone())
}

/// Wrap flat issue fields for POST /issues.json when the agent omitted `{issue: ...}`.
pub fn normalize_issue_post_body(path: &str, method: &str, body: Option<&Value>) -> Option<Value> {
    if !method.eq_ignore_ascii_case("POST") {
        return body.cloned();
    }
    let bare = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    if bare != "/issues.json" && bare != "issues.json" {
        return body.cloned();
    }
    let Some(body) = body else {
        return None;
    };
    wrap_flat_issue_fields(body)
}

/// Normalize issue writes for `redmine_api_request`.
///
/// Agents sometimes put `{issue:...}` in `query`, which Redmine treats as a String and
/// crashes with TypeError (HTTP 500). Move `query.issue` into JSON body and wrap flat fields.
pub fn normalize_issue_api_request(
    path: &str,
    method: &str,
    body: Option<&Value>,
    query: &mut std::collections::HashMap<String, String>,
) -> Option<Value> {
    let method_up = method.to_ascii_uppercase();

    if method_up == "POST" {
        return normalize_issue_post_body(path, method, body);
    }

    if method_up != "PUT" && method_up != "PATCH" {
        return body.cloned();
    }
    if !is_issue_update_path(path) {
        return body.cloned();
    }

    let mut issue_obj = serde_json::Map::new();

    if let Some(issue_raw) = query.remove("issue") {
        if let Ok(parsed) = serde_json::from_str::<Value>(&issue_raw) {
            if let Some(map) = parsed.as_object() {
                for (k, v) in map {
                    issue_obj.insert(k.clone(), v.clone());
                }
            }
        }
    }

    if let Some(b) = body {
        if let Some(issue) = b.get("issue") {
            if let Some(map) = parse_issue_object(issue) {
                for (k, v) in map {
                    issue_obj.insert(k, v);
                }
            }
        } else if let Some(map) = b.as_object() {
            for (k, v) in map {
                issue_obj.insert(k.clone(), v.clone());
            }
        }
    }

    if issue_obj.is_empty() {
        return body.cloned();
    }
    Some(json!({ "issue": Value::Object(issue_obj) }))
}

#[derive(Debug, Clone, Default)]
pub struct ApiErrorContext {
    pub profile: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
}

pub fn structured_api_error(
    status: u16,
    body: &str,
    ctx: &ApiErrorContext,
) -> Value {
    let redmine_errors = parse_redmine_errors(body);
    let kind = match status {
        403 => "forbidden",
        404 => "not_found",
        422 => "validation_failed",
        500..=599 => "server_error",
        _ => "api_error",
    };

    let hint = hint_for_status(status, &redmine_errors, ctx);

    json!({
        "error": kind,
        "status": status,
        "profile": ctx.profile,
        "method": ctx.method,
        "path": ctx.path,
        "redmine_errors": redmine_errors,
        "body": if body.trim().is_empty() { Value::Null } else { Value::String(body.to_string()) },
        "hint": hint,
    })
}

fn parse_redmine_errors(body: &str) -> Vec<String> {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        if let Some(arr) = v.get("errors").and_then(|e| e.as_array()) {
            return arr
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
        }
    }
    if body.trim().is_empty() {
        return vec![];
    }
    vec![body.to_string()]
}

fn hint_for_status(status: u16, errors: &[String], ctx: &ApiErrorContext) -> String {
    let path = ctx.path.as_deref().unwrap_or("");
    let method = ctx.method.as_deref().unwrap_or("");
    let profile = ctx.profile.as_deref().unwrap_or("default");

    match status {
        403 if path.contains("/projects.json") && method.eq_ignore_ascii_case("GET") => {
            format!(
                "Profile `{profile}` cannot list projects (Reporter role often returns empty). Use redmine_projects action=list (includes known-project fallback for openclaw), or redmine_issues action=list with query project_id=<id>."
            )
        }
        403 if path.contains("/projects.json") && method.eq_ignore_ascii_case("POST") => {
            "Creating projects requires admin or 'manage projects' permission. Ask the operator to create the project in Redmine UI, or use an admin profile.".into()
        }
        403 if path.contains("/issues/") => {
            format!(
                "Profile `{profile}` lacks permission for {method} {path}. Check project membership or use a profile with issue access."
            )
        }
        403 => format!(
            "Profile `{profile}` forbidden for {method} {path}. Try redmine_current_user to inspect capabilities; do not call redmine_provision_user in a loop."
        ),
        422 if errors.iter().any(|e| e.contains("題名")) => {
            "Validation failed: subject missing. Use redmine_issues action=create with flat args (subject, project_id, tracker_id, status_id, description). Do not POST /issues.json without wrapping fields in {issue:{...}} unless using auto-wrap.".into()
        }
        422 if errors.iter().any(|e| e.contains("プロジェクト")) => {
            "Validation failed: project missing. Pass project_id (numeric id or identifier string) to redmine_issues action=create.".into()
        }
        422 => format!(
            "Redmine rejected the request ({method} {path}). Prefer redmine_issues action=create/update with flat args. Errors: {}",
            errors.join("; ")
        ),
        404 => format!("Not found: {method} {path}. Verify issue/project id exists."),
        500..=599 => format!(
            "Redmine server error on {method} {path}. Retry once; if persistent, check Redmine logs."
        ),
        _ => format!("Redmine API error status={status} on {method} {path}."),
    }
}

/// Build `{issue: {...}}` body from flat tool arguments (create = required core fields, update = partial).
pub fn issue_body_from_args(args: &Value, for_create: bool) -> Result<Value, McpError> {
    if for_create {
        let project_id = args
            .get("project_id")
            .ok_or_else(|| McpError::InvalidArgs("project_id is required for create".into()))?;
        let tracker_id = args
            .get("tracker_id")
            .ok_or_else(|| McpError::InvalidArgs("tracker_id is required for create".into()))?;
        let subject = args
            .get("subject")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidArgs("subject is required for create".into()))?;
        let description = args
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidArgs("description is required for create".into()))?;
        let status_id = args.get("status_id").cloned().unwrap_or(json!(1));

        let mut issue = json!({
            "project_id": project_id,
            "tracker_id": tracker_id,
            "status_id": status_id,
            "subject": subject,
            "description": description,
        });
        merge_optional_issue_fields(&mut issue, args);
        return Ok(json!({ "issue": issue }));
    }

    let mut issue = json!({});
    let map = issue.as_object_mut().unwrap();
    for key in [
        "project_id",
        "tracker_id",
        "status_id",
        "subject",
        "description",
        "priority_id",
        "assigned_to_id",
        "parent_id",
        "due_date",
        "estimated_hours",
        "done_ratio",
        "is_private",
        "notes",
    ] {
        if let Some(v) = args.get(key) {
            map.insert(key.to_string(), v.clone());
        }
    }
    if map.is_empty() {
        return Err(McpError::InvalidArgs(
            "update requires at least one field to change (status_id, subject, description, notes, etc.)".into(),
        ));
    }
    Ok(json!({ "issue": issue }))
}

fn merge_optional_issue_fields(issue: &mut Value, args: &Value) {
    let Some(map) = issue.as_object_mut() else {
        return;
    };
    for key in [
        "priority_id",
        "assigned_to_id",
        "parent_id",
        "due_date",
        "estimated_hours",
        "done_ratio",
        "is_private",
        "notes",
    ] {
        if let Some(v) = args.get(key) {
            map.insert(key.to_string(), v.clone());
        }
    }
}

pub fn projects_list_needs_fallback(value: &Value, profile: Option<&str>) -> bool {
    let is_openclaw = profile
        .map(|p| p.eq_ignore_ascii_case("openclaw"))
        .unwrap_or(false);
    if !is_openclaw {
        return false;
    }
    value
        .get("projects")
        .and_then(|p| p.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_filesystem_paths() {
        assert!(validate_redmine_path("/home/node/.openclaw/openclaw.json").is_err());
        assert!(validate_redmine_path("/.config/opencode/opencode.json").is_err());
        assert!(validate_redmine_path("/issues.json").is_ok());
    }

    #[test]
    fn wraps_flat_issue_post_body() {
        let body = json!({"subject": "x", "project_id": "1", "tracker_id": 2});
        let out = normalize_issue_post_body("/issues.json", "POST", Some(&body)).unwrap();
        assert!(out.get("issue").is_some());
        assert_eq!(out["issue"]["subject"], "x");
    }

    #[test]
    fn keeps_existing_issue_wrapper() {
        let body = json!({"issue": {"subject": "x"}});
        let out = normalize_issue_post_body("/issues.json", "POST", Some(&body)).unwrap();
        assert_eq!(out, body);
    }

    #[test]
    fn moves_query_issue_into_put_body() {
        use std::collections::HashMap;
        let mut q = HashMap::new();
        q.insert(
            "issue".to_string(),
            r#"{"notes":"done","status_id":3,"done_ratio":100}"#.to_string(),
        );
        let out = normalize_issue_api_request("/issues/295.json", "PUT", None, &mut q).unwrap();
        assert!(q.get("issue").is_none());
        assert_eq!(out["issue"]["notes"], "done");
        assert_eq!(out["issue"]["status_id"], 3);
        assert_eq!(out["issue"]["done_ratio"], 100);
    }

    #[test]
    fn wraps_flat_put_body() {
        use std::collections::HashMap;
        let mut q = HashMap::new();
        let body = json!({"notes": "x", "status_id": 2});
        let out =
            normalize_issue_api_request("/issues/220.json", "PUT", Some(&body), &mut q).unwrap();
        assert_eq!(out["issue"]["notes"], "x");
        assert_eq!(out["issue"]["status_id"], 2);
    }

    #[test]
    fn structured_403_hint_for_projects() {
        let ctx = ApiErrorContext {
            profile: Some("openclaw".into()),
            method: Some("GET".into()),
            path: Some("/projects.json".into()),
        };
        let v = structured_api_error(403, "", &ctx);
        assert!(v["hint"].as_str().unwrap().contains("openclaw"));
    }

    #[test]
    fn fallback_flag_for_empty_openclaw_projects() {
        assert!(projects_list_needs_fallback(
            &json!({"projects": [], "total_count": 0}),
            Some("openclaw")
        ));
        assert!(!projects_list_needs_fallback(
            &json!({"projects": [{"id": 1}], "total_count": 1}),
            Some("openclaw")
        ));
    }
}
