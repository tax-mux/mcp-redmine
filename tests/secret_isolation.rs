//! Ensures example MCP client config is SSE URL only (no secrets, no docker/command).

#[test]
fn examples_mcp_json_is_sse_url_only() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/mcp.json");
    let text = std::fs::read_to_string(path).expect("examples/mcp.json");
    let v: serde_json::Value = serde_json::from_str(&text).expect("valid json");
    let server = &v["mcpServers"]["mcp-redmine"];

    assert!(server.get("command").is_none(), "must not use command/stdio");
    assert!(server.get("args").is_none(), "must not pass docker args");
    assert!(server.get("env").is_none(), "must not pass env/secrets");
    assert!(
        !text.contains("REDMINE_API_KEY"),
        "must not mention REDMINE_API_KEY"
    );
    assert!(
        !text.contains("REDMINE_API_KEYS"),
        "must not mention REDMINE_API_KEYS"
    );
    assert!(
        !text.to_ascii_lowercase().contains("api_key"),
        "must not contain api_key"
    );
    assert!(
        !text.contains("docker"),
        "must not reference docker in client config"
    );

    let url = server["url"].as_str().expect("url required");
    assert!(
        url.ends_with("/sse"),
        "url must be the SSE endpoint, got {url}"
    );
}

#[test]
fn gitignore_covers_dotenv_and_secrets() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/.gitignore");
    let text = std::fs::read_to_string(path).expect(".gitignore");
    assert!(
        text.lines().any(|l| l.trim() == ".env"),
        ".env must be gitignored"
    );
    assert!(
        text.lines().any(|l| l.trim() == "secrets/"),
        "secrets/ must be gitignored"
    );
}

#[test]
fn compose_publishes_sse_port_without_inline_key() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docker-compose.yml");
    let text = std::fs::read_to_string(path).expect("docker-compose.yml");
    assert!(text.contains("env_file"));
    assert!(
        text.contains("3100:8080") || text.contains("8080:8080"),
        "compose must publish MCP HTTP port"
    );
    assert!(!text.contains("REDMINE_API_KEY="));
    assert!(!text.contains("REDMINE_API_KEYS="));
    assert!(!text.contains("stdin_open"));
}

#[test]
fn host_shell_wrapper_removed() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/mcp-stdio.sh");
    assert!(
        !std::path::Path::new(path).exists(),
        "scripts/mcp-stdio.sh must not exist"
    );
}

#[test]
fn env_example_documents_multi_profile() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/.env.example");
    let text = std::fs::read_to_string(path).expect(".env.example");
    assert!(text.contains("REDMINE_API_KEYS"));
    assert!(text.contains("REDMINE_API_KEYS_FILE"));
    assert!(text.contains("REDMINE_PROFILE"));
    assert!(text.contains("never put in mcp.json"));
}

#[test]
fn readme_documents_multi_profile_without_client_keys() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/README.md");
    let text = std::fs::read_to_string(path).expect("README.md");
    assert!(text.contains("redmine_list_profiles"));
    assert!(text.contains("REDMINE_API_KEYS"));
    assert!(text.contains("profile"));
    assert!(text.contains("mcp.json"));
    assert!(text.contains("URL のみ") || text.contains("url"));
    assert!(
        text.contains("原則禁止") || text.contains("X-Redmine-Profile"),
        "README should document header-only identity policy"
    );
}
