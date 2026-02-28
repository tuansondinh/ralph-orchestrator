//! Minimal BDD runner for hooks acceptance placeholders.
//!
//! Step 0 scaffolding intentionally keeps all AC scenarios red while wiring:
//! - feature discovery from `features/hooks/*.feature`
//! - placeholder step-definition matching
//! - deterministic CI-safe execution path

use crate::executor::find_workspace_root;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

const HOOKS_FEATURE_DIR_WORKSPACE: &str = "crates/ralph-e2e/features/hooks";
const HOOKS_FEATURE_DIR_CRATE: &str = "features/hooks";

/// Configuration for executing the hooks BDD placeholder suite.
#[derive(Debug, Clone, Default)]
pub struct HooksBddConfig {
    /// Optional scenario filter (matches id, scenario title, tags, or feature filename).
    pub filter: Option<String>,
    /// Whether the suite is being executed in CI-safe mode.
    pub ci_safe_mode: bool,
}

impl HooksBddConfig {
    /// Creates a new hooks BDD run configuration.
    pub fn new(filter: Option<String>, ci_safe_mode: bool) -> Self {
        Self {
            filter,
            ci_safe_mode,
        }
    }
}

/// Discovery/execution errors for hooks BDD scaffolding.
#[derive(Debug, Error)]
pub enum HooksBddError {
    /// Workspace root could not be determined.
    #[error("workspace root not found")]
    WorkspaceRootNotFound,

    /// Hooks feature directory could not be found.
    #[error("hooks feature directory not found: {0}")]
    HooksFeatureDirNotFound(PathBuf),

    /// Failed to read the hooks feature directory.
    #[error("failed to read hooks feature directory {path}: {source}")]
    ReadFeatureDir {
        /// Path that failed to read.
        path: PathBuf,
        /// Source IO error.
        #[source]
        source: std::io::Error,
    },

    /// Failed to read a feature file.
    #[error("failed to read feature file {path}: {source}")]
    ReadFeatureFile {
        /// Feature file path.
        path: PathBuf,
        /// Source IO error.
        #[source]
        source: std::io::Error,
    },

    /// Feature file was malformed for the minimal parser.
    #[error("invalid feature file {path}: {reason}")]
    InvalidFeatureFile {
        /// Feature file path.
        path: PathBuf,
        /// Validation reason.
        reason: String,
    },
}

/// One discovered hooks BDD scenario from a `.feature` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HooksBddScenario {
    /// Stable AC ID tag when present (e.g. `AC-01`).
    pub scenario_id: String,
    /// Scenario title from `Scenario:` line.
    pub scenario_name: String,
    /// Feature file name (e.g. `scope-and-dispatch.feature`).
    pub feature_file: String,
    /// Scenario tags without `@` prefix.
    pub tags: Vec<String>,
    steps: Vec<HooksStep>,
}

/// Result of executing one placeholder scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HooksBddScenarioResult {
    /// Stable AC ID tag (or fallback scenario title if tag missing).
    pub scenario_id: String,
    /// Scenario title.
    pub scenario_name: String,
    /// Feature file name.
    pub feature_file: String,
    /// Whether the scenario passed.
    pub passed: bool,
    /// Pass/fail reason for terminal output.
    pub message: String,
}

/// Aggregated hooks BDD run results.
#[derive(Debug, Clone, Default)]
pub struct HooksBddRunResults {
    /// Individual scenario results in deterministic file/scenario order.
    pub results: Vec<HooksBddScenarioResult>,
}

impl HooksBddRunResults {
    /// Total number of executed scenarios.
    pub fn total_count(&self) -> usize {
        self.results.len()
    }

    /// Number of passed scenarios.
    pub fn passed_count(&self) -> usize {
        self.results.iter().filter(|result| result.passed).count()
    }

    /// Number of failed scenarios.
    pub fn failed_count(&self) -> usize {
        self.results.iter().filter(|result| !result.passed).count()
    }

    /// Returns true when every scenario passed.
    pub fn all_passed(&self) -> bool {
        self.results.iter().all(|result| result.passed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HooksStepKeyword {
    Given,
    When,
    Then,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HooksStep {
    keyword: HooksStepKeyword,
    text: String,
}

#[derive(Debug, Default)]
struct HooksStepContext {
    criterion_id: Option<String>,
    ci_safe_confirmed: bool,
}

/// Discovers hook BDD scenarios from `features/hooks/*.feature`.
pub fn discover_hooks_bdd_scenarios(
    filter: Option<&str>,
) -> Result<Vec<HooksBddScenario>, HooksBddError> {
    let hooks_dir = hooks_feature_dir()?;
    let mut feature_paths: Vec<PathBuf> = fs::read_dir(&hooks_dir)
        .map_err(|source| HooksBddError::ReadFeatureDir {
            path: hooks_dir.clone(),
            source,
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "feature"))
        .collect();

    feature_paths.sort();

    let mut scenarios = Vec::new();
    for feature_path in &feature_paths {
        scenarios.extend(parse_feature_file(feature_path)?);
    }

    if let Some(filter_text) = filter {
        let filter_lower = filter_text.to_lowercase();
        scenarios.retain(|scenario| matches_filter(scenario, &filter_lower));
    }

    Ok(scenarios)
}

/// Executes discovered hooks BDD placeholder scenarios.
///
/// This intentionally keeps placeholder scenarios red until full hooks behavior is implemented.
pub fn run_hooks_bdd_suite(config: &HooksBddConfig) -> Result<HooksBddRunResults, HooksBddError> {
    let scenarios = discover_hooks_bdd_scenarios(config.filter.as_deref())?;
    let mut results = Vec::with_capacity(scenarios.len());

    for scenario in scenarios {
        results.push(execute_placeholder_scenario(&scenario, config.ci_safe_mode));
    }

    Ok(HooksBddRunResults { results })
}

fn hooks_feature_dir() -> Result<PathBuf, HooksBddError> {
    let manifest_candidate =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(HOOKS_FEATURE_DIR_CRATE);
    if manifest_candidate.is_dir() {
        return Ok(manifest_candidate);
    }

    let workspace_root = find_workspace_root().ok_or(HooksBddError::WorkspaceRootNotFound)?;
    let workspace_candidate = workspace_root.join(HOOKS_FEATURE_DIR_WORKSPACE);
    if workspace_candidate.is_dir() {
        return Ok(workspace_candidate);
    }

    let crate_relative_candidate = workspace_root.join(HOOKS_FEATURE_DIR_CRATE);
    if crate_relative_candidate.is_dir() {
        return Ok(crate_relative_candidate);
    }

    Err(HooksBddError::HooksFeatureDirNotFound(workspace_candidate))
}

fn parse_feature_file(path: &Path) -> Result<Vec<HooksBddScenario>, HooksBddError> {
    let content = fs::read_to_string(path).map_err(|source| HooksBddError::ReadFeatureFile {
        path: path.to_path_buf(),
        source,
    })?;

    let feature_file = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| HooksBddError::InvalidFeatureFile {
            path: path.to_path_buf(),
            reason: "missing file name".to_string(),
        })?;

    parse_feature_content(&content, &feature_file).map_err(|reason| {
        HooksBddError::InvalidFeatureFile {
            path: path.to_path_buf(),
            reason,
        }
    })
}

fn parse_feature_content(
    content: &str,
    feature_file: &str,
) -> Result<Vec<HooksBddScenario>, String> {
    let mut scenarios = Vec::new();
    let mut feature_tags: Vec<String> = Vec::new();
    let mut pending_tags: Vec<String> = Vec::new();
    let mut current_scenario: Option<ScenarioBuilder> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed.starts_with('@') {
            pending_tags.extend(parse_tags(trimmed));
            continue;
        }

        if let Some(_feature_name) = trimmed.strip_prefix("Feature:") {
            feature_tags = std::mem::take(&mut pending_tags);
            continue;
        }

        if let Some(scenario_name) = trimmed.strip_prefix("Scenario:") {
            if let Some(builder) = current_scenario.take() {
                scenarios.push(builder.build(feature_file));
            }

            let mut tags = feature_tags.clone();
            tags.extend(std::mem::take(&mut pending_tags));

            current_scenario = Some(ScenarioBuilder::new(scenario_name.trim().to_string(), tags));
            continue;
        }

        if let Some((keyword, step_text)) = parse_step(trimmed)
            && let Some(builder) = &mut current_scenario
        {
            builder.steps.push(HooksStep {
                keyword,
                text: step_text.to_string(),
            });
        }
    }

    if let Some(builder) = current_scenario.take() {
        scenarios.push(builder.build(feature_file));
    }

    if scenarios.is_empty() {
        return Err("no scenarios discovered".to_string());
    }

    Ok(scenarios)
}

fn parse_tags(line: &str) -> Vec<String> {
    line.split_whitespace()
        .filter_map(|tag| tag.strip_prefix('@'))
        .map(ToString::to_string)
        .collect()
}

fn parse_step(line: &str) -> Option<(HooksStepKeyword, &str)> {
    if let Some(text) = line.strip_prefix("Given ") {
        return Some((HooksStepKeyword::Given, text));
    }

    if let Some(text) = line.strip_prefix("When ") {
        return Some((HooksStepKeyword::When, text));
    }

    line.strip_prefix("Then ")
        .map(|text| (HooksStepKeyword::Then, text))
}

fn matches_filter(scenario: &HooksBddScenario, filter_lower: &str) -> bool {
    scenario.scenario_id.to_lowercase().contains(filter_lower)
        || scenario.scenario_name.to_lowercase().contains(filter_lower)
        || scenario.feature_file.to_lowercase().contains(filter_lower)
        || scenario
            .tags
            .iter()
            .any(|tag| tag.to_lowercase().contains(filter_lower))
}

fn execute_placeholder_scenario(
    scenario: &HooksBddScenario,
    ci_safe_mode: bool,
) -> HooksBddScenarioResult {
    let mut context = HooksStepContext::default();

    for step in &scenario.steps {
        if let Err(message) = execute_step_definition(step, &mut context, ci_safe_mode) {
            return HooksBddScenarioResult {
                scenario_id: scenario.scenario_id.clone(),
                scenario_name: scenario.scenario_name.clone(),
                feature_file: scenario.feature_file.clone(),
                passed: false,
                message,
            };
        }
    }

    HooksBddScenarioResult {
        scenario_id: scenario.scenario_id.clone(),
        scenario_name: scenario.scenario_name.clone(),
        feature_file: scenario.feature_file.clone(),
        passed: false,
        message: format!(
            "pending: {} placeholder remains red until hooks implementation lands",
            scenario.scenario_id
        ),
    }
}

fn execute_step_definition(
    step: &HooksStep,
    context: &mut HooksStepContext,
    ci_safe_mode: bool,
) -> Result<(), String> {
    match step.keyword {
        HooksStepKeyword::Given => {
            let criterion_id = parse_given_placeholder_step(&step.text)
                .ok_or_else(|| format!("missing Given step definition for '{}'", step.text))?;
            context.criterion_id = Some(criterion_id);
            Ok(())
        }
        HooksStepKeyword::When => {
            if step.text != "the hooks BDD suite is executed in CI-safe mode" {
                return Err(format!("missing When step definition for '{}'", step.text));
            }

            if !ci_safe_mode {
                return Err("CI-safe mode not enabled; rerun hooks BDD with --mock".to_string());
            }

            context.ci_safe_confirmed = true;
            Ok(())
        }
        HooksStepKeyword::Then => {
            let reported_id = parse_then_reported_step(&step.text)
                .ok_or_else(|| format!("missing Then step definition for '{}'", step.text))?;

            let Some(given_id) = context.criterion_id.as_deref() else {
                return Err("Then step executed before criterion was captured in Given".to_string());
            };

            if !context.ci_safe_confirmed {
                return Err("CI-safe execution step not satisfied before Then".to_string());
            }

            if given_id != reported_id {
                return Err(format!(
                    "criterion mismatch: Given='{}', Then='{}'",
                    given_id, reported_id
                ));
            }

            Err(format!(
                "pending: {} placeholder scenario intentionally red until implementation",
                reported_id
            ))
        }
    }
}

fn parse_given_placeholder_step(text: &str) -> Option<String> {
    let prefix = "hooks acceptance criterion \"";
    let suffix = "\" is defined as a placeholder";

    text.strip_prefix(prefix)
        .and_then(|remaining| remaining.strip_suffix(suffix))
        .map(ToString::to_string)
}

fn parse_then_reported_step(text: &str) -> Option<&str> {
    let prefix = "scenario \"";
    let suffix = "\" is reported for later implementation";

    text.strip_prefix(prefix)
        .and_then(|remaining| remaining.strip_suffix(suffix))
}

#[derive(Debug, Clone)]
struct ScenarioBuilder {
    scenario_name: String,
    tags: Vec<String>,
    steps: Vec<HooksStep>,
}

impl ScenarioBuilder {
    fn new(scenario_name: String, tags: Vec<String>) -> Self {
        Self {
            scenario_name,
            tags,
            steps: Vec::new(),
        }
    }

    fn build(self, feature_file: &str) -> HooksBddScenario {
        let scenario_id = self
            .tags
            .iter()
            .find(|tag| is_acceptance_id(tag))
            .cloned()
            .unwrap_or_else(|| self.scenario_name.clone());

        HooksBddScenario {
            scenario_id,
            scenario_name: self.scenario_name,
            feature_file: feature_file.to_string(),
            tags: self.tags,
            steps: self.steps,
        }
    }
}

fn is_acceptance_id(tag: &str) -> bool {
    let Some(suffix) = tag.strip_prefix("AC-") else {
        return false;
    };

    suffix.len() == 2 && suffix.chars().all(|character| character.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_hooks_bdd_scenarios_finds_all_placeholder_scenarios() {
        let scenarios = discover_hooks_bdd_scenarios(None).expect("should discover scenarios");
        let scenario_ids: Vec<&str> = scenarios
            .iter()
            .map(|scenario| scenario.scenario_id.as_str())
            .collect();

        assert_eq!(scenarios.len(), 18);
        assert!(scenario_ids.contains(&"AC-01"));
        assert!(scenario_ids.contains(&"AC-18"));
    }

    #[test]
    fn discover_hooks_bdd_scenarios_applies_filter() {
        let scenarios =
            discover_hooks_bdd_scenarios(Some("AC-03")).expect("filtered discovery should work");

        assert_eq!(scenarios.len(), 1);
        assert_eq!(scenarios[0].scenario_id, "AC-03");
    }

    #[test]
    fn run_hooks_bdd_suite_is_red_in_ci_safe_mode() {
        let config = HooksBddConfig::new(Some("AC-01".to_string()), true);
        let results = run_hooks_bdd_suite(&config).expect("suite should run");

        assert_eq!(results.total_count(), 1);
        assert_eq!(results.failed_count(), 1);
        assert!(results.results[0].message.contains("pending"));
    }

    #[test]
    fn run_hooks_bdd_suite_reports_ci_safe_guard_failure_without_mock() {
        let config = HooksBddConfig::new(Some("AC-01".to_string()), false);
        let results = run_hooks_bdd_suite(&config).expect("suite should run");

        assert_eq!(results.total_count(), 1);
        assert_eq!(results.failed_count(), 1);
        assert!(
            results.results[0]
                .message
                .contains("CI-safe mode not enabled")
        );
    }

    #[test]
    fn parse_feature_content_parses_scenario_tags_and_steps() {
        let content = r#"
@hooks
Feature: Example

  @AC-42
  Scenario: AC-42 Example scenario
    Given hooks acceptance criterion "AC-42" is defined as a placeholder
    When the hooks BDD suite is executed in CI-safe mode
    Then scenario "AC-42" is reported for later implementation
"#;

        let scenarios = parse_feature_content(content, "example.feature").expect("parse succeeds");

        assert_eq!(scenarios.len(), 1);
        assert_eq!(scenarios[0].scenario_id, "AC-42");
        assert_eq!(scenarios[0].feature_file, "example.feature");
        assert_eq!(scenarios[0].steps.len(), 3);
    }

    #[test]
    fn parse_feature_content_requires_at_least_one_scenario() {
        let content = "Feature: Empty";
        let error = parse_feature_content(content, "empty.feature").expect_err("must fail");
        assert!(error.contains("no scenarios discovered"));
    }

    #[test]
    fn execute_step_definition_rejects_mismatched_acceptance_ids() {
        let step = HooksStep {
            keyword: HooksStepKeyword::Then,
            text: "scenario \"AC-02\" is reported for later implementation".to_string(),
        };

        let mut context = HooksStepContext {
            criterion_id: Some("AC-01".to_string()),
            ci_safe_confirmed: true,
        };

        let error = execute_step_definition(&step, &mut context, true).expect_err("must fail");
        assert!(error.contains("criterion mismatch"));
    }
}
