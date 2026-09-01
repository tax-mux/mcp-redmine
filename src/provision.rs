use std::collections::HashMap;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{McpError, RedmineError};
use crate::keys::KeyStore;
use crate::redmine::RedmineClient;
use crate::secret::redact_secret;

/// Create a Redmine user with an auto-generated password and store their API key.
/// If the login already exists, bind the existing user's API key to the profile.
/// Never returns password or api_key to the caller.
pub async fn provision_user(
    store: &mut KeyStore,
    login: &str,
    profile: Option<&str>,
    firstname: Option<&str>,
    lastname: Option<&str>,
    mail: Option<&str>,
) -> Result<Value, McpError> {
    let login = login.trim();
    if login.is_empty() {
        return Err(McpError::InvalidArgs("login is required".into()));
    }
    if !is_safe_login(login) {
        return Err(McpError::InvalidArgs(
            "login must be 1-60 chars of [A-Za-z0-9@._-]".into(),
        ));
    }

    let profile_name = profile
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(login);
    if !is_safe_login(profile_name) {
        return Err(McpError::InvalidArgs(
            "profile must be 1-60 chars of [A-Za-z0-9@._-]".into(),
        ));
    }

    if store.persist_path().is_none() {
        return Err(McpError::Internal(
            "REDMINE_API_KEYS_FILE is not set; refuse to provision without durable key storage"
                .into(),
        ));
    }

    let admin = store.admin_client()?;

    if let Some((user_id, mail_existing)) = find_user_id_by_login(&admin, login).await? {
        let api_key = fetch_user_api_key(&admin, user_id).await?;
        store.upsert_profile(profile_name, &api_key)?;
        return Ok(json!({
            "profile": profile_name,
            "login": login,
            "user_id": user_id,
            "mail": mail_existing,
            "persisted": true,
            "created": false
        }));
    }

    let password = generate_password();
    let firstname = firstname
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("User");
    let lastname = lastname
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(login);
    let mail = mail
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{login}@users.mcp-redmine.local"));

    let body = json!({
        "user": {
            "login": login,
            "firstname": firstname,
            "lastname": lastname,
            "mail": mail,
            "password": password,
            "password_confirmation": password,
            "must_change_passwd": false,
            "mail_notification": "none"
        }
    });

    let created = match admin.request("POST", "/users.json", None, Some(&body)).await {
        Ok(v) => v,
        Err(e) => {
            let msg = redact_secret(&e.to_string(), &password);
            // Race: user appeared between lookup and create — bind existing.
            if matches!(e, RedmineError::ApiError { status: 422, .. }) {
                if let Some((user_id, mail_existing)) = find_user_id_by_login(&admin, login).await? {
                    let api_key = fetch_user_api_key(&admin, user_id).await?;
                    store.upsert_profile(profile_name, &api_key)?;
                    return Ok(json!({
                        "profile": profile_name,
                        "login": login,
                        "user_id": user_id,
                        "mail": mail_existing,
                        "persisted": true,
                        "created": false
                    }));
                }
            }
            return Err(McpError::Internal(format!(
                "Failed to create Redmine user: {msg}"
            )));
        }
    };

    let user_id = created
        .pointer("/user/id")
        .and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|n| n as u64)))
        .ok_or_else(|| McpError::Internal("Create user response missing user.id".into()))?;

    let api_key = fetch_user_api_key(&admin, user_id).await?;
    store.upsert_profile(profile_name, &api_key)?;

    Ok(json!({
        "profile": profile_name,
        "login": login,
        "user_id": user_id,
        "mail": mail,
        "persisted": true,
        "created": true
    }))
}

async fn find_user_id_by_login(
    admin: &RedmineClient,
    login: &str,
) -> Result<Option<(u64, String)>, McpError> {
    let mut q = HashMap::new();
    q.insert("name".into(), login.to_string());
    q.insert("status".into(), "*".into());
    q.insert("limit".into(), "100".into());
    let listed = admin.request("GET", "/users.json", Some(&q), None).await?;
    let Some(users) = listed.get("users").and_then(|v| v.as_array()) else {
        return Ok(None);
    };
    for u in users {
        if u.get("login").and_then(|v| v.as_str()) == Some(login) {
            let id = u
                .get("id")
                .and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|n| n as u64)))
                .ok_or_else(|| McpError::Internal("user missing id".into()))?;
            let mail = u
                .get("mail")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return Ok(Some((id, mail)));
        }
    }
    Ok(None)
}

async fn fetch_user_api_key(admin: &RedmineClient, user_id: u64) -> Result<String, McpError> {
    let shown = admin
        .request_raw("GET", &format!("/users/{user_id}.json"), None, None)
        .await?;
    shown
        .pointer("/user/api_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            McpError::Internal(
                "User api_key missing (default profile must be a Redmine admin)".into(),
            )
        })
}

fn is_safe_login(s: &str) -> bool {
    let len = s.chars().count();
    (1..=60).contains(&len)
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '-'))
}

fn generate_password() -> String {
    format!(
        "{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_login() {
        assert!(!is_safe_login(""));
        assert!(!is_safe_login("has space"));
        assert!(is_safe_login("cursor"));
        assert!(is_safe_login("a_b-c.d@e"));
    }

    #[test]
    fn password_is_long_and_hexish() {
        let p = generate_password();
        assert_eq!(p.len(), 64);
        assert!(p.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
