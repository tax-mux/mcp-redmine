use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::McpError;

/// Optional operator-configured project list used when a restricted profile
/// cannot see `/projects.json` (empty or 403).
#[derive(Debug, Clone, Default)]
pub struct KnownProjectsConfig {
    /// Profile names (case-insensitive) that may receive the fallback list.
    profiles: HashSet<String>,
    projects: Vec<KnownProject>,
}

#[derive(Debug, Clone, Deserialize)]
struct KnownProject {
    id: u64,
    identifier: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct KnownProjectsFile {
    #[serde(default)]
    profiles: Vec<String>,
    #[serde(default)]
    projects: Vec<KnownProject>,
}

impl KnownProjectsConfig {
    /// Load from `REDMINE_KNOWN_PROJECTS_FILE` when set.
    /// Unset / empty path → empty config (no fallback).
    /// Missing file → empty config (no fallback; operator may add later).
    /// Present but invalid JSON → startup error.
    pub fn from_env() -> Result<Self, McpError> {
        let Ok(path) = std::env::var("REDMINE_KNOWN_PROJECTS_FILE") else {
            return Ok(Self::default());
        };
        let path = path.trim();
        if path.is_empty() {
            return Ok(Self::default());
        }
        let p = Path::new(path);
        if !p.exists() {
            tracing::info!(
                path = %p.display(),
                "REDMINE_KNOWN_PROJECTS_FILE not found; known-project fallback disabled"
            );
            return Ok(Self::default());
        }
        Self::from_path(p)
    }

    pub fn from_path(path: &Path) -> Result<Self, McpError> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            McpError::Internal(format!(
                "Failed to read REDMINE_KNOWN_PROJECTS_FILE {}: {e}",
                path.display()
            ))
        })?;
        Self::from_json_str(&raw).map_err(|e| {
            McpError::Internal(format!(
                "Invalid REDMINE_KNOWN_PROJECTS_FILE {}: {e}",
                path.display()
            ))
        })
    }

    pub fn from_json_str(raw: &str) -> Result<Self, String> {
        let file: KnownProjectsFile =
            serde_json::from_str(raw).map_err(|e| format!("JSON parse error: {e}"))?;
        let profiles = file
            .profiles
            .into_iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        Ok(Self {
            profiles,
            projects: file.projects,
        })
    }

    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }

    pub fn applies_to(&self, profile: Option<&str>) -> bool {
        if self.projects.is_empty() || self.profiles.is_empty() {
            return false;
        }
        profile
            .map(|p| self.profiles.contains(&p.to_ascii_lowercase()))
            .unwrap_or(false)
    }

    pub fn as_list_payload(&self) -> Value {
        let projects: Vec<Value> = self
            .projects
            .iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "identifier": p.identifier,
                    "name": p.name,
                })
            })
            .collect();
        let total = projects.len();
        json!({
            "projects": projects,
            "total_count": total,
            "_fallback": true,
            "_hint": "Configured known-project fallback: Redmine returned no projects (or forbade listing) for this profile. Verify membership or use a profile with broader access."
        })
    }

    pub fn profile_count(&self) -> usize {
        self.profiles.len()
    }

    pub fn project_count(&self) -> usize {
        self.projects.len()
    }
}

/// Whether an empty `/projects.json` response should be replaced by the configured fallback.
pub fn projects_list_needs_fallback(
    value: &Value,
    profile: Option<&str>,
    config: &KnownProjectsConfig,
) -> bool {
    if !config.applies_to(profile) {
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

    fn sample_config() -> KnownProjectsConfig {
        KnownProjectsConfig::from_json_str(
            r#"{
              "profiles": ["bot", "ReporterBot"],
              "projects": [
                {"id": 9, "identifier": "demo", "name": "Demo"}
              ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn applies_case_insensitive_and_requires_projects() {
        let cfg = sample_config();
        assert!(cfg.applies_to(Some("bot")));
        assert!(cfg.applies_to(Some("BOT")));
        assert!(cfg.applies_to(Some("reporterbot")));
        assert!(!cfg.applies_to(Some("alice")));
        assert!(!cfg.applies_to(None));
    }

    #[test]
    fn empty_config_never_applies() {
        let cfg = KnownProjectsConfig::default();
        assert!(!cfg.applies_to(Some("bot")));
        assert!(!projects_list_needs_fallback(
            &json!({"projects": [], "total_count": 0}),
            Some("bot"),
            &cfg
        ));
    }

    #[test]
    fn fallback_only_when_empty_and_profile_matches() {
        let cfg = sample_config();
        assert!(projects_list_needs_fallback(
            &json!({"projects": [], "total_count": 0}),
            Some("bot"),
            &cfg
        ));
        assert!(!projects_list_needs_fallback(
            &json!({"projects": [{"id": 1}], "total_count": 1}),
            Some("bot"),
            &cfg
        ));
        assert!(!projects_list_needs_fallback(
            &json!({"projects": [], "total_count": 0}),
            Some("alice"),
            &cfg
        ));
    }

    #[test]
    fn payload_marks_fallback() {
        let v = sample_config().as_list_payload();
        assert_eq!(v["total_count"], 1);
        assert_eq!(v["_fallback"], true);
        assert_eq!(v["projects"][0]["identifier"], "demo");
    }

    #[test]
    fn rejects_invalid_json() {
        assert!(KnownProjectsConfig::from_json_str("{not json").is_err());
    }
}
