use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::McpError;
use crate::redmine::RedmineClient;
use crate::secret::redact_secret;

/// In-container multi-profile API key store. Keys never leave this process via MCP.
#[derive(Clone)]
pub struct KeyStore {
    base_url: String,
    default_profile: String,
    /// profile name → API key (kept private)
    keys: BTreeMap<String, String>,
    /// When set, upsert writes `{"profiles":{...}}` here.
    persist_path: Option<PathBuf>,
}

impl KeyStore {
    pub fn from_env() -> Result<Self, McpError> {
        let base_url = std::env::var("REDMINE_URL").map_err(|_| {
            McpError::Internal("REDMINE_URL is not set (inject via container env_file)".into())
        })?;

        let mut keys = BTreeMap::new();
        let mut persist_path = None;

        if let Ok(path) = std::env::var("REDMINE_API_KEYS_FILE") {
            let path = path.trim();
            if !path.is_empty() {
                let p = PathBuf::from(path);
                if p.exists() {
                    merge_json_file(&mut keys, &p)?;
                }
                persist_path = Some(p);
            }
        }

        if let Ok(raw) = std::env::var("REDMINE_API_KEYS") {
            let raw = raw.trim();
            if !raw.is_empty() {
                merge_json_str(&mut keys, raw)?;
            }
        }

        if let Ok(single) = std::env::var("REDMINE_API_KEY") {
            let single = single.trim();
            if !single.is_empty() {
                keys.entry("default".to_string())
                    .or_insert_with(|| single.to_string());
            }
        }

        if keys.is_empty() {
            return Err(McpError::Internal(
                "No API keys configured: set REDMINE_API_KEY and/or REDMINE_API_KEYS / REDMINE_API_KEYS_FILE"
                    .into(),
            ));
        }

        let default_profile = std::env::var("REDMINE_PROFILE")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if keys.contains_key("default") {
                    "default".to_string()
                } else {
                    keys.keys().next().cloned().unwrap()
                }
            });

        if !keys.contains_key(&default_profile) {
            return Err(McpError::Internal(format!(
                "REDMINE_PROFILE={default_profile} is not present in the key store (available names are not secrets; check profile spelling)"
            )));
        }

        let store = Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            default_profile,
            keys,
            persist_path,
        };

        // Ensure file exists when path is configured (seed from current keys).
        if store.persist_path.is_some() {
            store.persist()?;
        }

        Ok(store)
    }

    /// For tests and explicit construction.
    pub fn new(
        base_url: String,
        default_profile: String,
        keys: BTreeMap<String, String>,
    ) -> Result<Self, McpError> {
        Self::new_with_persist(base_url, default_profile, keys, None)
    }

    pub fn new_with_persist(
        base_url: String,
        default_profile: String,
        keys: BTreeMap<String, String>,
        persist_path: Option<PathBuf>,
    ) -> Result<Self, McpError> {
        if keys.is_empty() {
            return Err(McpError::Internal("KeyStore requires at least one profile".into()));
        }
        if !keys.contains_key(&default_profile) {
            return Err(McpError::Internal(format!(
                "default profile `{default_profile}` missing from key store"
            )));
        }
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            default_profile,
            keys,
            persist_path,
        })
    }

    pub fn default_profile(&self) -> &str {
        &self.default_profile
    }

    pub fn persist_path(&self) -> Option<&Path> {
        self.persist_path.as_deref()
    }

    /// Profile names only — never keys.
    pub fn profile_names(&self) -> Vec<String> {
        self.keys.keys().cloned().collect()
    }

    pub fn resolve_key(&self, profile: Option<&str>) -> Result<&str, McpError> {
        let name = profile
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(self.default_profile.as_str());
        self.keys.get(name).map(|s| s.as_str()).ok_or_else(|| {
            McpError::InvalidArgs(format!(
                "Unknown profile `{name}`. Use redmine_list_profiles to see available names."
            ))
        })
    }

    pub fn client_for(&self, profile: Option<&str>) -> Result<RedmineClient, McpError> {
        let key = self.resolve_key(profile)?;
        Ok(RedmineClient::new(self.base_url.clone(), key.to_string()))
    }

    /// Admin / default profile client (used for user provisioning).
    pub fn admin_client(&self) -> Result<RedmineClient, McpError> {
        self.client_for(None)
    }

    /// Insert or replace a profile key and persist when a file path is configured.
    pub fn upsert_profile(&mut self, profile: &str, api_key: &str) -> Result<(), McpError> {
        let profile = profile.trim();
        let api_key = api_key.trim();
        if profile.is_empty() {
            return Err(McpError::InvalidArgs("profile name must not be empty".into()));
        }
        if api_key.is_empty() {
            return Err(McpError::Internal("refusing to store empty API key".into()));
        }
        if profile.eq_ignore_ascii_case("profiles") {
            return Err(McpError::InvalidArgs(
                "profile name `profiles` is reserved".into(),
            ));
        }
        self.keys.insert(profile.to_string(), api_key.to_string());
        self.persist()?;
        Ok(())
    }

    pub fn persist(&self) -> Result<(), McpError> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    McpError::Internal(format!(
                        "Failed to create keys dir {}: {e}",
                        parent.display()
                    ))
                })?;
            }
        }
        let mut profiles = serde_json::Map::new();
        for (name, key) in &self.keys {
            profiles.insert(name.clone(), serde_json::Value::String(key.clone()));
        }
        let doc = serde_json::json!({ "profiles": profiles });
        let text = serde_json::to_string_pretty(&doc).map_err(|e| {
            McpError::Internal(format!("Failed to serialize keys file: {e}"))
        })?;
        // Write in place (bind-mounted single files cannot be replaced via rename).
        std::fs::write(path, format!("{text}\n")).map_err(|e| {
            McpError::Internal(format!("Failed to write keys file {}: {e}", path.display()))
        })?;
        Ok(())
    }

    /// Redact every known API key from text.
    pub fn redact_all(&self, text: &str) -> String {
        let mut out = text.to_string();
        for key in self.keys.values() {
            out = redact_secret(&out, key);
        }
        out
    }
}

fn merge_json_file(keys: &mut BTreeMap<String, String>, path: &Path) -> Result<(), McpError> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        McpError::Internal(format!(
            "Failed to read REDMINE_API_KEYS_FILE {}: {e}",
            path.display()
        ))
    })?;
    merge_json_str(keys, &text)
}

fn merge_json_str(keys: &mut BTreeMap<String, String>, raw: &str) -> Result<(), McpError> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|e| {
        McpError::Internal(format!("Invalid REDMINE_API_KEYS JSON: {e}"))
    })?;
    let map = if let Some(profiles) = value.get("profiles").and_then(|v| v.as_object()) {
        profiles
    } else if let Some(obj) = value.as_object() {
        obj
    } else {
        return Err(McpError::Internal(
            "REDMINE_API_KEYS must be a JSON object of profile→key or {\"profiles\":{...}}".into(),
        ));
    };

    for (name, v) in map {
        if name == "profiles" {
            continue;
        }
        let key = v
            .as_str()
            .ok_or_else(|| {
                McpError::Internal(format!(
                    "Profile `{name}` value must be a string API key"
                ))
            })?
            .trim();
        if key.is_empty() {
            return Err(McpError::Internal(format!(
                "Profile `{name}` has an empty API key"
            )));
        }
        keys.insert(name.clone(), key.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn resolves_default_and_named_profiles() {
        let mut map = BTreeMap::new();
        map.insert("default".into(), "key-default".into());
        map.insert("alice".into(), "key-alice".into());
        let store = KeyStore::new("http://redmine.example".into(), "default".into(), map).unwrap();
        assert_eq!(store.resolve_key(None).unwrap(), "key-default");
        assert_eq!(store.resolve_key(Some("alice")).unwrap(), "key-alice");
        assert!(store.resolve_key(Some("missing")).is_err());
        let names = store.profile_names();
        assert_eq!(names, vec!["alice".to_string(), "default".to_string()]);
        assert!(!names.iter().any(|n| n.contains("key-")));
    }

    #[test]
    fn redact_all_masks_every_key() {
        let mut map = BTreeMap::new();
        map.insert("a".into(), "SECRET_A".into());
        map.insert("b".into(), "SECRET_B".into());
        let store = KeyStore::new("http://x".into(), "a".into(), map).unwrap();
        let out = store.redact_all("fail SECRET_A and SECRET_B");
        assert!(!out.contains("SECRET_A"));
        assert!(!out.contains("SECRET_B"));
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn merge_json_profiles_wrapper() {
        let mut keys = BTreeMap::new();
        merge_json_str(
            &mut keys,
            r#"{"profiles":{"default":"k1","bob":"k2"}}"#,
        )
        .unwrap();
        assert_eq!(keys.get("default").map(String::as_str), Some("k1"));
        assert_eq!(keys.get("bob").map(String::as_str), Some("k2"));
    }

    #[test]
    fn merge_json_file_loads() {
        let dir = tempfile_dir();
        let path = dir.join("keys.json");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            write!(f, r#"{{"carol":"key-carol","default":"key-def"}}"#).unwrap();
        }
        let mut keys = BTreeMap::new();
        merge_json_file(&mut keys, &path).unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys["carol"], "key-carol");
    }

    #[test]
    fn upsert_persists_and_reloads() {
        let dir = tempfile_dir();
        let path = dir.join("keys.json");
        let mut map = BTreeMap::new();
        map.insert("default".into(), "key-default".into());
        let mut store = KeyStore::new_with_persist(
            "http://x".into(),
            "default".into(),
            map,
            Some(path.clone()),
        )
        .unwrap();
        store.upsert_profile("cursor", "key-cursor").unwrap();

        let mut reloaded = BTreeMap::new();
        merge_json_file(&mut reloaded, &path).unwrap();
        assert_eq!(reloaded.get("cursor").map(String::as_str), Some("key-cursor"));
        assert_eq!(reloaded.get("default").map(String::as_str), Some("key-default"));
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mcp-redmine-keys-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
