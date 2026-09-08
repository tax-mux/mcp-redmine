use serde_json::{json, Value};

use crate::error::McpError;

/// Parsed attachment arguments for the redmine_issues tool.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttachmentSpec {
    /// Local file paths to upload and attach (None = no attach).
    pub paths: Option<Vec<String>>,
    /// Attachment ids to delete (update action only).
    pub delete_ids: Vec<u64>,
}

fn attachment_paths_list(v: &Value, what: &str) -> Result<Vec<String>, McpError> {
    let items: Vec<Value> = match v {
        Value::String(s) => vec![s.clone().into()],
        Value::Array(arr) => arr.clone(),
        _ => {
            return Err(McpError::InvalidArgs(format!(
                "{what} must be a string (single path) or an array of strings"
            )))
        }
    };
    let out: Vec<String> = items
        .into_iter()
        .map(|x| {
            x.as_str()
                .map(|s| s.to_string())
                .ok_or_else(|| {
                    McpError::InvalidArgs(format!(
                        "{what} entries must be strings (local file paths)"
                    ))
                })
        })
        .collect::<Result<Vec<String>, McpError>>()?;
    if out.is_empty() {
        return Err(McpError::InvalidArgs(format!(
            "{what} must not be empty"
        )));
    }
    if out.iter().any(|s| s.trim().is_empty()) {
        return Err(McpError::InvalidArgs(format!(
            "{what} entries must be non-empty paths"
        )));
    }
    Ok(out)
}

fn delete_ids_list(v: &Value) -> Result<Vec<u64>, McpError> {
    let what = "delete_attachment_ids";
    // Some MCP bridges stringify arguments ("3", even "[4]"): if a string parses as
    // JSON, normalize it back to its native type before validating.
    let v = if let Value::String(s) = v {
        match serde_json::from_str::<Value>(s.trim()) {
            Ok(json) => json,
            Err(_) => v.clone(),
        }
    } else {
        v.clone()
    };
    let v = &v;
    // Some bridges also deliver array elements as integer-valued floats (e.g. 4.0) — accept both.
    let parse_number = |n: &serde_json::Number, i: usize| -> Result<u64, McpError> {
        if let Some(x) = n.as_u64() {
            return Ok(x);
        }
        if let Some(f) = n.as_f64() {
            if f >= 0.0 && f.fract() == 0.0 {
                return Ok(f as u64);
            }
        }
        Err(McpError::InvalidArgs(format!("{what}[{i}] must be a positive integer")))
    };
    let parse_string = |s: &str, i: usize| -> Result<u64, McpError> {
        s.trim()
            .parse::<u64>()
            .map_err(|_| McpError::InvalidArgs(format!("{what}[{i}] must be a positive integer")))
    };
    let nums: Vec<u64> = match v {
        Value::Number(n) => vec![parse_number(n, 0)?],
        // Some MCP bridges deliver small scalars as numeric strings.
        Value::String(s) => vec![parse_string(s, 0)?],
        Value::Array(arr) => arr
            .iter()
            .enumerate()
            .map(|(i, x)| match x {
                Value::Number(n) => parse_number(n, i),
                Value::String(s) => parse_string(s, i),
                _ => Err(McpError::InvalidArgs(format!("{what}[{i}] must be a positive integer"))),
            })
            .collect::<Result<_, _>>()?,
        _ => {
            return Err(McpError::InvalidArgs(format!(
                "{what} must be an integer or an array of integers"
            )))
        }
    };
    if nums.is_empty() {
        return Err(McpError::InvalidArgs(format!("{what} must not be empty")));
    }
    Ok(nums)
}

/// Parse + validate `attachment_paths` / `delete_attachment_ids` for an action.
pub fn attachment_spec_for_action(action: &str, args: &Value) -> Result<AttachmentSpec, McpError> {
    let paths = match args.get("attachment_paths") {
        None => None,
        Some(v) => Some(attachment_paths_list(v, "attachment_paths")?),
    };
    let delete_ids = match args.get("delete_attachment_ids") {
        None => Vec::new(),
        Some(v) => delete_ids_list(v)?,
    };
    if action == "create" && !delete_ids.is_empty() {
        return Err(McpError::InvalidArgs(
            "delete_attachment_ids is only valid for action=update (a create has nothing to delete)"
                .into(),
        ));
    }
    Ok(AttachmentSpec { paths, delete_ids })
}

/// Insert the Redmine `uploads` field into an `{issue:{...}}` body.
/// Pure: `issue.uploads = [{token, filename}]` (Redmine "Attaching files" flow via
/// `POST /uploads.json`; the live docs define no attachment-delete issue parameter).
pub fn issue_body_with_uploads(
    body: &Value,
    uploads: &[(String, String)], // (token, filename) in upload order
) -> Result<Value, McpError> {
    if uploads.is_empty() {
        return Ok(body.clone());
    }
    let mut out = body.clone();
    let issue = out
        .get_mut("issue")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| {
            McpError::Internal("issue body must be an object to attach uploads".into())
        })?;
    let entries: Vec<Value> = uploads
        .iter()
        .map(|(token, filename)| json!({"token": token, "filename": filename}))
        .collect();
    issue.insert("uploads".into(), Value::Array(entries));
    Ok(out)
}

/// Redmine REST endpoints to DELETE one attachment id each (`DELETE /attachments/{id}&`).
/// Verified against the live Redmine: issue-update delete parameters are a no-op; only
/// this endpoint removes attachments (204 + attachment gone from the issue).
pub fn delete_attachment_paths(ids: &[u64]) -> Vec<String> {
    ids.iter()
        .map(|id| format!("/attachments/{id}.json"))
        .collect()
}

/// `(token, filename)` pairs from upload responses (`{"upload":{"id":..,"token":"2..."}}`)
/// aligned with the requested local paths.
///
/// The live Redmine response is `{"upload":{"id":<n>,"token":"2..."}}` (no filename),
/// so the filename falls back to the basename of the local path.
pub fn upload_entries(
    paths: &[String],
    uploads: &[Value],
) -> Result<Vec<(String, String)>, McpError> {
    if paths.is_empty() && uploads.is_empty() {
        return Ok(Vec::new());
    }
    if paths.len() != uploads.len() {
        return Err(McpError::InvalidArgs(format!(
            "upload response count ({}) does not match requested path count ({})",
            uploads.len(),
            paths.len()
        )));
    }
    let mut out = Vec::with_capacity(paths.len());
    for (i, (path, value)) in paths.iter().zip(uploads.iter()).enumerate() {
        let token = value
            .pointer("/upload/token")
            .and_then(|t| t.as_str())
            .ok_or_else(|| {
                McpError::InvalidArgs(format!(
                    "upload response #{i} (path={path}) has no upload.token"
                ))
            })?;
        let filename = value
            .pointer("/upload/filename")
            .and_then(|f| f.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| crate::redmine::RedmineClient::upload_filename(path));
        out.push((token.to_string(), filename));
    }
    Ok(out)
}

/// Upload ids collected from upload responses (for error annotation / cleanup hints).
pub fn uploaded_upload_ids(uploads: &[Value]) -> Vec<u64> {
    uploads
        .iter()
        .filter_map(|v| v.pointer("/upload/id").and_then(|n| n.as_u64()))
        .collect()
}

/// Annotate a failed issue mutation with the upload ids already sent, so the caller
/// knows which transient uploads may need manual cleanup / which are already bound.
pub fn annotate_error_with_uploaded(err: McpError, uploads: &[Value]) -> McpError {
    let ids = uploaded_upload_ids(uploads);
    if ids.is_empty() {
        return err;
    }
    McpError::Internal(format!(
        "{err} — redmine_upload_ids: {ids:?} (uploads were created in Redmine; if the issue mutation failed, these may remain unbound)"
    ))
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::error::McpError;

    #[test]
    fn attachment_spec_parses_string_and_array_paths() {
        let spec = attachment_spec_for_action(
            "create",
            &json!({"attachment_paths": "/tmp/a.png"}),
        )
        .unwrap();
        assert_eq!(spec.paths, Some(vec!["/tmp/a.png".into()]));

        let spec = attachment_spec_for_action(
            "update",
            &json!({"attachment_paths": ["/tmp/a.png", "/tmp/b.log"], "delete_attachment_ids": [7, 8]}),
        )
        .unwrap();
        assert_eq!(spec.paths, Some(vec!["/tmp/a.png".into(), "/tmp/b.log".into()]));
        assert_eq!(spec.delete_ids, vec![7, 8]);
    }

    #[test]
    fn attachment_spec_rejects_empty_or_non_string_paths() {
        assert!(attachment_spec_for_action("create", &json!({"attachment_paths": []})).is_err());
        assert!(attachment_spec_for_action("create", &json!({"attachment_paths": "  "})).is_err());
        assert!(attachment_spec_for_action("create", &json!({"attachment_paths": 5})).is_err());
        assert!(attachment_spec_for_action("create", &json!({"attachment_paths": [1]})).is_err());
    }

    #[test]
    fn attachment_spec_rejects_deletes_on_create() {
        let err = attachment_spec_for_action(
            "create",
            &json!({"attachment_paths": ["/tmp/a.png"], "delete_attachment_ids": [7]}),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("delete_attachment_ids"), "{msg}");
        assert!(msg.contains("update"), "{msg}");
    }

    #[test]
    fn attachment_spec_validate_delete_ids_types() {
        assert!(attachment_spec_for_action(
            "update",
            &json!({"delete_attachment_ids": "7"})
        )
        .is_ok(), "numeric-string id is accepted (some MCP bridges coerce scalars)");
        assert!(attachment_spec_for_action(
            "update",
            &json!({"delete_attachment_ids": ["7", 8]})
        )
        .is_ok(), "mixed number / numeric-string arrays are accepted");
        assert!(attachment_spec_for_action(
            "update",
            &json!({"delete_attachment_ids": "x"})
        )
        .is_err());
        assert!(attachment_spec_for_action(
            "update",
            &json!({"delete_attachment_ids": [7, "x"], "status_id": 2})
        )
        .is_err());
        let spec = attachment_spec_for_action(
            "update",
            &json!({"delete_attachment_ids": "[4, 5]"}),
        )
        .expect("stringified JSON arrays should be accepted");
        assert_eq!(spec.delete_ids, vec![4, 5]);
    }

    #[test]
    fn issue_body_with_uploads_builds_redmine_uploads_field() {
        let base = json!({"issue": {"id": 3, "status_id": 3}});
        let out = issue_body_with_uploads(
            &base,
            &[("2.token-a".into(), "a.png".into()), ("2.token-b".into(), "b.log".into())],
        )
        .unwrap();
        assert_eq!(
            out["issue"]["uploads"],
            json!([
                {"token": "2.token-a", "filename": "a.png"},
                {"token": "2.token-b", "filename": "b.log"}
            ])
        );
        assert_eq!(out["issue"]["status_id"], json!(3));
        assert!(out["issue"].get("delete_attachment_id").is_none());
    }

    #[test]
    fn issue_body_with_uploads_nochange_without_uploads() {
        let base = json!({"issue": {"status_id": 2}});
        assert_eq!(issue_body_with_uploads(&base, &[]).unwrap(), base);
    }

    #[test]
    fn delete_attachment_paths_one_endpoint_per_id() {
        assert_eq!(
            delete_attachment_paths(&[3, 7]),
            vec!["/attachments/3.json", "/attachments/7.json"]
        );
        assert!(delete_attachment_paths(&[]).is_empty());
    }

    #[test]
    fn upload_entries_pairs_token_and_filename_in_order() {
        let paths = vec!["/tmp/a.png".to_string(), "/logs/b.log".to_string()];
        let uploads = vec![
            json!({"upload": {"id": 2, "token": "2.aaaa", "filename": "a.png"}}),
            json!({"upload": {"id": 3, "token": "2.bbbb"}}),
        ];
        let entries = upload_entries(&paths, &uploads).unwrap();
        assert_eq!(
            entries,
            vec![("2.aaaa".to_string(), "a.png".to_string()), ("2.bbbb".to_string(), "b.log".to_string())],
            "filename should fall back to the local basename when missing in the response"
        );
    }

    #[test]
    fn upload_entries_rejects_count_mismatch() {
        let paths = vec!["/tmp/a.png".to_string()];
        let uploads = vec![
            json!({"upload": {"id": 2, "token": "2.a"}}),
            json!({"upload": {"id": 3, "token": "2.b"}}),
        ];
        let err = upload_entries(&paths, &uploads).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("does not match"), "{msg}");
    }

    #[test]
    fn uploaded_ids_and_annotation_carry_ids() {
        let uploads = vec![
            json!({"upload": {"id": 2, "token": "2.x"}}),
            json!({"upload": {"id": 3, "token": "2.y"}}),
        ];
        assert_eq!(uploaded_upload_ids(&uploads), vec![2, 3]);
        let one_path = vec!["/tmp/a".to_string()];
        let one_upload = vec![uploads.first().cloned().unwrap()];
        let body_out = issue_body_with_uploads(
            &json!({"issue": {}}),
            &upload_entries(&one_path, &one_upload).unwrap(),
        )
        .unwrap();
        assert_eq!(body_out["issue"]["uploads"][0]["token"], json!("2.x"));

        let err = annotate_error_with_uploaded(
            McpError::InvalidArgs("boom".into()),
            &uploads,
        );
        let msg = err.to_string();
        assert!(msg.contains("2"), "{msg}");
        assert!(msg.contains("3"), "{msg}");
        let passthrough = annotate_error_with_uploaded(McpError::InvalidArgs("boom".into()), &[]);
        assert_eq!(
            passthrough.to_string(),
            McpError::InvalidArgs("boom".into()).to_string()
        );
    }
}
