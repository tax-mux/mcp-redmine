use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::api_helpers::{
    coerce_api_body, known_projects_fallback, normalize_issue_api_request,
    normalize_status_filter_value, projects_list_needs_fallback, resolve_api_method,
    validate_issue_list_query, validate_redmine_path,
};
use crate::compact::{is_list_collection_get, strip_list_bodies};
use crate::error::McpError;
use crate::keys::KeyStore;
use crate::provision::provision_user;
use crate::redmine::RedmineClient;
use crate::secret::strip_secret_fields;
use crate::tools::common::{
    api_ctx, client_request, map_api_err, query_map, reject_credential_args, resolve_profile_arg,
};
use crate::tools::definitions::{
    TOOL_API_REQUEST, TOOL_CURRENT_USER, TOOL_ISSUES, TOOL_LIST_PROFILES, TOOL_METADATA,
    TOOL_PROJECTS, TOOL_PROVISION_USER, TOOL_WIKI,
};
use crate::tools::dispatch_issues::dispatch_issues;

async fn enrich_current_user(
    client: &RedmineClient,
    profile: Option<&str>,
    mut user: Value,
) -> Result<Value, McpError> {
    let admin = user
        .pointer("/user/admin")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let projects_probe = client_request(
        client,
        profile,
        "GET",
        "/projects.json",
        Some(&HashMap::from([("limit".into(), "1".into())])),
        None,
    )
    .await;

    let (can_list_projects, project_list_hint) = match projects_probe {
        Ok(v) => {
            let count = v
                .get("projects")
                .and_then(|p| p.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            (
                count > 0,
                if count == 0 && profile == Some("openclaw") {
                    Some("GET /projects.json returned empty; use redmine_projects action=list for known-project fallback".to_string())
                } else {
                    None
                },
            )
        }
        Err(McpError::Api { status, .. }) if status == 403 => (
            false,
            Some("GET /projects.json forbidden for this profile".to_string()),
        ),
        Err(_) => (false, None),
    };

    if let Value::Object(map) = &mut user {
        map.insert("profile".into(), json!(profile.unwrap_or("default")));
        map.insert(
            "capabilities".into(),
            json!({
                "admin": admin,
                "can_list_projects": can_list_projects,
                "can_create_issues": true,
                "can_update_issues": true,
                "project_list_hint": project_list_hint,
            }),
        );
    }
    Ok(user)
}

async fn dispatch_projects(
    client: &RedmineClient,
    profile: Option<&str>,
    args: &Value,
) -> Result<Value, McpError> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| McpError::InvalidArgs("Missing action".into()))?;

    match action {
        "list" => {
            let mut q = HashMap::new();
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100);
            q.insert("limit".into(), limit.to_string());
            let mut listed = match client_request(client, profile, "GET", "/projects.json", Some(&q), None).await {
                Ok(v) => v,
                Err(McpError::Api { status, .. }) if status == 403 && profile == Some("openclaw") => {
                    return Ok(known_projects_fallback());
                }
                Err(e) => return Err(e),
            };
            if projects_list_needs_fallback(&listed, profile) {
                listed = known_projects_fallback();
            } else {
                strip_list_bodies(&mut listed);
            }
            Ok(listed)
        }
        "get" => {
            let id = args
                .get("project_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| McpError::InvalidArgs("project_id required for get".into()))?;
            client_request(
                client,
                profile,
                "GET",
                &format!("/projects/{id}.json"),
                None,
                None,
            )
            .await
        }
        other => Err(McpError::InvalidArgs(format!(
            "Unsupported projects action: {other} (use list or get)"
        ))),
    }
}

async fn dispatch_metadata(client: &RedmineClient, profile: Option<&str>, args: &Value) -> Result<Value, McpError> {
    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("all");

    match kind {
        "trackers" => client_request(client, profile, "GET", "/trackers.json", None, None).await,
        "issue_statuses" => {
            client_request(client, profile, "GET", "/issue_statuses.json", None, None).await
        }
        "issue_priorities" => {
            client_request(
                client,
                profile,
                "GET",
                "/enumerations/issue_priorities.json",
                None,
                None,
            )
            .await
        }
        "all" => {
            let trackers = client_request(client, profile, "GET", "/trackers.json", None, None).await?;
            let statuses =
                client_request(client, profile, "GET", "/issue_statuses.json", None, None).await?;
            let priorities = client_request(
                client,
                profile,
                "GET",
                "/enumerations/issue_priorities.json",
                None,
                None,
            )
            .await?;
            Ok(json!({
                "trackers": trackers.get("trackers").cloned().unwrap_or(json!([])),
                "issue_statuses": statuses.get("issue_statuses").cloned().unwrap_or(json!([])),
                "issue_priorities": priorities.get("issue_priorities").cloned().unwrap_or(json!([])),
            }))
        }
        other => Err(McpError::InvalidArgs(format!(
            "Unsupported metadata kind: {other} (use trackers, issue_statuses, issue_priorities, or all)"
        ))),
    }
}

pub async fn dispatch_tool(
    name: &str,
    args: Value,
    store: Arc<Mutex<KeyStore>>,
    header_profile: Option<&str>,
) -> Result<Value, McpError> {
    reject_credential_args(&args)?;

    if name == TOOL_LIST_PROFILES {
        let store = store.lock().await;
        return Ok(json!({
            "profiles": store.profile_names(),
            "default": store.default_profile()
        }));
    }

    if name == TOOL_PROVISION_USER {
        let login = args
            .get("login")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::InvalidArgs("login is required".into()))?;
        let profile = args.get("profile").and_then(|v| v.as_str());
        let firstname = args.get("firstname").and_then(|v| v.as_str());
        let lastname = args.get("lastname").and_then(|v| v.as_str());
        let mail = args.get("mail").and_then(|v| v.as_str());
        let mut store = store.lock().await;
        let mut result =
            provision_user(&mut store, login, profile, firstname, lastname, mail).await?;
        strip_secret_fields(&mut result);
        let text = result.to_string();
        let safe = store.redact_all(&text);
        if safe != text {
            result = serde_json::from_str(&safe).unwrap_or(result);
        }
        return Ok(result);
    }

    let profile = resolve_profile_arg(&args, header_profile).map(|s| s.to_string());
    let profile_ref = profile.as_deref();
    let client = {
        let store = store.lock().await;
        store.client_for(profile_ref)?
    };

    let mut result = match name {
        TOOL_CURRENT_USER => {
            let user = client_request(&client, profile_ref, "GET", "/users/current.json", None, None).await?;
            enrich_current_user(&client, profile_ref, user).await?
        }
        TOOL_PROJECTS => dispatch_projects(&client, profile_ref, &args).await?,
        TOOL_METADATA => dispatch_metadata(&client, profile_ref, &args).await?,
        TOOL_ISSUES => dispatch_issues(&client, profile_ref, &args).await?,
        TOOL_API_REQUEST => {
            let method = resolve_api_method(&args)?;
            let path_raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| McpError::InvalidArgs("Missing path".into()))?;
            let path = validate_redmine_path(path_raw)?;
            let mut q = query_map(args.get("query")).unwrap_or_default();
            if let Some(status) = q.get("status_id").cloned() {
                q.insert("status_id".into(), normalize_status_filter_value(&status));
            }
            if is_list_collection_get(&method, &path) && path.contains("/issues.json") {
                validate_issue_list_query(&q)?;
            }
            let coerced = coerce_api_body(args.get("body"));
            let body =
                normalize_issue_api_request(&path, &method, coerced.as_ref(), &mut q);
            let q_ref = if q.is_empty() { None } else { Some(q) };
            let mut response = client_request(
                &client,
                profile_ref,
                &method,
                &path,
                q_ref.as_ref(),
                body.as_ref(),
            )
            .await?;
            if is_list_collection_get(&method, &path) {
                strip_list_bodies(&mut response);
                if path.contains("/projects.json")
                    && projects_list_needs_fallback(&response, profile_ref)
                {
                    response = known_projects_fallback();
                }
            }
            response
        }
        TOOL_WIKI => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .ok_or_else(|| McpError::InvalidArgs("Missing action".into()))?;
            let project_id = args
                .get("project_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| McpError::InvalidArgs("project_id is required".into()))?;
            match action {
                "list" => {
                    let include = args.get("include_attachments").and_then(|v| v.as_bool());
                    client.wiki_list_pages(project_id, include.unwrap_or(false)).await.map_err(|e| {
                        map_api_err(
                            e,
                            api_ctx(profile_ref, "GET", &format!("/projects/{project_id}/wiki/index.json")),
                        )
                    })?
                }
                "get" => {
                    let title = args
                        .get("title")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| McpError::InvalidArgs("title is required for get".into()))?;
                    let include = args.get("include_attachments").and_then(|v| v.as_bool());
                    client
                        .wiki_get_page(project_id, title, include.unwrap_or(false))
                        .await
                        .map_err(|e| {
                            map_api_err(
                                e,
                                api_ctx(
                                    profile_ref,
                                    "GET",
                                    &format!("/projects/{project_id}/wiki/{title}.json"),
                                ),
                            )
                        })?
                }
                "create" | "update" => {
                    let title = args
                        .get("title")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| McpError::InvalidArgs("title is required".into()))?;
                    let text = args
                        .get("text")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            McpError::InvalidArgs("text is required for create/update".into())
                        })?
                        .to_string();
                    let comments = args.get("comments").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let version = args.get("version").and_then(|v| v.as_i64()).map(|v| v as i32);
                    client
                        .wiki_create_or_update_page(project_id, title, text, comments, version)
                        .await
                        .map_err(|e| {
                            map_api_err(
                                e,
                                api_ctx(
                                    profile_ref,
                                    "PUT",
                                    &format!("/projects/{project_id}/wiki/{title}.json"),
                                ),
                            )
                        })?
                }
                "delete" => {
                    let title = args
                        .get("title")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| McpError::InvalidArgs("title is required for delete".into()))?;
                    client.wiki_delete_page(project_id, title).await.map_err(|e| {
                        map_api_err(
                            e,
                            api_ctx(
                                profile_ref,
                                "DELETE",
                                &format!("/projects/{project_id}/wiki/{title}.json"),
                            ),
                        )
                    })?
                }
                other => {
                    return Err(McpError::InvalidArgs(format!(
                        "Unsupported wiki action: {other} (use list, get, create, update, delete)"
                    )));
                }
            }
        }
        _ => return Err(McpError::Internal(format!("Unknown tool: {name}"))),
    };

    strip_secret_fields(&mut result);
    let store = store.lock().await;
    let text = result.to_string();
    let safe = store.redact_all(&text);
    if safe != text {
        result = serde_json::from_str(&safe).unwrap_or(result);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::schema_exposes_credential_fields;
    use crate::tools::common::safe_error_text;
    use crate::tools::definitions::{all_tool_definitions, TOOL_API_REQUEST, TOOL_CURRENT_USER, TOOL_ISSUES, TOOL_LIST_PROFILES, TOOL_METADATA, TOOL_PROJECTS, TOOL_PROVISION_USER};
    use std::collections::BTreeMap;

    fn test_store() -> Arc<Mutex<KeyStore>> {
        let mut map = BTreeMap::new();
        map.insert("default".into(), "should-not-appear".into());
        map.insert("alice".into(), "alice-secret-key".into());
        Arc::new(Mutex::new(
            KeyStore::new("https://example.invalid".into(), "default".into(), map).unwrap(),
        ))
    }

    #[test]
    fn tool_schemas_have_no_credential_fields() {
        let tools = all_tool_definitions();
        assert!(!schema_exposes_credential_fields(&tools));
        let text = tools.to_string();
        assert!(!text.to_ascii_lowercase().contains("api_key"));
        assert!(!text.contains("REDMINE_API_KEY"));
        assert!(text.contains(TOOL_LIST_PROFILES));
        assert!(text.contains(TOOL_PROVISION_USER));
        assert!(text.contains(TOOL_PROJECTS));
        assert!(text.contains(TOOL_METADATA));
        assert!(text.contains("\"update\""));
        assert!(text.contains("\"profile\""));
    }

    #[test]
    fn list_profiles_returns_names_only() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store = test_store();
        let out = rt
            .block_on(dispatch_tool(TOOL_LIST_PROFILES, json!({}), store, None))
            .unwrap();
        let text = out.to_string();
        assert!(text.contains("alice"));
        assert!(text.contains("default"));
        assert!(!text.contains("should-not-appear"));
        assert!(!text.contains("alice-secret-key"));
    }

    #[test]
    fn dispatch_rejects_api_key_argument() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store = test_store();
        let err = rt.block_on(dispatch_tool(
            TOOL_CURRENT_USER,
            json!({"api_key": "attacker-supplied"}),
            store.clone(),
            None,
        ));
        assert!(err.is_err());
        let guard = rt.block_on(store.lock());
        let msg = safe_error_text(&err.unwrap_err(), &guard);
        assert!(!msg.contains("should-not-appear"));
        assert!(!msg.contains("alice-secret-key"));
        assert!(msg.contains("invalid_arguments"));
    }

    #[test]
    fn dispatch_rejects_bad_api_request_path() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store = test_store();
        let err = rt.block_on(dispatch_tool(
            TOOL_API_REQUEST,
            json!({"method": "GET", "path": "/home/node/.openclaw/openclaw.json"}),
            store.clone(),
            None,
        ));
        assert!(err.is_err());
    }

    #[test]
    fn dispatch_delete_action_guides_to_api_request() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store = test_store();
        let err = rt.block_on(dispatch_tool(
            TOOL_ISSUES,
            json!({"action": "delete", "issue_id": "1"}),
            store.clone(),
            None,
        ));
        let guard = rt.block_on(store.lock());
        let msg = safe_error_text(&err.unwrap_err(), &guard);
        assert!(msg.contains("DELETE"), "{msg}");
        assert!(msg.contains("/issues/"), "{msg}");
    }

    #[test]
    fn dispatch_rejects_unknown_issue_list_query_key() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store = test_store();
        let err = rt.block_on(dispatch_tool(
            TOOL_ISSUES,
            json!({
                "action": "list",
                "query": { "user_filter_id": "me", "project_id": "mcp-redmine" }
            }),
            store.clone(),
            None,
        ));
        let guard = rt.block_on(store.lock());
        let msg = safe_error_text(&err.unwrap_err(), &guard);
        assert!(msg.contains("user_filter_id"), "{msg}");
        assert!(msg.contains("assigned_to_id"), "{msg}");
    }

    #[test]
    fn dispatch_rejects_journals_api_path() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store = test_store();
        let err = rt.block_on(dispatch_tool(
            TOOL_API_REQUEST,
            json!({"path": "/issues/1/journals.json"}),
            store.clone(),
            None,
        ));
        let guard = rt.block_on(store.lock());
        let msg = safe_error_text(&err.unwrap_err(), &guard);
        assert!(msg.contains("include"), "{msg}");
    }

    #[test]
    fn unknown_profile_errors_without_keys() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store = test_store();
        let err = rt.block_on(dispatch_tool(
            TOOL_CURRENT_USER,
            json!({"profile": "nope"}),
            store.clone(),
            None,
        ));
        assert!(err.is_err());
        let guard = rt.block_on(store.lock());
        let msg = safe_error_text(&err.unwrap_err(), &guard);
        assert!(msg.contains("Unknown profile") || msg.contains("invalid_arguments"));
        assert!(!msg.contains("should-not-appear"));
    }
}
