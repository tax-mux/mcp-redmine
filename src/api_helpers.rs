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

    // Agents invent /issues/N/journals.json; journals come from GET include=journals.
    let bare_for_journals = path.trim_end_matches('/');
    if bare_for_journals.contains("/journals") {
        return Err(McpError::InvalidArgs(
            "path must not include /journals. Use redmine_issues action=get with include=[\"journals\"] (e.g. issue_id + include journals), not /issues/:id/journals.json".into(),
        ));
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

/// Filter include values; drop non-Redmine tokens agents invent (e.g. "description").
pub fn sanitize_issue_include(raw: Option<&Value>) -> Option<String> {
    const ALLOWED: &[&str] = &[
        "children",
        "attachments",
        "relations",
        "changesets",
        "journals",
        "watchers",
        "allowed_statuses",
    ];
    let Some(v) = raw else {
        return None;
    };
    let parts: Vec<String> = match v {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|x| x.as_str())
            .flat_map(|s| s.split(','))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Value::String(s) => s
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => return None,
    };
    let filtered: Vec<&str> = parts
        .iter()
        .map(|s| s.as_str())
        .filter(|s| ALLOWED.iter().any(|a| a.eq_ignore_ascii_case(s)))
        .collect();
    if filtered.is_empty() {
        None
    } else {
        Some(filtered.join(","))
    }
}

/// Known Redmine `/issues.json` query keys (plus custom-field / advanced filter forms).
pub fn is_allowed_issue_list_query_key(key: &str) -> bool {
    const ALLOWED: &[&str] = &[
        "offset",
        "limit",
        "sort",
        "include",
        "project_id",
        "subproject_id",
        "tracker_id",
        "status_id",
        "assigned_to_id",
        "author_id",
        "category_id",
        "priority_id",
        "fixed_version_id",
        "parent_id",
        "parent_issue_id",
        "subject",
        "description",
        "created_on",
        "updated_on",
        "start_date",
        "due_date",
        "watcher_id",
        "set_filter",
        "query_id",
        "group_by",
        "f[]",
        "c[]",
    ];
    if ALLOWED.contains(&key) {
        return true;
    }
    if key.starts_with("cf_") {
        return true;
    }
    // Advanced filter forms: f[], op[field], v[field], v[field][]
    if key.starts_with("op[") || key.starts_with("v[") || key.starts_with("f[") {
        return true;
    }
    false
}

/// Reject invented list filters (e.g. `user_filter_id`) so agents do not get silent empty results.
pub fn validate_issue_list_query(query: &std::collections::HashMap<String, String>) -> Result<(), McpError> {
    let mut unknown: Vec<&str> = query
        .keys()
        .filter(|k| !is_allowed_issue_list_query_key(k))
        .map(|s| s.as_str())
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort();
    let mut msg = format!(
        "Unknown issue list query key(s): {}. Allowed examples: project_id, status_id, assigned_to_id, tracker_id, offset, limit, sort.",
        unknown.join(", ")
    );
    if unknown
        .iter()
        .any(|k| *k == "user_filter_id" || *k == "user_id" || *k == "me")
    {
        msg.push_str(" Hint: use assigned_to_id=me (or a numeric user id), not user_filter_id.");
    }
    Err(McpError::InvalidArgs(msg))
}

/// Map common status names to Redmine status ids. Leaves open/closed/* and numeric ids alone.
pub fn normalize_status_filter_value(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() || t == "*" || t.eq_ignore_ascii_case("open") || t.eq_ignore_ascii_case("closed")
    {
        return t.to_string();
    }
    // comma-separated lists: normalize each token
    if t.contains(',') {
        return t
            .split(',')
            .map(|p| normalize_status_filter_value(p.trim()))
            .collect::<Vec<_>>()
            .join(",");
    }
    if t.chars().all(|c| c.is_ascii_digit()) {
        return t.to_string();
    }
    let lower = t.to_ascii_lowercase();
    match lower.as_str() {
        "new" => "1".into(),
        "in_progress" | "in-progress" | "progress" => "2".into(),
        "resolved" => "3".into(),
        "feedback" => "4".into(),
        "closed_status" | "done" => "5".into(),
        "rejected" => "6".into(),
        _ => match t {
            "新規" => "1".into(),
            "進行中" => "2".into(),
            "解決" => "3".into(),
            "フィードバック" => "4".into(),
            "終了" => "5".into(),
            "却下" => "6".into(),
            _ => t.to_string(),
        },
    }
}

/// Parse api_request body when agents stringify JSON.
pub fn coerce_api_body(body: Option<&Value>) -> Option<Value> {
    let Some(body) = body else {
        return None;
    };
    match body {
        Value::String(s) => serde_json::from_str(s).ok().or_else(|| Some(body.clone())),
        other => Some(other.clone()),
    }
}

/// Default method to GET when agents omit it (common in logs).
pub fn resolve_api_method(args: &Value) -> Result<String, McpError> {
    match args.get("method") {
        None => Ok("GET".into()),
        Some(Value::String(s)) if s.trim().is_empty() => Ok("GET".into()),
        Some(Value::String(s)) => Ok(s.trim().to_ascii_uppercase()),
        Some(other) => Err(McpError::InvalidArgs(format!(
            "method must be a string (GET/POST/PUT/PATCH/DELETE), got {other}"
        ))),
    }
}

/// Strip agent-added quotes/whitespace and stringify numeric IDs.
///
/// OpenCode/pi often pass `issue_id: "\"483\""` (literal quote chars) or a JSON number.
pub fn normalize_redmine_id_value(value: &Value) -> Option<String> {
    match value {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => {
            let mut t = s.trim().to_string();
            // Agents sometimes wrap IDs in one or more layers of quotes.
            loop {
                let stripped = t
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'')
                    .trim()
                    .to_string();
                if stripped == t || stripped.is_empty() {
                    t = stripped;
                    break;
                }
                t = stripped;
            }
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        }
        _ => None,
    }
}

/// Resolve issue id from `issue_id`, top-level `id`, or `query.id` / `query.issue_id`.
pub fn resolve_issue_id_arg(args: &Value) -> Result<String, McpError> {
    if let Some(v) = args.get("issue_id") {
        if let Some(s) = normalize_redmine_id_value(v) {
            return Ok(s);
        }
    }
    if let Some(v) = args.get("id") {
        if let Some(s) = normalize_redmine_id_value(v) {
            return Ok(s);
        }
    }
    if let Some(query) = args.get("query") {
        for key in ["id", "issue_id"] {
            if let Some(v) = query.get(key) {
                if let Some(s) = normalize_redmine_id_value(v) {
                    return Ok(s);
                }
            }
        }
    }
    Err(McpError::InvalidArgs(
        "issue_id required for get/update (also accepts id or query.id; do not quote the id)".into(),
    ))
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

/// Merge nested `issue` (object or JSON string) into flat tool args.
/// Top-level keys win over keys inside `issue`.
pub fn flatten_issue_tool_args(args: &Value) -> Result<Value, McpError> {
    let Some(raw_issue) = args.get("issue") else {
        return Ok(args.clone());
    };

    let nested = match raw_issue {
        Value::Object(map) => map.clone(),
        Value::String(s) => {
            let parsed: Value = serde_json::from_str(s).map_err(|_| {
                McpError::InvalidArgs(
                    "issue must be an object or a JSON object string (got invalid JSON string)"
                        .into(),
                )
            })?;
            parsed.as_object().cloned().ok_or_else(|| {
                McpError::InvalidArgs("issue JSON string must decode to an object".into())
            })?
        }
        _ => {
            return Err(McpError::InvalidArgs(
                "issue must be an object or a JSON object string".into(),
            ));
        }
    };

    let mut out = serde_json::Map::new();
    for (k, v) in nested {
        out.insert(k, v);
    }
    if let Some(top) = args.as_object() {
        for (k, v) in top {
            if k == "issue" {
                continue;
            }
            out.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Object(out))
}

/// Build `{issue: {...}}` body from flat tool arguments (create = required core fields, update = partial).
/// Accepts nested `issue` object/string and flattens it first.
pub fn issue_body_from_args(args: &Value, for_create: bool) -> Result<Value, McpError> {
    let args = flatten_issue_tool_args(args)?;
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
        merge_optional_issue_fields(&mut issue, &args);
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

/// Build a non-null ACK for successful create/update so agents do not treat 204/empty as failure.
///
/// Existing Redmine payload fields are preserved; `ok` / `action` / `issue_id` / `http_status` /
/// `changed` are added (or replace a bare `null` body).
pub fn issue_mutation_ack(
    action: &str,
    issue_id: Option<&str>,
    http_status: u16,
    mut redmine_body: Value,
    request_wrapped: &Value,
) -> Value {
    let mut changed: Vec<String> = request_wrapped
        .get("issue")
        .and_then(|i| i.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    changed.sort();

    let id = issue_id
        .map(str::to_string)
        .or_else(|| {
            redmine_body.pointer("/issue/id").and_then(|v| match v {
                Value::Number(n) => Some(n.to_string()),
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
        })
        .unwrap_or_default();

    if redmine_body.is_null() {
        return json!({
            "ok": true,
            "action": action,
            "issue_id": id,
            "http_status": http_status,
            "changed": changed,
        });
    }

    if let Some(obj) = redmine_body.as_object_mut() {
        obj.insert("ok".into(), json!(true));
        obj.insert("action".into(), json!(action));
        if !id.is_empty() {
            obj.insert("issue_id".into(), json!(id));
        }
        obj.insert("http_status".into(), json!(http_status));
        obj.insert("changed".into(), json!(changed));
        return redmine_body;
    }

    json!({
        "ok": true,
        "action": action,
        "issue_id": id,
        "http_status": http_status,
        "changed": changed,
        "redmine": redmine_body,
    })
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

    #[test]
    fn normalizes_quoted_and_numeric_ids() {
        assert_eq!(
            normalize_redmine_id_value(&json!("\"483\"")).as_deref(),
            Some("483")
        );
        assert_eq!(
            normalize_redmine_id_value(&json!("'483'")).as_deref(),
            Some("483")
        );
        assert_eq!(
            normalize_redmine_id_value(&json!(483)).as_deref(),
            Some("483")
        );
        assert_eq!(
            normalize_redmine_id_value(&json!("  483  ")).as_deref(),
            Some("483")
        );
        assert_eq!(normalize_redmine_id_value(&json!("\"\"")), None);
        assert_eq!(normalize_redmine_id_value(&json!("")), None);
    }

    #[test]
    fn resolves_issue_id_aliases() {
        assert_eq!(
            resolve_issue_id_arg(&json!({"issue_id": "\"610\""})).unwrap(),
            "610"
        );
        assert_eq!(
            resolve_issue_id_arg(&json!({"id": 610})).unwrap(),
            "610"
        );
        assert_eq!(
            resolve_issue_id_arg(&json!({"query": {"id": 610}})).unwrap(),
            "610"
        );
        assert_eq!(
            resolve_issue_id_arg(&json!({"query": {"issue_id": "\"610\""}})).unwrap(),
            "610"
        );
        assert!(resolve_issue_id_arg(&json!({"action": "get"})).is_err());
        assert!(resolve_issue_id_arg(&json!({"issue_id": ""})).is_err());
    }

    #[test]
    fn rejects_journals_path_with_include_hint() {
        let err = validate_redmine_path("/issues/42/journals.json").unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("include"), "{msg}");
        assert!(msg.contains("journals"), "{msg}");
    }

    #[test]
    fn defaults_missing_api_method_to_get() {
        assert_eq!(resolve_api_method(&json!({"path": "/issues.json"})).unwrap(), "GET");
        assert_eq!(resolve_api_method(&json!({"method": "post"})).unwrap(), "POST");
    }

    #[test]
    fn sanitizes_include_drops_description() {
        let v = json!(["journals", "description", "children"]);
        assert_eq!(
            sanitize_issue_include(Some(&v)).as_deref(),
            Some("journals,children")
        );
        assert_eq!(sanitize_issue_include(Some(&json!(["description"]))), None);
    }

    #[test]
    fn normalizes_resolved_status_name() {
        assert_eq!(normalize_status_filter_value("resolved"), "3");
        assert_eq!(normalize_status_filter_value("open"), "open");
        assert_eq!(normalize_status_filter_value("1,resolved"), "1,3");
    }

    #[test]
    fn coerces_string_api_body() {
        let raw = json!("{\"notes\":\"x\"}");
        let out = coerce_api_body(Some(&raw)).unwrap();
        assert_eq!(out["notes"], "x");
    }

    #[test]
    fn flattens_nested_issue_object_for_create() {
        let args = json!({
            "action": "create",
            "issue": {
                "project_id": "mcp-redmine",
                "tracker_id": 2,
                "status_id": 1,
                "subject": "nested",
                "description": "body"
            }
        });
        let out = issue_body_from_args(&args, true).unwrap();
        assert_eq!(out["issue"]["subject"], "nested");
        assert_eq!(out["issue"]["project_id"], "mcp-redmine");
        assert_eq!(out["issue"]["tracker_id"], 2);
    }

    #[test]
    fn flattens_nested_issue_json_string_for_create() {
        let args = json!({
            "action": "create",
            "issue": "{\"project_id\":\"mcp-redmine\",\"tracker_id\":2,\"status_id\":1,\"subject\":\"str\",\"description\":\"d\"}"
        });
        let out = issue_body_from_args(&args, true).unwrap();
        assert_eq!(out["issue"]["subject"], "str");
    }

    #[test]
    fn top_level_wins_over_nested_issue() {
        let args = json!({
            "action": "update",
            "issue_id": "1",
            "notes": "from-top",
            "issue": { "notes": "from-nested", "status_id": 2 }
        });
        let out = issue_body_from_args(&args, false).unwrap();
        assert_eq!(out["issue"]["notes"], "from-top");
        assert_eq!(out["issue"]["status_id"], 2);
    }

    #[test]
    fn rejects_invalid_issue_json_string() {
        let args = json!({"action": "create", "issue": "not-json"});
        assert!(issue_body_from_args(&args, true).is_err());
    }

    #[test]
    fn mutation_ack_replaces_null_body() {
        let req = json!({"issue": {"notes": "hi", "status_id": 2}});
        let out = issue_mutation_ack("update", Some("627"), 204, Value::Null, &req);
        assert_eq!(out["ok"], true);
        assert_eq!(out["action"], "update");
        assert_eq!(out["issue_id"], "627");
        assert_eq!(out["http_status"], 204);
        assert_eq!(out["changed"], json!(["notes", "status_id"]));
    }

    #[test]
    fn mutation_ack_enriches_create_payload() {
        let req = json!({"issue": {"subject": "x", "project_id": "mcp-redmine"}});
        let redmine = json!({"issue": {"id": 42, "subject": "x"}});
        let out = issue_mutation_ack("create", None, 201, redmine, &req);
        assert_eq!(out["ok"], true);
        assert_eq!(out["issue_id"], "42");
        assert_eq!(out["http_status"], 201);
        assert_eq!(out["issue"]["subject"], "x");
        assert_eq!(out["changed"], json!(["project_id", "subject"]));
    }

    #[test]
    fn rejects_unknown_issue_list_query_keys() {
        let mut q = std::collections::HashMap::new();
        q.insert("user_filter_id".into(), "me".into());
        q.insert("project_id".into(), "mcp-redmine".into());
        let err = validate_issue_list_query(&q).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("user_filter_id"), "{msg}");
        assert!(msg.contains("assigned_to_id=me"), "{msg}");
    }

    #[test]
    fn allows_known_issue_list_query_keys() {
        let mut q = std::collections::HashMap::new();
        q.insert("project_id".into(), "mcp-redmine".into());
        q.insert("status_id".into(), "open".into());
        q.insert("assigned_to_id".into(), "me".into());
        q.insert("limit".into(), "25".into());
        q.insert("cf_12".into(), "x".into());
        assert!(validate_issue_list_query(&q).is_ok());
    }
}
