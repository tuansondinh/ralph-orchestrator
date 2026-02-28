//! Lifecycle hook runtime contracts and orchestration primitives.

mod engine;
mod executor;

pub use crate::config::{
    HookDefaults, HookMutationConfig, HookOnError, HookPhaseEvent, HookSpec, HookSuspendMode,
    HooksConfig,
};
pub use engine::{
    HookEngine, HookInvocationPayload, HookPayloadBuilderInput, HookPayloadContext,
    HookPayloadContextInput, HookPayloadIteration, HookPayloadLoop, HookPayloadMetadata,
    ResolvedHookSpec,
};
pub use executor::{
    HookExecutor, HookExecutorContract, HookExecutorError, HookRunRequest, HookRunResult,
    HookStreamOutput,
};
