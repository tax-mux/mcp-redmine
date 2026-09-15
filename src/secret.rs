use serde_json::Value;

/// Field names that must never leave the process toward the MCP client / LLM.
const SECRET_FIELD_NAMES: &[&str] = &[
    "api_key",
    "apiKey",
    "api-key",
    "redmine_api_key",
    "REDMINE_API_KEY",
    "password",
    "passwd",
];

/// Recursively remove known secret fields from JSON values.
pub fn strip_secret_fields(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|k, _| {
                !SECRET_FIELD_NAMES
                    .iter()
                    .any(|s| k.eq_ignore_ascii_case(s))
            });
            for v in map.values_mut() {
                strip_secret_fields(v);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_secret_fields(item);
            }
        }
        _ => {}
    }
}

/// Replace every occurrence of `secret` in `text` with a fixed placeholder.
pub fn redact_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    text.replace(secret, "[REDACTED]")
}

/// True when tool arguments try to pass credentials (reject; key lives in-container only).
pub fn args_attempt_credential_injection(args: &Value) -> bool {
    match args {
        Value::Object(map) => {
            for (k, v) in map {
                if SECRET_FIELD_NAMES
                    .iter()
                    .any(|s| k.eq_ignore_ascii_case(s))
                {
                    return true;
                }
                if args_attempt_credential_injection(v) {
                    return true;
                }
            }
            false
        }
        Value::Array(items) => items.iter().any(args_attempt_credential_injection),
        _ => false,
    }
}

/// True when a JSON schema (or any value) mentions forbidden credential property names.
pub fn schema_exposes_credential_fields(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if SECRET_FIELD_NAMES
                    .iter()
                    .any(|s| k.eq_ignore_ascii_case(s))
                {
                    return true;
                }
                // properties.api_key style: key is under "properties"
                if k == "properties" {
                    if let Value::Object(props) = v {
                        if props.keys().any(|pk| {
                            SECRET_FIELD_NAMES
                                .iter()
                                .any(|s| pk.eq_ignore_ascii_case(s))
                        }) {
                            return true;
                        }
                    }
                }
                if schema_exposes_credential_fields(v) {
                    return true;
                }
            }
            false
        }
        Value::Array(items) => items.iter().any(schema_exposes_credential_fields),
        Value::String(s) => SECRET_FIELD_NAMES
            .iter()
            .any(|name| s.eq_ignore_ascii_case(name)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_api_key_from_current_user_payload() {
        let mut v = json!({
            "user": {
                "id": 5,
                "login": "alice",
                "api_key": "super-secret-key-should-not-leak",
                "mail": "a@example.com"
            }
        });
        strip_secret_fields(&mut v);
        assert!(v["user"].get("api_key").is_none());
        assert_eq!(v["user"]["login"], "alice");
        let s = v.to_string();
        assert!(!s.contains("super-secret-key-should-not-leak"));
    }

    #[test]
    fn redacts_key_from_error_text() {
        let key = "abc123SECRET";
        let msg = format!("API error: status=401, body=Invalid key {key}");
        let out = redact_secret(&msg, key);
        assert!(!out.contains(key));
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn rejects_credential_args() {
        assert!(args_attempt_credential_injection(&json!({"api_key": "x"})));
        assert!(args_attempt_credential_injection(
            &json!({"headers": {"X-Redmine-API-Key": "x", "api_key": "y"}})
        ));
        assert!(!args_attempt_credential_injection(
            &json!({"action": "list", "limit": 10})
        ));
    }
}
