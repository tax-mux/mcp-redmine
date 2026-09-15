use serde_json::{json, Value};

use crate::error::McpError;

/// Merge nested `issue` (object or JSON string) into flat tool args.
/// Top-level keys win over keys inside `issue`.
pub fn flatten_issue_tool_args(args: &Value) -> Result<Value, McpError> {
    let Some(raw_issue) = args.get("issue") else {
        return Ok(normalize_parent_dual(args));
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
     // Some Redmine deployments use `parent_id`, others `parent_issue_id`.
     // Emit BOTH so a parent attachment works regardless of which name the backend honors.
    normalize_parent_dual_on_map(&mut out);
    Ok(Value::Object(out))
}

/// Ensure both `parent_id` and `parent_issue_id` are present in the issue field map
/// when a parent is specified. Different Redmine deployments accept different names.
fn normalize_parent_dual_on_map(out: &mut serde_json::Map<String, Value>) {
    let val: Option<Value> = out
         .get("parent_id")
         .or_else(|| out.get("parent_issue_id"))
         .cloned();
    match val {
        Some(v) => {
            out.insert("parent_id".to_string(), v.clone());
            out.insert("parent_issue_id".to_string(), v);
         }
        None => {}
     }
}

/// Apply `normalize_parent_dual_on_map` to a top-level args Value, returning the result.
fn normalize_parent_dual(args: &Value) -> Value {
    if let Value::Object(map) = args {
        let mut map = map.clone();
        normalize_parent_dual_on_map(&mut map);
        Value::Object(map)
     } else {
        args.clone()
     }
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
         "parent_issue_id",
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
/// Fields in a Redmine `update` that silently no-op (parent/child relations are
///   only settable at create time). Returns warning messages for each detected field.
fn detect_posthoc_parent_warnings(request_wrapped: &Value) -> Vec<String> {
    let mut warnings = Vec::new();
    let m = request_wrapped.get("issue").and_then(|i| i.as_object());
    if let Some(m) = m {
        if m.get("pid").is_some() {
            warnings.push(
                "pid is create-only in Redmine (post-hoc ignored). Set parent at create time instead." 
                .to_string(),
            );
          }
        if m.get("children_add").is_some() {
            warnings.push(
                "children_add is create-only in Redmine. Link child at create time instead." 
                .to_string(),
            );
          }
        if m.get("children_remove").is_some() {
            warnings.push(
                "children_remove is create-only in Redmine. Use parent_id at create time instead." 
                .to_string(),
            );
          }
        }
    warnings
      }

pub fn issue_mutation_ack(
    action: &str,
    issue_id: Option<&str>,
    http_status: u16,
    mut redmine_body: Value,
    request_wrapped: &Value,
) -> Value {
    let posthoc_warnings = detect_posthoc_parent_warnings(request_wrapped);
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
        let mut ack = json!({
              "ok": true,
              "action": action,
              "issue_id": id,
              "http_status": http_status,
              "changed": changed,
             });
        if !posthoc_warnings.is_empty() {
            ack["warnings"] = json!(posthoc_warnings);
            }
        return ack;
        }

    if let Some(obj) = redmine_body.as_object_mut() {
        obj.insert("ok".into(), json!(true));
        obj.insert("action".into(), json!(action));
        if !id.is_empty() {
            obj.insert("issue_id".into(), json!(id));
         }
        obj.insert("http_status".into(), json!(http_status));
        obj.insert("changed".into(), json!(changed));
        if !posthoc_warnings.is_empty() {
            obj.insert("warnings".into(), json!(posthoc_warnings));
          }
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
         "parent_issue_id",
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


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn create_normalizes_parent_issue_id_alias_to_dual_fields() {
        let args = json!({
             "action": "create",
             "parent_issue_id": 761,
             "project_id": "mcp-redmine",
             "tracker_id": 1,
             "subject": "child-765",
             "description": "x"
         });
        let out = issue_body_from_args(&args, true).unwrap();
        assert_eq!(out["issue"]["parent_id"], 761);
        assert_eq!(out["issue"]["parent_issue_id"], 761);
     }

     #[test]
    fn update_normalizes_parent_issue_id_alias_to_dual_fields() {
        let args = json!({ "action": "update", "issue_id": "42", "parent_issue_id": 99 });
        let out = issue_body_from_args(&args, false).unwrap();
        assert_eq!(out["issue"]["parent_id"], 99);
        assert_eq!(out["issue"]["parent_issue_id"], 99);
     }

     #[test]
    fn create_accepts_parent_id_field_directly() {
        let args = json!({
             "action": "create",
             "parent_id": 10,
             "project_id": "mcp-redmine",
             "tracker_id": 1,
             "subject": "child-765-direct",
             "description": "x"
         });
        let out = issue_body_from_args(&args, true).unwrap();
        assert_eq!(out["issue"]["parent_id"], 10);
        // parent_issue_id also set as the dual-field ensures work on both Redmine variants
        assert_eq!(out["issue"]["parent_issue_id"], 10);
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
    fn ack_detects_posthoc_pid_warning() {
        let req = json!({"issue": {"pid": 5, "status_id": 2}});
        let out = issue_mutation_ack("update", Some("10"), 204, Value::Null, &req);
        assert_eq!(out["ok"], true);
        assert!(out.get("warnings").is_some(), "warnings should be present for pid");
        let ws: Vec<String> = out["warnings"].as_array().unwrap()
            .iter().map(|v| v.to_string()).collect();
        assert!(ws.len() >= 1, "should have at least one warning");
    }

     #[test]
    fn ack_detects_posthoc_children_add_warning() {
        let req = json!({"issue": {"children_add": 20}});
        let out = issue_mutation_ack("update", Some("10"), 204, Value::Null, &req);
        assert!(out.get("warnings").is_some(), "warnings should be present for children_add");
    }

      #[test]
     fn ack_no_warnings_for_normal_update() {
        let req = json!({"issue": {"status_id": 3, "notes": "done"}});
        let out = issue_mutation_ack("update", Some("10"), 204, Value::Null, &req);
        assert!(out.get("warnings").is_none(), "no warnings for normal update");
     }
    }
