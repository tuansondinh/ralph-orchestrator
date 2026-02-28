use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Input contract for executing a single lifecycle hook command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookRunRequest {
    /// Canonical lifecycle phase-event key (for example `pre.loop.start`).
    pub phase_event: String,

    /// Stable hook identifier from config (`hooks.events.<phase>[].name`).
    pub hook_name: String,

    /// Command argv (`command[0]` executable + args).
    pub command: Vec<String>,

    /// Project workspace root used as the base for relative cwd resolution.
    pub workspace_root: PathBuf,

    /// Optional per-hook working directory override.
    pub cwd: Option<PathBuf>,

    /// Optional per-hook environment variable overrides.
    pub env: HashMap<String, String>,

    /// Hook timeout guardrail in seconds.
    pub timeout_seconds: u64,

    /// Max captured bytes per output stream.
    pub max_output_bytes: u64,

    /// JSON lifecycle payload that will be written to stdin.
    pub stdin_payload: serde_json::Value,
}

/// Captured hook stream output with truncation metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookStreamOutput {
    /// Captured UTF-8 output text.
    pub content: String,

    /// Whether the captured output was truncated.
    pub truncated: bool,
}

/// Structured outcome for one hook invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookRunResult {
    /// Hook execution start time.
    pub started_at: DateTime<Utc>,

    /// Hook execution end time.
    pub ended_at: DateTime<Utc>,

    /// Total wall-clock duration in milliseconds.
    pub duration_ms: u64,

    /// Process exit code (None when terminated by signal/timeout without code).
    pub exit_code: Option<i32>,

    /// Whether execution hit timeout enforcement.
    pub timed_out: bool,

    /// Captured/truncated stdout.
    pub stdout: HookStreamOutput,

    /// Captured/truncated stderr.
    pub stderr: HookStreamOutput,
}

/// Hook executor errors.
#[derive(Debug, thiserror::Error)]
pub enum HookExecutorError {
    /// Placeholder used until executor behavior is implemented in follow-up steps.
    #[error("hook executor is not implemented yet")]
    NotImplemented,
}

/// Contract for executing one hook run request.
pub trait HookExecutorContract {
    /// Executes a hook command invocation.
    fn run(&self, request: HookRunRequest) -> Result<HookRunResult, HookExecutorError>;
}

/// Default hook executor implementation.
#[derive(Debug, Clone, Default)]
pub struct HookExecutor;

impl HookExecutor {
    /// Creates a new hook executor.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl HookExecutorContract for HookExecutor {
    fn run(&self, _request: HookRunRequest) -> Result<HookRunResult, HookExecutorError> {
        Err(HookExecutorError::NotImplemented)
    }
}
