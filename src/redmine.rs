use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::error::RedmineError;
use crate::secret::{redact_secret, strip_secret_fields};

#[derive(Clone)]
pub struct RedmineClient {
    http: Client,
    base_url: String,
    /// Kept private; never serialize or expose via MCP.
    api_key: String,
}

impl RedmineClient {
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            http: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        }
    }

    /// For redacting error / log text only. Do not put this in tool results.
    pub fn api_key_for_redaction(&self) -> &str {
        &self.api_key
    }

    pub async fn request(
        &self,
        method: &str,
        path: &str,
        query: Option<&HashMap<String, String>>,
        body: Option<&Value>,
    ) -> Result<Value, RedmineError> {
        let (_status, value) = self.request_with_status(method, path, query, body).await?;
        Ok(value)
    }

    /// Like [`request`], but also returns the HTTP status (for mutation ACKs).
    pub async fn request_with_status(
        &self,
        method: &str,
        path: &str,
        query: Option<&HashMap<String, String>>,
        body: Option<&Value>,
    ) -> Result<(u16, Value), RedmineError> {
        let (status, mut value) = self.request_raw_with_status(method, path, query, body).await?;
        strip_secret_fields(&mut value);
        Ok((status, value))
    }

    /// Like [`request_with_status`], but keeps secret fields (for in-process provisioning only).
    pub async fn request_raw(
        &self,
        method: &str,
        path: &str,
        query: Option<&HashMap<String, String>>,
        body: Option<&Value>,
    ) -> Result<Value, RedmineError> {
        let (_status, value) = self.request_raw_with_status(method, path, query, body).await?;
        Ok(value)
    }

    /// Resolve a Redmine REST path against the base URL (absolute URLs pass through).
    pub fn url_for(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!(
                "{}{}",
                self.base_url,
                if path.starts_with('/') {
                    path.to_string()
                } else {
                    format!("/{path}")
                }
            )
        }
    }

    async fn request_raw_with_status(
        &self,
        method: &str,
        path: &str,
        query: Option<&HashMap<String, String>>,
        body: Option<&Value>,
    ) -> Result<(u16, Value), RedmineError> {
        let url = self.url_for(path);

        let mut builder = match method.to_uppercase().as_str() {
            "GET" => self.http.get(&url),
            "POST" => self.http.post(&url),
            "PUT" => self.http.put(&url),
            "PATCH" => self.http.patch(&url),
            "DELETE" => self.http.delete(&url),
            "HEAD" => self.http.head(&url),
            other => {
                return Err(RedmineError::ApiError {
                    status: 0,
                    body: format!("Unsupported HTTP method: {other}"),
                });
            }
        };

        builder = builder.header("X-Redmine-API-Key", &self.api_key);
        builder = builder.header("Content-Type", "application/json");

        if let Some(q) = query {
            builder = builder.query(q);
        }
        if let Some(b) = body {
            builder = builder.json(b);
        }

        let response = builder.send().await?;
        let status = response.status().as_u16();
        let text = response.text().await?;

        if !(200..300).contains(&status) {
            let safe_body = redact_secret(&text, &self.api_key);
            return Err(RedmineError::ApiError {
                status,
                body: safe_body,
            });
        }

        if text.trim().is_empty() {
            return Ok((status, Value::Null));
        }

        let value: Value = serde_json::from_str(&text)?;
        Ok((status, value))
    }

    pub async fn current_user(&self) -> Result<Value, RedmineError> {
        self.request("GET", "/users/current.json", None, None).await
    }

    pub async fn issues_list(
        &self,
        query: Option<&HashMap<String, String>>,
    ) -> Result<Value, RedmineError> {
        self.request("GET", "/issues.json", query, None).await
    }

    pub async fn issue_get(
        &self,
        issue_id: &str,
        include: Option<&str>,
    ) -> Result<Value, RedmineError> {
        let mut q = HashMap::new();
        if let Some(inc) = include {
            q.insert("include".to_string(), inc.to_string());
        }
        let path = format!("/issues/{issue_id}.json");
        self.request(
            "GET",
            &path,
            if q.is_empty() { None } else { Some(&q) },
            None,
        )
        .await
    }

    // ------- Attachment upload (multipart) -------

    /// Pure header assembly for `POST /uploads.json` (attaching files).
    /// Verified against the live Redmine: header `X-Redmine-API-Key` authenticates
    /// uploads; a form-field `auth_token` returns 401. Body is raw file bytes.
    pub fn build_upload_headers(api_key: &str) -> Vec<(String, String)> {
        vec![
            ("Content-Type".to_string(), "application/octet-stream".to_string()),
            ("X-Redmine-API-Key".to_string(), api_key.to_string()),
        ]
    }

    /// Build the structured error returned when the upload request fails (non-2xx).
    /// Redacts the API key from the server body and keeps path + status for diagnosis.
    pub fn upload_failed_error(path: &str, status: u16, body: &str, api_key: &str) -> RedmineError {
        RedmineError::UploadFailed {
            path: path.to_string(),
            status,
            body: redact_secret(body, api_key),
        }
    }

    /// Filename sent in the multipart part (basename of the local path).
    pub fn upload_filename(file_path: &str) -> String {
        std::path::Path::new(file_path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| file_path.to_string())
    }

    /// POST `/uploads.json` with a local file (raw bytes body, header auth).
    /// Returns the `upload` object (`{id, token}`) used to attach to issues.
    pub async fn upload_attachment(&self, file_path: &str) -> Result<Value, RedmineError> {
        // Fail fast with a path-carrying error instead of a transport error.
        std::fs::metadata(file_path).map_err(|_| {
            RedmineError::File(format!("attachment file not found: path={file_path}"))
        })?;
        let bytes = tokio::fs::read(file_path).await.map_err(|e| {
            RedmineError::File(format!("attachment file unreadable: path={file_path}; {e}"))
        })?;

        let url = self.url_for("/uploads.json");
        let mut request = self.http.post(&url);
        for (name, value) in Self::build_upload_headers(&self.api_key) {
            request = request.header(name, value);
        }

        let response = request.body(bytes).send().await?;
        let status = response.status().as_u16();
        let text = response.text().await?;

        if !(200..300).contains(&status) {
            return Err(Self::upload_failed_error(file_path, status, &text, &self.api_key));
        }

        if text.trim().is_empty() {
            return Err(RedmineError::File(format!(
                "attachment upload did not return JSON: path={file_path}",
            )));
        }

        let mut value: Value = serde_json::from_str(&text)?;
        strip_secret_fields(&mut value);
        Ok(value)
    }

    // ------- Wiki API helpers -------

    /// GET `/projects/{project}/wiki/index.json` optionally including attachments.
    pub async fn wiki_list_pages(
        &self,
        project: &str,
        include_attachments: bool,
    ) -> Result<Value, RedmineError> {
        let mut q = HashMap::new();
        if include_attachments {
            q.insert("include".to_string(), "attachments".into());
        }
        self.request(
            "GET",
            &format!("/projects/{project}/wiki/index.json"),
            if q.is_empty() { None } else { Some(&q) },
            None,
        )
        .await
    }

    /// GET `/projects/{project}/wiki/{title}.json` optionally including attachments.
    pub async fn wiki_get_page(
        &self,
        project: &str,
        title: &str,
        include_attachments: bool,
    ) -> Result<Value, RedmineError> {
        let mut q = HashMap::new();
        if include_attachments {
            q.insert("include".to_string(), "attachments".into());
        }
        self.request(
            "GET",
            &format!("/projects/{project}/wiki/{title}.json"),
            if q.is_empty() { None } else { Some(&q) },
            None,
        )
        .await
    }

    /// PUT `/projects/{project}/wiki/{page_title}.json` — create or update a wiki page.
    pub async fn wiki_create_or_update_page(
        &self,
        project: &str,
        title: &str,
        text: String,
        comments: Option<String>,
        version: Option<i32>,
    ) -> Result<Value, RedmineError> {
        let mut body = json!({ "wiki_page": { "text": text } });
        if let Some(c) = &comments {
            body["wiki_page"]["comments"] = c.clone().into();
        }
        if let Some(v) = version {
            body["wiki_page"]["version"] = v.into();
        }
        // Use PUT for create/update as per Redmine Wiki REST API spec.
        self.request(
            "PUT",
            &format!("/projects/{project}/wiki/{title}.json"),
            None,
            Some(&body),
        )
        .await
    }

    /// DELETE `/projects/{project}/wiki/{title}.json`.
    pub async fn wiki_delete_page(
        &self,
        project: &str,
        title: &str,
    ) -> Result<Value, RedmineError> {
        self.request(
            "DELETE",
            &format!("/projects/{project}/wiki/{title}.json"),
            None,
            None,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_holds_key_privately() {
        let c = RedmineClient::new(
            "https://example.invalid".into(),
            "unit-test-secret-key".into(),
        );
        assert_eq!(c.api_key_for_redaction(), "unit-test-secret-key");
    }

    #[test]
    fn upload_headers_carry_key_as_header_only() {
        let headers = RedmineClient::build_upload_headers("secret-123");
        assert_eq!(
            headers,
            vec![
                ("Content-Type".to_string(), "application/octet-stream".to_string()),
                ("X-Redmine-API-Key".to_string(), "secret-123".to_string()),
            ],
            "key must ride the X-Redmine-API-Key header (form auth_token is 401 on this Redmine)"
        );
    }

    #[test]
    fn upload_filename_uses_basename() {
        assert_eq!(
            RedmineClient::upload_filename("/tmp/logs/screenshot.png"),
            "screenshot.png"
        );
        assert_eq!(RedmineClient::upload_filename("notes.md"), "notes.md");
    }

    #[test]
    fn upload_failed_error_keeps_path_and_status_and_redacts_key() {
        let err = RedmineClient::upload_failed_error(
            "/tmp/secret.txt",
            403,
            r#"{"errors":["api=secret-123-leaked"]}"#,
            "secret-123-leaked",
        );
        let msg = err.to_string();
        assert!(msg.contains("path=/tmp/secret.txt"), "must carry path: {msg}");
        assert!(msg.contains("status=403"), "must carry status: {msg}");
        assert!(!msg.contains("secret-123-leaked"), "api key must be redacted: {msg}");
    }

    #[test]
    fn url_for_resolves_against_base_url() {
        let c = RedmineClient::new("https://rm.example/".into(), "k".into());
        assert_eq!(c.url_for("/attachments.json"), "https://rm.example/attachments.json");
        assert_eq!(c.url_for("attachments.json"), "https://rm.example/attachments.json");
        assert_eq!(c.url_for("https://abs.example/x"), "https://abs.example/x");
    }

    #[tokio::test]
    async fn upload_attachment_reports_missing_file_with_path() {
        let c = RedmineClient::new("https://example.invalid".into(), "k".into());
        let err = c.upload_attachment("/definitely/not/here/xyz.bin")
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("/definitely/not/here/xyz.bin"),
            "error must carry path: {msg}"
        );
        match err {
            RedmineError::File(_) => {}
            other => panic!("expected File error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn upload_attachment_reports_unreadable_file_with_path() {
        // A directory passes fs::metadata() but cannot be read as a file.
        let dir = std::env::temp_dir();
        let c = RedmineClient::new("https://example.invalid".into(), "k".into());
        let err = c.upload_attachment(dir.to_str().unwrap()).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(dir.to_str().unwrap()), "error must carry path: {msg}");
        match err {
            RedmineError::File(_) => {}
            other => panic!("expected File error, got {other:?}"),
        }
    }
}
