use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::Mutex;

use mcp_redmine::api_helpers::KnownProjectsConfig;
use mcp_redmine::keys::KeyStore;
use mcp_redmine::sse::{router, AppState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let store = KeyStore::from_env()?;
    tracing::info!(
        profiles = ?store.profile_names(),
        default = %store.default_profile(),
        persist = ?store.persist_path().map(|p| p.display().to_string()),
        "Loaded Redmine API key profiles (keys stay in-process)"
    );

    let known_projects = KnownProjectsConfig::from_env()?;
    tracing::info!(
        fallback_profiles = known_projects.profile_count(),
        known_projects = known_projects.project_count(),
        "Loaded known-project fallback config"
    );

    let port: u16 = std::env::var("MCP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);
    let bind = std::env::var("MCP_BIND").unwrap_or_else(|_| "0.0.0.0".to_string());
    let addr: SocketAddr = format!("{bind}:{port}").parse()?;

    let state = AppState {
        store: Arc::new(Mutex::new(store)),
        known_projects: known_projects.shared(),
        sessions: Arc::new(Mutex::new(Default::default())),
    };

    let app = router(state);

    tracing::info!(%addr, "Starting mcp-redmine SSE server (API keys stay in-process)");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
