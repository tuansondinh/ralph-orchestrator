use crate::hooks::{HookRunResult, HookStreamOutput};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;

/// Final outcome category for a hook invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookDisposition {
    Pass,
    Warn,
    Block,
    Suspend,
}

/// Structured diagnostics record persisted for each hook invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookRunTelemetryEntry {
    pub timestamp: DateTime<Utc>,
    pub loop_id: String,
    pub phase_event: String,
    pub hook_name: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: HookStreamOutput,
    pub stderr: HookStreamOutput,
    pub disposition: HookDisposition,
}

impl HookRunTelemetryEntry {
    /// Creates a telemetry record from executor output and lifecycle metadata.
    #[must_use]
    pub fn from_run_result(
        loop_id: impl Into<String>,
        phase_event: impl Into<String>,
        hook_name: impl Into<String>,
        disposition: HookDisposition,
        run_result: &HookRunResult,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            loop_id: loop_id.into(),
            phase_event: phase_event.into(),
            hook_name: hook_name.into(),
            started_at: run_result.started_at,
            ended_at: run_result.ended_at,
            duration_ms: run_result.duration_ms,
            exit_code: run_result.exit_code,
            timed_out: run_result.timed_out,
            stdout: run_result.stdout.clone(),
            stderr: run_result.stderr.clone(),
            disposition,
        }
    }
}

/// JSONL writer for hook invocation telemetry (`hook-runs.jsonl`).
pub struct HookRunLogger {
    writer: BufWriter<File>,
}

impl HookRunLogger {
    pub fn new(session_dir: &Path) -> std::io::Result<Self> {
        let log_file = session_dir.join("hook-runs.jsonl");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)?;

        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    pub fn log(&mut self, entry: &HookRunTelemetryEntry) -> std::io::Result<()> {
        serde_json::to_writer(&mut self.writer, entry)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}
