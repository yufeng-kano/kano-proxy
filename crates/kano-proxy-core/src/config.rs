//! Process configuration from environment variables. Names match the Worker `Env`
//! (apps/api/src/env.ts) so an operator's existing secret names carry over unchanged;
//! the Cloudflare bindings (`DB`, `CACHE`, `AGENT_TUNNEL`) become `DATABASE_URL` and
//! in-process state.

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CoreConfig {
    /// `0.0.0.0:8787` unless `LISTEN_ADDR` says otherwise.
    pub listen_addr: SocketAddr,
    /// Postgres connection string (`DATABASE_URL`).
    pub database_url: String,
    /// Built SPA (with `/docs/` inside) to serve with an SPA fallback; unset = API only.
    pub web_dist_dir: Option<PathBuf>,
    pub app_url: String,
    pub google_redirect_uri: String,
    pub google_client_id: Option<String>,
    pub google_client_secret: Option<String>,
    pub session_secret: Option<String>,
    pub token_encryption_key: Option<String>,
    pub cli_token_secret: Option<String>,
    pub claude_code_oauth_client_id: Option<String>,
    pub codex_oauth_client_id: Option<String>,
    pub grok_oauth_client_id: Option<String>,
    pub antigravity_oauth_client_id: Option<String>,
    pub antigravity_oauth_client_secret: Option<String>,
    pub antigravity_client_version: Option<String>,
    pub antigravity_client_build: Option<String>,
    pub antigravity_hub_version: Option<String>,
    /// Days of `request_logs` to keep; invalid or absent = 90.
    pub request_log_retention_days: u32,
    /// Per-attempt wait for upstream response headers; invalid or absent = 180 000.
    pub upstream_first_byte_timeout_ms: u64,
    pub github_repo: Option<String>,
    pub github_token: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is required")]
    Missing(&'static str),
    #[error("{0} is invalid: {1}")]
    Invalid(&'static str, String),
}

fn opt(name: &str) -> Option<String> {
    env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

impl CoreConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let listen_addr = opt("LISTEN_ADDR")
            .unwrap_or_else(|| "0.0.0.0:8787".to_string())
            .parse()
            .map_err(|e: std::net::AddrParseError| ConfigError::Invalid("LISTEN_ADDR", e.to_string()))?;
        let database_url = opt("DATABASE_URL").ok_or(ConfigError::Missing("DATABASE_URL"))?;
        let app_url = opt("APP_URL").unwrap_or_else(|| "http://127.0.0.1:5173".to_string());
        let google_redirect_uri = opt("GOOGLE_REDIRECT_URI")
            .unwrap_or_else(|| "http://127.0.0.1:8787/api/auth/callback".to_string());
        Ok(Self {
            listen_addr,
            database_url,
            web_dist_dir: opt("WEB_DIST_DIR").map(PathBuf::from),
            app_url,
            google_redirect_uri,
            google_client_id: opt("GOOGLE_CLIENT_ID"),
            google_client_secret: opt("GOOGLE_CLIENT_SECRET"),
            session_secret: opt("SESSION_SECRET"),
            token_encryption_key: opt("TOKEN_ENCRYPTION_KEY"),
            cli_token_secret: opt("CLI_TOKEN_SECRET"),
            claude_code_oauth_client_id: opt("CLAUDE_CODE_OAUTH_CLIENT_ID"),
            codex_oauth_client_id: opt("CODEX_OAUTH_CLIENT_ID"),
            grok_oauth_client_id: opt("GROK_OAUTH_CLIENT_ID"),
            antigravity_oauth_client_id: opt("ANTIGRAVITY_OAUTH_CLIENT_ID"),
            antigravity_oauth_client_secret: opt("ANTIGRAVITY_OAUTH_CLIENT_SECRET"),
            antigravity_client_version: opt("ANTIGRAVITY_CLIENT_VERSION"),
            antigravity_client_build: opt("ANTIGRAVITY_CLIENT_BUILD"),
            antigravity_hub_version: opt("ANTIGRAVITY_HUB_VERSION"),
            request_log_retention_days: opt("REQUEST_LOG_RETENTION_DAYS")
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(90),
            upstream_first_byte_timeout_ms: opt("UPSTREAM_FIRST_BYTE_TIMEOUT_MS")
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(180_000),
            github_repo: opt("GITHUB_REPO"),
            github_token: opt("GITHUB_TOKEN"),
        })
    }
}
