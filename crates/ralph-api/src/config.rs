use std::env;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    TrustedLocal,
    Token,
}

impl AuthMode {
    pub fn as_contract_mode(self) -> &'static str {
        match self {
            Self::TrustedLocal => "trusted_local",
            Self::Token => "token",
        }
    }
}

impl FromStr for AuthMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "trusted_local" | "trusted-local" | "local" => Ok(Self::TrustedLocal),
            "token" => Ok(Self::Token),
            other => {
                anyhow::bail!("invalid auth mode '{other}'. expected one of: trusted_local, token")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub host: String,
    pub port: u16,
    pub served_by: String,
    pub auth_mode: AuthMode,
    pub token: Option<String>,
    pub idempotency_ttl_secs: u64,
    pub workspace_root: PathBuf,
    pub loop_process_interval_ms: u64,
    pub ralph_command: String,
}

impl Default for ApiConfig {
    fn default() -> Self {
        let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        Self {
            host: "0.0.0.0".to_string(),
            port: 3000,
            served_by: "ralph-api".to_string(),
            auth_mode: AuthMode::TrustedLocal,
            token: None,
            idempotency_ttl_secs: 60 * 60,
            workspace_root,
            loop_process_interval_ms: 30_000,
            ralph_command: "ralph".to_string(),
        }
    }
}

impl ApiConfig {
    pub fn from_env() -> Result<Self> {
        let mut config = Self::default();

        if let Ok(host) = env::var("RALPH_API_HOST") {
            config.host = host;
        }

        if let Ok(port) = env::var("RALPH_API_PORT") {
            config.port = port
                .parse::<u16>()
                .with_context(|| format!("failed parsing RALPH_API_PORT='{port}' as u16"))?;
        }

        if let Ok(served_by) = env::var("RALPH_API_SERVED_BY") {
            config.served_by = served_by;
        }

        if let Ok(mode) = env::var("RALPH_API_AUTH_MODE") {
            config.auth_mode = mode.parse::<AuthMode>()?;
        }

        if let Ok(token) = env::var("RALPH_API_TOKEN")
            && !token.trim().is_empty()
        {
            config.token = Some(token);
        }

        if let Ok(ttl) = env::var("RALPH_API_IDEMPOTENCY_TTL_SECS") {
            config.idempotency_ttl_secs = ttl.parse::<u64>().with_context(|| {
                format!("failed parsing RALPH_API_IDEMPOTENCY_TTL_SECS='{ttl}' as u64")
            })?;
        }

        if let Ok(workspace_root) = env::var("RALPH_API_WORKSPACE_ROOT") {
            config.workspace_root = PathBuf::from(workspace_root);
        }

        if let Ok(interval_ms) = env::var("RALPH_API_LOOP_PROCESS_INTERVAL_MS") {
            config.loop_process_interval_ms = interval_ms.parse::<u64>().with_context(|| {
                format!("failed parsing RALPH_API_LOOP_PROCESS_INTERVAL_MS='{interval_ms}' as u64")
            })?;
        }

        if let Ok(ralph_command) = env::var("RALPH_API_RALPH_COMMAND")
            && !ralph_command.trim().is_empty()
        {
            config.ralph_command = ralph_command;
        }

        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.auth_mode == AuthMode::Token
            && self
                .token
                .as_deref()
                .is_none_or(|token| token.trim().is_empty())
        {
            anyhow::bail!("RALPH_API_TOKEN must be configured when auth mode is token");
        }

        Ok(())
    }
}
