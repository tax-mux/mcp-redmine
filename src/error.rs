use serde_json::Value;
use thiserror::Error;

use crate::api_helpers::{structured_api_error, ApiErrorContext};
use crate::keys::KeyStore;

#[derive(Error, Debug)]
pub enum RedmineError {
    #[error("API error: status={status}, body={body}")]
    ApiError { status: u16, body: String },

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Default)]
pub struct ApiErrorDetails {
    pub status: u16,
    pub body: String,
    pub ctx: ApiErrorContext,
}

#[derive(Error, Debug)]
pub enum McpError {
    #[error("Redmine error: {0}")]
    Redmine(#[from] RedmineError),

    #[error("Redmine API error: status={status}")]
    Api {
        status: u16,
        body: String,
        ctx: ApiErrorContext,
    },

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Invalid arguments: {0}")]
    InvalidArgs(String),
}

impl McpError {
    pub fn to_structured_json(&self, ctx: &ApiErrorContext, store: &KeyStore) -> Value {
        match self {
            McpError::Api { status, body, ctx } => structured_api_error(*status, body, ctx),
            McpError::Redmine(RedmineError::ApiError { status, body }) => {
                structured_api_error(*status, body, ctx)
            }
            McpError::InvalidArgs(msg) => {
                serde_json::json!({
                    "error": "invalid_arguments",
                    "message": msg,
                    "profile": ctx.profile,
                    "method": ctx.method,
                    "path": ctx.path,
                    "hint": "Fix tool arguments. For issues use redmine_issues with action list/get/create/update and flat fields."
                })
            }
            other => {
                let text = store.redact_all(&other.to_string());
                serde_json::json!({
                    "error": "internal",
                    "message": text,
                    "profile": ctx.profile,
                })
            }
        }
    }
}

impl ApiErrorDetails {
    pub fn new(status: u16, body: String, ctx: ApiErrorContext) -> Self {
        Self { status, body, ctx }
    }
}
