use serde_json::{json, Value};

#[derive(Debug, Clone, Default)]
pub struct ApiErrorContext {
    pub profile: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
}

pub fn structured_api_error(status: u16, body: &str, ctx: &ApiErrorContext) -> Value {
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
         422 if path.contains("/relations") => {
              let msgs = errors.join("; ");
              let hint = format!("Relation type validation failed on {method} {path}. Standard Redmine types: precedes, precedes_reverse, relates, blocks. Note: precedes_reverse is reverse of precedes. Project may restrict a subset; check project settings in UI. Errors: {}", msgs);
              hint
             },
        422 => format!(
            "Redmine rejected the request ({method} {path}). Prefer redmine_issues action=create/update with flat args. Errors: {}",
            errors.join("; ")
        ),
        404 => format!("Not found: {method} {path}. Verify issue/project id exists."),
        500..=599 => format!(
            "Redmine server error on {method} {path}. Retry once; if persistent, check Redmine logs."
        ),
        _ => format!("HTTP {status} from Redmine for {method} {path}."),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

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
     fn relations_422_lists_available_types() {
        let ctx = ApiErrorContext {
            profile: Some("default".into()),
            method: Some("POST".into()),
             path: Some("/issues/1/relations.json".into()),
        };
        let v = structured_api_error(422, "", &ctx);
        let hint = v["hint"].as_str().unwrap();
        assert!(hint.contains("precedes"), "should list precedes: {}", hint);
        assert!(hint.contains("blocks"), "should list blocks: {}", hint);
     }
    }