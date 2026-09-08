use serde_json::Value;

use crate::error::McpError;

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
pub fn validate_issue_list_query(
    query: &std::collections::HashMap<String, String>,
) -> Result<(), McpError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_filesystem_paths() {
        assert!(validate_redmine_path("/home/node/.openclaw/openclaw.json").is_err());
        assert!(validate_redmine_path("/.config/opencode/opencode.json").is_err());
        assert!(validate_redmine_path("/issues.json").is_ok());
    }

    #[test]
    fn rejects_journals_path_with_include_hint() {
        let err = validate_redmine_path("/issues/42/journals.json").unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("include"), "{msg}");
        assert!(msg.contains("journals"), "{msg}");
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
