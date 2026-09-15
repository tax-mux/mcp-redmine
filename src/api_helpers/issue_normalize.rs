use serde_json::{json, Value};

use crate::error::McpError;

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

/// True when the endpoint parses a JSON body natively (Redmine `parse_json` in the IssueController).
///
/// `POST /issues.json` and `PUT/PATCH /issues/:id.json` accept a JSON body. Every other endpoint
/// (e.g. `/issues/:id/relations.json`) reads nested params from the form/query side, so a nested
/// JSON body must be expanded to bracket-notation params instead of being sent as JSON.
pub fn is_json_native_path(path: &str, method: &str) -> bool {
    let m = method.to_ascii_uppercase();
    let bare = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    if m == "POST" && (bare == "/issues.json" || bare == "issues.json") {
        return true;
      }
    if m == "PUT" || m == "PATCH" {
        return is_issue_update_path(path);
      }
    false
}

/// Recursively flatten a Rails-style nested JSON value into bracket-notation form params:
/// `parent[child]=value`; arrays become `parent[]=value` (repeated per element).
///
/// A value passed with no `prefix` (top level) is handled by [`rails_nested_to_params`].
pub fn flatten_rails_nested(value: &Value, prefix: Option<&str>, out: &mut std::collections::HashMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let key = match prefix {
                    Some(p) => format!("{p}[{k}]"),
                    None => k.clone(),
                 };
                flatten_rails_nested(v, Some(&key), out);
              }
          }
        Value::Array(arr) => {
            let key = match prefix {
                Some(p) => format!("{p}[]"),
                None => "[]".to_string(),
              };
            for v in arr {
                flatten_rails_nested(v, Some(&key), out);
              }
          }
        Value::Null => {}
        Value::String(s) => {
            if let Some(p) = prefix {
                out.insert(p.to_string(), s.clone());
              }
          }
        Value::Number(n) => {
            if let Some(p) = prefix {
                out.insert(p.to_string(), n.to_string());
              }
          }
        Value::Bool(b) => {
            if let Some(p) = prefix {
                out.insert(p.to_string(), b.to_string());
              }
          }
      }
}

/// Flatten a top-level JSON object into bracket-notation params.
///
/// - Nested objects/arrays: expanded to `key[child]=value` / `key[]=value`.
/// - Leaf values at the top level: kept as a plain `key=value` entry.
///
/// This is the exact form Rails accepts for `relation[...]` / `issue[...]` nested params, which
/// (unlike a JSON body) reaches `params[:relation]` on controllers such as IssueRelationsController.
pub fn rails_nested_to_params(value: &Value) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    if let Value::Object(map) = value {
        for (k, v) in map {
            match v {
                Value::Object(_) | Value::Array(_) => flatten_rails_nested(v, Some(k), &mut out),
                Value::Null => {}
                Value::String(s) => {
                    out.insert(k.clone(), s.clone());
                 }
                Value::Number(n) => {
                    out.insert(k.clone(), n.to_string());
                  }
                Value::Bool(b) => {
                    out.insert(k.clone(), b.to_string());
                   }
             }
         }
     }
    out
}

/// True when the top level of `value` contains a nested object or array that must be expanded to
/// bracket notation rather than sent as a JSON body.
pub fn has_nested_rails_params(value: &Value) -> bool {
    matches!(value, Value::Object(map) if map.values().any(|v| matches!(v, Value::Object(_) | Value::Array(_))))
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


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn defaults_missing_api_method_to_get() {
        assert_eq!(resolve_api_method(&json!({"path": "/issues.json"})).unwrap(), "GET");
        assert_eq!(resolve_api_method(&json!({"method": "post"})).unwrap(), "POST");
    }

    #[test]
    fn coerces_string_api_body() {
        let raw = json!("{\"notes\":\"x\"}");
        let out = coerce_api_body(Some(&raw)).unwrap();
        assert_eq!(out["notes"], "x");
    }

    #[test]
    fn rails_nested_to_params_expands_relation_wrapper() {
        let body = json!({"relation": {"issue_to_id": 756, "relation_type": "precedes"}});
        let out = rails_nested_to_params(&body);
        assert_eq!(out.get("relation[issue_to_id]").map(String::as_str), Some("756"));
        assert_eq!(
            out.get("relation[relation_type]").map(String::as_str),
            Some("precedes")
          );
      }

    #[test]
    fn rails_nested_to_params_keeps_leaves_flat() {
        let body = json!({"project_id": 3, "subject": "x"});
        let out = rails_nested_to_params(&body);
        assert_eq!(out.get("project_id").map(String::as_str), Some("3"));
        assert_eq!(out.get("subject").map(String::as_str), Some("x"));
      }

    #[test]
    fn is_json_native_path_detects_issue_endpoints() {
        assert!(is_json_native_path("/issues.json", "POST"));
        assert!(is_json_native_path("/issues/42.json", "PUT"));
        assert!(!is_json_native_path("/issues/42/relations.json", "POST"));
        assert!(!is_json_native_path("/projects.json", "POST"));
      }

    #[test]
    fn has_nested_rails_params_flags_nested_only() {
        assert!(has_nested_rails_params(&json!({"relation": {"issue_to_id": 1}})));
        assert!(!has_nested_rails_params(&json!({"subject": "x"})));
        assert!(!has_nested_rails_params(&json!(null)));
      }

}
