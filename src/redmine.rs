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
        let mut value = self.request_raw(method, path, query, body).await?;
        strip_secret_fields(&mut value);
        Ok(value)
    }

    /// Like [`request`], but keeps secret fields (for in-process provisioning only).
    pub async fn request_raw(
        &self,
        method: &str,
        path: &str,
        query: Option<&HashMap<String, String>>,
        body: Option<&Value>,
    ) -> Result<Value, RedmineError> {
        let url = if path.starts_with("http://") || path.starts_with("https://") {
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
        };

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
            return Ok(Value::Null);
        }

        let value: Value = serde_json::from_str(&text)?;
        Ok(value)
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
}
