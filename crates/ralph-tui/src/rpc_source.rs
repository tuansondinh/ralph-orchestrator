//! RPC event source for reading JSON-RPC events from a subprocess.
//!
//! This module provides an async event reader that:
//! - Reads JSON lines from a subprocess's stdout
//! - Parses each line as an `RpcEvent`
//! - Translates events into `TuiState` mutations
//!
//! This replaces the in-process `EventBus` observer when running in subprocess mode.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tracing::{debug, warn};

use ralph_proto::json_rpc::RpcEvent;

use crate::state::{TaskCounts, TuiState};
use crate::state_mutations::{
    append_error_line, apply_loop_completed, apply_task_active, apply_task_close,
};
use crate::text_renderer::{text_to_lines, truncate};

/// Runs the RPC event reader, processing events from the given async reader.
///
/// This function reads JSON lines from the subprocess stdout, parses them as
/// `RpcEvent`, and applies the corresponding mutations to the TUI state.
///
/// # Arguments
/// * `reader` - Any async reader (typically `tokio::process::ChildStdout`)
/// * `state` - Shared TUI state to mutate
/// * `cancel_rx` - Watch channel to signal cancellation
///
/// The function exits when:
/// - EOF is reached (subprocess exited)
/// - An unrecoverable error occurs
/// - The cancel signal is received
pub async fn run_rpc_event_reader<R>(
    reader: R,
    state: Arc<Mutex<TuiState>>,
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,
) where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    let mut received_any_event = false;

    loop {
        tokio::select! {
            biased;

            // Check for cancellation
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    debug!("RPC event reader cancelled");
                    break;
                }
            }

            // Read next line
            result = lines.next_line() => {
                match result {
                    Ok(Some(line)) => {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }

                        match serde_json::from_str::<RpcEvent>(line) {
                            Ok(event) => {
                                received_any_event = true;
                                apply_rpc_event(&event, &state);
                            }
                            Err(e) => {
                                debug!(error = %e, line = %line, "Failed to parse RPC event");
                            }
                        }
                    }
                    Ok(None) => {
                        // EOF - subprocess exited
                        debug!("RPC event reader reached EOF");
                        if let Ok(mut s) = state.lock() {
                            if !received_any_event {
                                // Subprocess died before sending any events (e.g., worktree
                                // creation failure, config error, lock conflict).
                                warn!("Subprocess exited without sending any RPC events");
                                s.subprocess_error = Some(
                                    "Subprocess exited before starting. Check .ralph/diagnostics/logs/ for details.".to_string()
                                );
                                // Create an error iteration so the content pane shows the message
                                s.start_new_iteration();
                                if let Some(handle) = s.latest_iteration_lines_handle()
                                    && let Ok(mut lines) = handle.lock()
                                {
                                    lines.push(Line::from(vec![
                                        Span::styled(
                                            "\u{26A0} ",
                                            ratatui::style::Style::default()
                                                .fg(ratatui::style::Color::Red)
                                                .add_modifier(ratatui::style::Modifier::BOLD),
                                        ),
                                        Span::raw("Subprocess exited before starting the orchestration loop."),
                                    ]));
                                    lines.push(Line::raw(""));
                                    lines.push(Line::raw("Possible causes:"));
                                    lines.push(Line::raw("  - Loop lock held by another process (stale .ralph/loop.lock)"));
                                    lines.push(Line::raw("  - Worktree creation failed (branch name collision)"));
                                    lines.push(Line::raw("  - Configuration error in hat/config files"));
                                    lines.push(Line::raw(""));
                                    lines.push(Line::raw("Check logs: .ralph/diagnostics/logs/"));
                                }
                            }
                            s.loop_completed = true;
                            s.finish_latest_iteration();
                        }
                        break;
                    }
                    Err(e) => {
                        warn!(error = %e, "Error reading from subprocess stdout");
                        break;
                    }
                }
            }
        }
    }
}

/// Freezes the current RPC text buffer into the iteration's lines.
///
/// When streaming text deltas arrive, they accumulate in `rpc_text_buffer` and
/// their rendered lines are kept as "unfrozen" (replaceable) at the end of the
/// iteration buffer. This function converts that text into permanent lines,
/// clearing the accumulator so that subsequent non-text events (tool calls,
/// errors) appear after the frozen text.
fn freeze_rpc_text(s: &mut TuiState) {
    if s.rpc_text_buffer.is_empty() {
        return;
    }

    // The last `rpc_text_line_count` lines in the buffer are unfrozen text.
    // They were produced from previous renders of the accumulating buffer.
    // Now we do one final render and replace them.
    let final_lines = text_to_lines(&s.rpc_text_buffer);
    if let Some(handle) = s.latest_iteration_lines_handle()
        && let Ok(mut buffer_lines) = handle.lock()
    {
        // Remove the previous unfrozen text lines
        let keep = buffer_lines.len().saturating_sub(s.rpc_text_line_count);
        buffer_lines.truncate(keep);
        // Append the final render
        buffer_lines.extend(final_lines);
    }

    s.rpc_text_buffer.clear();
    s.rpc_text_line_count = 0;
}

/// Applies an RPC event to the TUI state.
fn apply_rpc_event(event: &RpcEvent, state: &Arc<Mutex<TuiState>>) {
    let Ok(mut s) = state.lock() else {
        return;
    };

    match event {
        RpcEvent::LoopStarted {
            max_iterations,
            backend,
            ..
        } => {
            s.loop_started = Some(Instant::now());
            s.max_iterations = *max_iterations;
            s.pending_backend = Some(backend.clone());
        }

        RpcEvent::IterationStart {
            iteration,
            max_iterations,
            hat_display,
            backend,
            ..
        } => {
            s.max_iterations = *max_iterations;
            s.pending_backend = Some(backend.clone());

            // Start a new iteration buffer with metadata
            // (this also resets the rpc text accumulation buffer)
            s.start_new_iteration_with_metadata(Some(hat_display.clone()), Some(backend.clone()));

            // Update iteration counter
            s.iteration = *iteration;
            s.iteration_started = Some(Instant::now());

            // Update last event tracking
            s.last_event = Some("iteration_start".to_string());
            s.last_event_at = Some(Instant::now());
        }

        RpcEvent::IterationEnd {
            loop_complete_triggered,
            ..
        } => {
            // Freeze any accumulated text before ending the iteration
            freeze_rpc_text(&mut s);

            s.prev_iteration = s.iteration;
            s.finish_latest_iteration();

            // Freeze loop elapsed if loop is completing
            if *loop_complete_triggered {
                s.final_loop_elapsed = s.loop_started.map(|start| start.elapsed());
            }

            s.last_event = Some("iteration_end".to_string());
            s.last_event_at = Some(Instant::now());
        }

        RpcEvent::TextDelta { delta, .. } => {
            // Accumulate text in the buffer rather than rendering each delta
            // independently. This produces flowing paragraphs instead of
            // one-delta-per-line garbled output.
            s.rpc_text_buffer.push_str(delta);

            // Re-render the full accumulated text to lines
            let new_lines = text_to_lines(&s.rpc_text_buffer);
            let new_line_count = new_lines.len();

            if let Some(handle) = s.latest_iteration_lines_handle()
                && let Ok(mut buffer_lines) = handle.lock()
            {
                // Remove the previous unfrozen text lines (from the last render)
                let keep = buffer_lines.len().saturating_sub(s.rpc_text_line_count);
                buffer_lines.truncate(keep);
                // Append the fresh render of the full accumulated text
                buffer_lines.extend(new_lines);
            }
            s.rpc_text_line_count = new_line_count;

            s.last_event = Some("text_delta".to_string());
            s.last_event_at = Some(Instant::now());
        }

        RpcEvent::ToolCallStart {
            tool_name, input, ..
        } => {
            // Freeze accumulated text before adding the tool call line
            freeze_rpc_text(&mut s);

            // Format tool call header
            let mut spans = vec![Span::styled(
                format!("\u{2699} [{}]", tool_name),
                Style::default().fg(Color::Blue),
            )];

            // Add summary if available
            if let Some(summary) = format_tool_summary(tool_name, input) {
                spans.push(Span::styled(
                    format!(" {}", summary),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            let line = Line::from(spans);

            if let Some(handle) = s.latest_iteration_lines_handle()
                && let Ok(mut buffer_lines) = handle.lock()
            {
                buffer_lines.push(line);
            }

            s.last_event = Some("tool_call_start".to_string());
            s.last_event_at = Some(Instant::now());
        }

        RpcEvent::ToolCallEnd {
            output, is_error, ..
        } => {
            let (prefix, color) = if *is_error {
                ("\u{2717} ", Color::Red)
            } else {
                ("\u{2713} ", Color::DarkGray)
            };

            let truncated = truncate(output, 200);
            let line = Line::from(Span::styled(
                format!(" {}{}", prefix, truncated),
                Style::default().fg(color),
            ));

            if let Some(handle) = s.latest_iteration_lines_handle()
                && let Ok(mut buffer_lines) = handle.lock()
            {
                buffer_lines.push(line);
            }

            s.last_event = Some("tool_call_end".to_string());
            s.last_event_at = Some(Instant::now());
        }

        RpcEvent::Error { code, message, .. } => {
            // Freeze accumulated text before adding the error line
            freeze_rpc_text(&mut s);

            append_error_line(&mut s, code, message);

            s.last_event = Some("error".to_string());
            s.last_event_at = Some(Instant::now());
        }

        RpcEvent::HatChanged {
            to_hat,
            to_hat_display,
            ..
        } => {
            use ralph_proto::HatId;
            s.pending_hat = Some((HatId::new(to_hat), to_hat_display.clone()));

            s.last_event = Some("hat_changed".to_string());
            s.last_event_at = Some(Instant::now());
        }

        RpcEvent::TaskStatusChanged {
            task_id,
            to_status,
            title,
            ..
        } => {
            match to_status.as_str() {
                "running" | "in_progress" => {
                    apply_task_active(&mut s, task_id, title, to_status);
                }
                "closed" | "done" | "completed" => {
                    apply_task_close(&mut s, task_id);
                }
                _ => {}
            }

            s.last_event = Some("task_status_changed".to_string());
            s.last_event_at = Some(Instant::now());
        }

        RpcEvent::TaskCountsUpdated {
            total,
            open,
            closed,
            ready,
        } => {
            s.set_task_counts(TaskCounts::new(*total, *open, *closed, *ready));

            s.last_event = Some("task_counts_updated".to_string());
            s.last_event_at = Some(Instant::now());
        }

        RpcEvent::GuidanceAck { .. } => {
            // Just update liveness
            s.last_event = Some("guidance_ack".to_string());
            s.last_event_at = Some(Instant::now());
        }

        RpcEvent::LoopTerminated {
            total_iterations, ..
        } => {
            s.iteration = *total_iterations;
            apply_loop_completed(&mut s);

            s.last_event = Some("loop_terminated".to_string());
            s.last_event_at = Some(Instant::now());
        }

        RpcEvent::Response { .. } => {
            // Responses are typically handled by the caller of get_state/etc.
            // Just update liveness
            s.last_event = Some("response".to_string());
            s.last_event_at = Some(Instant::now());
        }

        RpcEvent::OrchestrationEvent { topic, .. } => {
            // Generic orchestration events - just update liveness
            s.last_event = Some(topic.clone());
            s.last_event_at = Some(Instant::now());
        }
    }
}

/// Extracts the most relevant field from tool input for display.
fn format_tool_summary(name: &str, input: &Value) -> Option<String> {
    match name {
        "Read" | "Edit" | "Write" => input
            .get("path")
            .or_else(|| input.get("file_path"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        "Bash" => {
            let cmd = input.get("command")?.as_str()?;
            Some(truncate(cmd, 60))
        }
        "Grep" => input.get("pattern")?.as_str().map(|s| s.to_string()),
        "Glob" => input.get("pattern")?.as_str().map(|s| s.to_string()),
        "Task" => input.get("description")?.as_str().map(|s| s.to_string()),
        "WebFetch" => input.get("url")?.as_str().map(|s| s.to_string()),
        "WebSearch" => input.get("query")?.as_str().map(|s| s.to_string()),
        "LSP" => {
            let op = input.get("operation")?.as_str()?;
            let file = input.get("filePath")?.as_str()?;
            Some(format!("{} @ {}", op, file))
        }
        "NotebookEdit" => input.get("notebook_path")?.as_str().map(|s| s.to_string()),
        "TodoWrite" => Some("updating todo list".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ralph_proto::json_rpc::{RpcEvent, TerminationReason};
    use serde_json::json;

    fn make_state() -> Arc<Mutex<TuiState>> {
        Arc::new(Mutex::new(TuiState::new()))
    }

    #[test]
    fn test_loop_started_sets_timer() {
        let state = make_state();
        {
            let mut s = state.lock().unwrap();
            s.loop_started = None;
        }

        let event = RpcEvent::LoopStarted {
            prompt: "test".to_string(),
            max_iterations: Some(10),
            backend: "claude".to_string(),
            started_at: 0,
        };
        apply_rpc_event(&event, &state);

        let s = state.lock().unwrap();
        assert!(s.loop_started.is_some());
        assert_eq!(s.max_iterations, Some(10));
    }

    #[test]
    fn test_iteration_start_creates_buffer() {
        let state = make_state();

        let event = RpcEvent::IterationStart {
            iteration: 1,
            max_iterations: Some(10),
            hat: "builder".to_string(),
            hat_display: "🔨Builder".to_string(),
            backend: "claude".to_string(),
            started_at: 0,
        };
        apply_rpc_event(&event, &state);

        let s = state.lock().unwrap();
        assert_eq!(s.total_iterations(), 1);
        assert_eq!(s.iteration, 1);
    }

    #[test]
    fn test_text_delta_appends_content() {
        let state = make_state();

        // Start an iteration first
        let start_event = RpcEvent::IterationStart {
            iteration: 1,
            max_iterations: None,
            hat: "builder".to_string(),
            hat_display: "🔨Builder".to_string(),
            backend: "claude".to_string(),
            started_at: 0,
        };
        apply_rpc_event(&start_event, &state);

        // Now add text
        let text_event = RpcEvent::TextDelta {
            iteration: 1,
            delta: "Hello world".to_string(),
        };
        apply_rpc_event(&text_event, &state);

        let s = state.lock().unwrap();
        let lines = s.iterations[0].lines.lock().unwrap();
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_tool_call_start_adds_header() {
        let state = make_state();

        // Start an iteration first
        let start_event = RpcEvent::IterationStart {
            iteration: 1,
            max_iterations: None,
            hat: "builder".to_string(),
            hat_display: "🔨Builder".to_string(),
            backend: "claude".to_string(),
            started_at: 0,
        };
        apply_rpc_event(&start_event, &state);

        let tool_event = RpcEvent::ToolCallStart {
            iteration: 1,
            tool_name: "Bash".to_string(),
            tool_call_id: "tool_1".to_string(),
            input: json!({"command": "ls -la"}),
        };
        apply_rpc_event(&tool_event, &state);

        let s = state.lock().unwrap();
        let lines = s.iterations[0].lines.lock().unwrap();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_loop_terminated_marks_complete() {
        let state = make_state();

        let event = RpcEvent::LoopTerminated {
            reason: TerminationReason::Completed,
            total_iterations: 5,
            duration_ms: 10000,
            total_cost_usd: 0.25,
            terminated_at: 0,
        };
        apply_rpc_event(&event, &state);

        let s = state.lock().unwrap();
        assert!(s.loop_completed);
        assert_eq!(s.iteration, 5);
    }

    #[test]
    fn test_task_counts_updated() {
        let state = make_state();

        let event = RpcEvent::TaskCountsUpdated {
            total: 10,
            open: 3,
            closed: 7,
            ready: 2,
        };
        apply_rpc_event(&event, &state);

        let s = state.lock().unwrap();
        assert_eq!(s.task_counts.total, 10);
        assert_eq!(s.task_counts.open, 3);
        assert_eq!(s.task_counts.closed, 7);
        assert_eq!(s.task_counts.ready, 2);
    }

    #[test]
    fn test_small_text_deltas_form_flowing_paragraph_not_one_per_line() {
        // Simulates Pi's text_delta streaming pattern: many small deltas (5-50
        // chars each) without newlines. The TUI should render these as flowing
        // paragraph text, NOT one line per delta.
        let state = make_state();

        // Start an iteration
        apply_rpc_event(
            &RpcEvent::IterationStart {
                iteration: 1,
                max_iterations: None,
                hat: "scoper".to_string(),
                hat_display: "🔎 Scoper".to_string(),
                backend: "pi".to_string(),
                started_at: 0,
            },
            &state,
        );

        // Send 8 small text deltas (typical Pi streaming pattern)
        let deltas = vec![
            "Rust",
            " is a systems",
            " programming language",
            " that runs",
            " blazingly fast,",
            " prevents segfaults,",
            " and guarantees",
            " thread safety.",
        ];
        for delta in deltas {
            apply_rpc_event(
                &RpcEvent::TextDelta {
                    iteration: 1,
                    delta: delta.to_string(),
                },
                &state,
            );
        }

        let s = state.lock().unwrap();
        let lines = s.iterations[0].lines.lock().unwrap();

        // With 8 small deltas forming ~120 chars of text (no newlines), the
        // result should be a flowing paragraph (1-3 lines when wrapped at
        // typical terminal width), NOT 8 separate lines.
        assert!(
            lines.len() <= 3,
            "Small text deltas without newlines should form a flowing paragraph, \
             not one line per delta. Expected <= 3 lines but got {} lines: {:?}",
            lines.len(),
            lines.iter().map(|l| l.to_string()).collect::<Vec<_>>()
        );

        // Verify the full text is present and contiguous
        let full_text: String = lines.iter().map(|l| l.to_string()).collect();
        assert!(
            full_text.contains("Rust is a systems programming language"),
            "Text should flow as a paragraph. Got: {:?}",
            full_text
        );
    }

    #[test]
    fn test_text_deltas_frozen_by_tool_call_preserve_order() {
        // When text deltas are followed by a tool call, the accumulated text
        // should be frozen and appear before the tool call line.
        let state = make_state();

        apply_rpc_event(
            &RpcEvent::IterationStart {
                iteration: 1,
                max_iterations: None,
                hat: "builder".to_string(),
                hat_display: "🔨 Builder".to_string(),
                backend: "pi".to_string(),
                started_at: 0,
            },
            &state,
        );

        // Send streaming text, then a tool call, then more text
        for delta in ["I'll ", "review ", "the code."] {
            apply_rpc_event(
                &RpcEvent::TextDelta {
                    iteration: 1,
                    delta: delta.to_string(),
                },
                &state,
            );
        }

        apply_rpc_event(
            &RpcEvent::ToolCallStart {
                iteration: 1,
                tool_name: "Read".to_string(),
                tool_call_id: "t1".to_string(),
                input: json!({"file_path": "src/main.rs"}),
            },
            &state,
        );

        for delta in ["Now ", "checking."] {
            apply_rpc_event(
                &RpcEvent::TextDelta {
                    iteration: 1,
                    delta: delta.to_string(),
                },
                &state,
            );
        }

        let s = state.lock().unwrap();
        let lines = s.iterations[0].lines.lock().unwrap();
        let line_strs: Vec<String> = lines.iter().map(|l| l.to_string()).collect();

        // Find positions: text1 should be before tool, text2 after tool
        let text1_idx = line_strs.iter().position(|l| l.contains("review the code"));
        let tool_idx = line_strs.iter().position(|l| l.contains("Read"));
        let text2_idx = line_strs.iter().position(|l| l.contains("checking"));

        assert!(
            text1_idx.is_some(),
            "text1 should be present: {:?}",
            line_strs
        );
        assert!(
            tool_idx.is_some(),
            "tool should be present: {:?}",
            line_strs
        );
        assert!(
            text2_idx.is_some(),
            "text2 should be present: {:?}",
            line_strs
        );

        assert!(
            text1_idx.unwrap() < tool_idx.unwrap(),
            "text1 should come before tool: {:?}",
            line_strs
        );
        assert!(
            tool_idx.unwrap() < text2_idx.unwrap(),
            "tool should come before text2: {:?}",
            line_strs
        );
    }

    #[test]
    fn test_format_tool_summary() {
        // Primary key: "path" (Claude Code convention)
        assert_eq!(
            format_tool_summary("Read", &json!({"path": "/foo/bar.rs"})),
            Some("/foo/bar.rs".to_string())
        );
        // Fallback key: "file_path"
        assert_eq!(
            format_tool_summary("Edit", &json!({"file_path": "/foo/bar.rs"})),
            Some("/foo/bar.rs".to_string())
        );
        assert_eq!(
            format_tool_summary("Bash", &json!({"command": "ls"})),
            Some("ls".to_string())
        );
        assert_eq!(format_tool_summary("Unknown", &json!({})), None);
    }

    #[test]
    fn test_truncate() {
        use crate::text_renderer::truncate;
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello...");
    }

    // =========================================================================
    // Subprocess Death Detection Tests
    // =========================================================================

    #[tokio::test]
    async fn test_eof_without_events_sets_subprocess_error() {
        // Given a reader that immediately returns EOF (simulating subprocess death)
        let empty_input: &[u8] = b"";
        let state = make_state();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

        // When the event reader runs to completion
        run_rpc_event_reader(empty_input, state.clone(), cancel_rx).await;

        // Then subprocess_error should be set (subprocess died before sending events)
        let s = state.lock().unwrap();
        assert!(
            s.subprocess_error.is_some(),
            "should set subprocess_error on EOF without events"
        );
        assert!(s.loop_completed, "should mark loop as completed");
    }

    #[tokio::test]
    async fn test_eof_without_events_creates_error_iteration() {
        // Given a reader that immediately returns EOF
        let empty_input: &[u8] = b"";
        let state = make_state();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

        // When the event reader runs to completion
        run_rpc_event_reader(empty_input, state.clone(), cancel_rx).await;

        // Then an error iteration should be created so the content pane has something to show
        let s = state.lock().unwrap();
        assert_eq!(s.total_iterations(), 1, "should create one error iteration");
        let lines = s.iterations[0].lines.lock().unwrap();
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            text.contains("Subprocess exited"),
            "error iteration should contain error message, got: {}",
            text
        );
    }

    #[tokio::test]
    async fn test_eof_after_loop_started_does_not_set_subprocess_error() {
        // Given a reader with a valid LoopStarted event then EOF
        let event = RpcEvent::LoopStarted {
            prompt: "test".to_string(),
            max_iterations: Some(10),
            backend: "claude".to_string(),
            started_at: 0,
        };
        let line = format!("{}\n", serde_json::to_string(&event).unwrap());
        let state = make_state();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

        // When the event reader processes the event and then hits EOF
        run_rpc_event_reader(line.as_bytes(), state.clone(), cancel_rx).await;

        // Then subprocess_error should NOT be set (subprocess ran normally)
        let s = state.lock().unwrap();
        assert!(
            s.subprocess_error.is_none(),
            "should NOT set subprocess_error when events were received"
        );
    }
}
