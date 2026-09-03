use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::api_helpers::{
    coerce_api_body, issue_body_from_args, known_projects_fallback, normalize_issue_api_request,
    normalize_status_filter_value, projects_list_needs_fallback, resolve_api_method,
    resolve_issue_id_arg, sanitize_issue_include, validate_redmine_path, ApiErrorContext,
};
use crate::compact::{is_list_collection_get, strip_list_bodies};
use crate::error::{McpError, RedmineError};
use crate::keys::KeyStore;
use crate::provision::provision_user;
use crate::redmine::RedmineClient;
use crate::secret::{args_attempt_credential_injection, strip_secret_fields};

pub const TOOL_CURRENT_USER: &str = "redmine_current_user";
pub const TOOL_ISSUES: &str = "redmine_issues";
pub const TOOL_API_REQUEST: &str = "redmine_api_request";
pub const TOOL_LIST_PROFILES: &str = "redmine_list_profiles";
pub const TOOL_PROVISION_USER: &str = "redmine_provision_user";
pub const TOOL_WIKI: &str = "redmine_wiki";
pub const TOOL_PROJECTS: &str = "redmine_projects";
pub const TOOL_METADATA: &str = "redmine_metadata";

const PROFILE_PROP: &str = "profile";

fn profile_property() -> Value {
    json!({
        "type": "string",
        "description": "Key profile name stored in the container (not a secret token). Omit to use X-Redmine-Profile header or the default profile."
    })
}


pub fn all_tool_definitions() -> Value {
    json!([
        {
            "name": TOOL_LIST_PROFILES,
            "description": "List Redmine profile names available in the container. Never returns tokens.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        },
        {
            "name": TOOL_PROVISION_USER,
            "description": "Create a Redmine user with an auto-generated password (not returned), store their REST token under a profile name, and persist to the mounted keys file. Requires an admin token in the default profile. Never pass passwords or tokens as arguments.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "login": { "type": "string", "description": "Redmine login (also default profile name)" },
                    "profile": { "type": "string", "description": "Profile name to store the API key under (default: login)" },
                    "firstname": { "type": "string" },
                    "lastname": { "type": "string" },
                    "mail": { "type": "string", "description": "Email (default: {login}@users.mcp-redmine.local)" }
                },
                "required": ["login"],
                "additionalProperties": false
            }
        },
        {
            "name": TOOL_CURRENT_USER,
            "description": "Get the authenticated Redmine user via /users/current.json plus profile name and capability hints (admin flag, project listing). Secret fields are stripped.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "profile": profile_property()
                },
                "additionalProperties": false
            }
        },
        {
            "name": TOOL_ISSUES,
            "description": "Manage Redmine issues: list, get, create, or update. list returns compact metadata (id/subject/status/project/updated_on, no description). create/update accept flat args or nested issue object/JSON string; MCP wraps {issue:{...}}. For journals use notes on update (not description). Never pass credentials as arguments.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "get", "create", "update"],
                        "description": "list, get, create, or update"
                    },
                    "issue_id": {
                        "type": ["string", "integer"],
                        "description": "Required for get and update. Also accepts top-level id or query.id. Do not wrap the id in quotes."
                    },
                    "include": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional include values for get (e.g. journals, children)"
                    },
                    "query": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Query parameters for list (project_id, status_id, etc.)"
                    },
                    "limit": { "type": "integer", "description": "Page size for list (default 25)" },
                    "project_id": { "type": "string", "description": "Project ID or identifier (required for create)" },
                    "tracker_id": { "type": "integer", "description": "Tracker ID (required for create)" },
                    "status_id": { "type": "integer", "description": "Status ID (required for create, optional for update)" },
                    "subject": { "type": "string", "description": "Issue subject (required for create)" },
                    "description": { "type": "string", "description": "Issue description body (required for create; overwrites body on update)" },
                    "notes": { "type": "string", "description": "Journal note for update (does not replace description)" },
                    "profile": profile_property()
                },
                "required": ["action"],
                "additionalProperties": true
            }
        },
        {
            "name": TOOL_PROJECTS,
            "description": "List or get Redmine projects. list returns only id/name/identifier plus total_count (and paging). get returns full project detail. openclaw profile gets a known-project fallback when Redmine returns an empty list.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "get"], "description": "list or get" },
                    "project_id": { "type": "string", "description": "Project id or identifier (required for get)" },
                    "limit": { "type": "integer", "description": "Page size for list (default 100)" },
                    "profile": profile_property()
                },
                "required": ["action"],
                "additionalProperties": false
            }
        },
        {
            "name": TOOL_METADATA,
            "description": "Fetch Redmine enumerations used when creating/updating issues: trackers, issue_statuses, issue_priorities.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["trackers", "issue_statuses", "issue_priorities", "all"],
                        "description": "Which metadata to fetch (default: all)"
                    },
                    "profile": profile_property()
                },
                "additionalProperties": false
            }
        },
        {
            "name": TOOL_API_REQUEST,
            "description": "Call any Redmine REST path. Paths must be Redmine REST (e.g. /issues.json), not local files. POST /issues.json auto-wraps flat fields in {issue:{...}}. GET list endpoints omit description bodies.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "method": { "type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"], "description": "HTTP method (default GET if omitted)" },
                    "path": { "type": "string", "description": "REST path, e.g. /projects.json" },
                    "query": { "type": "object", "additionalProperties": { "type": "string" } },
                    "body": { "type": "object" },
                    "profile": profile_property()
                },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": TOOL_WIKI,
            "description": "Manage Redmine wiki pages: list, get, create, update, delete. Authentication uses an in-container profile key; never pass credentials as arguments.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Project identifier (required)" },
                    "title": { "type": "string", "description": "Wiki page title (optional for list, required otherwise)" },
                    "action": { "type": "string", "enum": ["list", "get", "create", "update", "delete"] },
                    "include_attachments": { "type": "boolean" },
                    "text": { "type": "string", "description": "Wiki page content for create/update" },
                    "comments": { "type": "string" },
                    "version": { "type": "integer" },
                    "profile": profile_property()
                },
                "required": ["project_id", "action"],
                "additionalProperties": false
            }
        }
    ])
}

fn query_map(value: Option<&Value>) -> Option<HashMap<String, String>> {
    let obj = value?.as_object()?;
    let mut map = HashMap::new();
    for (k, v) in obj {
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            other => other.to_string(),
        };
        map.insert(k.clone(), s);
    }
    Some(map)
}

fn reject_credential_args(args: &Value) -> Result<(), McpError> {
    if args_attempt_credential_injection(args) {
        return Err(McpError::InvalidArgs(
            "Credential fields (api_key / REDMINE_API_KEY / password) must not be passed as tool arguments; the API key stays inside the container".into(),
        ));
    }
    Ok(())
}

fn resolve_profile_arg<'a>(args: &'a Value, header_profile: Option<&'a str>) -> Option<&'a str> {
    args.get(PROFILE_PROP)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| header_profile.map(str::trim).filter(|s| !s.is_empty()))
}

fn api_ctx(profile: Option<&str>, method: &str, path: &str) -> ApiErrorContext {
    ApiErrorContext {
        profile: profile.map(str::to_string),
        method: Some(method.to_string()),
        path: Some(path.to_string()),
    }
}

fn map_api_err(e: RedmineError, ctx: ApiErrorContext) -> McpError {
    match e {
        RedmineError::ApiError { status, body } => McpError::Api {
            status,
            body,
            ctx,
        },
        other => McpError::Redmine(other),
    }
}

async fn client_request(
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
        TOOL_ISSUES => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .ok_or_else(|| McpError::InvalidArgs("Missing action".into()))?;
            match action {
                "list" => {
                    let mut q = query_map(args.get("query")).unwrap_or_default();
                    if let Some(limit) = args.get("limit").and_then(|v| v.as_u64()) {
                        q.insert("limit".into(), limit.to_string());
                    } else {
                        q.entry("limit".into()).or_insert_with(|| "25".into());
                    }
                    if let Some(status) = q.get("status_id").cloned() {
                        q.insert("status_id".into(), normalize_status_filter_value(&status));
                    }
                    let mut listed = client_request(
                        &client,
                        profile_ref,
                        "GET",
                        "/issues.json",
                        Some(&q),
                        None,
                    )
                    .await?;
                    strip_list_bodies(&mut listed);
                    listed
                }
                "get" => {
                    let id = resolve_issue_id_arg(&args)?;
                    let include = sanitize_issue_include(args.get("include"));
                    let mut q = HashMap::new();
                    if let Some(inc) = include {
                        q.insert("include".into(), inc);
                    }
                    client_request(
                        &client,
                        profile_ref,
                        "GET",
                        &format!("/issues/{id}.json"),
                        if q.is_empty() { None } else { Some(&q) },
                        None,
                    )
                    .await?
                }
                "create" => {
                    let body = issue_body_from_args(&args, true)?;
                    client_request(
                        &client,
                        profile_ref,
                        "POST",
                        "/issues.json",
                        None,
                        Some(&body),
                    )
                    .await?
                }
                "update" => {
                    let id = resolve_issue_id_arg(&args)?;
                    let body = issue_body_from_args(&args, false)?;
                    client_request(
                        &client,
                        profile_ref,
                        "PUT",
                        &format!("/issues/{id}.json"),
                        None,
                        Some(&body),
                    )
                    .await?
                }
                "delete" => {
                    return Err(McpError::InvalidArgs(
                        "redmine_issues does not support action=delete. Use redmine_api_request with method=DELETE and path=/issues/{id}.json".into(),
                    ));
                }
                other => {
                    return Err(McpError::InvalidArgs(format!(
                        "Unsupported issues action: {other} (use list, get, create, or update)"
                    )));
                }
            }
        }
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

pub fn safe_error_text(err: &McpError, store: &KeyStore) -> String {
    let structured = err.to_structured_json(&ApiErrorContext::default(), store);
    serde_json::to_string_pretty(&structured).unwrap_or_else(|_| store.redact_all(&err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::schema_exposes_credential_fields;
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
