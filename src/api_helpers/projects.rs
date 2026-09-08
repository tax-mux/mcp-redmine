use serde_json::{json, Value};

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
