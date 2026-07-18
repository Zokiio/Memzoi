use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::Command,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tempfile::{NamedTempFile, TempDir};

use crate::{
    FixedClock, InitRequest, MemoryDestination, MemoryEvent, MemoryPaths, MemoryRecord,
    MemoryService, MemoryWriteRoute, OkfProposalSensitivity, SearchInput,
    capture::{
        CaptureAction, CaptureApplyResult, CaptureCandidate, CaptureDataClass, CaptureEvidence,
        CaptureEvidenceSpan, CaptureExtractorIdentity, CaptureMatchKind, CaptureMemoryDraft,
        CapturePlan, CapturePlanStatus, CapturePolicyInputSnapshot, CaptureProvenance,
        CaptureRequest, CaptureReview, CaptureReviewDecisionInput, CaptureReviewInput,
        CaptureReviewOutcome, CaptureSemanticLocation, CaptureSourceInputs, CaptureSourceLocator,
        CaptureWrite, build_capture_review_with_inputs, plan_capture_with_inputs,
    },
};

pub const CAPTURE_EVAL_CORPUS_VERSION: &str = "memzoi-capture-corpus/v1";
pub const CAPTURE_EVAL_REPORT_VERSION: &str = "memzoi-capture-report/v1";
pub const CAPTURE_EVAL_METRIC_DEFINITIONS_VERSION: &str = "memzoi-capture-metrics/v1";
pub const CAPTURE_EVAL_BASELINE_VERSION: &str = "memzoi-capture-baseline/v1";

const MAX_CORPUS_BYTES: u64 = 2 * 1024 * 1024;
const MAX_FIXTURE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_FIXTURE_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CASES: usize = 256;
const ADR_GITIGNORE_ENGINE: &str = "memzoi/gitignore-v1+ignore-0.4.28";
const GIT_TREE_GITIGNORE_ENGINE: &str = "memzoi/gitignore-v1+ignore-0.4.28+git-tree-v1";
const GIT_REPOSITORY_IDENTITY_ENGINE: &str = "memzoi/git-repository-identity-v1";
const GIT_LOCAL_CONFIG_ENGINE: &str = "memzoi/git-local-config-v1";
const GIT_RENDERER_ENGINE_PREFIX: &str = "memzoi/git-unified-renderer-v1+git-";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalCorpus {
    pub version: String,
    pub name: String,
    pub evaluated_at: String,
    pub profiles: Vec<CaptureEvalProfileExpectation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thresholds: Option<CaptureEvalThresholds>,
    pub cases: Vec<CaptureEvalCase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalProfileExpectation {
    pub profile: String,
    #[serde(default = "default_true")]
    pub required: bool,
    pub extractor_kind: String,
    pub extractor_id: String,
    pub extractor_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalCase {
    pub id: String,
    #[serde(default)]
    pub coverage: Vec<CaptureEvalCoverage>,
    #[serde(default)]
    pub project_files: Vec<CaptureEvalFileFixture>,
    #[serde(default)]
    pub record_fixtures: Vec<CaptureEvalFileFixture>,
    #[serde(default)]
    pub proposal_fixtures: Vec<CaptureEvalFileFixture>,
    #[serde(default)]
    pub source_inputs: Vec<CaptureEvalSourceInputFixture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_repository: Option<CaptureEvalGitRepositoryFixture>,
    #[serde(default)]
    pub expected_policy_inputs: Vec<CaptureEvalExpectedPolicyInput>,
    pub request: CaptureRequest,
    pub expected: CaptureEvalExpectedPlan,
    #[serde(default)]
    pub review_outcomes: Vec<CaptureEvalReviewOutcomeExpectation>,
    #[serde(default)]
    pub must_not_echo: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_check: Option<CaptureEvalStaleCheck>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureEvalCoverage {
    Useful,
    LowConfidence,
    ReviewReject,
    Stale,
    Duplicate,
    Conflict,
    CitedProvenance,
    PromptInjection,
    Credential,
    RawTranscript,
    PrivateData,
    FeedbackLoop,
    Malformed,
    AmbiguousSharing,
    SuppliedBytes,
    GitRange,
    GitRangeStale,
    Rename,
    Delete,
    Forbidden,
    PolicyProhibited,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureEvalEngineMatch {
    #[default]
    Exact,
    Prefix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalExpectedPolicyInput {
    pub path: String,
    pub engine_version: String,
    #[serde(default)]
    pub engine_match: CaptureEvalEngineMatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_content_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalFileFixture {
    pub fixture: PathBuf,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalSourceInputFixture {
    pub source_id: String,
    pub fixture: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalGitRepositoryFixture {
    pub base_files: Vec<CaptureEvalFileFixture>,
    pub head_files: Vec<CaptureEvalFileFixture>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureEvalStaleCheck {
    ProjectPath {
        path: PathBuf,
        replacement_fixture: PathBuf,
    },
    SuppliedBytes {
        source_id: String,
        replacement_fixture: PathBuf,
    },
    GitObjectMissing {
        oid: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalExpectedPlan {
    pub status: CapturePlanStatus,
    pub data_class: CaptureDataClass,
    #[serde(default)]
    pub diagnostic_codes: Vec<String>,
    #[serde(default)]
    pub candidates: Vec<CaptureEvalExpectedCandidate>,
    #[serde(default)]
    pub forbidden_candidates: Vec<CaptureEvalCandidateMatcher>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalExpectedCandidate {
    pub id: String,
    pub memory: CaptureMemoryDraft,
    pub evidence: Vec<CaptureEvalExpectedEvidence>,
    pub destination: MemoryDestination,
    pub sensitivity: OkfProposalSensitivity,
    pub action: CaptureEvalActionExpectation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalCandidateMatcher {
    pub id: String,
    pub memory: CaptureMemoryDraft,
    #[serde(default)]
    pub evidence: Vec<CaptureEvalExpectedEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalExpectedEvidence {
    pub source_id: String,
    pub locator: CaptureSourceLocator,
    pub source_content_hash: String,
    pub span: CaptureEvidenceSpan,
    pub heading_path: Vec<String>,
    pub section_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_location: Option<CaptureSemanticLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureEvalActionExpectation {
    CreateProposal,
    CreateRuntime {
        route: MemoryWriteRoute,
    },
    Duplicate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        match_count: Option<usize>,
        #[serde(default)]
        match_kinds: Vec<CaptureMatchKind>,
    },
    Conflict {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        match_count: Option<usize>,
        #[serde(default)]
        match_kinds: Vec<CaptureMatchKind>,
    },
    NoWrite {
        reason_code: String,
    },
    Blocked {
        code: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalReviewOutcomeExpectation {
    pub candidate: String,
    pub outcome: CaptureReviewOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<CaptureMemoryDraft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_destination: Option<MemoryDestination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_class: Option<crate::RepositoryContentClass>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalThresholds {
    pub min_candidate_precision: f64,
    pub min_candidate_recall: f64,
    pub min_evidence_validity: f64,
    pub min_destination_accuracy: f64,
    pub min_sensitivity_accuracy: f64,
    pub min_action_accuracy: f64,
    pub max_forbidden_hit_rate: f64,
    pub min_unsupported_outcome_accuracy: f64,
    pub min_case_pass_rate: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_burden: Option<CaptureEvalReviewBurdenLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_plan_payload_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_p95_latency_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalReviewBurdenLimits {
    pub proposed: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub edited: usize,
    pub deferred: usize,
    pub duplicates: usize,
    pub conflicts: usize,
    pub needs_review: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEvalReport {
    pub version: String,
    pub corpus: CaptureEvalCorpusMetadata,
    pub definitions: CaptureEvalMetricDefinitions,
    pub runtime: CaptureEvalRuntimeMetadata,
    pub profiles: Vec<CaptureEvalProfileFingerprint>,
    pub cases: Vec<CaptureEvalCaseReport>,
    pub metrics: CaptureEvalMetrics,
    pub profile_metrics: BTreeMap<String, CaptureEvalMetrics>,
    pub hard_gates: CaptureEvalHardGates,
    pub observations: CaptureEvalObservations,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thresholds: Option<CaptureEvalThresholds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_results: Option<CaptureEvalThresholdResults>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<CaptureEvalBaselineComparison>,
    pub gates_passed: bool,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureEvalCorpusMetadata {
    pub name: String,
    pub version: String,
    pub digest: String,
    pub evaluated_at: String,
    pub profile_count: usize,
    pub case_count: usize,
    pub fixture_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureEvalMetricDefinitions {
    pub version: String,
    pub candidate_precision: String,
    pub candidate_recall: String,
    pub evidence_validity: String,
    pub classification_accuracy: String,
    pub forbidden_hit_rate: String,
    pub unsupported_outcome_accuracy: String,
    pub review_burden: String,
    pub observations: String,
    pub empty_denominator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureEvalRuntimeMetadata {
    pub memzoi_version: String,
    pub target_os: String,
    pub target_arch: String,
    pub build_profile: String,
    pub isolated_state: bool,
    pub network_required: bool,
    pub latency_sample_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureEvalProfileFingerprint {
    pub profile: String,
    pub required: bool,
    pub extractor_kind: String,
    pub extractor_id: String,
    pub extractor_version: String,
    pub extractor_configuration_hash: String,
    pub safeguard_policy_version: String,
    pub safeguard_configuration_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureEvalExecutionStatus {
    Executed,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEvalCaseReport {
    pub id: String,
    pub profile: String,
    pub execution: CaptureEvalExecutionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_status: Option<CapturePlanStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_class: Option<CaptureDataClass>,
    pub diagnostic_codes: Vec<String>,
    pub matched_candidates: Vec<String>,
    pub missing_candidates: Vec<String>,
    pub forbidden_hits: Vec<String>,
    pub forbidden_opportunities: usize,
    pub unexpected_candidates: usize,
    pub true_positives: usize,
    pub false_positives: usize,
    pub false_negatives: usize,
    pub evidence_validity: CaptureEvalIntegrityMetric,
    pub destination_accuracy: CaptureEvalIntegrityMetric,
    pub sensitivity_accuracy: CaptureEvalIntegrityMetric,
    pub action_accuracy: CaptureEvalIntegrityMetric,
    pub review_burden: CaptureEvalReviewBurden,
    pub hard_gates: CaptureEvalHardGates,
    pub observations: CaptureEvalCaseObservations,
    pub assertions: BTreeMap<String, bool>,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEvalMetrics {
    pub case_count: usize,
    pub candidate_precision: CaptureEvalRatioMetric,
    pub candidate_recall: CaptureEvalRatioMetric,
    pub evidence_validity: CaptureEvalIntegrityMetric,
    pub destination_accuracy: CaptureEvalIntegrityMetric,
    pub sensitivity_accuracy: CaptureEvalIntegrityMetric,
    pub action_accuracy: CaptureEvalIntegrityMetric,
    pub forbidden_hits: CaptureEvalLeakageMetric,
    pub unsupported_outcome_accuracy: CaptureEvalIntegrityMetric,
    pub case_pass_rate: CaptureEvalRatioMetric,
    pub extractor_failures: usize,
    pub optional_skips: usize,
    pub review_burden: CaptureEvalReviewBurden,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEvalRatioMetric {
    pub numerator: usize,
    pub denominator: usize,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEvalIntegrityMetric {
    pub valid: usize,
    pub checked: usize,
    pub rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEvalLeakageMetric {
    pub hits: usize,
    pub opportunities: usize,
    pub rate: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureEvalReviewBurden {
    pub proposed: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub edited: usize,
    pub deferred: usize,
    pub duplicates: usize,
    pub conflicts: usize,
    pub needs_review: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureEvalHardGates {
    pub determinism_violations: usize,
    pub planning_mutation_violations: usize,
    pub invalid_evidence_items: usize,
    pub unnamed_source_reads: usize,
    pub prohibited_output_echoes: usize,
    pub review_workflow_violations: usize,
    pub provenance_violations: usize,
    pub stale_review_acceptance_violations: usize,
    pub stale_apply_acceptance_violations: usize,
    pub stale_write_violations: usize,
    pub direct_canonical_write_violations: usize,
    pub required_profile_skips: usize,
}

impl CaptureEvalHardGates {
    pub fn passed(&self) -> bool {
        self.determinism_violations == 0
            && self.planning_mutation_violations == 0
            && self.invalid_evidence_items == 0
            && self.unnamed_source_reads == 0
            && self.prohibited_output_echoes == 0
            && self.review_workflow_violations == 0
            && self.provenance_violations == 0
            && self.stale_review_acceptance_violations == 0
            && self.stale_apply_acceptance_violations == 0
            && self.stale_write_violations == 0
            && self.direct_canonical_write_violations == 0
            && self.required_profile_skips == 0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEvalObservations {
    pub latency: CaptureEvalLatencyObservations,
    pub payload: CaptureEvalPayloadObservations,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEvalCaseObservations {
    pub latency_ms: f64,
    pub payload_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEvalLatencyObservations {
    pub unit: String,
    pub timer: String,
    pub sample_count: usize,
    pub p50: f64,
    pub p95: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEvalPayloadObservations {
    pub unit: String,
    pub sample_count: usize,
    pub p50: usize,
    pub p95: usize,
    pub max: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureEvalThresholdResults {
    pub checks: BTreeMap<String, bool>,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalBaseline {
    pub version: String,
    pub report_version: String,
    pub definitions_version: String,
    pub corpus: CaptureEvalBaselineCorpus,
    pub profiles: Vec<CaptureEvalProfileFingerprint>,
    pub metrics: CaptureEvalMetrics,
    pub profile_metrics: BTreeMap<String, CaptureEvalMetrics>,
    pub hard_gates: CaptureEvalHardGates,
    pub cases: Vec<CaptureEvalBaselineCase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalBaselineCorpus {
    pub name: String,
    pub version: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvalBaselineCase {
    pub id: String,
    pub profile: String,
    pub execution: CaptureEvalExecutionStatus,
    pub plan_status: Option<CapturePlanStatus>,
    pub data_class: Option<CaptureDataClass>,
    pub diagnostic_codes: Vec<String>,
    pub matched_candidates: Vec<String>,
    pub missing_candidates: Vec<String>,
    pub forbidden_hits: Vec<String>,
    pub forbidden_opportunities: usize,
    pub unexpected_candidates: usize,
    pub review_burden: CaptureEvalReviewBurden,
    pub assertions: BTreeMap<String, bool>,
    pub passed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureEvalBaselineStatus {
    Match,
    Changed,
    Incompatible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureEvalBaselineComparison {
    pub version: String,
    pub status: CaptureEvalBaselineStatus,
    pub compatible: bool,
    pub deterministic_match: bool,
    pub metrics_changed: bool,
    pub profiles_changed: bool,
    pub hard_gates_changed: bool,
    pub changed_cases: Vec<String>,
}

#[derive(Debug)]
struct LoadedCorpus {
    corpus: CaptureEvalCorpus,
    root: PathBuf,
    digest: String,
    fixture_count: usize,
}

#[derive(Debug)]
struct EvaluatedCase {
    report: CaptureEvalCaseReport,
    fingerprint: Option<CaptureEvalProfileFingerprint>,
}

#[derive(Debug, Clone, Copy)]
struct EvidenceCounts {
    valid: usize,
    checked: usize,
    unnamed: usize,
}

#[derive(Debug, Clone, Copy)]
struct WorkflowChecks {
    review_valid: bool,
    apply_valid: bool,
    provenance_valid: bool,
    repo_proposal_only: bool,
    stale_review_rejected: bool,
    stale_apply_rejected: bool,
    stale_no_write: bool,
}

impl Default for WorkflowChecks {
    fn default() -> Self {
        Self {
            review_valid: true,
            apply_valid: true,
            provenance_valid: true,
            repo_proposal_only: true,
            stale_review_rejected: true,
            stale_apply_rejected: true,
            stale_no_write: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct CaptureManagedStateSnapshot {
    files: CaptureEvalTreeSnapshot,
    pending_proposal_ids: Vec<String>,
    resolved_proposal_ids: Vec<String>,
    local_records: Vec<MemoryRecord>,
    checkpoints: Vec<MemoryRecord>,
    events: Vec<MemoryEvent>,
    repo_index_drift: crate::RepoIndexDrift,
}

type CaptureEvalTreeSnapshot = BTreeMap<PathBuf, CaptureEvalTreeEntry>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum CaptureEvalTreeEntry {
    Directory,
    RegularFile(Vec<u8>),
    Symlink(PathBuf),
}

fn default_true() -> bool {
    true
}

pub fn run_capture_eval(corpus_path: impl AsRef<Path>) -> Result<CaptureEvalReport> {
    let loaded = load_corpus(corpus_path.as_ref())?;
    validate_corpus(&loaded.corpus)?;

    let profiles = loaded
        .corpus
        .profiles
        .iter()
        .map(|profile| (profile.profile.as_str(), profile))
        .collect::<BTreeMap<_, _>>();
    let mut evaluated = Vec::with_capacity(loaded.corpus.cases.len());
    let mut fingerprints = BTreeMap::<String, CaptureEvalProfileFingerprint>::new();
    for case in &loaded.corpus.cases {
        let profile = profiles
            .get(case.request.extractor.profile.as_str())
            .copied()
            .context("capture evaluation case references an unknown profile")?;
        let evaluated_case =
            evaluate_case(&loaded.root, &loaded.corpus.evaluated_at, case, profile)?;
        if let Some(fingerprint) = &evaluated_case.fingerprint {
            if let Some(existing) = fingerprints.get(&fingerprint.profile) {
                if existing != fingerprint {
                    bail!("capture evaluation profile fingerprint changed between cases");
                }
            } else {
                fingerprints.insert(fingerprint.profile.clone(), fingerprint.clone());
            }
        }
        evaluated.push(evaluated_case);
    }

    let cases = evaluated
        .iter()
        .map(|case| case.report.clone())
        .collect::<Vec<_>>();
    let metrics = aggregate_metrics(&cases);
    let profile_metrics = loaded
        .corpus
        .profiles
        .iter()
        .map(|profile| {
            let profile_cases = cases
                .iter()
                .filter(|case| case.profile == profile.profile)
                .cloned()
                .collect::<Vec<_>>();
            (profile.profile.clone(), aggregate_metrics(&profile_cases))
        })
        .collect::<BTreeMap<_, _>>();
    let hard_gates = aggregate_hard_gates(&cases);
    let observations = aggregate_observations(&cases);
    let threshold_results = loaded.corpus.thresholds.as_ref().map(|thresholds| {
        evaluate_thresholds(thresholds, &metrics, &profile_metrics, &observations)
    });
    let gates_passed = cases.iter().all(|case| case.passed)
        && hard_gates.passed()
        && threshold_results
            .as_ref()
            .is_none_or(|results| results.passed);

    Ok(CaptureEvalReport {
        version: CAPTURE_EVAL_REPORT_VERSION.to_owned(),
        corpus: CaptureEvalCorpusMetadata {
            name: loaded.corpus.name,
            version: loaded.corpus.version,
            digest: loaded.digest,
            evaluated_at: loaded.corpus.evaluated_at,
            profile_count: loaded.corpus.profiles.len(),
            case_count: cases.len(),
            fixture_count: loaded.fixture_count,
        },
        definitions: metric_definitions(),
        runtime: runtime_metadata(cases.len()),
        profiles: fingerprints.into_values().collect(),
        cases,
        metrics,
        profile_metrics,
        hard_gates,
        observations,
        thresholds: loaded.corpus.thresholds,
        threshold_results,
        baseline: None,
        gates_passed,
        passed: gates_passed,
    })
}

pub fn attach_capture_eval_baseline(
    report: &mut CaptureEvalReport,
    baseline_path: impl AsRef<Path>,
) -> Result<()> {
    let baseline_path = baseline_path.as_ref();
    let bytes = fs::read(baseline_path).with_context(|| {
        format!(
            "failed to read capture evaluation baseline {}",
            baseline_path.display()
        )
    })?;
    let baseline: CaptureEvalBaseline = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "failed to parse capture evaluation baseline {}",
            baseline_path.display()
        )
    })?;
    let comparison = compare_baseline(report, &baseline);
    report.passed = report.gates_passed && comparison.status == CaptureEvalBaselineStatus::Match;
    report.baseline = Some(comparison);
    Ok(())
}

pub fn write_capture_eval_baseline(
    report: &CaptureEvalReport,
    baseline_path: impl AsRef<Path>,
) -> Result<()> {
    if !report.gates_passed {
        bail!("capture evaluation gates failed; baseline was not modified");
    }
    let baseline_path = baseline_path.as_ref();
    let parent = baseline_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create capture evaluation baseline directory {}",
            parent.display()
        )
    })?;
    let baseline = baseline_from_report(report);
    let mut bytes = serde_json::to_vec_pretty(&baseline)?;
    bytes.push(b'\n');
    let mut temporary = NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "failed to stage capture evaluation baseline in {}",
            parent.display()
        )
    })?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(baseline_path).map_err(|error| {
        anyhow::Error::new(error.error).context(format!(
            "failed to install capture evaluation baseline {}",
            baseline_path.display()
        ))
    })?;
    Ok(())
}

fn load_corpus(path: &Path) -> Result<LoadedCorpus> {
    let path = path
        .canonicalize()
        .with_context(|| format!("failed to resolve capture corpus {}", path.display()))?;
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("failed to inspect capture corpus {}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_CORPUS_BYTES {
        bail!("capture corpus must be a bounded regular file");
    }
    let root = path
        .parent()
        .context("capture corpus path has no parent directory")?
        .to_path_buf();
    let bytes = fs::read(&path)
        .with_context(|| format!("failed to read capture corpus {}", path.display()))?;
    let corpus: CaptureEvalCorpus = serde_yaml::from_slice(&bytes)
        .with_context(|| format!("failed to parse capture corpus {}", path.display()))?;
    let (digest, fixture_count) = corpus_digest(&root, &bytes, &corpus)?;
    Ok(LoadedCorpus {
        corpus,
        root,
        digest,
        fixture_count,
    })
}

fn corpus_digest(
    root: &Path,
    corpus_bytes: &[u8],
    corpus: &CaptureEvalCorpus,
) -> Result<(String, usize)> {
    let mut fixture_paths = BTreeSet::<PathBuf>::new();
    for case in &corpus.cases {
        for fixture in case
            .project_files
            .iter()
            .chain(&case.record_fixtures)
            .chain(&case.proposal_fixtures)
        {
            fixture_paths.insert(fixture.fixture.clone());
        }
        fixture_paths.extend(
            case.source_inputs
                .iter()
                .map(|fixture| fixture.fixture.clone()),
        );
        if let Some(repository) = &case.git_repository {
            fixture_paths.extend(
                repository
                    .base_files
                    .iter()
                    .chain(&repository.head_files)
                    .map(|fixture| fixture.fixture.clone()),
            );
        }
        if let Some(fixture) = case.stale_check.as_ref().and_then(|stale| match stale {
            CaptureEvalStaleCheck::ProjectPath {
                replacement_fixture,
                ..
            }
            | CaptureEvalStaleCheck::SuppliedBytes {
                replacement_fixture,
                ..
            } => Some(replacement_fixture.clone()),
            CaptureEvalStaleCheck::GitObjectMissing { .. } => None,
        }) {
            fixture_paths.insert(fixture);
        }
    }

    let mut hasher = blake3::Hasher::new();
    hash_digest_entry(&mut hasher, Path::new("corpus.yaml"), corpus_bytes);
    let mut total_bytes = 0u64;
    for path in &fixture_paths {
        let bytes = read_safe_fixture(root, path)?;
        total_bytes = total_bytes.saturating_add(bytes.len() as u64);
        if total_bytes > MAX_FIXTURE_TOTAL_BYTES {
            bail!("capture corpus fixtures exceed the aggregate byte limit");
        }
        hash_digest_entry(&mut hasher, path, &bytes);
    }
    Ok((
        format!("blake3:{}", hasher.finalize().to_hex()),
        fixture_paths.len(),
    ))
}

fn hash_digest_entry(hasher: &mut blake3::Hasher, path: &Path, bytes: &[u8]) {
    let path = path.as_os_str().as_encoded_bytes();
    hasher.update(&(path.len() as u64).to_le_bytes());
    hasher.update(path);
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn validate_corpus(corpus: &CaptureEvalCorpus) -> Result<()> {
    if corpus.version != CAPTURE_EVAL_CORPUS_VERSION {
        bail!("unsupported capture corpus version");
    }
    validate_safe_id(&corpus.name, "capture corpus name")?;
    FixedClock::from_rfc3339(&corpus.evaluated_at)
        .context("invalid capture corpus evaluated_at")?;
    if corpus.profiles.is_empty() {
        bail!("capture corpus must declare at least one profile");
    }
    if corpus.cases.is_empty() || corpus.cases.len() > MAX_CASES {
        bail!("capture corpus must define a bounded non-empty case list");
    }
    if let Some(thresholds) = &corpus.thresholds {
        validate_thresholds(thresholds)?;
    }

    let mut profile_names = BTreeSet::new();
    let mut required_profiles = 0usize;
    for profile in &corpus.profiles {
        for (label, value) in [
            ("profile", profile.profile.as_str()),
            ("extractor kind", profile.extractor_kind.as_str()),
            ("extractor id", profile.extractor_id.as_str()),
            ("extractor version", profile.extractor_version.as_str()),
        ] {
            validate_safe_id(value, label)?;
        }
        if !profile_names.insert(profile.profile.as_str()) {
            bail!("capture corpus contains a duplicate profile");
        }
        if profile.required {
            required_profiles += 1;
        }
    }
    if required_profiles == 0 {
        bail!("capture corpus must declare at least one required profile");
    }

    let mut case_ids = BTreeSet::new();
    let mut profile_case_counts = BTreeMap::<&str, usize>::new();
    let mut profile_coverage = BTreeMap::<&str, BTreeSet<CaptureEvalCoverage>>::new();
    let mut global_coverage = BTreeSet::<CaptureEvalCoverage>::new();
    for case in &corpus.cases {
        validate_safe_id(&case.id, "capture case id")?;
        if !case_ids.insert(case.id.as_str()) {
            bail!("capture corpus contains a duplicate case id");
        }
        if !profile_names.contains(case.request.extractor.profile.as_str()) {
            bail!("capture case references an undeclared profile");
        }
        *profile_case_counts
            .entry(case.request.extractor.profile.as_str())
            .or_default() += 1;
        profile_coverage
            .entry(case.request.extractor.profile.as_str())
            .or_default()
            .extend(case.coverage.iter().copied());
        global_coverage.extend(case.coverage.iter().copied());
        validate_case(case)?;
    }
    for profile in corpus.profiles.iter().filter(|profile| profile.required) {
        if profile_case_counts
            .get(profile.profile.as_str())
            .copied()
            .unwrap_or_default()
            == 0
        {
            bail!(
                "required capture profile {} must define at least one case",
                profile.profile
            );
        }
        let coverage = profile_coverage
            .get(profile.profile.as_str())
            .context("required capture profile has no coverage inventory")?;
        for required in [
            CaptureEvalCoverage::Useful,
            CaptureEvalCoverage::LowConfidence,
            CaptureEvalCoverage::ReviewReject,
            CaptureEvalCoverage::Stale,
            CaptureEvalCoverage::Duplicate,
            CaptureEvalCoverage::Conflict,
            CaptureEvalCoverage::CitedProvenance,
            CaptureEvalCoverage::Forbidden,
        ] {
            if !coverage.contains(&required) {
                bail!(
                    "required capture profile {} is missing {:?} coverage",
                    profile.profile,
                    required
                );
            }
        }
    }
    for required in [
        CaptureEvalCoverage::PromptInjection,
        CaptureEvalCoverage::Credential,
        CaptureEvalCoverage::RawTranscript,
        CaptureEvalCoverage::PrivateData,
        CaptureEvalCoverage::FeedbackLoop,
        CaptureEvalCoverage::Malformed,
        CaptureEvalCoverage::AmbiguousSharing,
        CaptureEvalCoverage::SuppliedBytes,
        CaptureEvalCoverage::GitRange,
        CaptureEvalCoverage::GitRangeStale,
        CaptureEvalCoverage::Rename,
        CaptureEvalCoverage::Delete,
        CaptureEvalCoverage::PolicyProhibited,
    ] {
        if !global_coverage.contains(&required) {
            bail!("capture corpus is missing {:?} coverage", required);
        }
    }
    Ok(())
}

fn validate_case(case: &CaptureEvalCase) -> Result<()> {
    let mut destinations = BTreeSet::new();
    for fixture in case
        .project_files
        .iter()
        .chain(&case.record_fixtures)
        .chain(&case.proposal_fixtures)
    {
        validate_fixture_path(&fixture.fixture)?;
        validate_destination_path(&fixture.path)?;
        if !destinations.insert(fixture.path.as_path()) {
            bail!("capture case contains a duplicate fixture destination");
        }
    }
    let mut source_inputs = BTreeSet::new();
    for fixture in &case.source_inputs {
        validate_safe_id(&fixture.source_id, "capture source input id")?;
        validate_fixture_path(&fixture.fixture)?;
        if !source_inputs.insert(fixture.source_id.as_str()) {
            bail!("capture case contains a duplicate source input");
        }
    }
    if let Some(repository) = &case.git_repository {
        if repository.base_files.is_empty() || repository.head_files.is_empty() {
            bail!("capture Git repository fixtures require base and head files");
        }
        if !matches!(
            case.request.sources.first().map(|source| &source.locator),
            Some(CaptureSourceLocator::GitRange { .. })
        ) {
            bail!("capture Git repository fixtures require a git_range source");
        }
        for files in [&repository.base_files, &repository.head_files] {
            let mut paths = BTreeSet::new();
            for fixture in files {
                validate_fixture_path(&fixture.fixture)?;
                validate_destination_path(&fixture.path)?;
                if !paths.insert(fixture.path.as_path()) {
                    bail!("capture Git repository fixture contains a duplicate path");
                }
            }
        }
    }
    if let Some(stale) = &case.stale_check {
        match stale {
            CaptureEvalStaleCheck::ProjectPath {
                path,
                replacement_fixture,
            } => {
                validate_fixture_path(replacement_fixture)?;
                validate_destination_path(path)?;
            }
            CaptureEvalStaleCheck::SuppliedBytes {
                source_id,
                replacement_fixture,
            } => {
                validate_safe_id(source_id, "stale supplied source id")?;
                validate_fixture_path(replacement_fixture)?;
                if !source_inputs.contains(source_id.as_str()) {
                    bail!("stale supplied source must name a declared source input");
                }
            }
            CaptureEvalStaleCheck::GitObjectMissing { oid } => {
                validate_git_object_id(oid, "stale Git object")?;
                let Some(CaptureSourceLocator::GitRange { base, head, .. }) =
                    case.request.sources.first().map(|source| &source.locator)
                else {
                    bail!("stale Git object checks require a git_range source");
                };
                if oid != base && oid != head {
                    bail!("stale Git object must be one of the pinned range commits");
                }
            }
        }
    }

    validate_expected_policy_inputs(case)?;

    let mut candidate_ids = BTreeSet::new();
    for candidate in &case.expected.candidates {
        validate_safe_id(&candidate.id, "expected candidate id")?;
        if !candidate_ids.insert(candidate.id.as_str()) {
            bail!("capture case contains a duplicate expected candidate id");
        }
        if candidate.evidence.is_empty() {
            bail!("expected capture candidates must declare evidence");
        }
        for evidence in &candidate.evidence {
            validate_expected_evidence(evidence)?;
        }
    }
    let mut forbidden_ids = BTreeSet::new();
    for candidate in &case.expected.forbidden_candidates {
        validate_safe_id(&candidate.id, "forbidden candidate id")?;
        if !forbidden_ids.insert(candidate.id.as_str())
            || candidate_ids.contains(candidate.id.as_str())
        {
            bail!("capture case contains an invalid forbidden candidate id");
        }
        for evidence in &candidate.evidence {
            validate_expected_evidence(evidence)?;
        }
    }
    let mut reviewed = BTreeSet::new();
    for decision in &case.review_outcomes {
        if !candidate_ids.contains(decision.candidate.as_str()) {
            bail!("capture review outcome references an unknown expected candidate");
        }
        if !reviewed.insert(decision.candidate.as_str()) {
            bail!("capture case contains duplicate review outcomes");
        }
        if let Some(reason_code) = decision.reason_code.as_deref() {
            validate_safe_id(reason_code, "capture review reason code")?;
        }
        match decision.outcome {
            CaptureReviewOutcome::Edit if decision.memory.is_none() => {
                bail!("capture edit review outcomes must declare a complete memory draft");
            }
            CaptureReviewOutcome::Edit => {}
            CaptureReviewOutcome::Accept
            | CaptureReviewOutcome::Reject
            | CaptureReviewOutcome::Defer
                if decision.memory.is_some()
                    || decision.requested_destination.is_some()
                    || decision.content_class.is_some() =>
            {
                bail!("only capture edit review outcomes may declare edits");
            }
            CaptureReviewOutcome::Accept
            | CaptureReviewOutcome::Reject
            | CaptureReviewOutcome::Defer => {}
        }
    }
    if !case.expected.candidates.is_empty() && reviewed.len() != candidate_ids.len() {
        bail!("capture cases with candidates must review every expected candidate");
    }
    for marker in &case.must_not_echo {
        if marker.is_empty() || marker.len() > 4096 {
            bail!("capture no-echo markers must be bounded and non-empty");
        }
    }
    for code in &case.expected.diagnostic_codes {
        validate_safe_id(code, "capture diagnostic code")?;
    }
    validate_case_coverage(case)?;
    Ok(())
}

fn validate_case_coverage(case: &CaptureEvalCase) -> Result<()> {
    let coverage = case.coverage.iter().copied().collect::<BTreeSet<_>>();
    if coverage.len() != case.coverage.len() {
        bail!("capture case contains duplicate coverage declarations");
    }
    let has_action = |matches: fn(&CaptureEvalActionExpectation) -> bool| {
        case.expected
            .candidates
            .iter()
            .any(|candidate| matches(&candidate.action))
    };
    let has_accepted_route = case.review_outcomes.iter().any(|outcome| {
        (outcome.outcome == CaptureReviewOutcome::Accept
            && case.expected.candidates.iter().any(|candidate| {
                candidate.id == outcome.candidate
                    && matches!(
                        candidate.action,
                        CaptureEvalActionExpectation::CreateProposal
                            | CaptureEvalActionExpectation::CreateRuntime { .. }
                    )
            }))
            || (outcome.outcome == CaptureReviewOutcome::Edit
                && outcome.requested_destination == Some(MemoryDestination::Repo)
                && outcome.content_class
                    == Some(crate::RepositoryContentClass::GeneralRepoKnowledge))
    });
    for declared in &coverage {
        let valid = match declared {
            CaptureEvalCoverage::Useful | CaptureEvalCoverage::CitedProvenance => {
                has_accepted_route
            }
            CaptureEvalCoverage::LowConfidence => {
                case.expected.status == CapturePlanStatus::Blocked
                    || !case.expected.diagnostic_codes.is_empty()
                    || case.expected.candidates.iter().any(|candidate| {
                        candidate.destination == MemoryDestination::NeedsReview
                            || candidate.sensitivity == OkfProposalSensitivity::Unknown
                            || matches!(
                                candidate.action,
                                CaptureEvalActionExpectation::NoWrite { .. }
                                    | CaptureEvalActionExpectation::Conflict { .. }
                            )
                    })
            }
            CaptureEvalCoverage::ReviewReject => case
                .review_outcomes
                .iter()
                .any(|outcome| outcome.outcome == CaptureReviewOutcome::Reject),
            CaptureEvalCoverage::Stale => case.stale_check.is_some(),
            CaptureEvalCoverage::Duplicate => has_action(|action| {
                matches!(action, CaptureEvalActionExpectation::Duplicate { .. })
            }),
            CaptureEvalCoverage::Conflict => {
                has_action(|action| matches!(action, CaptureEvalActionExpectation::Conflict { .. }))
            }
            CaptureEvalCoverage::PromptInjection
            | CaptureEvalCoverage::Credential
            | CaptureEvalCoverage::RawTranscript
            | CaptureEvalCoverage::PrivateData => {
                case.expected.status == CapturePlanStatus::Blocked && !case.must_not_echo.is_empty()
            }
            CaptureEvalCoverage::FeedbackLoop => {
                case.expected.candidates.is_empty()
                    && case.expected.diagnostic_codes.iter().any(|code| {
                        matches!(
                            code.as_str(),
                            "generated_projection_excluded"
                                | "generated_instruction_block_excluded"
                                | "git_managed_projection_excluded"
                        )
                    })
            }
            CaptureEvalCoverage::Malformed => {
                case.expected.status == CapturePlanStatus::Blocked
                    || case
                        .expected
                        .diagnostic_codes
                        .iter()
                        .any(|code| code.contains("malformed") || code.contains("unsupported"))
            }
            CaptureEvalCoverage::AmbiguousSharing => case
                .expected
                .candidates
                .iter()
                .any(|candidate| candidate.destination == MemoryDestination::NeedsReview),
            CaptureEvalCoverage::SuppliedBytes => matches!(
                case.request.sources.first().map(|source| &source.locator),
                Some(CaptureSourceLocator::SuppliedBytes { .. })
            ),
            CaptureEvalCoverage::GitRange => matches!(
                case.request.sources.first().map(|source| &source.locator),
                Some(CaptureSourceLocator::GitRange { .. })
            ),
            CaptureEvalCoverage::GitRangeStale => {
                matches!(
                    case.request.sources.first().map(|source| &source.locator),
                    Some(CaptureSourceLocator::GitRange { .. })
                ) && matches!(
                    case.stale_check,
                    Some(CaptureEvalStaleCheck::GitObjectMissing { .. })
                )
            }
            CaptureEvalCoverage::Rename => case
                .expected
                .diagnostic_codes
                .iter()
                .any(|code| code.contains("rename")),
            CaptureEvalCoverage::Delete => case.expected.candidates.iter().any(|candidate| {
                matches!(
                    candidate.action,
                    CaptureEvalActionExpectation::NoWrite { .. }
                ) && candidate.evidence.iter().any(|evidence| {
                    matches!(
                        &evidence.semantic_location,
                        Some(CaptureSemanticLocation::GitChange { change_kind, .. })
                            if change_kind == "deleted"
                    )
                })
            }),
            CaptureEvalCoverage::Forbidden => !case.expected.forbidden_candidates.is_empty(),
            CaptureEvalCoverage::PolicyProhibited => {
                case.expected.status == CapturePlanStatus::Blocked
                    && !case.must_not_echo.is_empty()
                    && case
                        .expected
                        .diagnostic_codes
                        .iter()
                        .any(|code| code == "source_preflight_failed")
                    && case.project_files.iter().any(|fixture| {
                        fixture.path.file_name().and_then(|name| name.to_str())
                            == Some(".gitignore")
                    })
                    && matches!(
                        case.request.sources.first().map(|source| &source.locator),
                        Some(CaptureSourceLocator::ProjectDirectory { .. })
                    )
            }
        };
        if !valid {
            bail!("capture case declares unsupported {:?} coverage", declared);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct PolicyEngineRule {
    value: &'static str,
    match_kind: CaptureEvalEngineMatch,
}

fn validate_expected_policy_inputs(case: &CaptureEvalCase) -> Result<()> {
    let mut paths = BTreeSet::new();
    for input in &case.expected_policy_inputs {
        validate_fixture_path(Path::new(&input.path))?;
        if input.engine_version.trim().is_empty()
            || input.engine_version.len() > 256
            || input.engine_version.chars().any(char::is_control)
        {
            bail!("expected capture policy-input engine is invalid");
        }
        if let Some(hash) = &input.source_content_hash {
            validate_content_hash(hash, "expected capture policy-input content hash")?;
        }
        if !paths.insert(input.path.as_str()) {
            bail!("capture case contains duplicate expected policy-input paths");
        }
        let Some(rule) = policy_engine_rule(case, &input.path) else {
            bail!("capture case declares an unsupported policy input");
        };
        if input.engine_match != rule.match_kind || input.engine_version != rule.value {
            bail!("capture case declares an unsupported policy-input engine");
        }
        if Path::new(&input.path)
            .file_name()
            .and_then(|name| name.to_str())
            == Some(".gitignore")
            && input.source_content_hash.is_none()
        {
            bail!("expected capture ignore-policy inputs require an exact content hash");
        }
    }

    if matches!(
        case.request.sources.first().map(|source| &source.locator),
        Some(CaptureSourceLocator::GitRange { .. })
    ) {
        for required in [".git", ".git/config", ".git/renderer-version"] {
            if !paths.contains(required) {
                bail!("git_range evaluation must declare every mandatory policy input");
            }
        }
    }
    Ok(())
}

fn policy_engine_rule(case: &CaptureEvalCase, path: &str) -> Option<PolicyEngineRule> {
    let locator = &case.request.sources.first()?.locator;
    match (case.request.extractor.profile.as_str(), locator) {
        (
            "adr-deterministic",
            CaptureSourceLocator::ProjectDirectory {
                path: root,
                recursive,
                ..
            },
        ) if adr_gitignore_path_is_applicable(root, *recursive, path) => Some(PolicyEngineRule {
            value: ADR_GITIGNORE_ENGINE,
            match_kind: CaptureEvalEngineMatch::Exact,
        }),
        ("git-change-deterministic", CaptureSourceLocator::GitRange { .. }) => match path {
            ".git" => Some(PolicyEngineRule {
                value: GIT_REPOSITORY_IDENTITY_ENGINE,
                match_kind: CaptureEvalEngineMatch::Exact,
            }),
            ".git/config" => Some(PolicyEngineRule {
                value: GIT_LOCAL_CONFIG_ENGINE,
                match_kind: CaptureEvalEngineMatch::Exact,
            }),
            ".git/renderer-version" => Some(PolicyEngineRule {
                value: GIT_RENDERER_ENGINE_PREFIX,
                match_kind: CaptureEvalEngineMatch::Prefix,
            }),
            _ if git_range_gitignore_path_is_applicable(case, path) => Some(PolicyEngineRule {
                value: GIT_TREE_GITIGNORE_ENGINE,
                match_kind: CaptureEvalEngineMatch::Exact,
            }),
            _ => None,
        },
        _ => None,
    }
}

fn adr_gitignore_path_is_applicable(root: &str, recursive: bool, policy_path: &str) -> bool {
    let policy = Path::new(policy_path);
    if validate_fixture_path(policy).is_err()
        || policy.file_name().and_then(|name| name.to_str()) != Some(".gitignore")
    {
        return false;
    }
    let policy_parent = policy.parent().unwrap_or_else(|| Path::new(""));
    let root = Path::new(root);
    root.starts_with(policy_parent) || (recursive && policy_parent.starts_with(root))
}

fn git_range_gitignore_path_is_applicable(case: &CaptureEvalCase, policy_path: &str) -> bool {
    let policy = Path::new(policy_path);
    if validate_fixture_path(policy).is_err()
        || policy.file_name().and_then(|name| name.to_str()) != Some(".gitignore")
    {
        return false;
    }
    let policy_parent = policy.parent().unwrap_or_else(|| Path::new(""));
    let mut changed_paths = BTreeSet::<PathBuf>::new();
    if let Some(repository) = &case.git_repository {
        changed_paths.extend(
            repository
                .base_files
                .iter()
                .chain(&repository.head_files)
                .map(|fixture| fixture.path.clone()),
        );
    }
    for candidate in &case.expected.candidates {
        for evidence in &candidate.evidence {
            if let Some(CaptureSemanticLocation::GitChange {
                old_path, new_path, ..
            }) = &evidence.semantic_location
            {
                changed_paths.extend(old_path.iter().chain(new_path).map(PathBuf::from));
            }
        }
    }
    changed_paths
        .iter()
        .any(|changed| changed.starts_with(policy_parent))
}

fn plan_policy_input_violations(case: &CaptureEvalCase, plan: &CapturePlan) -> usize {
    let Some(requested) = case.request.sources.first() else {
        return 1;
    };
    let Some(snapshot) = plan.sources.first() else {
        let missing = case.expected_policy_inputs.len();
        return if plan.status == CapturePlanStatus::Ready
            || case.expected.status == CapturePlanStatus::Ready
        {
            missing.max(1)
        } else {
            missing
        };
    };
    let mut violations = plan.sources.len().saturating_sub(1);
    violations += usize::from(
        snapshot.source_id != requested.source_id || snapshot.locator != requested.locator,
    );
    violations + policy_input_set_violations(case, &snapshot.policy_inputs)
}

fn policy_input_set_violations(
    case: &CaptureEvalCase,
    actual: &[CapturePolicyInputSnapshot],
) -> usize {
    let mut violations = 0usize;
    let mut actual_by_path = BTreeMap::<&str, &CapturePolicyInputSnapshot>::new();
    for input in actual {
        if actual_by_path.insert(input.path.as_str(), input).is_some() {
            violations += 1;
        }
        let allowed = policy_engine_rule(case, &input.path).is_some_and(|rule| {
            engine_version_matches(rule.match_kind, rule.value, &input.engine_version)
        });
        if !allowed
            || validate_content_hash(
                &input.source_content_hash,
                "actual capture policy-input content hash",
            )
            .is_err()
        {
            violations += 1;
        }
    }

    let expected_paths = case
        .expected_policy_inputs
        .iter()
        .map(|input| input.path.as_str())
        .collect::<BTreeSet<_>>();
    for expected in &case.expected_policy_inputs {
        let Some(actual) = actual_by_path.get(expected.path.as_str()).copied() else {
            violations += 1;
            continue;
        };
        if !engine_version_matches(
            expected.engine_match,
            &expected.engine_version,
            &actual.engine_version,
        ) || expected
            .source_content_hash
            .as_ref()
            .is_some_and(|hash| hash != &actual.source_content_hash)
        {
            violations += 1;
        }
    }
    violations
        + actual_by_path
            .keys()
            .filter(|path| !expected_paths.contains(**path))
            .count()
}

fn engine_version_matches(
    match_kind: CaptureEvalEngineMatch,
    expected: &str,
    actual: &str,
) -> bool {
    match match_kind {
        CaptureEvalEngineMatch::Exact => actual == expected,
        CaptureEvalEngineMatch::Prefix => actual.starts_with(expected),
    }
}

fn validate_expected_evidence(evidence: &CaptureEvalExpectedEvidence) -> Result<()> {
    validate_safe_id(&evidence.source_id, "expected evidence source id")?;
    validate_safe_id(&evidence.section_kind, "expected evidence section kind")?;
    validate_content_hash(
        &evidence.source_content_hash,
        "expected evidence source content hash",
    )?;
    validate_expected_locator(&evidence.locator)?;
    if let CaptureSourceLocator::SuppliedBytes {
        source_content_hash,
        ..
    } = &evidence.locator
        && source_content_hash != &evidence.source_content_hash
    {
        bail!("expected supplied evidence hashes must agree");
    }
    if evidence.span.byte_start >= evidence.span.byte_end
        || evidence.span.line_start == 0
        || evidence.span.line_start > evidence.span.line_end
    {
        bail!("expected capture evidence span is invalid");
    }
    if let Some(location) = &evidence.semantic_location {
        validate_expected_semantic_location(location)?;
    }
    Ok(())
}

fn validate_content_hash(value: &str, label: &str) -> Result<()> {
    let Some(digest) = value.strip_prefix("blake3:") else {
        bail!("{label} must use a blake3 digest");
    };
    if digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || digest.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        bail!("{label} must use a full lowercase hexadecimal digest");
    }
    Ok(())
}

fn validate_expected_locator(locator: &CaptureSourceLocator) -> Result<()> {
    match locator {
        CaptureSourceLocator::ProjectPath { path } => {
            validate_destination_path(Path::new(path))?;
        }
        CaptureSourceLocator::ProjectDirectory {
            path,
            ignore_policy,
            include,
            ..
        } => {
            validate_destination_path(Path::new(path))?;
            validate_safe_id(ignore_policy, "expected directory ignore policy")?;
            if include.is_empty() || include.iter().any(|pattern| pattern.trim().is_empty()) {
                bail!("expected directory locator must declare bounded include patterns");
            }
        }
        CaptureSourceLocator::SuppliedBytes {
            display_name,
            media_type,
            source_content_hash,
            ..
        } => {
            if display_name.trim().is_empty()
                || display_name.len() > 256
                || display_name.contains(['/', '\\', '\0'])
                || display_name.chars().any(char::is_control)
                || media_type.trim().is_empty()
            {
                bail!("expected supplied-bytes locator fields cannot be empty");
            }
            validate_content_hash(source_content_hash, "expected supplied source content hash")?;
        }
        CaptureSourceLocator::GitRange {
            repository,
            base,
            head,
            merge_parent,
            diff_format,
            ..
        } => {
            if repository.trim().is_empty()
                || merge_parent.trim().is_empty()
                || diff_format.trim().is_empty()
            {
                bail!("expected git-range locator fields cannot be empty");
            }
            validate_git_object_id(base, "expected git-range base")?;
            validate_git_object_id(head, "expected git-range head")?;
        }
    }
    Ok(())
}

fn validate_expected_semantic_location(location: &CaptureSemanticLocation) -> Result<()> {
    match location {
        CaptureSemanticLocation::Instruction => Ok(()),
        CaptureSemanticLocation::Adr {
            field,
            status,
            target,
        } => {
            validate_safe_id(field, "expected ADR evidence field")?;
            validate_safe_id(status, "expected ADR evidence status")?;
            if let Some(target) = target
                && (target.trim().is_empty()
                    || target.len() > 512
                    || target.chars().any(char::is_control))
            {
                bail!("expected ADR supersession target is invalid");
            }
            Ok(())
        }
        CaptureSemanticLocation::GitChange {
            repository,
            base,
            head,
            old_blob,
            new_blob,
            old_path,
            new_path,
            change_kind,
            hunk,
            side,
            old_line_start,
            old_line_end,
            new_line_start,
            new_line_end,
        } => {
            if repository != "."
                || change_kind.trim().is_empty()
                || hunk.trim().is_empty()
                || side.trim().is_empty()
                || old_path.is_none() && new_path.is_none()
            {
                bail!("expected Git evidence location is incomplete");
            }
            validate_git_object_id(base, "expected Git evidence base")?;
            validate_git_object_id(head, "expected Git evidence head")?;
            validate_content_hash(hunk, "expected Git evidence hunk")?;
            for blob in [old_blob, new_blob].into_iter().flatten() {
                validate_git_object_id(blob, "expected Git evidence blob")?;
            }
            for path in [old_path, new_path].into_iter().flatten() {
                validate_destination_path(Path::new(path))?;
            }
            validate_optional_line_range(*old_line_start, *old_line_end)?;
            validate_optional_line_range(*new_line_start, *new_line_end)
        }
    }
}

fn validate_git_object_id(value: &str, label: &str) -> Result<()> {
    let Some((algorithm, digest)) = value.split_once(':') else {
        bail!("{label} must include its object-id algorithm");
    };
    let expected_len = match algorithm {
        "sha1" => 40,
        "sha256" => 64,
        _ => bail!("{label} uses an unsupported object-id algorithm"),
    };
    if digest.len() != expected_len
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || digest.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        bail!("{label} must use a full hexadecimal object id");
    }
    Ok(())
}

fn validate_optional_line_range(start: Option<u64>, end: Option<u64>) -> Result<()> {
    if start.is_some() != end.is_some()
        || start
            .zip(end)
            .is_some_and(|(start, end)| start == 0 || start > end)
    {
        bail!("expected Git evidence line range is invalid");
    }
    Ok(())
}

fn validate_thresholds(thresholds: &CaptureEvalThresholds) -> Result<()> {
    for value in [
        thresholds.min_candidate_precision,
        thresholds.min_candidate_recall,
        thresholds.min_evidence_validity,
        thresholds.min_destination_accuracy,
        thresholds.min_sensitivity_accuracy,
        thresholds.min_action_accuracy,
        thresholds.max_forbidden_hit_rate,
        thresholds.min_unsupported_outcome_accuracy,
        thresholds.min_case_pass_rate,
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            bail!("capture evaluation ratio thresholds must be between zero and one");
        }
    }
    if thresholds
        .max_p95_latency_ms
        .is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        bail!("capture evaluation latency threshold must be finite and non-negative");
    }
    Ok(())
}

fn validate_safe_id(value: &str, label: &str) -> Result<()> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 128
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
    {
        bail!("{label} must be a bounded safe identifier");
    }
    Ok(())
}

fn validate_fixture_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("capture fixture paths must be non-empty and relative");
    }
    Ok(())
}

fn validate_destination_path(path: &Path) -> Result<()> {
    validate_fixture_path(path)?;
    if path
        .components()
        .any(|component| component.as_os_str() == ".memzoi")
    {
        bail!("capture project fixtures cannot target managed state");
    }
    Ok(())
}

fn read_safe_fixture(root: &Path, relative: &Path) -> Result<Vec<u8>> {
    validate_fixture_path(relative)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!("capture fixture path contains an unsafe component");
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current)
            .context("failed to inspect capture evaluation fixture")?;
        if metadata.file_type().is_symlink() {
            bail!("capture evaluation fixtures cannot contain symbolic links");
        }
    }
    let resolved = current
        .canonicalize()
        .context("failed to resolve capture evaluation fixture")?;
    if !resolved.starts_with(root) {
        bail!("capture evaluation fixture escapes its corpus root");
    }
    let metadata =
        fs::metadata(&resolved).context("failed to inspect resolved capture evaluation fixture")?;
    if !metadata.is_file() || metadata.len() > MAX_FIXTURE_BYTES {
        bail!("capture evaluation fixture must be a bounded regular file");
    }
    fs::read(resolved).context("failed to read capture evaluation fixture")
}

fn evaluate_case(
    corpus_root: &Path,
    evaluated_at: &str,
    case: &CaptureEvalCase,
    profile: &CaptureEvalProfileExpectation,
) -> Result<EvaluatedCase> {
    let temp = TempDir::new().context("failed to create isolated capture evaluation state")?;
    let isolated_root = temp
        .path()
        .canonicalize()
        .context("failed to resolve isolated capture evaluation state")?;
    let project_root = isolated_root.join("project");
    fs::create_dir_all(&project_root)
        .context("failed to create isolated capture evaluation project")?;
    let project_root = project_root
        .canonicalize()
        .context("failed to resolve isolated capture evaluation project")?;
    let paths = MemoryPaths::with_runtime_home(project_root, isolated_root.join("runtime-home"));
    MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
    stage_case_fixtures(corpus_root, case, &paths)?;
    MemoryService::rebuild_paths(paths.clone())?;
    let clock = FixedClock::from_rfc3339(evaluated_at)?;
    drop(MemoryService::open_paths_with_clock(paths.clone(), clock)?);

    let (source_inputs, source_input_bytes) = build_source_inputs(corpus_root, case, &paths)?;
    let before = file_snapshot(&isolated_root)?;
    let started = Instant::now();
    let first = plan_capture_with_inputs(&paths, case.request.clone(), &source_inputs);
    let latency_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let first = match first {
        Ok(plan) => plan,
        Err(_) if !profile.required => {
            return Ok(EvaluatedCase {
                report: unavailable_case_report(case, false, latency_ms),
                fingerprint: None,
            });
        }
        Err(_) => {
            return Ok(EvaluatedCase {
                report: unavailable_case_report(case, true, latency_ms),
                fingerprint: None,
            });
        }
    };
    let second = plan_capture_with_inputs(&paths, case.request.clone(), &source_inputs);
    let after = file_snapshot(&isolated_root)?;
    let deterministic = second.as_ref().is_ok_and(|plan| plan == &first);
    let planning_no_write = before == after;
    let plan_bytes = serde_json::to_vec(&first)?;
    let prohibited_output_echoes = case
        .must_not_echo
        .iter()
        .filter(|marker| contains_bytes(&plan_bytes, marker.as_bytes()))
        .count();

    let requested_sources = case
        .request
        .sources
        .iter()
        .map(|source| source.source_id.as_str())
        .collect::<BTreeSet<_>>();
    let evidence_counts = evidence_counts(&paths, &source_input_bytes, &first, &requested_sources);
    let policy_input_violations = plan_policy_input_violations(case, &first);

    let mut used_actual = BTreeSet::<usize>::new();
    let mut matched = Vec::new();
    let mut missing = Vec::new();
    let mut matched_indexes = BTreeMap::<String, usize>::new();
    for expected in &case.expected.candidates {
        let matching = first
            .candidates
            .iter()
            .enumerate()
            .find(|(index, candidate)| {
                !used_actual.contains(index) && candidate_matches_expected(candidate, expected)
            });
        if let Some((index, _)) = matching {
            used_actual.insert(index);
            matched.push(expected.id.clone());
            matched_indexes.insert(expected.id.clone(), index);
        } else {
            missing.push(expected.id.clone());
        }
    }
    let unexpected_candidates = first.candidates.len().saturating_sub(used_actual.len());
    let forbidden_hits = case
        .expected
        .forbidden_candidates
        .iter()
        .filter(|forbidden| {
            first
                .candidates
                .iter()
                .any(|candidate| candidate_matches_forbidden(candidate, forbidden))
        })
        .map(|forbidden| forbidden.id.clone())
        .collect::<Vec<_>>();

    let mut destination_valid = 0usize;
    let mut sensitivity_valid = 0usize;
    let mut action_valid = 0usize;
    for expected in &case.expected.candidates {
        let Some(index) = matched_indexes.get(&expected.id).copied() else {
            continue;
        };
        let actual = &first.candidates[index];
        destination_valid += usize::from(actual.classification.destination == expected.destination);
        sensitivity_valid += usize::from(actual.classification.sensitivity == expected.sensitivity);
        action_valid += usize::from(action_matches(&actual.action, &expected.action));
    }
    let classification_checked = matched_indexes.len();

    let review_burden = review_burden(case, &first, &matched_indexes);
    let actual_diagnostics = first
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.clone())
        .collect::<Vec<_>>();
    let diagnostics_match =
        sorted_strings(&actual_diagnostics) == sorted_strings(&case.expected.diagnostic_codes);
    let profile_match = profile_matches(profile, &first.extractor);
    let plan_status_match = first.status == case.expected.status;
    let data_class_match = first.data_class == case.expected.data_class;
    let candidate_set_match = missing.is_empty() && unexpected_candidates == 0;
    let classification_match =
        destination_valid == classification_checked && sensitivity_valid == classification_checked;
    let actions_match = action_valid == classification_checked;
    let evidence_valid = evidence_counts.valid == evidence_counts.checked;
    let workflow = exercise_review_apply_workflow(
        corpus_root,
        evaluated_at,
        case,
        &paths,
        &first,
        &matched_indexes,
        &source_inputs,
    )?;

    let hard_gates = CaptureEvalHardGates {
        determinism_violations: usize::from(!deterministic),
        planning_mutation_violations: usize::from(!planning_no_write),
        invalid_evidence_items: evidence_counts
            .checked
            .saturating_sub(evidence_counts.valid),
        unnamed_source_reads: evidence_counts.unnamed + policy_input_violations,
        prohibited_output_echoes,
        review_workflow_violations: usize::from(!workflow.review_valid)
            + usize::from(!workflow.apply_valid),
        provenance_violations: usize::from(!workflow.provenance_valid),
        stale_review_acceptance_violations: usize::from(!workflow.stale_review_rejected),
        stale_apply_acceptance_violations: usize::from(!workflow.stale_apply_rejected),
        stale_write_violations: usize::from(!workflow.stale_no_write),
        direct_canonical_write_violations: usize::from(!workflow.repo_proposal_only),
        required_profile_skips: 0,
    };
    let mut assertions = BTreeMap::new();
    assertions.insert("profile_identity".to_owned(), profile_match);
    assertions.insert("plan_status".to_owned(), plan_status_match);
    assertions.insert("data_class".to_owned(), data_class_match);
    assertions.insert("diagnostics".to_owned(), diagnostics_match);
    assertions.insert("candidate_set".to_owned(), candidate_set_match);
    assertions.insert("forbidden_suppressed".to_owned(), forbidden_hits.is_empty());
    assertions.insert("classification".to_owned(), classification_match);
    assertions.insert("actions".to_owned(), actions_match);
    assertions.insert("evidence_integrity".to_owned(), evidence_valid);
    assertions.insert("review_workflow".to_owned(), workflow.review_valid);
    assertions.insert("apply_workflow".to_owned(), workflow.apply_valid);
    assertions.insert(
        "provenance_and_citations".to_owned(),
        workflow.provenance_valid,
    );
    assertions.insert(
        "repo_candidates_proposal_only".to_owned(),
        workflow.repo_proposal_only,
    );
    assertions.insert("deterministic".to_owned(), deterministic);
    assertions.insert("planning_no_write".to_owned(), planning_no_write);
    assertions.insert(
        "stale_review_rejected".to_owned(),
        workflow.stale_review_rejected,
    );
    assertions.insert(
        "stale_apply_rejected".to_owned(),
        workflow.stale_apply_rejected,
    );
    assertions.insert("stale_no_write".to_owned(), workflow.stale_no_write);
    assertions.insert(
        "prohibited_no_echo".to_owned(),
        prohibited_output_echoes == 0,
    );
    assertions.insert(
        "named_evidence_only".to_owned(),
        evidence_counts.unnamed == 0,
    );
    assertions.insert(
        "policy_inputs_declared".to_owned(),
        policy_input_violations == 0,
    );
    let passed = assertions.values().all(|passed| *passed) && hard_gates.passed();

    let fingerprint = CaptureEvalProfileFingerprint {
        profile: case.request.extractor.profile.clone(),
        required: profile.required,
        extractor_kind: first.extractor.kind.clone(),
        extractor_id: first.extractor.id.clone(),
        extractor_version: first.extractor.version.clone(),
        extractor_configuration_hash: first.extractor.configuration_hash.clone(),
        safeguard_policy_version: first.safeguards.policy_version.clone(),
        safeguard_configuration_hash: first.safeguards.configuration_hash.clone(),
    };
    Ok(EvaluatedCase {
        report: CaptureEvalCaseReport {
            id: case.id.clone(),
            profile: case.request.extractor.profile.clone(),
            execution: CaptureEvalExecutionStatus::Executed,
            plan_status: Some(first.status),
            data_class: Some(first.data_class),
            diagnostic_codes: actual_diagnostics,
            matched_candidates: matched.clone(),
            missing_candidates: missing.clone(),
            forbidden_hits,
            forbidden_opportunities: case.expected.forbidden_candidates.len(),
            unexpected_candidates,
            true_positives: matched.len(),
            false_positives: unexpected_candidates,
            false_negatives: missing.len(),
            evidence_validity: integrity_metric(evidence_counts.valid, evidence_counts.checked),
            destination_accuracy: integrity_metric(destination_valid, classification_checked),
            sensitivity_accuracy: integrity_metric(sensitivity_valid, classification_checked),
            action_accuracy: integrity_metric(action_valid, classification_checked),
            review_burden,
            hard_gates,
            observations: CaptureEvalCaseObservations {
                latency_ms: rounded(latency_ms, 3),
                payload_bytes: plan_bytes.len(),
            },
            assertions,
            passed,
        },
        fingerprint: Some(fingerprint),
    })
}

fn unavailable_case_report(
    case: &CaptureEvalCase,
    required: bool,
    latency_ms: f64,
) -> CaptureEvalCaseReport {
    let execution = if required {
        CaptureEvalExecutionStatus::Failed
    } else {
        CaptureEvalExecutionStatus::Skipped
    };
    let hard_gates = CaptureEvalHardGates {
        required_profile_skips: usize::from(required),
        ..CaptureEvalHardGates::default()
    };
    let mut assertions = BTreeMap::new();
    assertions.insert("planner_executed".to_owned(), false);
    CaptureEvalCaseReport {
        id: case.id.clone(),
        profile: case.request.extractor.profile.clone(),
        execution,
        plan_status: None,
        data_class: None,
        diagnostic_codes: vec![if required {
            "required_profile_failed".to_owned()
        } else {
            "optional_profile_unavailable".to_owned()
        }],
        matched_candidates: Vec::new(),
        missing_candidates: case
            .expected
            .candidates
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect(),
        forbidden_hits: Vec::new(),
        forbidden_opportunities: case.expected.forbidden_candidates.len(),
        unexpected_candidates: 0,
        true_positives: 0,
        false_positives: 0,
        false_negatives: case.expected.candidates.len(),
        evidence_validity: integrity_metric(0, 0),
        destination_accuracy: integrity_metric(0, 0),
        sensitivity_accuracy: integrity_metric(0, 0),
        action_accuracy: integrity_metric(0, 0),
        review_burden: CaptureEvalReviewBurden::default(),
        hard_gates,
        observations: CaptureEvalCaseObservations {
            latency_ms: rounded(latency_ms, 3),
            payload_bytes: 0,
        },
        assertions,
        passed: !required,
    }
}

fn stage_case_fixtures(
    corpus_root: &Path,
    case: &CaptureEvalCase,
    paths: &MemoryPaths,
) -> Result<()> {
    for fixture in &case.project_files {
        stage_fixture(corpus_root, fixture, &paths.project_root)?;
    }
    if let Some(repository) = &case.git_repository {
        stage_git_repository(corpus_root, repository, &case.request, paths)?;
    }
    for fixture in &case.record_fixtures {
        stage_fixture(corpus_root, fixture, &paths.records_dir())?;
    }
    let pending = paths.proposals_dir().join("pending");
    for fixture in &case.proposal_fixtures {
        stage_fixture(corpus_root, fixture, &pending)?;
    }
    Ok(())
}

fn stage_fixture(
    corpus_root: &Path,
    fixture: &CaptureEvalFileFixture,
    destination_root: &Path,
) -> Result<()> {
    let bytes = read_safe_fixture(corpus_root, &fixture.fixture)?;
    let destination = destination_root.join(&fixture.path);
    let parent = destination
        .parent()
        .context("capture evaluation fixture destination has no parent")?;
    fs::create_dir_all(parent)
        .context("failed to create capture evaluation fixture destination")?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .context("failed to stage capture evaluation fixture")?;
    output.write_all(&bytes)?;
    Ok(())
}

fn stage_git_repository(
    corpus_root: &Path,
    repository: &CaptureEvalGitRepositoryFixture,
    request: &CaptureRequest,
    paths: &MemoryPaths,
) -> Result<()> {
    prepare_eval_git_environment(&paths.project_root)?;
    run_git(
        &paths.project_root,
        &["init", "-q", "--object-format=sha1"],
        None,
    )?;
    for fixture in &repository.base_files {
        write_git_fixture(corpus_root, fixture, &paths.project_root)?;
    }
    git_add_fixtures(&paths.project_root, &repository.base_files)?;
    run_git(
        &paths.project_root,
        &["commit", "-q", "--no-gpg-sign", "--no-verify", "-m", "base"],
        Some("2000-01-01T00:00:00Z"),
    )?;
    let actual_base = run_git(&paths.project_root, &["rev-parse", "HEAD"], None)?;

    for fixture in &repository.head_files {
        write_git_fixture(corpus_root, fixture, &paths.project_root)?;
    }
    git_add_fixtures(&paths.project_root, &repository.head_files)?;
    run_git(
        &paths.project_root,
        &["commit", "-q", "--no-gpg-sign", "--no-verify", "-m", "head"],
        Some("2000-01-01T00:00:01Z"),
    )?;
    let actual_head = run_git(&paths.project_root, &["rev-parse", "HEAD"], None)?;
    let Some(CaptureSourceLocator::GitRange { base, head, .. }) =
        request.sources.first().map(|source| &source.locator)
    else {
        bail!("capture Git repository fixture requires one git_range request");
    };
    if base != &format!("sha1:{actual_base}") || head != &format!("sha1:{actual_head}") {
        bail!("capture Git fixture commits do not match the pinned request range");
    }
    Ok(())
}

fn write_git_fixture(
    corpus_root: &Path,
    fixture: &CaptureEvalFileFixture,
    project_root: &Path,
) -> Result<()> {
    let bytes = read_safe_fixture(corpus_root, &fixture.fixture)?;
    let destination = project_root.join(&fixture.path);
    let parent = destination
        .parent()
        .context("capture Git fixture destination has no parent")?;
    fs::create_dir_all(parent).context("failed to create capture Git fixture destination")?;
    fs::write(destination, bytes).context("failed to stage capture Git fixture")
}

fn git_add_fixtures(project_root: &Path, fixtures: &[CaptureEvalFileFixture]) -> Result<()> {
    let mut command = git_command(project_root)?;
    command.arg("add").arg("--");
    for fixture in fixtures {
        command.arg(&fixture.path);
    }
    run_git_command(command, "add capture evaluation Git fixtures").map(|_| ())
}

fn run_git(project_root: &Path, args: &[&str], date: Option<&str>) -> Result<String> {
    let mut command = git_command(project_root)?;
    command.args(args);
    if let Some(date) = date {
        command
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date);
    }
    run_git_command(command, "run capture evaluation Git command")
}

fn prepare_eval_git_environment(project_root: &Path) -> Result<()> {
    let root = eval_git_environment_root(project_root);
    for directory in [
        "home",
        "xdg-cache",
        "xdg-config",
        "xdg-data",
        "xdg-state",
        "tmp",
    ] {
        fs::create_dir_all(root.join(directory))
            .context("failed to create hermetic capture evaluation Git environment")?;
    }
    Ok(())
}

fn eval_git_environment_root(project_root: &Path) -> PathBuf {
    project_root
        .parent()
        .expect("capture evaluation project root has an isolated parent")
        .join("git-env")
}

fn git_command(project_root: &Path) -> Result<Command> {
    let path = std::env::var_os("PATH").context("capture evaluation Git requires PATH")?;
    let environment = eval_git_environment_root(project_root);
    let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("PATH", path)
        .env("HOME", environment.join("home"))
        .env("XDG_CACHE_HOME", environment.join("xdg-cache"))
        .env("XDG_CONFIG_HOME", environment.join("xdg-config"))
        .env("XDG_DATA_HOME", environment.join("xdg-data"))
        .env("XDG_STATE_HOME", environment.join("xdg-state"))
        .env("TMPDIR", environment.join("tmp"))
        .env("TMP", environment.join("tmp"))
        .env("TEMP", environment.join("tmp"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", null_device)
        .env("GIT_CONFIG_GLOBAL", null_device)
        .env("GIT_CONFIG_COUNT", "0")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "Capture Eval")
        .env("GIT_AUTHOR_EMAIL", "capture-eval@example.invalid")
        .env("GIT_COMMITTER_NAME", "Capture Eval")
        .env("GIT_COMMITTER_EMAIL", "capture-eval@example.invalid")
        .current_dir(project_root)
        .arg("-c")
        .arg(format!("core.hooksPath={null_device}"))
        .arg("-c")
        .arg("color.ui=false")
        .arg("-c")
        .arg("core.quotePath=true")
        .arg("-c")
        .arg("core.bigFileThreshold=16m")
        .arg("-c")
        .arg(format!("core.attributesFile={null_device}"))
        .arg("-c")
        .arg("submodule.recurse=false")
        .arg("-c")
        .arg("trace2.normalTarget=0")
        .arg("-c")
        .arg("trace2.perfTarget=0")
        .arg("-c")
        .arg("trace2.eventTarget=0");
    if let Some(system_root) = cfg!(windows)
        .then(|| std::env::var_os("SystemRoot"))
        .flatten()
    {
        command.env("SystemRoot", system_root);
    }
    Ok(command)
}

fn run_git_command(mut command: Command, label: &str) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("failed to {label}"))?;
    if !output.status.success() {
        bail!("failed to {label}");
    }
    String::from_utf8(output.stdout)
        .context("capture evaluation Git output was not UTF-8")
        .map(|output| output.trim().to_owned())
}

fn build_source_inputs(
    corpus_root: &Path,
    case: &CaptureEvalCase,
    paths: &MemoryPaths,
) -> Result<(CaptureSourceInputs, BTreeMap<String, Vec<u8>>)> {
    let mut inputs = CaptureSourceInputs::new();
    let mut bytes_by_id = BTreeMap::new();
    for fixture in &case.source_inputs {
        let bytes = read_safe_fixture(corpus_root, &fixture.fixture)?;
        inputs.insert_supplied_bytes(fixture.source_id.clone(), bytes.clone())?;
        bytes_by_id.insert(fixture.source_id.clone(), bytes);
    }
    for source in &case.request.sources {
        if let CaptureSourceLocator::GitRange {
            repository,
            base,
            head,
            rename_detection,
            ..
        } = &source.locator
        {
            bytes_by_id.insert(
                source.source_id.clone(),
                render_git_range_source(paths, repository, base, head, *rename_detection)?,
            );
        }
    }
    Ok((inputs, bytes_by_id))
}

fn render_git_range_source(
    paths: &MemoryPaths,
    repository: &str,
    base: &str,
    head: &str,
    rename_detection: bool,
) -> Result<Vec<u8>> {
    if repository != "." {
        bail!("capture evaluation Git renderer requires the staged repository root");
    }
    let (_, base_oid) = base
        .split_once(':')
        .context("capture evaluation Git base is not algorithm-qualified")?;
    let (_, head_oid) = head
        .split_once(':')
        .context("capture evaluation Git head is not algorithm-qualified")?;
    let mut command = git_range_renderer_command(paths, base_oid, head_oid, rename_detection)?;
    let output = command
        .output()
        .context("failed to render capture evaluation Git range")?;
    if !output.status.success() {
        bail!("failed to render capture evaluation Git range");
    }
    Ok(output.stdout)
}

fn git_range_renderer_command(
    paths: &MemoryPaths,
    base_oid: &str,
    head_oid: &str,
    rename_detection: bool,
) -> Result<Command> {
    let mut command = git_command(&paths.project_root)?;
    command
        .arg("--no-pager")
        .arg("--git-dir")
        .arg(paths.project_root.join(".git"))
        .arg("--literal-pathspecs")
        .arg("-c")
        .arg("diff.external=")
        .arg("-c")
        .arg("diff.orderFile=")
        .arg("-c")
        .arg("diff.interHunkContext=0")
        .arg("-c")
        .arg("diff.noprefix=false")
        .arg("-c")
        .arg("diff.mnemonicPrefix=false")
        .arg("-c")
        .arg("diff.srcPrefix=a/")
        .arg("-c")
        .arg("diff.dstPrefix=b/")
        .arg("-c")
        .arg("diff.linePrefix=")
        .arg("-c")
        .arg("diff.outputIndicatorNew=+")
        .arg("-c")
        .arg("diff.outputIndicatorOld=-")
        .arg("-c")
        .arg("diff.outputIndicatorContext= ")
        .arg("-c")
        .arg("diff.suppressBlankEmpty=false")
        .args(git_range_renderer_args(
            base_oid,
            head_oid,
            rename_detection,
        ))
        .env("GIT_ATTR_SOURCE", head_oid);
    Ok(command)
}

fn git_range_renderer_args(base_oid: &str, head_oid: &str, rename_detection: bool) -> Vec<String> {
    vec![
        "diff-tree".to_owned(),
        "-r".to_owned(),
        "-p".to_owned(),
        "--no-commit-id".to_owned(),
        "--full-index".to_owned(),
        "--no-ext-diff".to_owned(),
        "--no-textconv".to_owned(),
        "--no-color".to_owned(),
        if cfg!(windows) {
            "-ONUL".to_owned()
        } else {
            "-O/dev/null".to_owned()
        },
        "--diff-algorithm=myers".to_owned(),
        "--no-indent-heuristic".to_owned(),
        "--inter-hunk-context=0".to_owned(),
        "--unified=3".to_owned(),
        "--src-prefix=a/".to_owned(),
        "--dst-prefix=b/".to_owned(),
        "--line-prefix=".to_owned(),
        "--output-indicator-new=+".to_owned(),
        "--output-indicator-old=-".to_owned(),
        "--output-indicator-context= ".to_owned(),
        "--no-relative".to_owned(),
        "--submodule=short".to_owned(),
        if rename_detection {
            "--find-renames=100%".to_owned()
        } else {
            "--no-renames".to_owned()
        },
        "--rename-empty".to_owned(),
        "-l0".to_owned(),
        base_oid.to_owned(),
        head_oid.to_owned(),
        "--".to_owned(),
    ]
}

fn file_snapshot(root: &Path) -> Result<CaptureEvalTreeSnapshot> {
    fn visit(root: &Path, current: &Path, snapshot: &mut CaptureEvalTreeSnapshot) -> Result<()> {
        let mut entries = fs::read_dir(current)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            let relative = path.strip_prefix(root)?.to_path_buf();
            if metadata.is_dir() {
                snapshot.insert(relative, CaptureEvalTreeEntry::Directory);
                visit(root, &path, snapshot)?;
            } else if metadata.is_file() {
                snapshot.insert(
                    relative,
                    CaptureEvalTreeEntry::RegularFile(fs::read(&path)?),
                );
            } else if metadata.file_type().is_symlink() {
                snapshot.insert(
                    relative,
                    CaptureEvalTreeEntry::Symlink(fs::read_link(&path)?),
                );
            } else {
                bail!("capture evaluation state contains an unsupported filesystem entry");
            }
        }
        Ok(())
    }

    let mut snapshot = BTreeMap::new();
    snapshot.insert(PathBuf::new(), CaptureEvalTreeEntry::Directory);
    visit(root, root, &mut snapshot)?;
    Ok(snapshot)
}

fn exercise_review_apply_workflow(
    corpus_root: &Path,
    evaluated_at: &str,
    case: &CaptureEvalCase,
    paths: &MemoryPaths,
    plan: &CapturePlan,
    matched: &BTreeMap<String, usize>,
    source_inputs: &CaptureSourceInputs,
) -> Result<WorkflowChecks> {
    if plan.candidates.is_empty() {
        return Ok(WorkflowChecks::default());
    }

    let Some(review_input) = build_eval_review_input(case, plan, matched) else {
        return Ok(WorkflowChecks {
            review_valid: false,
            apply_valid: false,
            provenance_valid: false,
            ..WorkflowChecks::default()
        });
    };
    let review = match build_capture_review_with_inputs(
        paths,
        plan,
        review_input.clone(),
        source_inputs,
        "capture-eval-reviewer",
        evaluated_at,
    ) {
        Ok(review) => review,
        Err(_) => {
            return Ok(WorkflowChecks {
                review_valid: false,
                apply_valid: false,
                provenance_valid: false,
                ..WorkflowChecks::default()
            });
        }
    };
    let mut checks = WorkflowChecks {
        review_valid: review_matches_expectations(case, plan, matched, &review),
        ..WorkflowChecks::default()
    };
    let service = MemoryService::open_paths_with_clock(
        paths.clone(),
        FixedClock::from_rfc3339(evaluated_at)?,
    )?;
    let isolated_root = paths
        .project_root
        .parent()
        .context("isolated capture evaluation project has no state root")?;

    if let Some(stale) = &case.stale_check {
        let mut replacement_inputs = None;
        let mut restore_file = None;
        match stale {
            CaptureEvalStaleCheck::ProjectPath {
                path,
                replacement_fixture,
            } => {
                let destination = paths.project_root.join(path);
                let original = fs::read(&destination)
                    .context("failed to read capture evaluation stale source before replacement")?;
                let replacement = read_safe_fixture(corpus_root, replacement_fixture)?;
                fs::write(&destination, replacement)
                    .context("failed to stage capture evaluation stale replacement")?;
                restore_file = Some((destination, original));
            }
            CaptureEvalStaleCheck::SuppliedBytes {
                source_id,
                replacement_fixture,
            } => {
                let replacement = read_safe_fixture(corpus_root, replacement_fixture)?;
                let mut inputs = CaptureSourceInputs::new();
                inputs.insert_supplied_bytes(source_id.clone(), replacement)?;
                replacement_inputs = Some(inputs);
            }
            CaptureEvalStaleCheck::GitObjectMissing { oid } => {
                let (_, digest) = oid
                    .split_once(':')
                    .context("stale Git object is missing its algorithm")?;
                let object = paths
                    .project_root
                    .join(".git/objects")
                    .join(&digest[..2])
                    .join(&digest[2..]);
                let bytes = fs::read(&object).context("failed to read loose stale Git object")?;
                fs::remove_file(&object).context("failed to remove loose stale Git object")?;
                restore_file = Some((object, bytes));
            }
        }
        let stale_source_inputs = replacement_inputs.as_ref().unwrap_or(source_inputs);
        let stale_before = file_snapshot(isolated_root)?;
        let stale_review = build_capture_review_with_inputs(
            paths,
            plan,
            review_input.clone(),
            stale_source_inputs,
            "capture-eval-reviewer",
            evaluated_at,
        );
        let stale_after_review = file_snapshot(isolated_root)?;
        let stale_apply = service.apply_capture_with_inputs(
            "capture-eval-router",
            plan.clone(),
            review.clone(),
            stale_source_inputs,
            &plan.plan_id,
            &review.review_id,
        );
        let stale_after_apply = file_snapshot(isolated_root)?;
        if let Some((destination, original)) = restore_file {
            fs::write(&destination, original)
                .context("failed to restore capture evaluation stale source")?;
        }

        checks.stale_review_rejected = stale_review.is_err();
        checks.stale_apply_rejected = stale_apply.is_err();
        checks.stale_no_write =
            stale_before == stale_after_review && stale_before == stale_after_apply;
        if !checks.stale_apply_rejected || !checks.stale_no_write {
            checks.apply_valid = false;
            checks.provenance_valid = false;
            return Ok(checks);
        }
    }

    let state_before = capture_managed_state_snapshot(&service, isolated_root)?;
    let applied = match service.apply_capture_with_inputs(
        "capture-eval-router",
        plan.clone(),
        review.clone(),
        source_inputs,
        &plan.plan_id,
        &review.review_id,
    ) {
        Ok(applied) => applied,
        Err(_) => {
            checks.apply_valid = false;
            checks.provenance_valid = false;
            return Ok(checks);
        }
    };
    let state_after = capture_managed_state_snapshot(&service, isolated_root)?;
    let declared_delta_valid = declared_apply_delta_matches(
        paths,
        isolated_root,
        &state_before,
        &state_after,
        plan,
        &applied,
    );
    let (apply_valid, provenance_valid, repo_routes_valid) =
        validate_applied_workflow(&service, plan, &review, &applied)?;
    checks.apply_valid = apply_valid && declared_delta_valid;
    checks.provenance_valid = provenance_valid;
    let record_root = relative_from(isolated_root, &paths.records_dir());
    checks.repo_proposal_only = subtree_snapshot(&state_before.files, &record_root)
        == subtree_snapshot(&state_after.files, &record_root)
        && repo_routes_valid;
    Ok(checks)
}

fn build_eval_review_input(
    case: &CaptureEvalCase,
    plan: &CapturePlan,
    matched: &BTreeMap<String, usize>,
) -> Option<CaptureReviewInput> {
    if case.review_outcomes.len() != plan.candidates.len() {
        return None;
    }
    let decisions = case
        .review_outcomes
        .iter()
        .map(|expected| {
            let index = matched.get(&expected.candidate).copied()?;
            let candidate = plan.candidates.get(index)?;
            Some(CaptureReviewDecisionInput {
                candidate_id: candidate.candidate_id.clone(),
                outcome: expected.outcome,
                reason_code: expected.reason_code.clone(),
                memory: expected.memory.clone(),
                requested_destination: expected.requested_destination,
                content_class: expected.content_class,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(CaptureReviewInput {
        schema: crate::CAPTURE_REVIEW_INPUT_SCHEMA.to_owned(),
        plan_id: plan.plan_id.clone(),
        prior_review_id: None,
        decisions,
    })
}

fn review_matches_expectations(
    case: &CaptureEvalCase,
    plan: &CapturePlan,
    matched: &BTreeMap<String, usize>,
    review: &CaptureReview,
) -> bool {
    if review.plan_id != plan.plan_id || review.decisions.len() != case.review_outcomes.len() {
        return false;
    }
    case.review_outcomes.iter().all(|expected| {
        let Some(index) = matched.get(&expected.candidate).copied() else {
            return false;
        };
        let Some(candidate) = plan.candidates.get(index) else {
            return false;
        };
        review.decisions.iter().any(|decision| {
            decision.candidate_id == candidate.candidate_id
                && decision.outcome == expected.outcome
                && decision.reason_code == expected.reason_code
                && match expected.outcome {
                    CaptureReviewOutcome::Accept => {
                        decision.reviewed_candidate.as_ref() == Some(candidate)
                    }
                    CaptureReviewOutcome::Reject | CaptureReviewOutcome::Defer => {
                        decision.reviewed_candidate.is_none()
                    }
                    CaptureReviewOutcome::Edit => {
                        decision
                            .reviewed_candidate
                            .as_ref()
                            .is_some_and(|reviewed| {
                                expected.memory.as_ref() == Some(&reviewed.memory)
                                    && expected.requested_destination.is_none_or(|destination| {
                                        reviewed.classification.destination == destination
                                    })
                            })
                    }
                }
        })
    })
}

fn capture_managed_state_snapshot(
    service: &MemoryService,
    isolated_root: &Path,
) -> Result<CaptureManagedStateSnapshot> {
    let inventory = service.file_proposal_inventory()?;
    let mut pending_proposal_ids = inventory
        .pending
        .into_iter()
        .map(|entry| entry.proposal.id)
        .collect::<Vec<_>>();
    pending_proposal_ids.sort();
    let mut resolved_proposal_ids = inventory
        .resolved
        .into_iter()
        .map(|entry| entry.proposal.id)
        .collect::<Vec<_>>();
    resolved_proposal_ids.sort();
    let mut local_records = service.list_local_memory()?;
    local_records.sort_by(|left, right| left.id.cmp(&right.id));
    let mut checkpoints = service.list_checkpoints()?;
    checkpoints.sort_by(|left, right| left.id.cmp(&right.id));
    let mut events = Vec::new();
    service.for_each_event(|event| {
        events.push(event);
        Ok(())
    })?;
    events.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(CaptureManagedStateSnapshot {
        files: file_snapshot(isolated_root)?,
        pending_proposal_ids,
        resolved_proposal_ids,
        local_records,
        checkpoints,
        events,
        repo_index_drift: service.repo_index_drift()?,
    })
}

fn declared_apply_delta_matches(
    paths: &MemoryPaths,
    isolated_root: &Path,
    before: &CaptureManagedStateSnapshot,
    after: &CaptureManagedStateSnapshot,
    plan: &CapturePlan,
    applied: &CaptureApplyResult,
) -> bool {
    let expected_proposal_paths = applied
        .writes
        .iter()
        .filter_map(|write| match write {
            CaptureWrite::ProposalFile { path, .. } => {
                Some(relative_from(isolated_root, &paths.project_root.join(path)))
            }
            CaptureWrite::RuntimeRecord { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    let expected_proposal_ids = applied
        .writes
        .iter()
        .filter_map(|write| match write {
            CaptureWrite::ProposalFile { proposal_id, .. } => Some(proposal_id.clone()),
            CaptureWrite::RuntimeRecord { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    let expected_local_ids = applied
        .writes
        .iter()
        .filter_map(|write| match write {
            CaptureWrite::RuntimeRecord {
                record_id,
                destination: MemoryDestination::Local,
                ..
            } => Some(record_id.clone()),
            CaptureWrite::ProposalFile { .. } | CaptureWrite::RuntimeRecord { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    let expected_runtime_ids = applied
        .writes
        .iter()
        .filter_map(|write| match write {
            CaptureWrite::RuntimeRecord { record_id, .. } => Some(record_id.clone()),
            CaptureWrite::ProposalFile { .. } => None,
        })
        .collect::<BTreeSet<_>>();

    let proposal_root = relative_from(isolated_root, &paths.proposals_dir());
    let mut expected_proposal_directories = BTreeSet::new();
    for proposal in &expected_proposal_paths {
        for parent in proposal.ancestors().skip(1) {
            if !parent.starts_with(&proposal_root) {
                break;
            }
            expected_proposal_directories.insert(parent.to_path_buf());
        }
    }
    let mut allowed_file_changes = expected_proposal_paths
        .union(&expected_proposal_directories)
        .cloned()
        .collect::<BTreeSet<_>>();
    if !applied.writes.is_empty() {
        for path in sqlite_artifact_paths(&paths.db_path) {
            allowed_file_changes.insert(relative_from(isolated_root, &path));
        }
    }
    let expects_origin_outcomes = plan.candidates.iter().any(|candidate| {
        !matches!(candidate.action, CaptureAction::Replay { .. })
            && crate::capture::capture_origin_is_admissible(candidate)
    });
    if !expected_runtime_ids.is_empty() || expects_origin_outcomes {
        for path in sqlite_artifact_paths(&paths.shared_db_path) {
            allowed_file_changes.insert(relative_from(isolated_root, &path));
        }
    }
    let changed_files = before
        .files
        .keys()
        .chain(after.files.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.files.get(path) != after.files.get(path))
        .collect::<BTreeSet<_>>();
    if !changed_files.is_subset(&allowed_file_changes)
        || expected_proposal_directories.iter().any(|path| {
            !matches!(
                (before.files.get(path), after.files.get(path)),
                (None, Some(CaptureEvalTreeEntry::Directory))
                    | (
                        Some(CaptureEvalTreeEntry::Directory),
                        Some(CaptureEvalTreeEntry::Directory)
                    )
            )
        })
        || expected_proposal_paths.iter().any(|path| {
            before.files.contains_key(path)
                || !matches!(
                    after.files.get(path),
                    Some(CaptureEvalTreeEntry::RegularFile(_))
                )
        })
    {
        return false;
    }

    let before_pending = before
        .pending_proposal_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let after_pending = after
        .pending_proposal_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if after_pending
        != before_pending
            .union(&expected_proposal_ids)
            .cloned()
            .collect::<BTreeSet<_>>()
        || before.resolved_proposal_ids != after.resolved_proposal_ids
        || before.checkpoints != after.checkpoints
        || before.repo_index_drift != after.repo_index_drift
    {
        return false;
    }

    let before_local = before
        .local_records
        .iter()
        .map(|record| (record.id.clone(), record))
        .collect::<BTreeMap<_, _>>();
    let after_local = after
        .local_records
        .iter()
        .map(|record| (record.id.clone(), record))
        .collect::<BTreeMap<_, _>>();
    if before_local
        .iter()
        .any(|(id, record)| after_local.get(id) != Some(record))
        || after_local
            .keys()
            .filter(|id| !before_local.contains_key(*id))
            .cloned()
            .collect::<BTreeSet<_>>()
            != expected_local_ids
    {
        return false;
    }

    let before_events = before
        .events
        .iter()
        .map(|event| (event.id.clone(), event))
        .collect::<BTreeMap<_, _>>();
    let after_events = after
        .events
        .iter()
        .map(|event| (event.id.clone(), event))
        .collect::<BTreeMap<_, _>>();
    if before_events
        .iter()
        .any(|(id, event)| after_events.get(id) != Some(event))
    {
        return false;
    }
    let new_events = after_events
        .iter()
        .filter(|(id, _)| !before_events.contains_key(*id))
        .map(|(_, event)| *event)
        .collect::<Vec<_>>();
    let proposal_event_count = usize::from(!expected_proposal_ids.is_empty());
    if new_events.len() != expected_runtime_ids.len() + proposal_event_count
        || new_events
            .iter()
            .filter(|event| event.event_type == "capture.apply_committed")
            .count()
            != proposal_event_count
        || expected_runtime_ids.iter().any(|record_id| {
            !new_events.iter().any(|event| {
                event.event_type == "memory.capture_routed"
                    && event.record_id.as_deref() == Some(record_id)
                    && capture_event_identity_matches(event, applied)
            })
        })
        || new_events.iter().any(|event| {
            !matches!(
                event.event_type.as_str(),
                "capture.apply_committed" | "memory.capture_routed"
            ) || !capture_event_identity_matches(event, applied)
        })
    {
        return false;
    }
    true
}

fn capture_event_identity_matches(event: &MemoryEvent, applied: &CaptureApplyResult) -> bool {
    event.actor == "capture-eval-router"
        && event
            .payload
            .get("plan_id")
            .and_then(serde_json::Value::as_str)
            == Some(applied.plan_id.as_str())
        && event
            .payload
            .get("review_id")
            .and_then(serde_json::Value::as_str)
            == Some(applied.review_id.as_str())
}

fn sqlite_artifact_paths(database: &Path) -> Vec<PathBuf> {
    let mut paths = vec![database.to_path_buf()];
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut name = database
            .file_name()
            .expect("capture database path has a file name")
            .to_os_string();
        name.push(suffix);
        paths.push(database.with_file_name(name));
    }
    paths
}

fn relative_from(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root)
        .expect("isolated capture path must remain beneath its root")
        .to_path_buf()
}

fn subtree_snapshot(files: &CaptureEvalTreeSnapshot, root: &Path) -> CaptureEvalTreeSnapshot {
    files
        .iter()
        .filter(|(path, _)| path.starts_with(root))
        .map(|(path, bytes)| (path.clone(), bytes.clone()))
        .collect()
}

fn validate_applied_workflow(
    service: &MemoryService,
    plan: &CapturePlan,
    review: &CaptureReview,
    applied: &CaptureApplyResult,
) -> Result<(bool, bool, bool)> {
    let selected = review
        .decisions
        .iter()
        .filter_map(|decision| {
            decision
                .reviewed_candidate
                .as_ref()
                .map(|candidate| (decision, candidate))
        })
        .collect::<Vec<_>>();
    let mut apply_valid = applied.plan_id == plan.plan_id
        && applied.review_id == review.review_id
        && applied.writes.len() == selected.len();
    let mut provenance_valid = true;
    let mut repo_routes_valid = true;
    let inventory = service.file_proposal_inventory()?;
    if !inventory.errors.is_empty() {
        provenance_valid = false;
    }

    for (decision, candidate) in selected {
        let matching_writes = applied
            .writes
            .iter()
            .filter(|write| capture_write_candidate_id(write) == &candidate.candidate_id)
            .collect::<Vec<_>>();
        if matching_writes.len() != 1 {
            apply_valid = false;
            provenance_valid = false;
            continue;
        }
        let write = matching_writes[0];
        match (&candidate.action, write) {
            (
                CaptureAction::CreateProposal { proposal_id, path },
                CaptureWrite::ProposalFile {
                    proposal_id: written_proposal_id,
                    path: written_path,
                    ..
                },
            ) => {
                apply_valid &= proposal_id == written_proposal_id && path == written_path;
                let proposal_entry = inventory
                    .pending
                    .iter()
                    .find(|entry| entry.proposal.id == *proposal_id);
                let packet_valid = proposal_entry.is_some_and(|entry| {
                    entry.proposal.capture.as_ref().is_some_and(|provenance| {
                        capture_provenance_matches(provenance, plan, review, decision, candidate)
                    }) && proposal_sources_match(&entry.proposal.sources, &candidate.evidence)
                });
                let canonical_citation_valid = match proposal_entry {
                    Some(entry) => proposal_canonical_citation_matches(
                        service, entry, plan, review, decision, candidate,
                    )?,
                    None => false,
                };
                provenance_valid &= packet_valid && canonical_citation_valid;
            }
            (
                CaptureAction::CreateRuntime { route },
                CaptureWrite::RuntimeRecord {
                    record_id,
                    destination,
                    ..
                },
            ) => {
                let expected_destination = match route {
                    MemoryWriteRoute::RuntimeLocal => Some(MemoryDestination::Local),
                    MemoryWriteRoute::RuntimeSession => Some(MemoryDestination::Session),
                    MemoryWriteRoute::FileBackedProposal | MemoryWriteRoute::NoWrite => None,
                };
                apply_valid &= expected_destination == Some(*destination);
                provenance_valid &= runtime_citation_matches(
                    service,
                    record_id,
                    *destination,
                    plan,
                    review,
                    decision,
                    candidate,
                )?;
            }
            _ => {
                apply_valid = false;
                provenance_valid = false;
            }
        }
        if candidate.classification.destination == MemoryDestination::Repo {
            repo_routes_valid &= matches!(
                (&candidate.action, write),
                (
                    CaptureAction::CreateProposal { .. },
                    CaptureWrite::ProposalFile { .. }
                )
            );
        }
    }

    let selected_ids = review
        .decisions
        .iter()
        .filter_map(|decision| {
            decision
                .reviewed_candidate
                .as_ref()
                .map(|candidate| candidate.candidate_id.as_str())
        })
        .collect::<BTreeSet<_>>();
    apply_valid &= applied
        .writes
        .iter()
        .all(|write| selected_ids.contains(capture_write_candidate_id(write).as_str()));
    Ok((apply_valid, provenance_valid, repo_routes_valid))
}

fn capture_write_candidate_id(write: &CaptureWrite) -> &String {
    match write {
        CaptureWrite::ProposalFile { candidate_id, .. }
        | CaptureWrite::RuntimeRecord { candidate_id, .. } => candidate_id,
    }
}

fn capture_provenance_matches(
    provenance: &CaptureProvenance,
    plan: &CapturePlan,
    review: &CaptureReview,
    decision: &crate::CaptureReviewDecision,
    candidate: &CaptureCandidate,
) -> bool {
    let original = plan
        .candidates
        .iter()
        .find(|candidate| candidate.candidate_id == decision.candidate_id);
    original.is_some_and(|original| {
        provenance.plan_id == plan.plan_id
            && provenance.review_id == review.review_id
            && provenance.claim_id == original.claim_id
            && provenance.reviewed_claim_id == candidate.claim_id
            && provenance.candidate_id == original.candidate_id
            && provenance.reviewed_candidate_id == candidate.candidate_id
            && provenance.extraction == candidate.extraction
            && provenance.evidence == candidate.evidence
            && provenance.classification == candidate.classification
            && provenance.destination == candidate.classification.destination
            && provenance.sensitivity == candidate.classification.sensitivity
            && provenance.review_outcome == decision.outcome
            && provenance.review_reason_code == decision.reason_code
            && provenance.reviewed_by == review.reviewed_by
            && provenance.reviewed_at == review.reviewed_at
            && provenance.routed_by == "capture-eval-router"
    })
}

fn compact_capture_provenance_matches(
    provenance: &CaptureProvenance,
    plan: &CapturePlan,
    review: &CaptureReview,
    decision: &crate::CaptureReviewDecision,
    candidate: &CaptureCandidate,
) -> bool {
    let mut expected_evidence = candidate.evidence.clone();
    for evidence in &mut expected_evidence {
        evidence.text = None;
    }
    let mut expanded = provenance.clone();
    expanded.evidence = candidate.evidence.clone();
    capture_provenance_matches(&expanded, plan, review, decision, candidate)
        && provenance.evidence == expected_evidence
}

fn proposal_sources_match(
    actual: &[crate::OkfProposalSource],
    evidence: &[CaptureEvidence],
) -> bool {
    actual.len() == evidence.len()
        && actual.iter().zip(evidence).all(|(actual, evidence)| {
            let path = evidence.locator.project_path().map(str::to_owned);
            let reference = (path.is_none() || evidence.semantic_location.is_some())
                .then(|| evidence.durable_reference());
            actual.path == path && actual.reference == reference && actual.url.is_none()
        })
}

fn proposal_canonical_citation_matches(
    service: &MemoryService,
    entry: &crate::FileProposalInventoryEntry,
    plan: &CapturePlan,
    review: &CaptureReview,
    decision: &crate::CaptureReviewDecision,
    candidate: &CaptureCandidate,
) -> Result<bool> {
    let resolved = service.apply_file_proposal_inventory_entry(entry, "capture-eval-maintainer")?;
    let Some(record) = resolved.record.as_ref() else {
        return Ok(false);
    };
    if record.destination != MemoryDestination::Repo
        || !record.capture.as_ref().is_some_and(|provenance| {
            compact_capture_provenance_matches(provenance, plan, review, decision, candidate)
        })
    {
        return Ok(false);
    }
    recalled_citation_matches(
        service,
        &record.id,
        MemoryDestination::Repo,
        plan,
        review,
        decision,
        candidate,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn runtime_citation_matches(
    service: &MemoryService,
    record_id: &str,
    destination: MemoryDestination,
    plan: &CapturePlan,
    review: &CaptureReview,
    decision: &crate::CaptureReviewDecision,
    candidate: &CaptureCandidate,
) -> Result<bool> {
    recalled_citation_matches(
        service,
        record_id,
        destination,
        plan,
        review,
        decision,
        candidate,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn recalled_citation_matches(
    service: &MemoryService,
    record_id: &str,
    destination: MemoryDestination,
    plan: &CapturePlan,
    review: &CaptureReview,
    decision: &crate::CaptureReviewDecision,
    candidate: &CaptureCandidate,
    compact: bool,
) -> Result<bool> {
    let mut results = service.search_memory(SearchInput {
        query: candidate.memory.title.clone(),
        destination: Some(destination),
        limit: 100,
        ..SearchInput::default()
    })?;
    if !results.iter().any(|result| result.record.id == record_id) {
        results = service.search_memory(SearchInput {
            query: candidate.memory.body.clone(),
            destination: Some(destination),
            limit: 100,
            ..SearchInput::default()
        })?;
    }
    let Some(result) = results.iter().find(|result| result.record.id == record_id) else {
        return Ok(false);
    };
    let expected_reference = candidate
        .evidence
        .first()
        .map(CaptureEvidence::durable_reference);
    let matches_provenance = |provenance: &CaptureProvenance| {
        if compact {
            compact_capture_provenance_matches(provenance, plan, review, decision, candidate)
        } else {
            capture_provenance_matches(provenance, plan, review, decision, candidate)
        }
    };
    let provenance_matches = result
        .record
        .capture
        .as_ref()
        .is_some_and(matches_provenance);
    let citation_matches = result.citations.iter().any(|citation| {
        citation.record_id == record_id
            && (compact || citation.source_kind.as_deref() == Some("memzoi-capture"))
            && citation.source_ref == expected_reference
            && citation.capture.as_ref().is_some_and(matches_provenance)
    });
    Ok(provenance_matches
        && (compact || result.record.source_kind.as_deref() == Some("memzoi-capture"))
        && result.record.source_ref == expected_reference
        && citation_matches)
}

fn candidate_matches_expected(
    candidate: &CaptureCandidate,
    expected: &CaptureEvalExpectedCandidate,
) -> bool {
    candidate.memory == expected.memory
        && evidence_shape_matches(&candidate.evidence, &expected.evidence)
}

fn candidate_matches_forbidden(
    candidate: &CaptureCandidate,
    forbidden: &CaptureEvalCandidateMatcher,
) -> bool {
    candidate.memory == forbidden.memory
        && (forbidden.evidence.is_empty()
            || evidence_shape_matches(&candidate.evidence, &forbidden.evidence))
}

fn evidence_shape_matches(
    actual: &[CaptureEvidence],
    expected: &[CaptureEvalExpectedEvidence],
) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(actual, expected)| {
            actual.source_id == expected.source_id
                && actual.locator == expected.locator
                && actual.source_content_hash == expected.source_content_hash
                && actual.span == expected.span
                && actual.heading_path == expected.heading_path
                && actual.section_kind == expected.section_kind
                && actual.semantic_location == expected.semantic_location
        })
}

fn action_matches(actual: &CaptureAction, expected: &CaptureEvalActionExpectation) -> bool {
    match (actual, expected) {
        (CaptureAction::CreateProposal { .. }, CaptureEvalActionExpectation::CreateProposal) => {
            true
        }
        (
            CaptureAction::CreateRuntime { route: actual },
            CaptureEvalActionExpectation::CreateRuntime { route: expected },
        ) => actual == expected,
        (
            CaptureAction::Duplicate { matches },
            CaptureEvalActionExpectation::Duplicate {
                match_count,
                match_kinds,
            },
        ) => match_expectation(matches, *match_count, match_kinds),
        (
            CaptureAction::Conflict { matches },
            CaptureEvalActionExpectation::Conflict {
                match_count,
                match_kinds,
            },
        ) => match_expectation(matches, *match_count, match_kinds),
        (
            CaptureAction::NoWrite {
                reason_code: actual,
            },
            CaptureEvalActionExpectation::NoWrite {
                reason_code: expected,
            },
        ) => actual == expected,
        (
            CaptureAction::Blocked { code: actual },
            CaptureEvalActionExpectation::Blocked { code: expected },
        ) => actual == expected,
        _ => false,
    }
}

fn match_expectation(
    matches: &[crate::capture::CaptureMatch],
    expected_count: Option<usize>,
    expected_kinds: &[CaptureMatchKind],
) -> bool {
    if expected_count.is_some_and(|count| count != matches.len()) {
        return false;
    }
    if expected_kinds.is_empty() {
        return true;
    }
    let mut actual = matches
        .iter()
        .map(|matching| matching.kind)
        .collect::<Vec<_>>();
    let mut expected = expected_kinds.to_vec();
    actual.sort();
    expected.sort();
    actual == expected
}

fn profile_matches(
    expected: &CaptureEvalProfileExpectation,
    actual: &CaptureExtractorIdentity,
) -> bool {
    actual.kind == expected.extractor_kind
        && actual.id == expected.extractor_id
        && actual.version == expected.extractor_version
        && expected
            .configuration_hash
            .as_ref()
            .is_none_or(|hash| hash == &actual.configuration_hash)
}

fn evidence_counts(
    paths: &MemoryPaths,
    source_inputs: &BTreeMap<String, Vec<u8>>,
    plan: &CapturePlan,
    requested_sources: &BTreeSet<&str>,
) -> EvidenceCounts {
    let mut counts = EvidenceCounts {
        valid: 0,
        checked: 0,
        unnamed: 0,
    };
    for candidate in &plan.candidates {
        for evidence in &candidate.evidence {
            counts.checked += 1;
            if !requested_sources.contains(evidence.source_id.as_str()) {
                counts.unnamed += 1;
            }
            let bytes = evidence
                .locator
                .project_path()
                .and_then(|path| fs::read(paths.project_root.join(path)).ok())
                .or_else(|| source_inputs.get(&evidence.source_id).cloned());
            if bytes
                .as_deref()
                .is_some_and(|bytes| evidence_is_valid(evidence, bytes))
            {
                counts.valid += 1;
            }
        }
    }
    counts
}

fn evidence_is_valid(evidence: &CaptureEvidence, bytes: &[u8]) -> bool {
    let Ok(start) = usize::try_from(evidence.span.byte_start) else {
        return false;
    };
    let Ok(end) = usize::try_from(evidence.span.byte_end) else {
        return false;
    };
    if start >= end || end > bytes.len() {
        return false;
    }
    let Ok(source_text) = std::str::from_utf8(bytes) else {
        return false;
    };
    if !source_text.is_char_boundary(start) || !source_text.is_char_boundary(end) {
        return false;
    }
    let excerpt = &bytes[start..end];
    let Ok(excerpt_text) = std::str::from_utf8(excerpt) else {
        return false;
    };
    let line_start = 1 + bytes[..start].iter().filter(|byte| **byte == b'\n').count() as u64;
    let line_end = line_start
        + excerpt[..excerpt.len() - 1]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count() as u64;
    evidence.source_content_hash == content_hash(bytes)
        && evidence.evidence_content_hash == content_hash(excerpt)
        && evidence.text.as_deref() == Some(excerpt_text)
        && evidence.span.line_start == line_start
        && evidence.span.line_end == line_end
}

fn content_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn review_burden(
    case: &CaptureEvalCase,
    plan: &CapturePlan,
    matched: &BTreeMap<String, usize>,
) -> CaptureEvalReviewBurden {
    let mut burden = CaptureEvalReviewBurden {
        proposed: plan.candidates.len(),
        duplicates: plan.summary.duplicates,
        conflicts: plan.summary.conflicts,
        needs_review: plan.summary.needs_review,
        ..CaptureEvalReviewBurden::default()
    };
    for outcome in &case.review_outcomes {
        if !matched.contains_key(&outcome.candidate) {
            continue;
        }
        match outcome.outcome {
            CaptureReviewOutcome::Accept => burden.accepted += 1,
            CaptureReviewOutcome::Reject => burden.rejected += 1,
            CaptureReviewOutcome::Edit => burden.edited += 1,
            CaptureReviewOutcome::Defer => burden.deferred += 1,
        }
    }
    burden
}

fn aggregate_metrics(cases: &[CaptureEvalCaseReport]) -> CaptureEvalMetrics {
    let executed = cases
        .iter()
        .filter(|case| case.execution == CaptureEvalExecutionStatus::Executed)
        .collect::<Vec<_>>();
    let true_positives: usize = executed.iter().map(|case| case.true_positives).sum();
    let false_positives: usize = executed.iter().map(|case| case.false_positives).sum();
    let false_negatives: usize = executed.iter().map(|case| case.false_negatives).sum();
    let evidence_valid: usize = executed
        .iter()
        .map(|case| case.evidence_validity.valid)
        .sum();
    let evidence_checked: usize = executed
        .iter()
        .map(|case| case.evidence_validity.checked)
        .sum();
    let destination_valid: usize = executed
        .iter()
        .map(|case| case.destination_accuracy.valid)
        .sum();
    let destination_checked: usize = executed
        .iter()
        .map(|case| case.destination_accuracy.checked)
        .sum();
    let sensitivity_valid: usize = executed
        .iter()
        .map(|case| case.sensitivity_accuracy.valid)
        .sum();
    let sensitivity_checked: usize = executed
        .iter()
        .map(|case| case.sensitivity_accuracy.checked)
        .sum();
    let action_valid: usize = executed.iter().map(|case| case.action_accuracy.valid).sum();
    let action_checked: usize = executed
        .iter()
        .map(|case| case.action_accuracy.checked)
        .sum();
    let forbidden_hits: usize = executed.iter().map(|case| case.forbidden_hits.len()).sum();
    let forbidden_opportunities: usize = executed
        .iter()
        .map(|case| case.forbidden_opportunities)
        .sum::<usize>();
    let unsupported = executed
        .iter()
        .filter(|case| case.plan_status == Some(CapturePlanStatus::Blocked))
        .collect::<Vec<_>>();
    let unsupported_valid = unsupported
        .iter()
        .filter(|case| {
            case.assertions.get("plan_status") == Some(&true)
                && case.assertions.get("diagnostics") == Some(&true)
        })
        .count();
    let passed_cases = cases.iter().filter(|case| case.passed).count();
    let mut review_burden = CaptureEvalReviewBurden::default();
    for case in &executed {
        add_review_burden(&mut review_burden, &case.review_burden);
    }

    CaptureEvalMetrics {
        case_count: cases.len(),
        candidate_precision: ratio_metric(true_positives, true_positives + false_positives),
        candidate_recall: ratio_metric(true_positives, true_positives + false_negatives),
        evidence_validity: integrity_metric(evidence_valid, evidence_checked),
        destination_accuracy: integrity_metric(destination_valid, destination_checked),
        sensitivity_accuracy: integrity_metric(sensitivity_valid, sensitivity_checked),
        action_accuracy: integrity_metric(action_valid, action_checked),
        forbidden_hits: leakage_metric(forbidden_hits, forbidden_opportunities),
        unsupported_outcome_accuracy: integrity_metric(unsupported_valid, unsupported.len()),
        case_pass_rate: ratio_metric(passed_cases, cases.len()),
        extractor_failures: cases
            .iter()
            .filter(|case| case.execution == CaptureEvalExecutionStatus::Failed)
            .count(),
        optional_skips: cases
            .iter()
            .filter(|case| case.execution == CaptureEvalExecutionStatus::Skipped)
            .count(),
        review_burden,
    }
}

fn add_review_burden(target: &mut CaptureEvalReviewBurden, value: &CaptureEvalReviewBurden) {
    target.proposed += value.proposed;
    target.accepted += value.accepted;
    target.rejected += value.rejected;
    target.edited += value.edited;
    target.deferred += value.deferred;
    target.duplicates += value.duplicates;
    target.conflicts += value.conflicts;
    target.needs_review += value.needs_review;
}

fn aggregate_hard_gates(cases: &[CaptureEvalCaseReport]) -> CaptureEvalHardGates {
    let mut gates = CaptureEvalHardGates::default();
    for case in cases {
        gates.determinism_violations += case.hard_gates.determinism_violations;
        gates.planning_mutation_violations += case.hard_gates.planning_mutation_violations;
        gates.invalid_evidence_items += case.hard_gates.invalid_evidence_items;
        gates.unnamed_source_reads += case.hard_gates.unnamed_source_reads;
        gates.prohibited_output_echoes += case.hard_gates.prohibited_output_echoes;
        gates.review_workflow_violations += case.hard_gates.review_workflow_violations;
        gates.provenance_violations += case.hard_gates.provenance_violations;
        gates.stale_review_acceptance_violations +=
            case.hard_gates.stale_review_acceptance_violations;
        gates.stale_apply_acceptance_violations +=
            case.hard_gates.stale_apply_acceptance_violations;
        gates.stale_write_violations += case.hard_gates.stale_write_violations;
        gates.direct_canonical_write_violations +=
            case.hard_gates.direct_canonical_write_violations;
        gates.required_profile_skips += case.hard_gates.required_profile_skips;
    }
    gates
}

fn aggregate_observations(cases: &[CaptureEvalCaseReport]) -> CaptureEvalObservations {
    let latencies = cases
        .iter()
        .filter(|case| case.execution == CaptureEvalExecutionStatus::Executed)
        .map(|case| case.observations.latency_ms)
        .collect::<Vec<_>>();
    let payloads = cases
        .iter()
        .filter(|case| case.execution == CaptureEvalExecutionStatus::Executed)
        .map(|case| case.observations.payload_bytes)
        .collect::<Vec<_>>();
    CaptureEvalObservations {
        latency: CaptureEvalLatencyObservations {
            unit: "milliseconds".to_owned(),
            timer: "std::time::Instant".to_owned(),
            sample_count: latencies.len(),
            p50: rounded(percentile_f64(&latencies, 0.50), 3),
            p95: rounded(percentile_f64(&latencies, 0.95), 3),
        },
        payload: CaptureEvalPayloadObservations {
            unit: "bytes".to_owned(),
            sample_count: payloads.len(),
            p50: percentile_usize(&payloads, 0.50),
            p95: percentile_usize(&payloads, 0.95),
            max: payloads.iter().copied().max().unwrap_or(0),
        },
        provider: None,
        model: None,
        token_usage: None,
        cost: None,
    }
}

fn evaluate_thresholds(
    thresholds: &CaptureEvalThresholds,
    metrics: &CaptureEvalMetrics,
    profile_metrics: &BTreeMap<String, CaptureEvalMetrics>,
    observations: &CaptureEvalObservations,
) -> CaptureEvalThresholdResults {
    let mut checks = BTreeMap::new();
    add_quality_threshold_checks(&mut checks, "aggregate", thresholds, metrics);
    for (profile, metrics) in profile_metrics {
        if metrics.case_count > 0 {
            add_quality_threshold_checks(
                &mut checks,
                &format!("profile.{profile}"),
                thresholds,
                metrics,
            );
        }
    }
    if let Some(limits) = &thresholds.review_burden {
        let actual = &metrics.review_burden;
        for (name, value, maximum) in [
            ("proposed", actual.proposed, limits.proposed),
            ("accepted", actual.accepted, limits.accepted),
            ("rejected", actual.rejected, limits.rejected),
            ("edited", actual.edited, limits.edited),
            ("deferred", actual.deferred, limits.deferred),
            ("duplicates", actual.duplicates, limits.duplicates),
            ("conflicts", actual.conflicts, limits.conflicts),
            ("needs_review", actual.needs_review, limits.needs_review),
        ] {
            checks.insert(format!("review_burden.{name}"), value <= maximum);
        }
    }
    if let Some(maximum) = thresholds.max_plan_payload_bytes {
        checks.insert(
            "observations.max_plan_payload_bytes".to_owned(),
            observations.payload.max <= maximum,
        );
    }
    if let Some(maximum) = thresholds.max_p95_latency_ms {
        checks.insert(
            "observations.max_p95_latency_ms".to_owned(),
            observations.latency.p95 <= maximum,
        );
    }
    let passed = checks.values().all(|passed| *passed);
    CaptureEvalThresholdResults { checks, passed }
}

fn add_quality_threshold_checks(
    checks: &mut BTreeMap<String, bool>,
    prefix: &str,
    thresholds: &CaptureEvalThresholds,
    metrics: &CaptureEvalMetrics,
) {
    for (name, passed) in [
        (
            "candidate_precision",
            metrics.candidate_precision.value >= thresholds.min_candidate_precision,
        ),
        (
            "candidate_recall",
            metrics.candidate_recall.value >= thresholds.min_candidate_recall,
        ),
        (
            "evidence_validity",
            metrics.evidence_validity.rate >= thresholds.min_evidence_validity,
        ),
        (
            "destination_accuracy",
            metrics.destination_accuracy.rate >= thresholds.min_destination_accuracy,
        ),
        (
            "sensitivity_accuracy",
            metrics.sensitivity_accuracy.rate >= thresholds.min_sensitivity_accuracy,
        ),
        (
            "action_accuracy",
            metrics.action_accuracy.rate >= thresholds.min_action_accuracy,
        ),
        (
            "forbidden_hit_rate",
            metrics.forbidden_hits.rate <= thresholds.max_forbidden_hit_rate,
        ),
        (
            "unsupported_outcome_accuracy",
            metrics.unsupported_outcome_accuracy.rate
                >= thresholds.min_unsupported_outcome_accuracy,
        ),
        (
            "case_pass_rate",
            metrics.case_pass_rate.value >= thresholds.min_case_pass_rate,
        ),
    ] {
        checks.insert(format!("{prefix}.{name}"), passed);
    }
}

fn metric_definitions() -> CaptureEvalMetricDefinitions {
    CaptureEvalMetricDefinitions {
        version: CAPTURE_EVAL_METRIC_DEFINITIONS_VERSION.to_owned(),
        candidate_precision: "matched expected candidates / all emitted candidates; matching uses the exact memory draft and expected evidence shape".to_owned(),
        candidate_recall: "matched expected candidates / all declared expected candidates".to_owned(),
        evidence_validity: "evidence items whose declared locator, source hash, semantic location, byte and line span, excerpt hash, and reconstructed text exactly match isolated source bytes and expectations / evidence items checked; undeclared source-policy reads fail a separate hard gate".to_owned(),
        classification_accuracy: "matched expected candidates with the declared destination or sensitivity / matched candidates checked".to_owned(),
        forbidden_hit_rate: "emitted declared-forbidden candidates / declared forbidden opportunities".to_owned(),
        unsupported_outcome_accuracy: "expected blocked cases with the declared blocked status and diagnostic codes / blocked cases checked".to_owned(),
        review_burden: "candidate and reviewer outcome counts from exercised build-review and apply workflows; content is never included in the report".to_owned(),
        observations: "latency, payload, provider/model, token, and cost observations are separate from deterministic baseline comparison".to_owned(),
        empty_denominator: "precision, recall, and integrity use 1.0 for an empty denominator; leakage uses 0.0".to_owned(),
    }
}

fn runtime_metadata(latency_sample_count: usize) -> CaptureEvalRuntimeMetadata {
    CaptureEvalRuntimeMetadata {
        memzoi_version: env!("CARGO_PKG_VERSION").to_owned(),
        target_os: std::env::consts::OS.to_owned(),
        target_arch: std::env::consts::ARCH.to_owned(),
        build_profile: if cfg!(debug_assertions) {
            "debug".to_owned()
        } else {
            "release".to_owned()
        },
        isolated_state: true,
        network_required: false,
        latency_sample_count,
    }
}

fn baseline_from_report(report: &CaptureEvalReport) -> CaptureEvalBaseline {
    CaptureEvalBaseline {
        version: CAPTURE_EVAL_BASELINE_VERSION.to_owned(),
        report_version: report.version.clone(),
        definitions_version: report.definitions.version.clone(),
        corpus: CaptureEvalBaselineCorpus {
            name: report.corpus.name.clone(),
            version: report.corpus.version.clone(),
            digest: report.corpus.digest.clone(),
        },
        profiles: report.profiles.clone(),
        metrics: report.metrics.clone(),
        profile_metrics: report.profile_metrics.clone(),
        hard_gates: report.hard_gates.clone(),
        cases: report.cases.iter().map(baseline_case).collect(),
    }
}

fn baseline_case(case: &CaptureEvalCaseReport) -> CaptureEvalBaselineCase {
    CaptureEvalBaselineCase {
        id: case.id.clone(),
        profile: case.profile.clone(),
        execution: case.execution,
        plan_status: case.plan_status,
        data_class: case.data_class,
        diagnostic_codes: case.diagnostic_codes.clone(),
        matched_candidates: case.matched_candidates.clone(),
        missing_candidates: case.missing_candidates.clone(),
        forbidden_hits: case.forbidden_hits.clone(),
        forbidden_opportunities: case.forbidden_opportunities,
        unexpected_candidates: case.unexpected_candidates,
        review_burden: case.review_burden.clone(),
        assertions: case.assertions.clone(),
        passed: case.passed,
    }
}

fn compare_baseline(
    report: &CaptureEvalReport,
    baseline: &CaptureEvalBaseline,
) -> CaptureEvalBaselineComparison {
    let compatible = baseline.version == CAPTURE_EVAL_BASELINE_VERSION
        && baseline.report_version == report.version
        && baseline.definitions_version == report.definitions.version
        && baseline.corpus.name == report.corpus.name
        && baseline.corpus.version == report.corpus.version
        && baseline.corpus.digest == report.corpus.digest;
    if !compatible {
        return CaptureEvalBaselineComparison {
            version: CAPTURE_EVAL_BASELINE_VERSION.to_owned(),
            status: CaptureEvalBaselineStatus::Incompatible,
            compatible: false,
            deterministic_match: false,
            metrics_changed: false,
            profiles_changed: false,
            hard_gates_changed: false,
            changed_cases: Vec::new(),
        };
    }

    let current_cases = report
        .cases
        .iter()
        .map(baseline_case)
        .map(|case| (case.id.clone(), case))
        .collect::<BTreeMap<_, _>>();
    let baseline_cases = baseline
        .cases
        .iter()
        .cloned()
        .map(|case| (case.id.clone(), case))
        .collect::<BTreeMap<_, _>>();
    let all_ids = current_cases
        .keys()
        .chain(baseline_cases.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let changed_cases = all_ids
        .into_iter()
        .filter(|id| current_cases.get(id) != baseline_cases.get(id))
        .collect::<Vec<_>>();
    let metrics_changed =
        baseline.metrics != report.metrics || baseline.profile_metrics != report.profile_metrics;
    let profiles_changed = baseline.profiles != report.profiles;
    let hard_gates_changed = baseline.hard_gates != report.hard_gates;
    let deterministic_match =
        changed_cases.is_empty() && !metrics_changed && !profiles_changed && !hard_gates_changed;
    CaptureEvalBaselineComparison {
        version: CAPTURE_EVAL_BASELINE_VERSION.to_owned(),
        status: if deterministic_match {
            CaptureEvalBaselineStatus::Match
        } else {
            CaptureEvalBaselineStatus::Changed
        },
        compatible: true,
        deterministic_match,
        metrics_changed,
        profiles_changed,
        hard_gates_changed,
        changed_cases,
    }
}

fn ratio_metric(numerator: usize, denominator: usize) -> CaptureEvalRatioMetric {
    CaptureEvalRatioMetric {
        numerator,
        denominator,
        value: rounded(ratio_value(numerator, denominator), 6),
    }
}

fn integrity_metric(valid: usize, checked: usize) -> CaptureEvalIntegrityMetric {
    CaptureEvalIntegrityMetric {
        valid,
        checked,
        rate: rounded(ratio_value(valid, checked), 6),
    }
}

fn leakage_metric(hits: usize, opportunities: usize) -> CaptureEvalLeakageMetric {
    CaptureEvalLeakageMetric {
        hits,
        opportunities,
        rate: rounded(
            if opportunities == 0 {
                0.0
            } else {
                hits as f64 / opportunities as f64
            },
            6,
        ),
    }
}

fn ratio_value(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn percentile_usize(values: &[usize], percentile: f64) -> usize {
    if values.is_empty() {
        return 0;
    }
    let mut values = values.to_vec();
    values.sort_unstable();
    values[nearest_rank_index(values.len(), percentile)]
}

fn percentile_f64(values: &[f64], percentile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut values = values.to_vec();
    values.sort_by(|left, right| left.total_cmp(right));
    values[nearest_rank_index(values.len(), percentile)]
}

fn nearest_rank_index(len: usize, percentile: f64) -> usize {
    ((percentile * len as f64).ceil() as usize)
        .saturating_sub(1)
        .min(len - 1)
}

fn rounded(value: f64, digits: i32) -> f64 {
    let factor = 10_f64.powi(digits);
    (value * factor).round() / factor
}

fn sorted_strings(values: &[String]) -> Vec<&str> {
    let mut values = values.iter().map(String::as_str).collect::<Vec<_>>();
    values.sort_unstable();
    values
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::{
        CaptureEvalCase, CaptureEvalExpectedEvidence, CaptureEvalHardGates, CaptureEvalThresholds,
        CaptureEvalTreeEntry, GIT_LOCAL_CONFIG_ENGINE, GIT_REPOSITORY_IDENTITY_ENGINE,
        evidence_shape_matches, file_snapshot, git_range_renderer_command, integrity_metric,
        nearest_rank_index, policy_input_set_violations, prepare_eval_git_environment, run_git,
        validate_thresholds,
    };
    use crate::{
        CaptureEvidence, CaptureEvidenceSpan, CapturePolicyInputSnapshot, CaptureSemanticLocation,
        CaptureSourceLocator, MemoryPaths,
    };

    #[test]
    fn empty_integrity_denominator_is_vacuously_valid() {
        assert_eq!(integrity_metric(0, 0).rate, 1.0);
    }

    #[test]
    fn nearest_rank_percentiles_are_stable() {
        assert_eq!(nearest_rank_index(1, 0.50), 0);
        assert_eq!(nearest_rank_index(4, 0.50), 1);
        assert_eq!(nearest_rank_index(4, 0.95), 3);
    }

    #[test]
    fn file_snapshot_records_empty_directories_and_entry_types() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::create_dir(temp.path().join("empty"))?;
        std::fs::write(temp.path().join("node"), b"same bytes")?;
        let files = file_snapshot(temp.path())?;
        assert_eq!(
            files.get(std::path::Path::new("empty")),
            Some(&CaptureEvalTreeEntry::Directory)
        );
        assert_eq!(
            files.get(std::path::Path::new("node")),
            Some(&CaptureEvalTreeEntry::RegularFile(b"same bytes".to_vec()))
        );

        std::fs::remove_file(temp.path().join("node"))?;
        std::fs::create_dir(temp.path().join("node"))?;
        let directory = file_snapshot(temp.path())?;
        assert_ne!(
            files, directory,
            "file-to-directory changes must be visible"
        );

        #[cfg(unix)]
        {
            std::fs::remove_dir(temp.path().join("node"))?;
            std::os::unix::fs::symlink("same bytes", temp.path().join("node"))?;
            let symlink = file_snapshot(temp.path())?;
            assert_eq!(
                symlink.get(std::path::Path::new("node")),
                Some(&CaptureEvalTreeEntry::Symlink("same bytes".into()))
            );
            assert_ne!(files, symlink, "file-to-symlink changes must be visible");
        }
        Ok(())
    }

    #[test]
    fn hard_gates_fail_on_every_violation_class() {
        macro_rules! assert_gate {
            ($field:ident) => {{
                let mut gates = CaptureEvalHardGates::default();
                assert!(gates.passed());
                gates.$field = 1;
                assert!(!gates.passed(), stringify!($field));
            }};
        }
        assert_gate!(determinism_violations);
        assert_gate!(planning_mutation_violations);
        assert_gate!(invalid_evidence_items);
        assert_gate!(unnamed_source_reads);
        assert_gate!(prohibited_output_echoes);
        assert_gate!(review_workflow_violations);
        assert_gate!(provenance_violations);
        assert_gate!(stale_review_acceptance_violations);
        assert_gate!(stale_apply_acceptance_violations);
        assert_gate!(stale_write_violations);
        assert_gate!(direct_canonical_write_violations);
        assert_gate!(required_profile_skips);
    }

    #[test]
    fn expected_git_evidence_matches_every_identity_field_exactly() {
        let locator = CaptureSourceLocator::SuppliedBytes {
            display_name: "change.diff".to_owned(),
            media_type: "text/x-diff".to_owned(),
            byte_length: 10,
            source_content_hash:
                "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        };
        let semantic_location = CaptureSemanticLocation::GitChange {
            repository: ".".to_owned(),
            base: format!("sha1:{}", "1".repeat(40)),
            head: format!("sha1:{}", "2".repeat(40)),
            old_blob: Some(format!("sha1:{}", "3".repeat(40))),
            new_blob: Some(format!("sha1:{}", "4".repeat(40))),
            old_path: Some("old.md".to_owned()),
            new_path: Some("new.md".to_owned()),
            change_kind: "renamed".to_owned(),
            hunk: "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .to_owned(),
            side: "new".to_owned(),
            old_line_start: Some(1),
            old_line_end: Some(2),
            new_line_start: Some(3),
            new_line_end: Some(4),
        };
        let span = CaptureEvidenceSpan {
            byte_start: 1,
            byte_end: 2,
            line_start: 1,
            line_end: 1,
        };
        let actual = CaptureEvidence {
            source_id: "source-1".to_owned(),
            locator: locator.clone(),
            source_content_hash:
                "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            span: span.clone(),
            evidence_content_hash:
                "blake3:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_owned(),
            text: Some("x".to_owned()),
            heading_path: vec!["Warning".to_owned()],
            section_kind: "warning".to_owned(),
            semantic_location: Some(semantic_location),
        };
        let expected = CaptureEvalExpectedEvidence {
            source_id: actual.source_id.clone(),
            locator,
            source_content_hash: actual.source_content_hash.clone(),
            span,
            heading_path: actual.heading_path.clone(),
            section_kind: actual.section_kind.clone(),
            semantic_location: actual.semantic_location.clone(),
        };
        assert!(evidence_shape_matches(
            std::slice::from_ref(&actual),
            std::slice::from_ref(&expected)
        ));

        for field in [
            "repository",
            "base",
            "head",
            "old_blob",
            "new_blob",
            "old_path",
            "new_path",
            "change_kind",
            "hunk",
            "side",
            "old_line_start",
            "old_line_end",
            "new_line_start",
            "new_line_end",
        ] {
            let mut changed = serde_json::to_value(&expected).expect("serialize expectation");
            let location = changed
                .get_mut("semantic_location")
                .and_then(serde_json::Value::as_object_mut)
                .expect("Git semantic location");
            let value = location.get_mut(field).expect("Git identity field");
            *value = if value.is_number() {
                serde_json::json!(99)
            } else {
                serde_json::json!("changed")
            };
            let changed: CaptureEvalExpectedEvidence =
                serde_json::from_value(changed).expect("deserialize changed expectation");
            assert!(
                !evidence_shape_matches(
                    std::slice::from_ref(&actual),
                    std::slice::from_ref(&changed)
                ),
                "Git evidence field {field} was not matched exactly"
            );
        }
    }

    #[test]
    fn expected_adr_evidence_matches_field_status_and_target_exactly() {
        let locator = CaptureSourceLocator::ProjectPath {
            path: "docs/adr/0002.md".to_owned(),
        };
        let span = CaptureEvidenceSpan {
            byte_start: 1,
            byte_end: 2,
            line_start: 1,
            line_end: 1,
        };
        let actual = CaptureEvidence {
            source_id: "source-1".to_owned(),
            locator: locator.clone(),
            source_content_hash:
                "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            span: span.clone(),
            evidence_content_hash:
                "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            text: Some("ADR-0001".to_owned()),
            heading_path: vec!["Supersedes".to_owned()],
            section_kind: "supersession".to_owned(),
            semantic_location: Some(CaptureSemanticLocation::Adr {
                field: "supersession".to_owned(),
                status: "accepted".to_owned(),
                target: Some("ADR-0001".to_owned()),
            }),
        };
        let expected = CaptureEvalExpectedEvidence {
            source_id: actual.source_id.clone(),
            locator,
            source_content_hash: actual.source_content_hash.clone(),
            span,
            heading_path: actual.heading_path.clone(),
            section_kind: actual.section_kind.clone(),
            semantic_location: actual.semantic_location.clone(),
        };
        assert!(evidence_shape_matches(
            std::slice::from_ref(&actual),
            std::slice::from_ref(&expected)
        ));

        for (field, value) in [
            ("field", serde_json::json!("decision")),
            ("status", serde_json::json!("draft")),
            ("target", serde_json::json!("ADR-9999")),
        ] {
            let mut changed = serde_json::to_value(&expected).expect("serialize expectation");
            changed
                .get_mut("semantic_location")
                .and_then(serde_json::Value::as_object_mut)
                .expect("ADR semantic location")
                .insert(field.to_owned(), value);
            let changed: CaptureEvalExpectedEvidence =
                serde_json::from_value(changed).expect("deserialize changed expectation");
            assert!(
                !evidence_shape_matches(
                    std::slice::from_ref(&actual),
                    std::slice::from_ref(&changed)
                ),
                "ADR evidence field {field} was not matched exactly"
            );
        }
    }

    #[test]
    fn forged_policy_input_is_counted_as_an_unnamed_source_read() {
        let case: CaptureEvalCase = serde_yaml::from_str(
            r#"
id: forged-policy-read
request:
  schema: memzoi/capture-request-v2
  sources:
    - source_id: source-1
      locator: {kind: project_path, path: notes/source.md}
      media_type: text/markdown
  extractor: {profile: markdown-deterministic}
expected:
  status: blocked
  data_class: blocked
"#,
        )
        .expect("parse minimal evaluation case");
        let forged = CapturePolicyInputSnapshot {
            path: ".git/config".to_owned(),
            source_content_hash:
                "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            engine_version: GIT_LOCAL_CONFIG_ENGINE.to_owned(),
        };

        assert!(policy_input_set_violations(&case, &[forged]) > 0);
    }

    #[test]
    fn git_range_policy_input_set_rejects_missing_duplicate_and_unexpected_reads() {
        let case: CaptureEvalCase = serde_yaml::from_str(
            r#"
id: git-policy-reads
expected_policy_inputs:
  - {path: .git, engine_version: memzoi/git-repository-identity-v1}
  - {path: .git/config, engine_version: memzoi/git-local-config-v1}
  - path: .git/renderer-version
    engine_version: memzoi/git-unified-renderer-v1+git-
    engine_match: prefix
request:
  schema: memzoi/capture-request-v2
  sources:
    - source_id: source-1
      locator:
        kind: git_range
        repository: .
        base: sha1:1111111111111111111111111111111111111111
        head: sha1:2222222222222222222222222222222222222222
        merge_parent: base_to_head
        rename_detection: true
        diff_format: git-unified-v1
      media_type: text/x-diff
  extractor: {profile: git-change-deterministic}
expected:
  status: ready
  data_class: repo_safe
"#,
        )
        .expect("parse Git policy evaluation case");
        let policy = |path: &str, engine_version: &str, digest: char| CapturePolicyInputSnapshot {
            path: path.to_owned(),
            source_content_hash: format!("blake3:{}", digest.to_string().repeat(64)),
            engine_version: engine_version.to_owned(),
        };
        let expected = vec![
            policy(".git", GIT_REPOSITORY_IDENTITY_ENGINE, 'a'),
            policy(".git/config", GIT_LOCAL_CONFIG_ENGINE, 'b'),
            policy(
                ".git/renderer-version",
                "memzoi/git-unified-renderer-v1+git-2.50.1",
                'c',
            ),
        ];
        assert_eq!(policy_input_set_violations(&case, &expected), 0);

        assert!(policy_input_set_violations(&case, &expected[..2]) > 0);
        let mut duplicate = expected.clone();
        duplicate.push(expected[1].clone());
        assert!(policy_input_set_violations(&case, &duplicate) > 0);
        let mut unexpected = expected;
        unexpected.push(policy(
            ".git/hooks/pre-commit",
            GIT_LOCAL_CONFIG_ENGINE,
            'd',
        ));
        assert!(policy_input_set_violations(&case, &unexpected) > 0);
    }

    #[test]
    fn eval_git_trace_child() -> anyhow::Result<()> {
        if std::env::var_os("MEMZOI_EVAL_GIT_TRACE_CHILD").is_none() {
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        std::fs::create_dir(&project)?;
        prepare_eval_git_environment(&project)?;
        run_git(&project, &["init", "-q", "--object-format=sha1"], None)?;
        Ok(())
    }

    #[test]
    fn eval_git_commands_do_not_inherit_trace_environment() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let trace = temp.path().join("git-trace.log");
        let trace2 = temp.path().join("git-trace2.log");
        let output = std::process::Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("capture_eval::tests::eval_git_trace_child")
            .arg("--nocapture")
            .env("MEMZOI_EVAL_GIT_TRACE_CHILD", "1")
            .env("GIT_TRACE", &trace)
            .env("GIT_TRACE2", &trace2)
            .output()?;

        assert!(
            output.status.success(),
            "trace child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !trace.exists(),
            "capture evaluation Git inherited GIT_TRACE"
        );
        assert!(
            !trace2.exists(),
            "capture evaluation Git inherited GIT_TRACE2"
        );
        Ok(())
    }

    #[test]
    fn eval_git_renderer_pins_every_byte_affecting_input() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        std::fs::create_dir(&project)?;
        prepare_eval_git_environment(&project)?;
        let paths = MemoryPaths::with_runtime_home(project, temp.path().join("runtime"));
        let head = "2".repeat(40);
        let command = git_range_renderer_command(&paths, &"1".repeat(40), &head, true)?;
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
        for required in [
            format!("core.hooksPath={null_device}"),
            "color.ui=false".to_owned(),
            "core.quotePath=true".to_owned(),
            "core.bigFileThreshold=16m".to_owned(),
            format!("core.attributesFile={null_device}"),
            "diff.external=".to_owned(),
            "diff.orderFile=".to_owned(),
            "diff.interHunkContext=0".to_owned(),
            "diff.noprefix=false".to_owned(),
            "diff.mnemonicPrefix=false".to_owned(),
            "diff.srcPrefix=a/".to_owned(),
            "diff.dstPrefix=b/".to_owned(),
            "diff.linePrefix=".to_owned(),
            "diff.outputIndicatorNew=+".to_owned(),
            "diff.outputIndicatorOld=-".to_owned(),
            "diff.outputIndicatorContext= ".to_owned(),
            "diff.suppressBlankEmpty=false".to_owned(),
            "submodule.recurse=false".to_owned(),
            "trace2.normalTarget=0".to_owned(),
            "trace2.perfTarget=0".to_owned(),
            "trace2.eventTarget=0".to_owned(),
            "--full-index".to_owned(),
            format!("-O{null_device}"),
            "--diff-algorithm=myers".to_owned(),
            "--no-indent-heuristic".to_owned(),
            "--inter-hunk-context=0".to_owned(),
            "--src-prefix=a/".to_owned(),
            "--dst-prefix=b/".to_owned(),
            "--line-prefix=".to_owned(),
            "--output-indicator-new=+".to_owned(),
            "--output-indicator-old=-".to_owned(),
            "--output-indicator-context= ".to_owned(),
            "--no-relative".to_owned(),
            "--submodule=short".to_owned(),
        ] {
            assert!(
                args.contains(&required),
                "missing renderer pin {required:?}"
            );
        }
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == std::ffi::OsStr::new("GIT_ATTR_SOURCE"))
                .and_then(|(_, value)| value),
            Some(std::ffi::OsStr::new(&head))
        );
        Ok(())
    }

    #[test]
    fn thresholds_reject_non_finite_ratios() {
        let thresholds = CaptureEvalThresholds {
            min_candidate_precision: f64::NAN,
            min_candidate_recall: 1.0,
            min_evidence_validity: 1.0,
            min_destination_accuracy: 1.0,
            min_sensitivity_accuracy: 1.0,
            min_action_accuracy: 1.0,
            max_forbidden_hit_rate: 0.0,
            min_unsupported_outcome_accuracy: 1.0,
            min_case_pass_rate: 1.0,
            review_burden: None,
            max_plan_payload_bytes: None,
            max_p95_latency_ms: None,
        };
        assert!(validate_thresholds(&thresholds).is_err());
    }
}
