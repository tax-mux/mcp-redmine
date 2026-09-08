use serde_json::{json, Value};
use std::collections::HashMap;

use crate::api_helpers::{
    annotate_error_with_uploaded, attachment_spec_for_action, delete_attachment_paths,
    issue_body_from_args, issue_body_with_uploads, issue_mutation_ack, normalize_status_filter_value,
    resolve_issue_id_arg, sanitize_issue_include, upload_entries, validate_issue_list_query,
};
use crate::compact::strip_list_bodies;
use crate::error::McpError;
use crate::redmine::RedmineClient;
use crate::tools::common::{
    client_request, client_request_with_status, query_map, upload_attachment_files,
};

pub(crate) async fn dispatch_issues(
    client: &RedmineClient,
    profile_ref: Option<&str>,
    args: &Value,
) -> Result<Value, McpError> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| McpError::InvalidArgs("Missing action".into()))?;
    Ok(match action {
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
                    validate_issue_list_query(&q)?;
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
                    let base_body = issue_body_from_args(&args, true)?;
                    let spec = attachment_spec_for_action("create", &args)?;
                    let uploads: Vec<Value> = match &spec.paths {
                        Some(paths) => upload_attachment_files(&client, paths).await?,
                        None => Vec::new(),
                    };
                    let paths = spec.paths.unwrap_or_default();
                    let entries = upload_entries(&paths, &uploads)?;
                    let body = issue_body_with_uploads(&base_body, &entries)?;
                    let (status, resp) = match client_request_with_status(
                        &client,
                        profile_ref,
                        "POST",
                        "/issues.json",
                        None,
                        Some(&body),
                    )
                    .await
                    {
                        Ok(r) => r,
                        Err(e) => return Err(annotate_error_with_uploaded(e, &uploads)),
                    };
                    issue_mutation_ack("create", None, status, resp, &body)
                }
                "update" => {
                    let id = resolve_issue_id_arg(&args)?;
                    let base_body = issue_body_from_args(&args, false)?;
                    let spec = attachment_spec_for_action("update", &args)?;
                    let uploads: Vec<Value> = match &spec.paths {
                        Some(paths) => upload_attachment_files(&client, paths).await?,
                        None => Vec::new(),
                    };
                    let paths = spec.paths.unwrap_or_default();
                    let entries = upload_entries(&paths, &uploads)?;
                    let body = issue_body_with_uploads(&base_body, &entries)?;
                    let (status, resp) = match client_request_with_status(
                        &client,
                        profile_ref,
                        "PUT",
                        &format!("/issues/{id}.json"),
                        None,
                        Some(&body),
                    )
                    .await
                    {
                        Ok(r) => r,
                        Err(e) => return Err(annotate_error_with_uploaded(e, &uploads)),
                    };
                    // Attachment deletion runs after the update: `DELETE /attachments/{id}`
                    // (verified working on the live Redmine; issue-update delete parameters
                    // are a no-op there).
                    let mut failed: Vec<String> = Vec::new();
                    for (delete_id, path) in spec.delete_ids.iter().zip(delete_attachment_paths(&spec.delete_ids)) {
                        if let Err(e) = client_request(&client, profile_ref, "DELETE", &path, None, None).await {
                            failed.push(format!("attachment #{delete_id}: {e}"));
                        }
                    }
                    if !failed.is_empty() {
                        return Err(McpError::Internal(format!(
                            "issue updated but attachment deletion failed: {failed:?}"
                        )));
                    }
                    let mut ack = issue_mutation_ack("update", Some(&id), status, resp, &body);
                    if !spec.delete_ids.is_empty() {
                        if let Some(obj) = ack.as_object_mut() {
                            obj.insert("deleted_attachments".into(), json!(spec.delete_ids));
                        }
                    }
                    ack
                }
                "delete" => {
                    return Err(McpError::InvalidArgs(
                        "redmine_issues does not support action=delete. Use redmine_api_request method=DELETE path=/issues/{id}.json. For relations use POST /issues/{id}/relations.json body {relation:{issue_to_id:N,relation_type:\"precedes\"}}.".into(),
                    ));
                }
                other => {
            return Err(McpError::InvalidArgs(format!(
                "Unsupported issues action: {other} (use list, get, create, or update)"
            )));
        }
    })
}
