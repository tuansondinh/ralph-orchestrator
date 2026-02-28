//! Lifecycle hook runtime contracts.
//!
//! Step 3 scaffolds the `HookExecutor` interfaces here; execution behavior
//! (spawn, stdin, timeout, output truncation) is implemented in follow-up steps.

mod executor;

pub use executor::{
    HookExecutor, HookExecutorContract, HookExecutorError, HookRunRequest, HookRunResult,
    HookStreamOutput,
};
