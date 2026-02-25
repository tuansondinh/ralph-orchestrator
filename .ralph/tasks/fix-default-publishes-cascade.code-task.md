---
status: pending
created: 2026-02-25
---
# Task: Fix default_publishes Cascade on Silent Agent Iterations

## Description
When an agent writes zero events during an iteration, `default_publishes` unconditionally advances the hat state machine. In a multi-hat chain, consecutive silent iterations cascade through every hat to `LOOP_COMPLETE`, terminating the loop with zero work done. This is especially harmful for parallel worktree loops where agent silence is more likely (fresh context, missing env, API errors).

Fix with two layers:
1. **Runtime guard**: Track consecutive default injections in `LoopState`. After one default fires without any real agent events in between, block further defaults and terminate with a new `CascadingDefaults` reason.
2. **Config validation**: Reject any hat where `default_publishes` equals the `completion_promise`. The completion event should always require explicit agent action.

GitHub issue: https://github.com/mikeyobrien/ralph-orchestrator/issues/187

## Background
`default_publishes` was designed as a convenience fallback — if a hat's agent forgets to emit an event, the orchestrator injects a sensible default so the chain doesn't stall. The problem is it treats silence identically to success. When the agent is truly broken (crashed, confused, API error), each iteration fires `default_publishes` for the active hat, which triggers the next hat, which also fires its default, cascading to loop completion.

The existing pattern for this kind of guard already exists in `LoopState`: `consecutive_failures`, `consecutive_malformed_events`, and `abandoned_task_redispatches` all track degenerate iteration patterns and feed into `check_termination()`. This fix follows the same pattern.

Three independent models (o3, Sonnet 4, Gemini 3.1) analyzed this bug and unanimously recommended Option 4 (consecutive default limiting) as the core fix, with two of three also recommending Option 1 (completion promise guard) as defense-in-depth.

## Reference Documentation
**Required:**
- Event loop core: `crates/ralph-core/src/event_loop/mod.rs` — `check_default_publishes()` (~line 1362), `check_termination()` (~line 434), `TerminationReason` enum (~line 38)
- Loop runner: `crates/ralph-cli/src/loop_runner.rs` — default injection block (~line 1083)
- Loop state: `crates/ralph-core/src/event_loop/loop_state.rs` — `LoopState` struct
- Config validation: `crates/ralph-core/src/config.rs` — `validate()` method, `HatConfig` struct (~line 1262)

**Additional References:**
- Existing default_publishes tests: `crates/ralph-core/src/event_loop/tests.rs` (~line 1026)
- **New cascade reproduction test**: `crates/ralph-core/src/event_loop/tests.rs` — `test_default_publishes_cascade_on_silent_agent` (documents the bug; assertions must be flipped when fix lands)
- Existing scenario test: `crates/ralph-core/tests/scenarios/default_publishes.yml`

## Technical Requirements

### Layer 1: Runtime Cascade Guard
1. Add `consecutive_default_publishes: u32` field to `LoopState` in `loop_state.rs`
2. In `loop_runner.rs`: reset counter to 0 when `agent_wrote_events` is true
3. In `loop_runner.rs`: before calling `check_default_publishes`, skip if counter > 0 (one consecutive default allowed, second blocked)
4. After injecting a default, increment the counter
5. Add `CascadingDefaults` variant to `TerminationReason` enum with exit code 1 and reason string `"cascading_defaults"`
6. In `check_termination()`: return `CascadingDefaults` when `consecutive_default_publishes >= 2`

### Layer 2: Config Validation
7. In `config.rs` `validate()`: for each hat, if `default_publishes` is `Some` and equals `completion_promise`, push a validation error
8. Error message: `"Hat '{hat_id}' has default_publishes='{topic}' which matches the completion promise. The completion event must be emitted explicitly by the agent, not injected as a default. Remove default_publishes from this hat or change it to a non-completion event."`

### Wiring
9. Add `CascadingDefaults` to all `TerminationReason` match arms (exit_code, as_str, is_success, loop terminate message, TUI display, history recording)
10. Update `print_termination` and any TUI rendering that matches on `TerminationReason`

## Dependencies
- No new crate dependencies
- Follows existing `consecutive_failures` / `consecutive_malformed_events` patterns in `LoopState`

## Implementation Approach
1. Add field to `LoopState`, default to 0
2. Add `CascadingDefaults` to `TerminationReason` and wire all match arms
3. Add validation check in `config.rs` `validate()`
4. Modify `loop_runner.rs` default injection block: reset/skip/increment logic
5. Add check in `check_termination()` for the new counter
6. Write unit tests
7. Update existing `default_publishes.yml` scenario or add new one
8. Run `cargo test` to confirm no regressions

## Acceptance Criteria

1. **Silent cascade is blocked**
   - Given a hat chain where all hats have `default_publishes` and a silent agent backend
   - When the loop runs
   - Then at most one `default_publishes` fires before the loop terminates with `CascadingDefaults`

2. **Single default still works**
   - Given a hat with `default_publishes` and a silent agent in one iteration
   - When the agent writes real events in the next iteration
   - Then the counter resets and defaults work again later

3. **Completion promise rejected in config**
   - Given a hat config where `default_publishes` equals the `completion_promise`
   - When `validate()` runs
   - Then a validation error is returned with an actionable message

4. **Legitimate configs unaffected**
   - Given existing preset configs (feature, code-assist, etc.) that don't set `default_publishes` to the completion promise
   - When `validate()` runs
   - Then no new validation errors are produced

5. **Existing presets updated**
   - Given `presets/bugfix.yml` and `crates/ralph-cli/presets/bugfix.yml` where the `committer` hat has `default_publishes: "LOOP_COMPLETE"`
   - When the fix lands
   - Then those presets have `default_publishes` removed from the committer hat (or changed to a non-completion event)

6. **New termination reason wired correctly**
   - Given `CascadingDefaults` termination
   - When the loop exits
   - Then exit code is 1, reason string is `"cascading_defaults"`, and the terminate event payload reflects this

7. **Existing tests pass**
   - Given the full test suite
   - When `cargo test` runs
   - Then all existing tests pass, including the 4 existing `default_publishes` tests

8. **New unit tests cover the guard**
   - Given the new code
   - When tests run
   - Then there are tests for: cascade blocked after 2 consecutive defaults, counter reset on real events, config rejection of completion-promise defaults, `CascadingDefaults` termination reason properties

9. **Existing cascade reproduction test flipped**
   - Given `test_default_publishes_cascade_on_silent_agent` which currently documents the bug
   - When the fix lands
   - Then flip the assertions to verify: `injected_defaults.len() <= 1` and `!injected_defaults.contains("LOOP_COMPLETE")`

## Metadata
- **Complexity**: Medium
- **Labels**: Bug Fix, Event Loop, default_publishes, Parallel Loops, Worktree
- **Required Skills**: Rust, ralph event loop architecture, LoopState patterns
