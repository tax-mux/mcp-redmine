use serde_json::Value;

/// Strip bulky body fields from Redmine **list** payloads.
/// Keep `subject` / metadata (incl. `updated_on`, nested `project`/`status` names) for drill-down.
/// Projects list keeps only `id` / `name` / `identifier` (plus top-level paging fields).
pub fn strip_list_bodies(value: &mut Value) {
    strip_issue_list_items(value);
    strip_project_list_items(value);
}

fn strip_issue_list_items(value: &mut Value) {
    let Some(Value::Array(items)) = value.get_mut("issues") else {
        return;
    };
    for item in items {
        trim_issue_list_item(item);
    }
}

fn strip_project_list_items(value: &mut Value) {
    let Some(Value::Array(items)) = value.get_mut("projects") else {
        return;
    };
    for item in items {
        trim_project_list_item(item);
    }
}

/// Keep fields needed for OpenClaw stale-ticket cron and compact summaries.
pub fn trim_issue_list_item(item: &mut Value) {
    let Some(map) = item.as_object_mut() else {
        return;
    };
    map.remove("description");
    // Drop rarely-needed bulk while preserving cron/summary metadata.
    for key in ["custom_fields", "relations", "watchers", "journals", "attachments"] {
        map.remove(key);
    }
}

/// List rows only need identity for pickers / counts; details belong on `get`.
pub fn trim_project_list_item(item: &mut Value) {
    let Some(map) = item.as_object_mut() else {
        return;
    };
    let id = map.remove("id");
    let name = map.remove("name");
    let identifier = map.remove("identifier");
    map.clear();
    if let Some(v) = id {
        map.insert("id".into(), v);
    }
    if let Some(v) = name {
        map.insert("name".into(), v);
    }
    if let Some(v) = identifier {
        map.insert("identifier".into(), v);
    }
}

/// True for GET collection endpoints that return many issues/projects with bodies.
pub fn is_list_collection_get(method: &str, path: &str) -> bool {
    if !method.eq_ignore_ascii_case("GET") {
        return false;
    }
    let path = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    matches!(
        path,
        "/issues.json" | "issues.json" | "/projects.json" | "projects.json"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_issue_bodies_and_thins_project_list_rows() {
        let mut v = json!({
            "issues": [
                {
                    "id": 1,
                    "subject": "A",
                    "description": "long body",
                    "updated_on": "2026-08-28T00:00:00Z",
                    "project": {"id": 9, "name": "mcp-redmine"},
                    "status": {"id": 1, "name": "新規"},
                    "custom_fields": [{"id": 1, "value": "x"}]
                }
            ],
            "projects": [
                {
                    "id": 10,
                    "name": "P",
                    "identifier": "p",
                    "description": "proj body",
                    "homepage": "",
                    "status": 1,
                    "is_public": false,
                    "created_on": "2026-01-01T00:00:00Z",
                    "updated_on": "2026-01-02T00:00:00Z",
                    "inherit_members": false
                }
            ],
            "total_count": 1,
            "offset": 0,
            "limit": 100
        });
        strip_list_bodies(&mut v);
        assert!(v["issues"][0].get("description").is_none());
        assert_eq!(v["issues"][0]["subject"], "A");
        assert_eq!(v["issues"][0]["updated_on"], "2026-08-28T00:00:00Z");
        assert_eq!(v["issues"][0]["project"]["name"], "mcp-redmine");
        assert!(v["issues"][0].get("custom_fields").is_none());
        assert_eq!(
            v["projects"][0],
            json!({"id": 10, "name": "P", "identifier": "p"})
        );
        assert_eq!(v["total_count"], 1);
        assert_eq!(v["offset"], 0);
        assert_eq!(v["limit"], 100);
    }

    #[test]
    fn does_not_touch_single_issue_get_shape() {
        let mut v = json!({
            "issue": {"id": 1, "subject": "A", "description": "keep me"}
        });
        strip_list_bodies(&mut v);
        assert_eq!(v["issue"]["description"], "keep me");
    }

    #[test]
    fn does_not_touch_single_project_get_shape() {
        let mut v = json!({
            "project": {
                "id": 10,
                "name": "P",
                "identifier": "p",
                "description": "keep me",
                "homepage": "https://example.invalid"
            }
        });
        strip_list_bodies(&mut v);
        assert_eq!(v["project"]["description"], "keep me");
        assert_eq!(v["project"]["homepage"], "https://example.invalid");
    }

    #[test]
    fn detects_list_collection_paths() {
        assert!(is_list_collection_get("GET", "/issues.json"));
        assert!(is_list_collection_get("get", "/issues.json?project_id=1"));
        assert!(is_list_collection_get("GET", "/projects.json"));
        assert!(!is_list_collection_get("GET", "/issues/42.json"));
        assert!(!is_list_collection_get("POST", "/issues.json"));
        assert!(!is_list_collection_get("GET", "/issue_statuses.json"));
    }

    #[test]
    fn trim_project_list_item_drops_unknown_keys() {
        let mut item = json!({
            "id": 1,
            "name": "N",
            "identifier": "n",
            "extra": true
        });
        trim_project_list_item(&mut item);
        let map = item.as_object().unwrap();
        assert_eq!(map.len(), 3);
        assert!(!map.contains_key("extra"));
    }
}
