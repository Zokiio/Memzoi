use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const RECALL_OPERATIONAL_EVIDENCE_VERSION: &str = "memzoi-recall-operational-evidence/v1";
pub const RECALL_OPERATIONAL_REPORT_VERSION: &str = "memzoi-recall-operational-report/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallOperationalScenario {
    Create,
    Update,
    Supersede,
    Tombstone,
    Expiry,
    Deletion,
    StaleState,
    PartialState,
    Corruption,
    InterruptedBuild,
    AtomicPromotion,
    Rollback,
    ModelReplacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallFallbackScenario {
    MissingModel,
    MissingIndex,
    StaleIndex,
    IncompleteIndex,
    IncompatibleIndex,
    CorruptIndex,
    QueryEmbeddingFailure,
    UnsupportedPlatform,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallTaskUtilityCase {
    pub id: String,
    pub kind: RecallTaskUtilityKind,
    pub completed: bool,
    pub answer_correct: bool,
    pub required_memory_used: bool,
    pub distractor_avoided: bool,
    pub citation_used: bool,
    pub false_confidence_avoided: bool,
    pub context_tokens: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallTaskUtilityKind {
    Answer,
    Coding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallOperationalCase {
    pub id: String,
    pub scenario: RecallOperationalScenario,
    pub canonical_unchanged: bool,
    pub expected_generation_active: bool,
    pub stale_generation_queryable: bool,
    pub deletion_verified: bool,
    pub rollback_verified: bool,
    pub passed: bool,
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallFallbackCase {
    pub id: String,
    pub scenario: RecallFallbackScenario,
    pub reason_code: String,
    pub normalized_lexical_parity: f64,
    pub partial_mixed_output: bool,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallPerformanceEvidence {
    pub record_count: usize,
    pub workload_seed: u64,
    pub warm_query_p50_ms: f64,
    pub warm_query_p95_ms: f64,
    pub cold_start_ms: f64,
    pub first_query_ms: f64,
    pub full_build_ms: f64,
    pub incremental_update_ms: f64,
    pub idle_memory_bytes: u64,
    pub build_memory_bytes: u64,
    pub search_memory_bytes: u64,
    pub model_disk_bytes: u64,
    pub index_disk_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecallCrossPlatformContract {
    ExactVectors,
    RankingTolerance {
        min_top_k_overlap: f64,
        max_rank_displacement: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEnvironmentEvidence {
    pub os: String,
    pub architecture: String,
    pub cpu: String,
    pub memory_bytes: u64,
    pub release_build: bool,
    pub timer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallTraceCounter {
    pub reason_code: RecallTraceReasonCode,
    pub count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallTraceReasonCode {
    SemanticSuccess,
    LexicalFallback,
    SafeSuppression,
    MissingModel,
    MissingIndex,
    StaleIndex,
    IncompleteIndex,
    IncompatibleIndex,
    CorruptIndex,
    QueryEmbeddingFailure,
    UnsupportedPlatform,
    BuildStarted,
    BuildCompleted,
    BuildFailed,
    Promotion,
    Rollback,
}

impl RecallTraceReasonCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SemanticSuccess => "semantic_success",
            Self::LexicalFallback => "lexical_fallback",
            Self::SafeSuppression => "safe_suppression",
            Self::MissingModel => "missing_model",
            Self::MissingIndex => "missing_index",
            Self::StaleIndex => "stale_index",
            Self::IncompleteIndex => "incomplete_index",
            Self::IncompatibleIndex => "incompatible_index",
            Self::CorruptIndex => "corrupt_index",
            Self::QueryEmbeddingFailure => "query_embedding_failure",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::BuildStarted => "build_started",
            Self::BuildCompleted => "build_completed",
            Self::BuildFailed => "build_failed",
            Self::Promotion => "promotion",
            Self::Rollback => "rollback",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallOperationalEvidence {
    pub version: String,
    pub candidate_digest: String,
    pub release_build_digest: String,
    pub lexical_projection_digest: String,
    pub environment: RecallEnvironmentEvidence,
    pub task_utility: Vec<RecallTaskUtilityCase>,
    pub operational: Vec<RecallOperationalCase>,
    pub fallback: Vec<RecallFallbackCase>,
    pub performance: RecallPerformanceEvidence,
    pub cross_platform: RecallCrossPlatformContract,
    pub deterministic_ties: bool,
    pub policy_gates_preserved: bool,
    pub trace_counters: Vec<RecallTraceCounter>,
    pub warm_p95_limit_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallOperationalReport {
    pub version: String,
    pub evidence_digest: String,
    pub candidate_digest: String,
    pub release_build_digest: String,
    pub task_utility_pass_rate: f64,
    pub task_utility_context_tokens: usize,
    pub operational_pass_rate: f64,
    pub fallback_parity: f64,
    pub workload_digest: String,
    pub performance: RecallPerformanceEvidence,
    pub environment: RecallEnvironmentEvidence,
    pub cross_platform: RecallCrossPlatformContract,
    pub trace_counters: BTreeMap<String, u64>,
    pub gates: RecallOperationalGates,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallOperationalGates {
    pub task_utility: bool,
    pub operational_coverage: bool,
    pub operational_behavior: bool,
    pub fallback_coverage: bool,
    pub fallback_parity: bool,
    pub workload_size: bool,
    pub warm_p95: bool,
    pub reproducible_environment: bool,
    pub cross_platform: bool,
    pub privacy_safe_trace: bool,
}

impl RecallOperationalGates {
    fn passed(&self) -> bool {
        self.task_utility
            && self.operational_coverage
            && self.operational_behavior
            && self.fallback_coverage
            && self.fallback_parity
            && self.workload_size
            && self.warm_p95
            && self.reproducible_environment
            && self.cross_platform
            && self.privacy_safe_trace
    }
}

pub fn run_recall_operational_eval(path: impl AsRef<Path>) -> Result<RecallOperationalReport> {
    let path = path.as_ref();
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read operational evidence {}", path.display()))?;
    let evidence: RecallOperationalEvidence = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse operational evidence {}", path.display()))?;
    validate_evidence(&evidence)?;

    let required_operational = required_operational_scenarios();
    let observed_operational = evidence
        .operational
        .iter()
        .map(|case| case.scenario)
        .collect::<BTreeSet<_>>();
    let required_fallback = required_fallback_scenarios();
    let observed_fallback = evidence
        .fallback
        .iter()
        .map(|case| case.scenario)
        .collect::<BTreeSet<_>>();
    let task_passed = evidence
        .task_utility
        .iter()
        .filter(|case| task_passes(case))
        .count();
    let operational_passed = evidence
        .operational
        .iter()
        .filter(|case| operational_case_passes(case))
        .count();
    let fallback_parity = evidence
        .fallback
        .iter()
        .map(|case| case.normalized_lexical_parity)
        .fold(1.0, f64::min);
    let trace_counters = evidence
        .trace_counters
        .iter()
        .map(|counter| (counter.reason_code.as_str().to_owned(), counter.count))
        .collect::<BTreeMap<_, _>>();
    let cross_platform = match evidence.cross_platform {
        RecallCrossPlatformContract::ExactVectors => {
            evidence.deterministic_ties && evidence.policy_gates_preserved
        }
        RecallCrossPlatformContract::RankingTolerance {
            min_top_k_overlap,
            max_rank_displacement: _,
        } => {
            (0.0..=1.0).contains(&min_top_k_overlap)
                && evidence.deterministic_ties
                && evidence.policy_gates_preserved
        }
    };
    let gates = RecallOperationalGates {
        task_utility: task_passed == evidence.task_utility.len()
            && evidence
                .task_utility
                .iter()
                .any(|case| case.kind == RecallTaskUtilityKind::Answer)
            && evidence
                .task_utility
                .iter()
                .any(|case| case.kind == RecallTaskUtilityKind::Coding),
        operational_coverage: observed_operational == required_operational,
        operational_behavior: operational_passed == evidence.operational.len(),
        fallback_coverage: observed_fallback == required_fallback,
        fallback_parity: fallback_parity == 1.0
            && evidence
                .fallback
                .iter()
                .all(|case| case.passed && !case.partial_mixed_output),
        workload_size: evidence.performance.record_count == 10_000,
        warm_p95: evidence.performance.warm_query_p95_ms <= evidence.warm_p95_limit_ms,
        reproducible_environment: evidence.environment.release_build
            && evidence.environment.memory_bytes > 0
            && !evidence.environment.timer.trim().is_empty(),
        cross_platform,
        privacy_safe_trace: !trace_counters.is_empty(),
    };
    let passed = gates.passed();
    let evidence_digest = digest_json(&evidence)?;
    Ok(RecallOperationalReport {
        version: RECALL_OPERATIONAL_REPORT_VERSION.into(),
        evidence_digest,
        candidate_digest: evidence.candidate_digest,
        release_build_digest: evidence.release_build_digest,
        task_utility_pass_rate: ratio(task_passed, evidence.task_utility.len()),
        task_utility_context_tokens: evidence
            .task_utility
            .iter()
            .map(|case| case.context_tokens)
            .sum(),
        operational_pass_rate: ratio(operational_passed, evidence.operational.len()),
        fallback_parity,
        workload_digest: recall_synthetic_workload_digest(
            evidence.performance.record_count,
            evidence.performance.workload_seed,
        ),
        performance: evidence.performance,
        environment: evidence.environment,
        cross_platform: evidence.cross_platform,
        trace_counters,
        gates,
        passed,
    })
}

fn digest_json(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json_canonicalizer::to_vec(value)?)
        .to_hex()
        .to_string())
}

fn validate_evidence(evidence: &RecallOperationalEvidence) -> Result<()> {
    if evidence.version != RECALL_OPERATIONAL_EVIDENCE_VERSION
        || evidence.candidate_digest.trim().is_empty()
        || evidence.release_build_digest.trim().is_empty()
        || evidence.lexical_projection_digest.trim().is_empty()
        || evidence.environment.os.trim().is_empty()
        || evidence.environment.architecture.trim().is_empty()
        || evidence.environment.cpu.trim().is_empty()
        || evidence.environment.timer.trim().is_empty()
        || evidence.environment.memory_bytes == 0
        || evidence.task_utility.is_empty()
        || evidence.operational.is_empty()
        || evidence.fallback.is_empty()
        || evidence.trace_counters.is_empty()
    {
        bail!("invalid operational evidence identity or empty suites");
    }
    let finite = [
        evidence.performance.warm_query_p50_ms,
        evidence.performance.warm_query_p95_ms,
        evidence.performance.cold_start_ms,
        evidence.performance.first_query_ms,
        evidence.performance.full_build_ms,
        evidence.performance.incremental_update_ms,
        evidence.warm_p95_limit_ms,
    ];
    if finite
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
        || evidence.performance.warm_query_p50_ms > evidence.performance.warm_query_p95_ms
    {
        bail!("invalid operational performance observations");
    }
    validate_unique_ids(
        evidence.task_utility.iter().map(|case| case.id.as_str()),
        "task",
    )?;
    validate_unique_ids(
        evidence.operational.iter().map(|case| case.id.as_str()),
        "operational",
    )?;
    validate_unique_ids(
        evidence.fallback.iter().map(|case| case.id.as_str()),
        "fallback",
    )?;
    validate_unique_ids(
        evidence
            .trace_counters
            .iter()
            .map(|case| case.reason_code.as_str()),
        "trace reason",
    )?;
    if evidence.fallback.iter().any(|case| {
        !(0.0..=1.0).contains(&case.normalized_lexical_parity) || !is_reason_code(&case.reason_code)
    }) || evidence
        .operational
        .iter()
        .any(|case| !is_reason_code(&case.reason_code))
    {
        bail!("invalid operational or fallback case");
    }
    Ok(())
}

fn task_passes(case: &RecallTaskUtilityCase) -> bool {
    case.completed
        && case.answer_correct
        && case.required_memory_used
        && case.distractor_avoided
        && case.citation_used
        && case.false_confidence_avoided
        && case.context_tokens > 0
}

fn operational_case_passes(case: &RecallOperationalCase) -> bool {
    case.passed
        && case.canonical_unchanged
        && !case.stale_generation_queryable
        && match case.scenario {
            RecallOperationalScenario::Deletion
            | RecallOperationalScenario::Tombstone
            | RecallOperationalScenario::Expiry => case.deletion_verified,
            RecallOperationalScenario::Rollback => case.rollback_verified,
            _ => case.expected_generation_active,
        }
}

fn required_operational_scenarios() -> BTreeSet<RecallOperationalScenario> {
    use RecallOperationalScenario::*;
    [
        Create,
        Update,
        Supersede,
        Tombstone,
        Expiry,
        Deletion,
        StaleState,
        PartialState,
        Corruption,
        InterruptedBuild,
        AtomicPromotion,
        Rollback,
        ModelReplacement,
    ]
    .into_iter()
    .collect()
}

fn required_fallback_scenarios() -> BTreeSet<RecallFallbackScenario> {
    use RecallFallbackScenario::*;
    [
        MissingModel,
        MissingIndex,
        StaleIndex,
        IncompleteIndex,
        IncompatibleIndex,
        CorruptIndex,
        QueryEmbeddingFailure,
        UnsupportedPlatform,
    ]
    .into_iter()
    .collect()
}

fn validate_unique_ids<'a>(ids: impl Iterator<Item = &'a str>, label: &str) -> Result<()> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if id.trim().is_empty() || !seen.insert(id) {
            bail!("invalid or duplicate {label} id");
        }
    }
    Ok(())
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn is_reason_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

pub fn recall_synthetic_workload_digest(record_count: usize, seed: u64) -> String {
    let mut state = seed;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"memzoi-recall-synthetic-workload/v1\0");
    hasher.update(&(record_count as u64).to_le_bytes());
    hasher.update(&seed.to_le_bytes());
    for index in 0..record_count {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        hasher.update(&(index as u64).to_le_bytes());
        hasher.update(&state.to_le_bytes());
        hasher.update(format!("synthetic-record-{index:05}").as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}
