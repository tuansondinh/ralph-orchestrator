use crate::config::{
    HookDefaults, HookMutationConfig, HookOnError, HookPhaseEvent, HookSpec, HookSuspendMode,
    HooksConfig,
};
use std::collections::HashMap;
use std::path::PathBuf;

/// Resolves configured hooks for a lifecycle phase-event.
#[derive(Debug, Clone)]
pub struct HookEngine {
    defaults: HookDefaults,
    hooks_by_phase_event: HashMap<HookPhaseEvent, Vec<HookSpec>>,
}

impl HookEngine {
    /// Creates a hook engine from validated hook configuration.
    #[must_use]
    pub fn new(config: &HooksConfig) -> Self {
        Self {
            defaults: config.defaults.clone(),
            hooks_by_phase_event: config.events.clone(),
        }
    }

    /// Resolves hooks for a canonical phase-event key in declaration order.
    #[must_use]
    pub fn resolve_phase_event(&self, phase_event: HookPhaseEvent) -> Vec<ResolvedHookSpec> {
        self.hooks_by_phase_event
            .get(&phase_event)
            .map(|hooks| {
                hooks
                    .iter()
                    .enumerate()
                    .map(|(declaration_order, hook)| {
                        ResolvedHookSpec::from_spec(
                            phase_event,
                            declaration_order,
                            &self.defaults,
                            hook,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolves hooks by phase-event string key.
    ///
    /// Unknown phase-event keys return an empty list.
    #[must_use]
    pub fn resolve_phase_event_str(&self, phase_event: &str) -> Vec<ResolvedHookSpec> {
        HookPhaseEvent::parse(phase_event)
            .map(|phase| self.resolve_phase_event(phase))
            .unwrap_or_default()
    }
}

/// Hook spec with defaults materialized for runtime dispatch.
#[derive(Debug, Clone)]
pub struct ResolvedHookSpec {
    pub phase_event: HookPhaseEvent,
    pub declaration_order: usize,
    pub name: String,
    pub command: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub timeout_seconds: u64,
    pub max_output_bytes: u64,
    pub on_error: HookOnError,
    pub suspend_mode: HookSuspendMode,
    pub mutate: HookMutationConfig,
}

impl ResolvedHookSpec {
    fn from_spec(
        phase_event: HookPhaseEvent,
        declaration_order: usize,
        defaults: &HookDefaults,
        spec: &HookSpec,
    ) -> Self {
        Self {
            phase_event,
            declaration_order,
            name: spec.name.clone(),
            command: spec.command.clone(),
            cwd: spec.cwd.clone(),
            env: spec.env.clone(),
            timeout_seconds: spec.timeout_seconds.unwrap_or(defaults.timeout_seconds),
            max_output_bytes: spec.max_output_bytes.unwrap_or(defaults.max_output_bytes),
            on_error: spec.on_error.unwrap_or(HookOnError::Warn),
            suspend_mode: spec.suspend_mode.unwrap_or(defaults.suspend_mode),
            mutate: spec.mutate.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook_spec(name: &str) -> HookSpec {
        HookSpec {
            name: name.to_string(),
            command: vec!["echo".to_string(), name.to_string()],
            cwd: None,
            env: HashMap::new(),
            timeout_seconds: None,
            max_output_bytes: None,
            on_error: Some(HookOnError::Warn),
            suspend_mode: None,
            mutate: HookMutationConfig::default(),
            extra: HashMap::new(),
        }
    }

    fn hooks_config(events: HashMap<HookPhaseEvent, Vec<HookSpec>>) -> HooksConfig {
        HooksConfig {
            enabled: true,
            defaults: HookDefaults {
                timeout_seconds: 45,
                max_output_bytes: 16_384,
                suspend_mode: HookSuspendMode::WaitThenRetry,
            },
            events,
            extra: HashMap::new(),
        }
    }

    #[test]
    fn resolve_phase_event_preserves_declaration_order() {
        let mut events = HashMap::new();
        events.insert(
            HookPhaseEvent::PreLoopStart,
            vec![
                hook_spec("env-guard"),
                hook_spec("workspace-check"),
                hook_spec("notify"),
            ],
        );
        events.insert(HookPhaseEvent::PostLoopStart, vec![hook_spec("post-loop")]);

        let engine = HookEngine::new(&hooks_config(events));
        let resolved = engine.resolve_phase_event(HookPhaseEvent::PreLoopStart);

        assert_eq!(resolved.len(), 3);
        assert_eq!(resolved[0].name, "env-guard");
        assert_eq!(resolved[0].declaration_order, 0);
        assert_eq!(resolved[1].name, "workspace-check");
        assert_eq!(resolved[1].declaration_order, 1);
        assert_eq!(resolved[2].name, "notify");
        assert_eq!(resolved[2].declaration_order, 2);
        assert!(
            resolved
                .iter()
                .all(|hook| hook.phase_event == HookPhaseEvent::PreLoopStart)
        );
    }

    #[test]
    fn resolve_phase_event_applies_defaults_and_per_hook_overrides() {
        let mut hook_with_overrides = hook_spec("manual-gate");
        hook_with_overrides.timeout_seconds = Some(9);
        hook_with_overrides.max_output_bytes = Some(777);
        hook_with_overrides.on_error = Some(HookOnError::Suspend);
        hook_with_overrides.suspend_mode = Some(HookSuspendMode::RetryBackoff);

        let mut events = HashMap::new();
        events.insert(
            HookPhaseEvent::PreIterationStart,
            vec![hook_spec("defaulted"), hook_with_overrides],
        );

        let engine = HookEngine::new(&hooks_config(events));
        let resolved = engine.resolve_phase_event(HookPhaseEvent::PreIterationStart);

        assert_eq!(resolved.len(), 2);

        assert_eq!(resolved[0].timeout_seconds, 45);
        assert_eq!(resolved[0].max_output_bytes, 16_384);
        assert_eq!(resolved[0].on_error, HookOnError::Warn);
        assert_eq!(resolved[0].suspend_mode, HookSuspendMode::WaitThenRetry);

        assert_eq!(resolved[1].timeout_seconds, 9);
        assert_eq!(resolved[1].max_output_bytes, 777);
        assert_eq!(resolved[1].on_error, HookOnError::Suspend);
        assert_eq!(resolved[1].suspend_mode, HookSuspendMode::RetryBackoff);
    }

    #[test]
    fn resolve_phase_event_returns_empty_for_unconfigured_or_unknown_phase() {
        let mut events = HashMap::new();
        events.insert(HookPhaseEvent::PreLoopStart, vec![hook_spec("env-guard")]);

        let engine = HookEngine::new(&hooks_config(events));

        let missing = engine.resolve_phase_event(HookPhaseEvent::PostIterationStart);
        assert!(missing.is_empty());

        let unknown = engine.resolve_phase_event_str("post.nonexistent.event");
        assert!(unknown.is_empty());
    }
}
