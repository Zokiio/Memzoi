use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tempfile::{NamedTempFile, TempDir};

use crate::{
    CheckpointInput, Clock, ContextPackInput, FixedClock, InitRequest, LocalMemoryInput,
    MemoryCitation, MemoryDestination, MemoryDraft, MemoryLane, MemoryPath, MemoryRecord,
    MemoryService, MemoryStatus, MemoryType, OkfProposalSensitivity, PrecheckInput,
    ProposalApprovalOverride, ProposeOptions, ScopeKind, SearchInput, Visibility, okf,
    search::path_matches_request,
};

pub const RECALL_EVAL_CORPUS_VERSION: &str = "memzoi-recall-corpus/v2";
pub const RECALL_EVAL_REPORT_VERSION: &str = "memzoi-recall-report/v2";
pub const RECALL_EVAL_METRIC_DEFINITIONS_VERSION: &str = "memzoi-trust-metrics/v1";
pub const RECALL_EVAL_BASELINE_VERSION: &str = "memzoi-recall-baseline/v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEvalCorpus {
    pub version: String,
    pub name: String,
    pub evaluated_at: String,
    #[serde(default = "default_records_root")]
    pub records_root: PathBuf,
    pub records: Vec<PathBuf>,
    #[serde(default = "default_proposals_root")]
    pub proposals_root: PathBuf,
    #[serde(default)]
    pub proposal_fixtures: Vec<RecallEvalProposalFixture>,
    #[serde(default)]
    pub runtime_fixtures: Vec<RecallEvalRuntimeFixture>,
    pub thresholds: RecallEvalThresholds,
    pub cases: Vec<RecallEvalCase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEvalProposalFixture {
    pub path: PathBuf,
    pub proposal_id: String,
    pub expected_record_id: String,
    pub expected_source_kind: String,
    pub expected_source_ref: String,
    pub expected_applies_to: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEvalRuntimeFixture {
    pub id: String,
    pub destination: MemoryDestination,
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "surface", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecallEvalCase {
    Search {
        id: String,
        query: String,
        relevant_ids: Vec<String>,
        #[serde(default)]
        forbidden: RecallEvalForbiddenIds,
        scope_kind: Option<ScopeKind>,
        scope_id: Option<String>,
        #[serde(rename = "type")]
        memory_type: Option<MemoryType>,
        lane: Option<MemoryLane>,
        #[serde(rename = "path")]
        path_prefix: Option<String>,
        k: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proposal_fixture: Option<String>,
    },
    Precheck {
        id: String,
        path: Option<String>,
        action: Option<String>,
        command: Option<String>,
        scope_kind: Option<ScopeKind>,
        relevant_ids: Vec<String>,
        #[serde(default)]
        forbidden: RecallEvalForbiddenIds,
    },
    Context {
        id: String,
        task: String,
        #[serde(rename = "path")]
        path_prefix: Option<String>,
        token_budget: Option<usize>,
        #[serde(default)]
        include_local: bool,
        #[serde(default)]
        include_session: bool,
        relevant_ids: Vec<String>,
        #[serde(default)]
        forbidden: RecallEvalForbiddenIds,
    },
    WriteGate {
        id: String,
        title: String,
        body: String,
        query: String,
        sensitivity: OkfProposalSensitivity,
        expected_issue_code: String,
        forbidden_record_id: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEvalForbiddenIds {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expired: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prohibited: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub destination: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub other: Vec<String>,
}

impl RecallEvalForbiddenIds {
    fn groups(&self) -> [(&'static str, &[String]); 6] {
        [
            ("stale", &self.stale),
            ("expired", &self.expired),
            ("scope", &self.scope),
            ("prohibited", &self.prohibited),
            ("destination", &self.destination),
            ("other", &self.other),
        ]
    }

    fn all(&self) -> BTreeSet<&str> {
        self.groups()
            .into_iter()
            .flat_map(|(_, ids)| ids.iter().map(String::as_str))
            .collect()
    }

    fn count(&self) -> usize {
        self.groups().into_iter().map(|(_, ids)| ids.len()).sum()
    }

    fn hits(&self, retrieved_ids: &[String]) -> Self {
        let retrieved = retrieved_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let matching = |ids: &[String]| {
            ids.iter()
                .filter(|id| retrieved.contains(id.as_str()))
                .cloned()
                .collect()
        };
        Self {
            stale: matching(&self.stale),
            expired: matching(&self.expired),
            scope: matching(&self.scope),
            prohibited: matching(&self.prohibited),
            destination: matching(&self.destination),
            other: matching(&self.other),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEvalThresholds {
    pub min_mean_recall_at_k: f64,
    pub min_mean_mrr: f64,
    pub min_precheck_precision: f64,
    pub min_precheck_recall: f64,
    pub max_stale_leakage_rate: f64,
    pub max_expired_leakage_rate: f64,
    pub max_scope_leakage_rate: f64,
    pub max_forbidden_hit_rate: f64,
    pub min_citation_integrity: f64,
    pub min_provenance_integrity: f64,
    pub min_case_pass_rate: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_estimated_usage: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_p95_latency_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalReport {
    pub version: String,
    pub corpus: RecallEvalCorpusMetadata,
    pub definitions: RecallEvalMetricDefinitions,
    pub runtime: RecallEvalRuntimeMetadata,
    pub proposal_fixtures: Vec<RecallEvalProposalFixtureReport>,
    pub cases: Vec<RecallEvalCaseReport>,
    pub metrics: RecallEvalMetrics,
    pub thresholds: RecallEvalThresholds,
    pub threshold_results: RecallEvalThresholdResults,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<RecallEvalBaselineComparison>,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallEvalCorpusMetadata {
    pub name: String,
    pub version: String,
    pub digest: String,
    pub evaluated_at: String,
    pub fixture_record_count: usize,
    pub runtime_fixture_count: usize,
    pub proposal_fixture_count: usize,
    pub case_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallEvalMetricDefinitions {
    pub version: String,
    pub search_recall_at_k: String,
    pub search_mrr: String,
    pub precheck_precision: String,
    pub precheck_recall: String,
    pub leakage_rate: String,
    pub forbidden_hit_rate: String,
    pub citation_integrity: String,
    pub provenance_integrity: String,
    pub token_usage: String,
    pub latency_percentiles: String,
    pub empty_denominator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallEvalRuntimeMetadata {
    pub memzoi_version: String,
    pub target_os: String,
    pub target_arch: String,
    pub build_profile: String,
    pub sqlite_version: String,
    pub timer: String,
    pub isolated_state: bool,
    pub network_required: bool,
    pub token_estimator: String,
    pub latency_sample_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallEvalProposalFixtureReport {
    pub proposal_id: String,
    pub record_id: String,
    pub source_kind: String,
    pub source_ref: String,
    pub applies_to: Vec<String>,
    pub source_preserved: bool,
    pub resolution_preserved: bool,
    pub lineage_preserved: bool,
    pub lineage_separate_from_evidence: bool,
    pub applicability_preserved: bool,
    pub passed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallEvalSurface {
    Search,
    Precheck,
    Context,
    WriteGate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalCaseReport {
    pub id: String,
    pub surface: RecallEvalSurface,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    pub relevant_ids: Vec<String>,
    pub forbidden: RecallEvalForbiddenIds,
    pub retrieved_ids: Vec<String>,
    pub retrieved: Vec<RecallEvalRetrievedRecord>,
    pub true_positives: usize,
    pub false_positives: usize,
    pub false_negatives: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recall_at_k: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mrr: Option<f64>,
    pub forbidden_hits: RecallEvalForbiddenIds,
    pub citation_integrity: RecallEvalIntegrityMetric,
    pub provenance_integrity: RecallEvalIntegrityMetric,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_usage: Option<usize>,
    pub latency_ms: f64,
    pub assertions: BTreeMap<String, bool>,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalRetrievedRecord {
    pub rank: usize,
    pub record_id: String,
    pub citations: Vec<MemoryCitation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalMetrics {
    pub search: RecallEvalSearchMetrics,
    pub precheck: RecallEvalPrecheckMetrics,
    pub leakage: RecallEvalLeakageMetrics,
    pub citation_integrity: RecallEvalIntegrityMetric,
    pub provenance_integrity: RecallEvalIntegrityMetric,
    pub token_usage: RecallEvalTokenUsageMetrics,
    pub latency: RecallEvalLatencyMetrics,
    pub case_pass_rate: RecallEvalRatioMetric,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalSearchMetrics {
    pub case_count: usize,
    pub mean_recall_at_k: f64,
    pub mean_mrr: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalPrecheckMetrics {
    pub case_count: usize,
    pub true_positives: usize,
    pub false_positives: usize,
    pub false_negatives: usize,
    pub precision: RecallEvalRatioMetric,
    pub recall: RecallEvalRatioMetric,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalLeakageMetrics {
    pub stale: RecallEvalLeakageMetric,
    pub expired: RecallEvalLeakageMetric,
    pub scope: RecallEvalLeakageMetric,
    pub prohibited: RecallEvalLeakageMetric,
    pub destination: RecallEvalLeakageMetric,
    pub forbidden: RecallEvalLeakageMetric,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalRatioMetric {
    pub numerator: usize,
    pub denominator: usize,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalLeakageMetric {
    pub hits: usize,
    pub opportunities: usize,
    pub rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalIntegrityMetric {
    pub valid: usize,
    pub checked: usize,
    pub rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalTokenUsageMetrics {
    pub unit: String,
    pub estimator: String,
    pub sample_count: usize,
    pub total: usize,
    pub mean: f64,
    pub p50: usize,
    pub p95: usize,
    pub max: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalLatencyMetrics {
    pub unit: String,
    pub timer: String,
    pub sample_count: usize,
    pub p50: f64,
    pub p95: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallEvalThresholdResults {
    pub min_mean_recall_at_k: bool,
    pub min_mean_mrr: bool,
    pub min_precheck_precision: bool,
    pub min_precheck_recall: bool,
    pub max_stale_leakage_rate: bool,
    pub max_expired_leakage_rate: bool,
    pub max_scope_leakage_rate: bool,
    pub max_forbidden_hit_rate: bool,
    pub min_citation_integrity: bool,
    pub min_provenance_integrity: bool,
    pub min_case_pass_rate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_estimated_usage: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_p95_latency_ms: Option<bool>,
}

impl RecallEvalThresholdResults {
    fn passed(&self) -> bool {
        self.min_mean_recall_at_k
            && self.min_mean_mrr
            && self.min_precheck_precision
            && self.min_precheck_recall
            && self.max_stale_leakage_rate
            && self.max_expired_leakage_rate
            && self.max_scope_leakage_rate
            && self.max_forbidden_hit_rate
            && self.min_citation_integrity
            && self.min_provenance_integrity
            && self.min_case_pass_rate
            && self.max_estimated_usage.unwrap_or(true)
            && self.max_p95_latency_ms.unwrap_or(true)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEvalBaseline {
    pub version: String,
    pub report_version: String,
    pub definitions_version: String,
    pub corpus: RecallEvalBaselineCorpus,
    pub metrics: RecallEvalBaselineMetrics,
    pub cases: Vec<RecallEvalBaselineCase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEvalBaselineCorpus {
    pub name: String,
    pub version: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEvalBaselineMetrics {
    pub mean_recall_at_k: f64,
    pub mean_mrr: f64,
    pub precheck_precision: f64,
    pub precheck_recall: f64,
    pub stale_leakage_rate: f64,
    pub expired_leakage_rate: f64,
    pub scope_leakage_rate: f64,
    pub prohibited_leakage_rate: f64,
    pub destination_leakage_rate: f64,
    pub forbidden_hit_rate: f64,
    pub citation_integrity: f64,
    pub provenance_integrity: f64,
    pub case_pass_rate: f64,
    pub estimated_usage_total: usize,
    pub estimated_usage_max: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEvalBaselineCase {
    pub id: String,
    pub surface: RecallEvalSurface,
    pub retrieved_ids: Vec<String>,
    pub estimated_usage: Option<usize>,
    pub passed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallEvalBaselineStatus {
    Match,
    Changed,
    Incompatible,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalBaselineComparison {
    pub version: String,
    pub status: RecallEvalBaselineStatus,
    pub compatible: bool,
    pub deterministic_match: bool,
    pub metric_deltas: Vec<RecallEvalMetricDelta>,
    pub changed_cases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalMetricDelta {
    pub metric: String,
    pub baseline: f64,
    pub current: f64,
    pub delta: f64,
}

#[derive(Debug)]
struct LoadedCorpus {
    corpus: RecallEvalCorpus,
    root: PathBuf,
    digest: String,
}

#[derive(Debug, Clone)]
struct EvalRecord {
    record: MemoryRecord,
    paths: Vec<MemoryPath>,
}

#[derive(Debug)]
struct AppliedProposalFixture {
    spec: RecallEvalProposalFixture,
    proposal_id: String,
    record_id: String,
    resolution_preserved: bool,
}

#[derive(Debug)]
struct EvaluatedCase {
    report: RecallEvalCaseReport,
    raw_latency_ms: f64,
    raw_recall_at_k: Option<f64>,
    raw_mrr: Option<f64>,
}

#[derive(Debug)]
struct RawMetrics {
    mean_recall_at_k: f64,
    mean_mrr: f64,
    precheck_precision: f64,
    precheck_recall: f64,
    stale_leakage_rate: f64,
    expired_leakage_rate: f64,
    scope_leakage_rate: f64,
    forbidden_hit_rate: f64,
    citation_integrity: f64,
    provenance_integrity: f64,
    case_pass_rate: f64,
    max_estimated_usage: usize,
    p95_latency_ms: f64,
}

pub fn run_recall_eval(corpus_path: impl AsRef<Path>) -> Result<RecallEvalReport> {
    let loaded = load_corpus(corpus_path.as_ref())?;
    validate_corpus(&loaded.corpus)?;

    let temp = TempDir::new().context("failed to create isolated recall evaluation state")?;
    let project_root = temp.path().join("project");
    fs::create_dir_all(&project_root)
        .context("failed to create isolated recall evaluation project")?;
    let project_root = project_root
        .canonicalize()
        .context("failed to canonicalize isolated recall evaluation root")?;
    let paths =
        crate::MemoryPaths::with_runtime_home(project_root, temp.path().join("runtime-home"));
    MemoryService::initialize_paths(paths.clone(), InitRequest { force: true })?;
    stage_record_fixtures(
        &loaded.root,
        &loaded.corpus.records_root,
        &loaded.corpus.records,
        &paths.records_dir(),
    )?;

    MemoryService::rebuild_paths_for_trusted_recall_eval(paths.clone())?;
    let clock = FixedClock::from_rfc3339(&loaded.corpus.evaluated_at)
        .context("invalid recall evaluation evaluated_at")?;
    let service = MemoryService::open_paths_with_clock(paths.clone(), clock)?;
    let applied_proposals = stage_and_apply_proposal_fixtures(
        &service,
        &loaded.root,
        &loaded.corpus.proposals_root,
        &loaded.corpus.proposal_fixtures,
    )?;

    drop(service);
    MemoryService::rebuild_paths_for_trusted_recall_eval(paths.clone())?;
    let service = MemoryService::open_paths_with_clock(paths.clone(), clock)?;
    let runtime_records = seed_runtime_fixtures(&service, &loaded.corpus.runtime_fixtures)?;
    let catalog = load_record_catalog(&paths.records_dir(), runtime_records)?;
    validate_case_record_ids(&loaded.corpus, &catalog)?;
    validate_forbidden_semantics(&loaded.corpus, &catalog)?;
    let proposal_reports = proposal_fixture_reports(&applied_proposals, &catalog)?;
    let proposal_status = proposal_reports
        .iter()
        .map(|report| (report.record_id.clone(), report.passed))
        .collect::<BTreeMap<_, _>>();

    let mut evaluated = Vec::with_capacity(loaded.corpus.cases.len());
    for case in &loaded.corpus.cases {
        evaluated.push(evaluate_case(
            &service,
            &paths.records_dir(),
            case,
            &catalog,
            &proposal_status,
        )?);
    }

    let (metrics, raw_metrics) = aggregate_metrics(&evaluated);
    let threshold_results = evaluate_thresholds(&loaded.corpus.thresholds, &raw_metrics);
    let cases = evaluated
        .into_iter()
        .map(|evaluated| evaluated.report)
        .collect::<Vec<_>>();
    let runtime = runtime_metadata(cases.len());
    let passed = threshold_results.passed();

    Ok(RecallEvalReport {
        version: RECALL_EVAL_REPORT_VERSION.to_owned(),
        corpus: RecallEvalCorpusMetadata {
            name: loaded.corpus.name,
            version: loaded.corpus.version,
            digest: loaded.digest,
            evaluated_at: loaded.corpus.evaluated_at,
            fixture_record_count: catalog
                .values()
                .filter(|record| record.record.destination == MemoryDestination::Repo)
                .count(),
            runtime_fixture_count: loaded.corpus.runtime_fixtures.len(),
            proposal_fixture_count: loaded.corpus.proposal_fixtures.len(),
            case_count: cases.len(),
        },
        definitions: metric_definitions(),
        runtime,
        proposal_fixtures: proposal_reports,
        cases,
        metrics,
        thresholds: loaded.corpus.thresholds,
        threshold_results,
        baseline: None,
        passed,
    })
}

pub fn attach_recall_eval_baseline(
    report: &mut RecallEvalReport,
    baseline_path: impl AsRef<Path>,
) -> Result<()> {
    let baseline_path = baseline_path.as_ref();
    let bytes = fs::read(baseline_path).with_context(|| {
        format!(
            "failed to read recall evaluation baseline {}",
            baseline_path.display()
        )
    })?;
    let baseline: RecallEvalBaseline = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "failed to parse recall evaluation baseline {}",
            baseline_path.display()
        )
    })?;
    let comparison = compare_baseline(report, &baseline);
    if !comparison.compatible {
        report.passed = false;
    }
    report.baseline = Some(comparison);
    Ok(())
}

pub fn write_recall_eval_baseline(
    report: &RecallEvalReport,
    baseline_path: impl AsRef<Path>,
) -> Result<()> {
    let baseline_path = baseline_path.as_ref();
    let parent = baseline_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create recall evaluation baseline directory {}",
            parent.display()
        )
    })?;
    let baseline = baseline_from_report(report);
    let mut bytes = serde_json::to_vec_pretty(&baseline)?;
    bytes.push(b'\n');
    let mut temporary = NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "failed to stage recall evaluation baseline in {}",
            parent.display()
        )
    })?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(baseline_path).map_err(|error| {
        anyhow::Error::new(error.error).context(format!(
            "failed to install recall evaluation baseline {}",
            baseline_path.display()
        ))
    })?;
    Ok(())
}

fn default_records_root() -> PathBuf {
    PathBuf::from(".")
}

fn default_proposals_root() -> PathBuf {
    PathBuf::from("proposals")
}

fn load_corpus(path: &Path) -> Result<LoadedCorpus> {
    let path = path
        .canonicalize()
        .with_context(|| format!("failed to resolve recall corpus {}", path.display()))?;
    if !path.is_file() {
        bail!("recall corpus is not a regular file: {}", path.display());
    }
    let root = path
        .parent()
        .context("recall corpus path has no parent directory")?
        .to_path_buf();
    let bytes = fs::read(&path)
        .with_context(|| format!("failed to read recall corpus {}", path.display()))?;
    let corpus: RecallEvalCorpus = serde_yaml::from_slice(&bytes)
        .with_context(|| format!("failed to parse recall corpus {}", path.display()))?;
    let digest = corpus_digest(&root, &bytes, &corpus)?;
    Ok(LoadedCorpus {
        corpus,
        root,
        digest,
    })
}

fn corpus_digest(root: &Path, corpus_bytes: &[u8], corpus: &RecallEvalCorpus) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    hash_digest_entry(&mut hasher, Path::new("corpus.yaml"), corpus_bytes);
    validate_fixture_root(&corpus.records_root, "records_root")?;
    for path in sorted_paths(&corpus.records) {
        validate_fixture_path(path, "record fixture")?;
        let bytes = read_safe_fixture(root, &corpus.records_root, path, "record fixture")?;
        hash_digest_entry(&mut hasher, &Path::new("records").join(path), &bytes);
    }
    validate_fixture_root(&corpus.proposals_root, "proposals_root")?;
    let proposal_paths = corpus
        .proposal_fixtures
        .iter()
        .map(|fixture| fixture.path.clone())
        .collect::<Vec<_>>();
    for path in sorted_paths(&proposal_paths) {
        validate_fixture_path(path, "proposal fixture")?;
        let bytes = read_safe_fixture(root, &corpus.proposals_root, path, "proposal fixture")?;
        hash_digest_entry(&mut hasher, &Path::new("proposals").join(path), &bytes);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn hash_digest_entry(hasher: &mut blake3::Hasher, path: &Path, bytes: &[u8]) {
    let path = path.to_string_lossy();
    hasher.update(&(path.len() as u64).to_le_bytes());
    hasher.update(path.as_bytes());
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn sorted_paths(paths: &[PathBuf]) -> Vec<&PathBuf> {
    let mut paths = paths.iter().collect::<Vec<_>>();
    paths.sort();
    paths
}

fn validate_corpus(corpus: &RecallEvalCorpus) -> Result<()> {
    if corpus.version != RECALL_EVAL_CORPUS_VERSION {
        bail!(
            "unsupported recall corpus version {:?}; expected {:?}",
            corpus.version,
            RECALL_EVAL_CORPUS_VERSION
        );
    }
    if corpus.name.trim().is_empty() {
        bail!("recall corpus name cannot be empty");
    }
    FixedClock::from_rfc3339(&corpus.evaluated_at).context("invalid recall corpus evaluated_at")?;
    if corpus.records.is_empty() {
        bail!("recall corpus must reference at least one record fixture");
    }
    if corpus.proposal_fixtures.is_empty() {
        bail!("recall corpus must define at least one proposal fixture");
    }
    if corpus.runtime_fixtures.is_empty() {
        bail!("recall corpus must define local and session runtime fixtures");
    }
    if corpus.cases.is_empty() {
        bail!("recall corpus must define at least one case");
    }
    validate_thresholds(&corpus.thresholds)?;

    validate_fixture_root(&corpus.records_root, "records_root")?;
    validate_fixture_root(&corpus.proposals_root, "proposals_root")?;
    validate_unique_paths(&corpus.records, "record fixture")?;

    let mut proposal_ids = BTreeSet::new();
    let mut proposal_records = BTreeSet::new();
    let mut proposal_paths = Vec::new();
    for fixture in &corpus.proposal_fixtures {
        proposal_paths.push(fixture.path.clone());
        if fixture.proposal_id.trim().is_empty()
            || fixture.expected_record_id.trim().is_empty()
            || fixture.expected_source_kind.trim().is_empty()
            || fixture.expected_source_ref.trim().is_empty()
        {
            bail!("proposal fixture metadata cannot contain empty identifiers");
        }
        if fixture.expected_applies_to.is_empty() {
            bail!(
                "proposal fixture {:?} must declare expected_applies_to",
                fixture.proposal_id
            );
        }
        if !proposal_ids.insert(fixture.proposal_id.as_str()) {
            bail!("duplicate proposal fixture id {:?}", fixture.proposal_id);
        }
        if !proposal_records.insert(fixture.expected_record_id.as_str()) {
            bail!(
                "duplicate proposal fixture record id {:?}",
                fixture.expected_record_id
            );
        }
    }
    validate_unique_paths(&proposal_paths, "proposal fixture")?;

    let mut runtime_ids = BTreeSet::new();
    let mut saw_local = false;
    let mut saw_session = false;
    for fixture in &corpus.runtime_fixtures {
        if fixture.id.trim().is_empty()
            || fixture.title.trim().is_empty()
            || fixture.body.trim().is_empty()
        {
            bail!("runtime fixture identifiers and content cannot be empty");
        }
        if !runtime_ids.insert(fixture.id.as_str()) {
            bail!("duplicate runtime fixture id {:?}", fixture.id);
        }
        match fixture.destination {
            MemoryDestination::Local => saw_local = true,
            MemoryDestination::Session => {
                saw_session = true;
                if fixture.memory_type != MemoryType::Episode || fixture.lane != MemoryLane::Session
                {
                    bail!(
                        "session runtime fixture {:?} must use type episode and lane session",
                        fixture.id
                    );
                }
            }
            destination => bail!(
                "runtime fixture {:?} has unsupported destination {}",
                fixture.id,
                destination
            ),
        }
    }
    if !saw_local || !saw_session {
        bail!("runtime fixtures must cover both local and session destinations");
    }

    let mut case_ids = BTreeSet::new();
    let mut surfaces = BTreeSet::new();
    let mut category_opportunities = BTreeMap::<&str, usize>::new();
    for case in &corpus.cases {
        let id = case_id(case);
        if id.trim().is_empty() {
            bail!("recall case id cannot be empty");
        }
        if !case_ids.insert(id) {
            bail!("duplicate recall case id {id:?}");
        }
        let surface = case_surface(case);
        surfaces.insert(surface_label(surface));
        let relevant = case_relevant_ids(case);
        let forbidden = case_forbidden(case);
        validate_unique_ids(id, "relevant_ids", relevant)?;
        validate_forbidden_ids(id, forbidden)?;
        let relevant_set = relevant.iter().map(String::as_str).collect::<BTreeSet<_>>();
        if let Some(overlap) = forbidden
            .all()
            .into_iter()
            .find(|id| relevant_set.contains(id))
        {
            bail!("recall case {id:?} record {overlap:?} cannot be both relevant and forbidden");
        }
        for (category, ids) in forbidden.groups() {
            *category_opportunities.entry(category).or_default() += ids.len();
        }

        match case {
            RecallEvalCase::Search {
                query,
                relevant_ids,
                scope_id,
                path_prefix,
                k,
                proposal_fixture,
                ..
            } => {
                require_non_empty(id, "query", query)?;
                require_relevant_ids(id, relevant_ids)?;
                if *k == 0 || *k > 100 {
                    bail!("recall case {id:?} k must be between 1 and 100");
                }
                validate_optional_text(id, "scope_id", scope_id.as_deref())?;
                validate_optional_text(id, "path", path_prefix.as_deref())?;
                if let Some(record_id) = proposal_fixture
                    && !proposal_records.contains(record_id.as_str())
                {
                    bail!(
                        "recall case {id:?} references unknown proposal fixture record {record_id:?}"
                    );
                }
            }
            RecallEvalCase::Precheck {
                path,
                action,
                command,
                relevant_ids,
                ..
            } => {
                require_relevant_ids(id, relevant_ids)?;
                validate_optional_text(id, "path", path.as_deref())?;
                validate_optional_text(id, "action", action.as_deref())?;
                validate_optional_text(id, "command", command.as_deref())?;
                if path.as_deref().is_none_or(str::is_empty)
                    && action.as_deref().is_none_or(str::is_empty)
                    && command.as_deref().is_none_or(str::is_empty)
                {
                    bail!("precheck case {id:?} must define path, action, or command");
                }
            }
            RecallEvalCase::Context {
                task,
                path_prefix,
                token_budget,
                relevant_ids,
                ..
            } => {
                require_non_empty(id, "task", task)?;
                require_relevant_ids(id, relevant_ids)?;
                validate_optional_text(id, "path", path_prefix.as_deref())?;
                if token_budget.is_some_and(|budget| budget == 0) {
                    bail!("context case {id:?} token_budget must be positive");
                }
            }
            RecallEvalCase::WriteGate {
                title,
                body,
                query,
                expected_issue_code,
                forbidden_record_id,
                sensitivity,
                ..
            } => {
                for (label, value) in [
                    ("title", title),
                    ("body", body),
                    ("query", query),
                    ("expected_issue_code", expected_issue_code),
                    ("forbidden_record_id", forbidden_record_id),
                ] {
                    require_non_empty(id, label, value)?;
                }
                if *sensitivity == OkfProposalSensitivity::RepoSafe {
                    bail!("write-gate case {id:?} must use non-repo-safe sensitivity");
                }
                *category_opportunities.entry("prohibited").or_default() += 1;
            }
        }
    }

    for required in ["search", "precheck", "context", "write_gate"] {
        if !surfaces.contains(required) {
            bail!("recall corpus must define at least one {required} case");
        }
    }
    for required in ["stale", "expired", "scope", "prohibited", "destination"] {
        if category_opportunities.get(required).copied().unwrap_or(0) == 0 {
            bail!("recall corpus must define at least one {required} forbidden opportunity");
        }
    }
    Ok(())
}

fn validate_thresholds(thresholds: &RecallEvalThresholds) -> Result<()> {
    for (label, value) in [
        ("min_mean_recall_at_k", thresholds.min_mean_recall_at_k),
        ("min_mean_mrr", thresholds.min_mean_mrr),
        ("min_precheck_precision", thresholds.min_precheck_precision),
        ("min_precheck_recall", thresholds.min_precheck_recall),
        ("max_stale_leakage_rate", thresholds.max_stale_leakage_rate),
        (
            "max_expired_leakage_rate",
            thresholds.max_expired_leakage_rate,
        ),
        ("max_scope_leakage_rate", thresholds.max_scope_leakage_rate),
        ("max_forbidden_hit_rate", thresholds.max_forbidden_hit_rate),
        ("min_citation_integrity", thresholds.min_citation_integrity),
        (
            "min_provenance_integrity",
            thresholds.min_provenance_integrity,
        ),
        ("min_case_pass_rate", thresholds.min_case_pass_rate),
    ] {
        validate_ratio(value, &format!("thresholds.{label}"))?;
    }
    if let Some(max_latency) = thresholds.max_p95_latency_ms
        && (!max_latency.is_finite() || max_latency < 0.0)
    {
        bail!("thresholds.max_p95_latency_ms must be a finite non-negative number");
    }
    Ok(())
}

fn validate_ratio(value: f64, label: &str) -> Result<()> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        bail!("{label} must be a finite number between 0 and 1");
    }
    Ok(())
}

fn validate_unique_paths(paths: &[PathBuf], label: &str) -> Result<()> {
    let mut seen = BTreeSet::new();
    for path in paths {
        validate_fixture_path(path, label)?;
        if !seen.insert(path) {
            bail!("duplicate {label} path {}", path.display());
        }
    }
    Ok(())
}

fn validate_unique_ids(case_id: &str, label: &str, ids: &[String]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if id.trim().is_empty() {
            bail!("recall case {case_id:?} {label} cannot contain an empty id");
        }
        if !seen.insert(id.as_str()) {
            bail!("recall case {case_id:?} {label} contains duplicate id {id:?}");
        }
    }
    Ok(())
}

fn validate_forbidden_ids(case_id: &str, forbidden: &RecallEvalForbiddenIds) -> Result<()> {
    let mut seen = BTreeSet::new();
    for (category, ids) in forbidden.groups() {
        validate_unique_ids(case_id, &format!("forbidden.{category}"), ids)?;
        for id in ids {
            if !seen.insert(id.as_str()) {
                bail!("recall case {case_id:?} forbidden id {id:?} appears in multiple categories");
            }
        }
    }
    Ok(())
}

fn require_non_empty(case_id: &str, label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("recall case {case_id:?} {label} cannot be empty");
    }
    Ok(())
}

fn require_relevant_ids(case_id: &str, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        bail!("recall case {case_id:?} must declare relevant_ids");
    }
    Ok(())
}

fn validate_optional_text(case_id: &str, label: &str, value: Option<&str>) -> Result<()> {
    if value.is_some_and(|value| value.trim().is_empty()) {
        bail!("recall case {case_id:?} {label} cannot be empty");
    }
    Ok(())
}

fn stage_record_fixtures(
    corpus_root: &Path,
    records_root: &Path,
    records: &[PathBuf],
    destination_root: &Path,
) -> Result<()> {
    for record_path in records {
        let bytes = read_safe_fixture(corpus_root, records_root, record_path, "record fixture")?;
        let destination = destination_root.join(record_path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create isolated fixture directory {}",
                    parent.display()
                )
            })?;
        }
        fs::write(&destination, bytes).with_context(|| {
            format!(
                "failed to stage isolated record fixture {}",
                record_path.display()
            )
        })?;
    }
    Ok(())
}

fn read_safe_fixture(
    corpus_root: &Path,
    fixture_root: &Path,
    fixture_path: &Path,
    label: &str,
) -> Result<Vec<u8>> {
    validate_fixture_root(fixture_root, "fixture root")?;
    validate_fixture_path(fixture_path, label)?;
    let root = corpus_root.join(fixture_root);
    ensure_directory_without_symlinks(corpus_root, fixture_root, &root, "fixture root")?;
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to resolve fixture root {}", root.display()))?;
    let source = root.join(fixture_path);
    ensure_regular_file_without_symlinks(&root, fixture_path, &source, label)?;
    let source = source
        .canonicalize()
        .with_context(|| format!("failed to resolve {label} {}", fixture_path.display()))?;
    if !source.starts_with(&root) {
        bail!("{label} escapes fixture root: {}", fixture_path.display());
    }
    fs::read(&source).with_context(|| format!("failed to read {label} {}", fixture_path.display()))
}

fn validate_fixture_root(path: &Path, label: &str) -> Result<()> {
    if path.is_absolute() {
        bail!("{label} must be relative to the corpus file");
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_) | Component::CurDir) {
            bail!("{label} contains traversal or an unsafe component");
        }
    }
    Ok(())
}

fn validate_fixture_path(path: &Path, label: &str) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("{label} paths must be non-empty and relative");
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!(
            "{label} path contains traversal or an unsafe component: {}",
            path.display()
        );
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
        bail!("{label} must use the .md extension: {}", path.display());
    }
    if matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("index.md" | "log.md")
    ) {
        bail!("{label} uses a reserved file name: {}", path.display());
    }
    Ok(())
}

fn ensure_directory_without_symlinks(
    root: &Path,
    relative: &Path,
    path: &Path,
    label: &str,
) -> Result<()> {
    ensure_path_chain_without_symlinks(root, relative, label)?;
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    if !metadata.is_dir() {
        bail!("{label} is not a directory: {}", path.display());
    }
    Ok(())
}

fn ensure_regular_file_without_symlinks(
    root: &Path,
    relative: &Path,
    path: &Path,
    label: &str,
) -> Result<()> {
    ensure_path_chain_without_symlinks(root, relative, label)?;
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", relative.display()))?;
    if !metadata.is_file() {
        bail!("{label} is not a regular file: {}", relative.display());
    }
    Ok(())
}

fn ensure_path_chain_without_symlinks(root: &Path, relative: &Path, label: &str) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if let Component::Normal(component) = component {
            current.push(component);
            let metadata = fs::symlink_metadata(&current)
                .with_context(|| format!("failed to inspect {label} {}", relative.display()))?;
            if metadata.file_type().is_symlink() {
                bail!(
                    "{label} path must not contain symlinks: {}",
                    relative.display()
                );
            }
        }
    }
    Ok(())
}

fn stage_and_apply_proposal_fixtures(
    service: &MemoryService,
    corpus_root: &Path,
    proposals_root: &Path,
    fixtures: &[RecallEvalProposalFixture],
) -> Result<Vec<AppliedProposalFixture>> {
    let pending_root = service.paths().proposals_dir().join("pending");
    fs::create_dir_all(&pending_root)?;
    let mut applied = Vec::with_capacity(fixtures.len());
    for fixture in fixtures {
        let bytes = read_safe_fixture(
            corpus_root,
            proposals_root,
            &fixture.path,
            "proposal fixture",
        )?;
        let pending_path = stage_proposal_fixture(&pending_root, &fixture.path, &bytes)?;
        let result = service
            .apply_file_proposal(&pending_path, "memzoi-eval")
            .with_context(|| {
                format!(
                    "failed to apply proposal fixture {}",
                    fixture.path.display()
                )
            })?;
        let record = result
            .record
            .context("proposal fixture apply returned no record")?;
        let resolution_preserved = result.resolved_path.is_file()
            && result.resolution.record_id.as_deref() == Some(record.id.as_str());
        applied.push(AppliedProposalFixture {
            spec: fixture.clone(),
            proposal_id: result.proposal.id,
            record_id: record.id,
            resolution_preserved,
        });
    }
    Ok(applied)
}

fn stage_proposal_fixture(
    pending_root: &Path,
    fixture_path: &Path,
    bytes: &[u8],
) -> Result<PathBuf> {
    validate_fixture_path(fixture_path, "proposal fixture")?;
    let pending_path = pending_root.join(fixture_path);
    let parent = pending_path
        .parent()
        .context("proposal fixture destination has no parent")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create proposal fixture directory {}",
            parent.display()
        )
    })?;
    let mut destination = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending_path)
        .with_context(|| {
            format!(
                "failed to stage proposal fixture {}",
                fixture_path.display()
            )
        })?;
    destination.write_all(bytes).with_context(|| {
        format!(
            "failed to write proposal fixture {}",
            fixture_path.display()
        )
    })?;
    Ok(pending_path)
}

fn seed_runtime_fixtures(
    service: &MemoryService,
    fixtures: &[RecallEvalRuntimeFixture],
) -> Result<Vec<MemoryRecord>> {
    let mut records = Vec::with_capacity(fixtures.len());
    for fixture in fixtures {
        let record = match fixture.destination {
            MemoryDestination::Local => service.create_local_memory(
                "memzoi-eval",
                LocalMemoryInput {
                    memory_type: fixture.memory_type,
                    lane: fixture.lane,
                    title: fixture.title.clone(),
                    body: fixture.body.clone(),
                },
            )?,
            MemoryDestination::Session => service.create_checkpoint(
                "memzoi-eval",
                CheckpointInput {
                    task: fixture.title.clone(),
                    note: fixture.body.clone(),
                },
            )?,
            _ => unreachable!("runtime fixture destinations are validated"),
        };
        if record.id != fixture.id {
            bail!(
                "runtime fixture {:?} produced unexpected stable id {:?}",
                fixture.id,
                record.id
            );
        }
        records.push(record);
    }
    Ok(records)
}

fn load_record_catalog(
    records_root: &Path,
    runtime_records: Vec<MemoryRecord>,
) -> Result<BTreeMap<String, EvalRecord>> {
    let mut catalog = BTreeMap::new();
    for parsed in okf::read_okf_record_files(records_root)? {
        let paths = parsed
            .applies_to
            .iter()
            .map(|path| MemoryPath {
                path: path.clone(),
                symbol: None,
                line_start: None,
                line_end: None,
            })
            .collect();
        let record = okf::project_okf_record(&parsed);
        let id = record.id.clone();
        if catalog
            .insert(id.clone(), EvalRecord { record, paths })
            .is_some()
        {
            bail!("duplicate evaluation record id {id:?}");
        }
    }
    for record in runtime_records {
        let id = record.id.clone();
        if catalog
            .insert(
                id.clone(),
                EvalRecord {
                    record,
                    paths: Vec::new(),
                },
            )
            .is_some()
        {
            bail!("duplicate evaluation record id {id:?}");
        }
    }
    Ok(catalog)
}

fn validate_case_record_ids(
    corpus: &RecallEvalCorpus,
    catalog: &BTreeMap<String, EvalRecord>,
) -> Result<()> {
    for case in &corpus.cases {
        if matches!(case, RecallEvalCase::WriteGate { .. }) {
            continue;
        }
        for id in case_relevant_ids(case) {
            if !catalog.contains_key(id.as_str()) {
                bail!(
                    "recall case {:?} references missing relevant record id {:?}",
                    case_id(case),
                    id
                );
            }
        }
        let forbidden_ids = case_forbidden(case).all();
        for id in forbidden_ids {
            if !catalog.contains_key(id) {
                bail!(
                    "recall case {:?} references missing forbidden record id {:?}",
                    case_id(case),
                    id
                );
            }
        }
    }
    Ok(())
}

fn validate_forbidden_semantics(
    corpus: &RecallEvalCorpus,
    catalog: &BTreeMap<String, EvalRecord>,
) -> Result<()> {
    let now = FixedClock::from_rfc3339(&corpus.evaluated_at)?.now_utc();
    for case in &corpus.cases {
        let forbidden = case_forbidden(case);
        for id in &forbidden.stale {
            let record = &catalog[id].record;
            let effectively_expired = record.status == MemoryStatus::Expired
                || crate::expiry::is_expired(record.expires_at.as_deref(), now)?;
            if record.status == MemoryStatus::Active || effectively_expired {
                bail!(
                    "recall case {:?} stale record {:?} must be inactive and not expired",
                    case_id(case),
                    id
                );
            }
        }
        for id in &forbidden.expired {
            let record = &catalog[id].record;
            if record.status != MemoryStatus::Expired
                && !crate::expiry::is_expired(record.expires_at.as_deref(), now)?
            {
                bail!(
                    "recall case {:?} expired record {:?} is not expired at evaluated_at",
                    case_id(case),
                    id
                );
            }
        }
        for id in &forbidden.scope {
            let record = &catalog[id];
            if !record_is_out_of_scope(case, record) {
                bail!(
                    "recall case {:?} scope-forbidden record {:?} is not out of scope or path",
                    case_id(case),
                    id
                );
            }
        }
        for id in &forbidden.destination {
            let record = &catalog[id].record;
            if requested_destinations(case).contains(&record.destination) {
                bail!(
                    "recall case {:?} destination-forbidden record {:?} is explicitly requested",
                    case_id(case),
                    id
                );
            }
        }
    }
    Ok(())
}

fn record_is_out_of_scope(case: &RecallEvalCase, record: &EvalRecord) -> bool {
    let (scope_kind, scope_id, requested_path) = match case {
        RecallEvalCase::Search {
            scope_kind,
            scope_id,
            path_prefix,
            ..
        } => (*scope_kind, scope_id.as_deref(), path_prefix.as_deref()),
        RecallEvalCase::Precheck {
            scope_kind, path, ..
        } => (*scope_kind, None, path.as_deref()),
        RecallEvalCase::Context { path_prefix, .. } => (None, None, path_prefix.as_deref()),
        RecallEvalCase::WriteGate { .. } => (None, None, None),
    };
    scope_kind.is_some_and(|scope| record.record.scope_kind != scope)
        || scope_id.is_some_and(|scope_id| record.record.scope_id.as_deref() != Some(scope_id))
        || requested_path.is_some_and(|requested| {
            !record
                .paths
                .iter()
                .any(|path| path_matches_request(&path.path, requested))
        })
}

fn requested_destinations(case: &RecallEvalCase) -> Vec<MemoryDestination> {
    match case {
        RecallEvalCase::Context {
            include_local,
            include_session,
            ..
        } => {
            let mut destinations = vec![MemoryDestination::Repo];
            if *include_local {
                destinations.push(MemoryDestination::Local);
            }
            if *include_session {
                destinations.push(MemoryDestination::Session);
            }
            destinations
        }
        _ => vec![MemoryDestination::Repo],
    }
}

fn proposal_fixture_reports(
    applied: &[AppliedProposalFixture],
    catalog: &BTreeMap<String, EvalRecord>,
) -> Result<Vec<RecallEvalProposalFixtureReport>> {
    let mut reports = Vec::with_capacity(applied.len());
    for fixture in applied {
        let catalog_record = catalog
            .get(&fixture.spec.expected_record_id)
            .with_context(|| {
                format!(
                    "proposal fixture record {:?} disappeared after rebuild",
                    fixture.spec.expected_record_id
                )
            })?;
        let record = &catalog_record.record;
        let source_preserved = record.source_kind.as_deref()
            == Some(fixture.spec.expected_source_kind.as_str())
            && record.source_ref.as_deref() == Some(fixture.spec.expected_source_ref.as_str());
        let lineage_preserved = fixture.proposal_id == fixture.spec.proposal_id
            && fixture.record_id == fixture.spec.expected_record_id
            && record.proposal_id.as_deref() == Some(fixture.spec.proposal_id.as_str());
        let lineage_separate_from_evidence = record.source_ref.as_deref()
            != record.proposal_id.as_deref()
            && record.proposal_id.is_some();
        let actual_paths = catalog_record
            .paths
            .iter()
            .map(|path| path.path.as_str())
            .collect::<BTreeSet<_>>();
        let expected_paths = fixture
            .spec
            .expected_applies_to
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let applicability_preserved = actual_paths == expected_paths;
        let passed = source_preserved
            && fixture.resolution_preserved
            && lineage_preserved
            && lineage_separate_from_evidence
            && applicability_preserved;
        reports.push(RecallEvalProposalFixtureReport {
            proposal_id: fixture.spec.proposal_id.clone(),
            record_id: fixture.spec.expected_record_id.clone(),
            source_kind: fixture.spec.expected_source_kind.clone(),
            source_ref: fixture.spec.expected_source_ref.clone(),
            applies_to: fixture.spec.expected_applies_to.clone(),
            source_preserved,
            resolution_preserved: fixture.resolution_preserved,
            lineage_preserved,
            lineage_separate_from_evidence,
            applicability_preserved,
            passed,
        });
    }
    Ok(reports)
}

fn evaluate_case(
    service: &MemoryService,
    records_root: &Path,
    case: &RecallEvalCase,
    catalog: &BTreeMap<String, EvalRecord>,
    proposal_status: &BTreeMap<String, bool>,
) -> Result<EvaluatedCase> {
    match case {
        RecallEvalCase::Search {
            id,
            query,
            relevant_ids,
            forbidden,
            scope_kind,
            scope_id,
            memory_type,
            lane,
            path_prefix,
            k,
            proposal_fixture,
        } => {
            let started = Instant::now();
            let results = service.search_memory(SearchInput {
                query: query.clone(),
                scope_kind: *scope_kind,
                scope_id: scope_id.clone(),
                memory_type: *memory_type,
                lane: *lane,
                path_prefix: path_prefix.clone(),
                limit: *k,
                ..SearchInput::default()
            })?;
            let latency = started.elapsed().as_secs_f64() * 1_000.0;
            let retrieved_ids = results
                .iter()
                .map(|result| result.record.id.clone())
                .collect::<Vec<_>>();
            let retrieved = retrieved_from_results(&results);
            let (citation_integrity, provenance_integrity) =
                integrity_for_results(&results, catalog, path_prefix.as_deref(), proposal_status);
            let counts = retrieval_counts(relevant_ids, &retrieved_ids);
            let recall_at_k = ratio_value(counts.true_positives, relevant_ids.len());
            let relevant = relevant_ids
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            let mrr = retrieved_ids
                .iter()
                .position(|id| relevant.contains(id.as_str()))
                .map_or(0.0, |index| 1.0 / (index + 1) as f64);
            let forbidden_hits = forbidden.hits(&retrieved_ids);
            let mut assertions = BTreeMap::new();
            assertions.insert("unique_results".to_owned(), all_unique(&retrieved_ids));
            if let Some(record_id) = proposal_fixture {
                assertions.insert(
                    "proposal_evidence_round_trip".to_owned(),
                    proposal_status.get(record_id).copied().unwrap_or(false)
                        && retrieved_ids.contains(record_id),
                );
            }
            evaluated_case(
                id,
                RecallEvalSurface::Search,
                Some(query.clone()),
                relevant_ids.clone(),
                forbidden.clone(),
                retrieved_ids,
                retrieved,
                counts,
                Some(recall_at_k),
                Some(mrr),
                forbidden_hits,
                citation_integrity,
                provenance_integrity,
                None,
                latency,
                assertions,
            )
        }
        RecallEvalCase::Precheck {
            id,
            path,
            action,
            command,
            scope_kind,
            relevant_ids,
            forbidden,
        } => {
            let started = Instant::now();
            let warnings = service.precheck(PrecheckInput {
                path: path.clone(),
                action: action.clone(),
                command: command.clone(),
                scope_kind: *scope_kind,
            })?;
            let latency = started.elapsed().as_secs_f64() * 1_000.0;
            let retrieved_ids = warnings
                .iter()
                .map(|warning| warning.record_id.clone())
                .collect::<Vec<_>>();
            let retrieved = warnings
                .iter()
                .enumerate()
                .map(|(index, warning)| RecallEvalRetrievedRecord {
                    rank: index + 1,
                    record_id: warning.record_id.clone(),
                    citations: warning.citations.clone(),
                })
                .collect();
            let (citation_integrity, provenance_integrity) =
                integrity_for_warnings(&warnings, catalog, path.as_deref(), proposal_status);
            let counts = retrieval_counts(relevant_ids, &retrieved_ids);
            let forbidden_hits = forbidden.hits(&retrieved_ids);
            let mut assertions = BTreeMap::new();
            assertions.insert("unique_results".to_owned(), all_unique(&retrieved_ids));
            assertions.insert(
                "path_only_supported".to_owned(),
                path.is_none() || action.is_some() || command.is_some() || !warnings.is_empty(),
            );
            assertions.insert(
                "warning_severity".to_owned(),
                warnings.iter().all(|warning| {
                    catalog.get(&warning.record_id).is_some_and(|record| {
                        warning.severity == expected_precheck_severity(record.record.memory_type)
                    })
                }),
            );
            evaluated_case(
                id,
                RecallEvalSurface::Precheck,
                action.clone().or_else(|| command.clone()),
                relevant_ids.clone(),
                forbidden.clone(),
                retrieved_ids,
                retrieved,
                counts,
                None,
                None,
                forbidden_hits,
                citation_integrity,
                provenance_integrity,
                None,
                latency,
                assertions,
            )
        }
        RecallEvalCase::Context {
            id,
            task,
            path_prefix,
            token_budget,
            include_local,
            include_session,
            relevant_ids,
            forbidden,
        } => {
            let started = Instant::now();
            let pack = service.build_context_pack(ContextPackInput {
                task: task.clone(),
                path_prefix: path_prefix.clone(),
                token_budget: *token_budget,
                include_local: *include_local,
                include_session: *include_session,
            })?;
            let latency = started.elapsed().as_secs_f64() * 1_000.0;
            let retrieved_ids = pack
                .records
                .iter()
                .map(|result| result.record.id.clone())
                .collect::<Vec<_>>();
            let retrieved = retrieved_from_results(&pack.records);
            let (citation_integrity, provenance_integrity) = integrity_for_results(
                &pack.records,
                catalog,
                path_prefix.as_deref(),
                proposal_status,
            );
            let counts = retrieval_counts(relevant_ids, &retrieved_ids);
            let forbidden_hits = forbidden.hits(&retrieved_ids);
            let mut requested_destinations = vec![MemoryDestination::Repo];
            if *include_local {
                requested_destinations.push(MemoryDestination::Local);
            }
            if *include_session {
                requested_destinations.push(MemoryDestination::Session);
            }
            let mut assertions = BTreeMap::new();
            assertions.insert("unique_results".to_owned(), all_unique(&retrieved_ids));
            assertions.insert(
                "destination_policy".to_owned(),
                pack.policy.requested_destinations == requested_destinations
                    && pack
                        .records
                        .iter()
                        .all(|result| requested_destinations.contains(&result.record.destination)),
            );
            assertions.insert(
                "usage_within_budget".to_owned(),
                pack.budget.estimated_used <= pack.budget.effective,
            );
            evaluated_case(
                id,
                RecallEvalSurface::Context,
                Some(task.clone()),
                relevant_ids.clone(),
                forbidden.clone(),
                retrieved_ids,
                retrieved,
                counts,
                None,
                None,
                forbidden_hits,
                citation_integrity,
                provenance_integrity,
                Some(pack.budget.estimated_used),
                latency,
                assertions,
            )
        }
        RecallEvalCase::WriteGate {
            id,
            title,
            body,
            query,
            sensitivity,
            expected_issue_code,
            forbidden_record_id,
        } => {
            let started = Instant::now();
            let result = service.propose_memory_with_options(
                "memzoi-eval",
                MemoryDraft {
                    memory_type: MemoryType::Fact,
                    lane: MemoryLane::Semantic,
                    scope_kind: ScopeKind::Repo,
                    scope_id: None,
                    visibility: Visibility::Repo,
                    title: title.clone(),
                    body: body.clone(),
                    tags: vec!["eval".to_owned()],
                    source_kind: Some("eval".to_owned()),
                    source_ref: Some("fixture://write-gate".to_owned()),
                    sensitivity: *sensitivity,
                    content_class: crate::RepositoryContentClass::GeneralRepoKnowledge,
                    confidence: 1.0,
                },
                ProposeOptions {
                    approval_override: Some(ProposalApprovalOverride::Auto),
                    apply: true,
                },
            )?;
            let search_results = service.search_memory(SearchInput {
                query: query.clone(),
                scope_kind: Some(ScopeKind::Repo),
                limit: 10,
                ..SearchInput::default()
            })?;
            let latency = started.elapsed().as_secs_f64() * 1_000.0;
            let mut retrieved_ids = search_results
                .iter()
                .map(|result| result.record.id.clone())
                .collect::<Vec<_>>();
            if let Some(record) = &result.record
                && !retrieved_ids.contains(&record.id)
            {
                retrieved_ids.push(record.id.clone());
            }
            let mut retrieved = retrieved_from_results(&search_results);
            if let Some(record) = &result.record
                && !retrieved
                    .iter()
                    .any(|retrieved| retrieved.record_id == record.id)
            {
                retrieved.push(RecallEvalRetrievedRecord {
                    rank: retrieved.len() + 1,
                    record_id: record.id.clone(),
                    citations: Vec::new(),
                });
            }
            let forbidden = RecallEvalForbiddenIds {
                prohibited: vec![forbidden_record_id.clone()],
                ..RecallEvalForbiddenIds::default()
            };
            let forbidden_hits = forbidden.hits(&retrieved_ids);
            let (citation_integrity, provenance_integrity) =
                integrity_for_results(&search_results, catalog, None, proposal_status);
            let issue_present = result.validation.as_ref().is_some_and(|validation| {
                validation
                    .issues
                    .iter()
                    .any(|issue| issue.code == *expected_issue_code)
            });
            let mut assertions = BTreeMap::new();
            assertions.insert(
                "not_applied".to_owned(),
                !result.applied && result.record.is_none(),
            );
            assertions.insert("expected_issue".to_owned(), issue_present);
            assertions.insert(
                "no_canonical_record".to_owned(),
                !records_root
                    .join(format!("{forbidden_record_id}.md"))
                    .exists(),
            );
            assertions.insert("unique_results".to_owned(), all_unique(&retrieved_ids));
            evaluated_case(
                id,
                RecallEvalSurface::WriteGate,
                Some(query.clone()),
                Vec::new(),
                forbidden,
                retrieved_ids.clone(),
                retrieved,
                RetrievalCounts {
                    true_positives: 0,
                    false_positives: retrieved_ids.len(),
                    false_negatives: 0,
                },
                None,
                None,
                forbidden_hits,
                citation_integrity,
                provenance_integrity,
                None,
                latency,
                assertions,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluated_case(
    id: &str,
    surface: RecallEvalSurface,
    query: Option<String>,
    relevant_ids: Vec<String>,
    forbidden: RecallEvalForbiddenIds,
    retrieved_ids: Vec<String>,
    retrieved: Vec<RecallEvalRetrievedRecord>,
    counts: RetrievalCounts,
    recall_at_k: Option<f64>,
    mrr: Option<f64>,
    forbidden_hits: RecallEvalForbiddenIds,
    citation_integrity: RecallEvalIntegrityMetric,
    provenance_integrity: RecallEvalIntegrityMetric,
    estimated_usage: Option<usize>,
    raw_latency_ms: f64,
    assertions: BTreeMap<String, bool>,
) -> Result<EvaluatedCase> {
    let relevant_complete = counts.false_negatives == 0;
    let no_forbidden_hits = forbidden_hits.count() == 0;
    let integrity_complete = citation_integrity.valid == citation_integrity.checked
        && provenance_integrity.valid == provenance_integrity.checked;
    let assertions_passed = assertions.values().all(|passed| *passed);
    let passed = relevant_complete && no_forbidden_hits && integrity_complete && assertions_passed;
    let raw_recall_at_k = recall_at_k;
    let raw_mrr = mrr;
    Ok(EvaluatedCase {
        report: RecallEvalCaseReport {
            id: id.to_owned(),
            surface,
            query,
            relevant_ids,
            forbidden,
            retrieved_ids,
            retrieved,
            true_positives: counts.true_positives,
            false_positives: counts.false_positives,
            false_negatives: counts.false_negatives,
            recall_at_k: recall_at_k.map(|value| rounded(value, 6)),
            mrr: mrr.map(|value| rounded(value, 6)),
            forbidden_hits,
            citation_integrity,
            provenance_integrity,
            estimated_usage,
            latency_ms: rounded(raw_latency_ms, 3),
            assertions,
            passed,
        },
        raw_latency_ms,
        raw_recall_at_k,
        raw_mrr,
    })
}

fn retrieved_from_results(results: &[crate::SearchResult]) -> Vec<RecallEvalRetrievedRecord> {
    results
        .iter()
        .enumerate()
        .map(|(index, result)| RecallEvalRetrievedRecord {
            rank: index + 1,
            record_id: result.record.id.clone(),
            citations: result.citations.clone(),
        })
        .collect()
}

fn expected_precheck_severity(memory_type: MemoryType) -> &'static str {
    match memory_type {
        MemoryType::Risk => "high",
        MemoryType::Warning | MemoryType::FailedAttempt => "warning",
        _ => "info",
    }
}

#[derive(Debug, Clone, Copy)]
struct RetrievalCounts {
    true_positives: usize,
    false_positives: usize,
    false_negatives: usize,
}

fn retrieval_counts(relevant_ids: &[String], retrieved_ids: &[String]) -> RetrievalCounts {
    let relevant = relevant_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let retrieved = retrieved_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let true_positives = relevant.intersection(&retrieved).count();
    RetrievalCounts {
        true_positives,
        false_positives: retrieved.difference(&relevant).count(),
        false_negatives: relevant.len().saturating_sub(true_positives),
    }
}

fn integrity_for_results(
    results: &[crate::SearchResult],
    catalog: &BTreeMap<String, EvalRecord>,
    requested_path: Option<&str>,
    proposal_status: &BTreeMap<String, bool>,
) -> (RecallEvalIntegrityMetric, RecallEvalIntegrityMetric) {
    let mut citation_valid = 0;
    let mut provenance_valid = 0;
    for result in results {
        let expected = catalog.get(&result.record.id);
        if expected.is_some_and(|expected| {
            citation_is_valid(
                &result.record.id,
                &result.citations,
                expected,
                requested_path,
            )
        }) {
            citation_valid += 1;
        }
        if expected.is_some_and(|expected| {
            provenance_is_valid(&result.citations, expected, proposal_status)
        }) {
            provenance_valid += 1;
        }
    }
    (
        integrity_metric(citation_valid, results.len()),
        integrity_metric(provenance_valid, results.len()),
    )
}

fn integrity_for_warnings(
    warnings: &[crate::PrecheckWarning],
    catalog: &BTreeMap<String, EvalRecord>,
    requested_path: Option<&str>,
    proposal_status: &BTreeMap<String, bool>,
) -> (RecallEvalIntegrityMetric, RecallEvalIntegrityMetric) {
    let mut citation_valid = 0;
    let mut provenance_valid = 0;
    for warning in warnings {
        let expected = catalog.get(&warning.record_id);
        if expected.is_some_and(|expected| {
            citation_is_valid(
                &warning.record_id,
                &warning.citations,
                expected,
                requested_path,
            )
        }) {
            citation_valid += 1;
        }
        if expected.is_some_and(|expected| {
            provenance_is_valid(&warning.citations, expected, proposal_status)
        }) {
            provenance_valid += 1;
        }
    }
    (
        integrity_metric(citation_valid, warnings.len()),
        integrity_metric(provenance_valid, warnings.len()),
    )
}

fn citation_is_valid(
    record_id: &str,
    citations: &[MemoryCitation],
    expected: &EvalRecord,
    requested_path: Option<&str>,
) -> bool {
    if citations.is_empty() {
        return false;
    }
    citations.iter().all(|citation| {
        citation.record_id == record_id
            && citation.record_id == expected.record.id
            && citation.memory_type == expected.record.memory_type
            && citation.scope_kind == expected.record.scope_kind
            && citation.destination == expected.record.destination
            && citation.visibility == expected.record.visibility
            && citation.path.as_deref().is_none_or(|citation_path| {
                expected.paths.iter().any(|path| path.path == citation_path)
                    && requested_path
                        .is_none_or(|requested| path_matches_request(citation_path, requested))
            })
            && requested_path.is_none_or(|requested| {
                citation
                    .path
                    .as_deref()
                    .is_some_and(|path| path_matches_request(path, requested))
            })
    })
}

fn provenance_is_valid(
    citations: &[MemoryCitation],
    expected: &EvalRecord,
    proposal_status: &BTreeMap<String, bool>,
) -> bool {
    let Some(expected_plane) = expected.record.destination.policy().plane else {
        return false;
    };
    let proposal_lineage_valid = expected
        .record
        .proposal_id
        .as_deref()
        .is_none_or(|proposal_id| {
            proposal_status
                .get(&expected.record.id)
                .copied()
                .unwrap_or(false)
                && expected.record.source_ref.as_deref() != Some(proposal_id)
        });
    !citations.is_empty()
        && proposal_lineage_valid
        && citations.iter().all(|citation| {
            citation.provenance == expected_plane
                && citation.source_kind == expected.record.source_kind
                && citation.source_ref == expected.record.source_ref
        })
}

fn aggregate_metrics(evaluated: &[EvaluatedCase]) -> (RecallEvalMetrics, RawMetrics) {
    let search_cases = evaluated
        .iter()
        .filter(|case| case.report.surface == RecallEvalSurface::Search)
        .collect::<Vec<_>>();
    let mean_recall_at_k = mean(
        &search_cases
            .iter()
            .filter_map(|case| case.raw_recall_at_k)
            .collect::<Vec<_>>(),
    );
    let mean_mrr = mean(
        &search_cases
            .iter()
            .filter_map(|case| case.raw_mrr)
            .collect::<Vec<_>>(),
    );

    let precheck_cases = evaluated
        .iter()
        .filter(|case| case.report.surface == RecallEvalSurface::Precheck)
        .collect::<Vec<_>>();
    let true_positives = precheck_cases
        .iter()
        .map(|case| case.report.true_positives)
        .sum();
    let false_positives = precheck_cases
        .iter()
        .map(|case| case.report.false_positives)
        .sum();
    let false_negatives = precheck_cases
        .iter()
        .map(|case| case.report.false_negatives)
        .sum();
    let precision = ratio_metric(true_positives, true_positives + false_positives);
    let recall = ratio_metric(true_positives, true_positives + false_negatives);

    let stale = aggregate_leakage(evaluated, |ids| &ids.stale);
    let expired = aggregate_leakage(evaluated, |ids| &ids.expired);
    let scope = aggregate_leakage(evaluated, |ids| &ids.scope);
    let prohibited = aggregate_leakage(evaluated, |ids| &ids.prohibited);
    let destination = aggregate_leakage(evaluated, |ids| &ids.destination);
    let forbidden_hits = evaluated
        .iter()
        .map(|case| case.report.forbidden_hits.count())
        .sum();
    let forbidden_opportunities = evaluated
        .iter()
        .map(|case| case.report.forbidden.count())
        .sum();
    let forbidden = leakage_metric(forbidden_hits, forbidden_opportunities);

    let citation_valid = evaluated
        .iter()
        .map(|case| case.report.citation_integrity.valid)
        .sum();
    let citation_checked = evaluated
        .iter()
        .map(|case| case.report.citation_integrity.checked)
        .sum();
    let provenance_valid = evaluated
        .iter()
        .map(|case| case.report.provenance_integrity.valid)
        .sum();
    let provenance_checked = evaluated
        .iter()
        .map(|case| case.report.provenance_integrity.checked)
        .sum();
    let citation_integrity = integrity_metric(citation_valid, citation_checked);
    let provenance_integrity = integrity_metric(provenance_valid, provenance_checked);

    let usages = evaluated
        .iter()
        .filter_map(|case| case.report.estimated_usage)
        .collect::<Vec<_>>();
    let token_usage = token_usage_metrics(&usages);
    let raw_latencies = evaluated
        .iter()
        .map(|case| case.raw_latency_ms)
        .collect::<Vec<_>>();
    let raw_p95_latency = percentile_f64(&raw_latencies, 0.95);
    let latency = RecallEvalLatencyMetrics {
        unit: "milliseconds".to_owned(),
        timer: "monotonic_instant".to_owned(),
        sample_count: raw_latencies.len(),
        p50: rounded(percentile_f64(&raw_latencies, 0.50), 3),
        p95: rounded(raw_p95_latency, 3),
    };
    let passed_cases = evaluated.iter().filter(|case| case.report.passed).count();
    let case_pass_rate = ratio_metric(passed_cases, evaluated.len());

    let raw = RawMetrics {
        mean_recall_at_k,
        mean_mrr,
        precheck_precision: ratio_value(true_positives, true_positives + false_positives),
        precheck_recall: ratio_value(true_positives, true_positives + false_negatives),
        stale_leakage_rate: raw_leakage_rate(&stale),
        expired_leakage_rate: raw_leakage_rate(&expired),
        scope_leakage_rate: raw_leakage_rate(&scope),
        forbidden_hit_rate: raw_leakage_rate(&forbidden),
        citation_integrity: ratio_value(citation_valid, citation_checked),
        provenance_integrity: ratio_value(provenance_valid, provenance_checked),
        case_pass_rate: ratio_value(passed_cases, evaluated.len()),
        max_estimated_usage: token_usage.max,
        p95_latency_ms: raw_p95_latency,
    };

    (
        RecallEvalMetrics {
            search: RecallEvalSearchMetrics {
                case_count: search_cases.len(),
                mean_recall_at_k: rounded(mean_recall_at_k, 6),
                mean_mrr: rounded(mean_mrr, 6),
            },
            precheck: RecallEvalPrecheckMetrics {
                case_count: precheck_cases.len(),
                true_positives,
                false_positives,
                false_negatives,
                precision,
                recall,
            },
            leakage: RecallEvalLeakageMetrics {
                stale,
                expired,
                scope,
                prohibited,
                destination,
                forbidden,
            },
            citation_integrity,
            provenance_integrity,
            token_usage,
            latency,
            case_pass_rate,
        },
        raw,
    )
}

fn aggregate_leakage(
    evaluated: &[EvaluatedCase],
    select: impl Fn(&RecallEvalForbiddenIds) -> &[String],
) -> RecallEvalLeakageMetric {
    let hits = evaluated
        .iter()
        .map(|case| select(&case.report.forbidden_hits).len())
        .sum();
    let opportunities = evaluated
        .iter()
        .map(|case| select(&case.report.forbidden).len())
        .sum();
    leakage_metric(hits, opportunities)
}

fn ratio_metric(numerator: usize, denominator: usize) -> RecallEvalRatioMetric {
    RecallEvalRatioMetric {
        numerator,
        denominator,
        value: rounded(ratio_value(numerator, denominator), 6),
    }
}

fn leakage_metric(hits: usize, opportunities: usize) -> RecallEvalLeakageMetric {
    RecallEvalLeakageMetric {
        hits,
        opportunities,
        rate: rounded(raw_leakage_rate_from_counts(hits, opportunities), 6),
    }
}

fn raw_leakage_rate(metric: &RecallEvalLeakageMetric) -> f64 {
    raw_leakage_rate_from_counts(metric.hits, metric.opportunities)
}

fn raw_leakage_rate_from_counts(hits: usize, opportunities: usize) -> f64 {
    if opportunities == 0 {
        0.0
    } else {
        hits as f64 / opportunities as f64
    }
}

fn integrity_metric(valid: usize, checked: usize) -> RecallEvalIntegrityMetric {
    RecallEvalIntegrityMetric {
        valid,
        checked,
        rate: rounded(ratio_value(valid, checked), 6),
    }
}

fn ratio_value(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn token_usage_metrics(values: &[usize]) -> RecallEvalTokenUsageMetrics {
    let total = values.iter().sum();
    RecallEvalTokenUsageMetrics {
        unit: "approx_words".to_owned(),
        estimator: "context_pack/approx_words-v1".to_owned(),
        sample_count: values.len(),
        total,
        mean: rounded(
            if values.is_empty() {
                0.0
            } else {
                total as f64 / values.len() as f64
            },
            3,
        ),
        p50: percentile_usize(values, 0.50),
        p95: percentile_usize(values, 0.95),
        max: values.iter().copied().max().unwrap_or(0),
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

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        1.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn evaluate_thresholds(
    thresholds: &RecallEvalThresholds,
    raw: &RawMetrics,
) -> RecallEvalThresholdResults {
    RecallEvalThresholdResults {
        min_mean_recall_at_k: raw.mean_recall_at_k >= thresholds.min_mean_recall_at_k,
        min_mean_mrr: raw.mean_mrr >= thresholds.min_mean_mrr,
        min_precheck_precision: raw.precheck_precision >= thresholds.min_precheck_precision,
        min_precheck_recall: raw.precheck_recall >= thresholds.min_precheck_recall,
        max_stale_leakage_rate: raw.stale_leakage_rate <= thresholds.max_stale_leakage_rate,
        max_expired_leakage_rate: raw.expired_leakage_rate <= thresholds.max_expired_leakage_rate,
        max_scope_leakage_rate: raw.scope_leakage_rate <= thresholds.max_scope_leakage_rate,
        max_forbidden_hit_rate: raw.forbidden_hit_rate <= thresholds.max_forbidden_hit_rate,
        min_citation_integrity: raw.citation_integrity >= thresholds.min_citation_integrity,
        min_provenance_integrity: raw.provenance_integrity >= thresholds.min_provenance_integrity,
        min_case_pass_rate: raw.case_pass_rate >= thresholds.min_case_pass_rate,
        max_estimated_usage: thresholds
            .max_estimated_usage
            .map(|maximum| raw.max_estimated_usage <= maximum),
        max_p95_latency_ms: thresholds
            .max_p95_latency_ms
            .map(|maximum| raw.p95_latency_ms <= maximum),
    }
}

fn metric_definitions() -> RecallEvalMetricDefinitions {
    RecallEvalMetricDefinitions {
        version: RECALL_EVAL_METRIC_DEFINITIONS_VERSION.to_owned(),
        search_recall_at_k: "mean over search cases of relevant IDs retrieved in the first k divided by declared relevant IDs".to_owned(),
        search_mrr: "mean over search cases of reciprocal rank of the first declared relevant ID, or zero when absent".to_owned(),
        precheck_precision: "micro TP / (TP + FP) across unique precheck result IDs".to_owned(),
        precheck_recall: "micro TP / (TP + FN) across declared precheck relevant IDs".to_owned(),
        leakage_rate: "category hits / declared category opportunities; expired is evaluated separately from other inactive states".to_owned(),
        forbidden_hit_rate: "all categorized forbidden hits / all declared forbidden opportunities".to_owned(),
        citation_integrity: "returned items with non-empty structurally matching citations / returned items checked".to_owned(),
        provenance_integrity: "returned items whose storage plane and evidence source match the record, with proposal lineage kept separate / returned items checked".to_owned(),
        token_usage: "deterministic ContextPack budget estimate reported in approx_words, not provider tokens".to_owned(),
        latency_percentiles: "nearest-rank p50 and p95 over one monotonic wall-time sample per case, including isolated audit writes".to_owned(),
        empty_denominator: "precision, recall, and integrity use 1.0 for an empty denominator; leakage uses 0.0".to_owned(),
    }
}

fn runtime_metadata(latency_sample_count: usize) -> RecallEvalRuntimeMetadata {
    RecallEvalRuntimeMetadata {
        memzoi_version: env!("CARGO_PKG_VERSION").to_owned(),
        target_os: std::env::consts::OS.to_owned(),
        target_arch: std::env::consts::ARCH.to_owned(),
        build_profile: if cfg!(debug_assertions) {
            "debug".to_owned()
        } else {
            "release".to_owned()
        },
        sqlite_version: rusqlite::version().to_owned(),
        timer: "std::time::Instant".to_owned(),
        isolated_state: true,
        network_required: false,
        token_estimator: "context_pack/approx_words-v1".to_owned(),
        latency_sample_count,
    }
}

fn baseline_from_report(report: &RecallEvalReport) -> RecallEvalBaseline {
    RecallEvalBaseline {
        version: RECALL_EVAL_BASELINE_VERSION.to_owned(),
        report_version: report.version.clone(),
        definitions_version: report.definitions.version.clone(),
        corpus: RecallEvalBaselineCorpus {
            name: report.corpus.name.clone(),
            version: report.corpus.version.clone(),
            digest: report.corpus.digest.clone(),
        },
        metrics: baseline_metrics(report),
        cases: report
            .cases
            .iter()
            .map(|case| RecallEvalBaselineCase {
                id: case.id.clone(),
                surface: case.surface,
                retrieved_ids: case.retrieved_ids.clone(),
                estimated_usage: case.estimated_usage,
                passed: case.passed,
            })
            .collect(),
    }
}

fn baseline_metrics(report: &RecallEvalReport) -> RecallEvalBaselineMetrics {
    RecallEvalBaselineMetrics {
        mean_recall_at_k: report.metrics.search.mean_recall_at_k,
        mean_mrr: report.metrics.search.mean_mrr,
        precheck_precision: report.metrics.precheck.precision.value,
        precheck_recall: report.metrics.precheck.recall.value,
        stale_leakage_rate: report.metrics.leakage.stale.rate,
        expired_leakage_rate: report.metrics.leakage.expired.rate,
        scope_leakage_rate: report.metrics.leakage.scope.rate,
        prohibited_leakage_rate: report.metrics.leakage.prohibited.rate,
        destination_leakage_rate: report.metrics.leakage.destination.rate,
        forbidden_hit_rate: report.metrics.leakage.forbidden.rate,
        citation_integrity: report.metrics.citation_integrity.rate,
        provenance_integrity: report.metrics.provenance_integrity.rate,
        case_pass_rate: report.metrics.case_pass_rate.value,
        estimated_usage_total: report.metrics.token_usage.total,
        estimated_usage_max: report.metrics.token_usage.max,
    }
}

fn compare_baseline(
    report: &RecallEvalReport,
    baseline: &RecallEvalBaseline,
) -> RecallEvalBaselineComparison {
    let compatible = baseline.version == RECALL_EVAL_BASELINE_VERSION
        && baseline.report_version == report.version
        && baseline.definitions_version == report.definitions.version
        && baseline.corpus.name == report.corpus.name
        && baseline.corpus.version == report.corpus.version
        && baseline.corpus.digest == report.corpus.digest;
    if !compatible {
        return RecallEvalBaselineComparison {
            version: RECALL_EVAL_BASELINE_VERSION.to_owned(),
            status: RecallEvalBaselineStatus::Incompatible,
            compatible: false,
            deterministic_match: false,
            metric_deltas: Vec::new(),
            changed_cases: Vec::new(),
        };
    }

    let current_metrics = baseline_metrics(report);
    let metric_deltas = metric_deltas(&baseline.metrics, &current_metrics);
    let current_cases = report
        .cases
        .iter()
        .map(|case| RecallEvalBaselineCase {
            id: case.id.clone(),
            surface: case.surface,
            retrieved_ids: case.retrieved_ids.clone(),
            estimated_usage: case.estimated_usage,
            passed: case.passed,
        })
        .collect::<Vec<_>>();
    let baseline_cases = baseline
        .cases
        .iter()
        .map(|case| (case.id.as_str(), case))
        .collect::<BTreeMap<_, _>>();
    let current_by_id = current_cases
        .iter()
        .map(|case| (case.id.as_str(), case))
        .collect::<BTreeMap<_, _>>();
    let all_ids = baseline_cases
        .keys()
        .chain(current_by_id.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let changed_cases = all_ids
        .into_iter()
        .filter(|id| baseline_cases.get(id) != current_by_id.get(id))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let deterministic_match =
        metric_deltas.iter().all(|delta| delta.delta == 0.0) && changed_cases.is_empty();
    RecallEvalBaselineComparison {
        version: RECALL_EVAL_BASELINE_VERSION.to_owned(),
        status: if deterministic_match {
            RecallEvalBaselineStatus::Match
        } else {
            RecallEvalBaselineStatus::Changed
        },
        compatible: true,
        deterministic_match,
        metric_deltas,
        changed_cases,
    }
}

fn metric_deltas(
    baseline: &RecallEvalBaselineMetrics,
    current: &RecallEvalBaselineMetrics,
) -> Vec<RecallEvalMetricDelta> {
    let pairs = [
        (
            "mean_recall_at_k",
            baseline.mean_recall_at_k,
            current.mean_recall_at_k,
        ),
        ("mean_mrr", baseline.mean_mrr, current.mean_mrr),
        (
            "precheck_precision",
            baseline.precheck_precision,
            current.precheck_precision,
        ),
        (
            "precheck_recall",
            baseline.precheck_recall,
            current.precheck_recall,
        ),
        (
            "stale_leakage_rate",
            baseline.stale_leakage_rate,
            current.stale_leakage_rate,
        ),
        (
            "expired_leakage_rate",
            baseline.expired_leakage_rate,
            current.expired_leakage_rate,
        ),
        (
            "scope_leakage_rate",
            baseline.scope_leakage_rate,
            current.scope_leakage_rate,
        ),
        (
            "prohibited_leakage_rate",
            baseline.prohibited_leakage_rate,
            current.prohibited_leakage_rate,
        ),
        (
            "destination_leakage_rate",
            baseline.destination_leakage_rate,
            current.destination_leakage_rate,
        ),
        (
            "forbidden_hit_rate",
            baseline.forbidden_hit_rate,
            current.forbidden_hit_rate,
        ),
        (
            "citation_integrity",
            baseline.citation_integrity,
            current.citation_integrity,
        ),
        (
            "provenance_integrity",
            baseline.provenance_integrity,
            current.provenance_integrity,
        ),
        (
            "case_pass_rate",
            baseline.case_pass_rate,
            current.case_pass_rate,
        ),
        (
            "estimated_usage_total",
            baseline.estimated_usage_total as f64,
            current.estimated_usage_total as f64,
        ),
        (
            "estimated_usage_max",
            baseline.estimated_usage_max as f64,
            current.estimated_usage_max as f64,
        ),
    ];
    pairs
        .into_iter()
        .map(|(metric, baseline, current)| RecallEvalMetricDelta {
            metric: metric.to_owned(),
            baseline,
            current,
            delta: rounded(current - baseline, 6),
        })
        .collect()
}

fn case_id(case: &RecallEvalCase) -> &str {
    match case {
        RecallEvalCase::Search { id, .. }
        | RecallEvalCase::Precheck { id, .. }
        | RecallEvalCase::Context { id, .. }
        | RecallEvalCase::WriteGate { id, .. } => id,
    }
}

fn case_surface(case: &RecallEvalCase) -> RecallEvalSurface {
    match case {
        RecallEvalCase::Search { .. } => RecallEvalSurface::Search,
        RecallEvalCase::Precheck { .. } => RecallEvalSurface::Precheck,
        RecallEvalCase::Context { .. } => RecallEvalSurface::Context,
        RecallEvalCase::WriteGate { .. } => RecallEvalSurface::WriteGate,
    }
}

fn surface_label(surface: RecallEvalSurface) -> &'static str {
    match surface {
        RecallEvalSurface::Search => "search",
        RecallEvalSurface::Precheck => "precheck",
        RecallEvalSurface::Context => "context",
        RecallEvalSurface::WriteGate => "write_gate",
    }
}

fn case_relevant_ids(case: &RecallEvalCase) -> &[String] {
    match case {
        RecallEvalCase::Search { relevant_ids, .. }
        | RecallEvalCase::Precheck { relevant_ids, .. }
        | RecallEvalCase::Context { relevant_ids, .. } => relevant_ids,
        RecallEvalCase::WriteGate { .. } => &[],
    }
}

fn case_forbidden(case: &RecallEvalCase) -> &RecallEvalForbiddenIds {
    static EMPTY: RecallEvalForbiddenIds = RecallEvalForbiddenIds {
        stale: Vec::new(),
        expired: Vec::new(),
        scope: Vec::new(),
        prohibited: Vec::new(),
        destination: Vec::new(),
        other: Vec::new(),
    };
    match case {
        RecallEvalCase::Search { forbidden, .. }
        | RecallEvalCase::Precheck { forbidden, .. }
        | RecallEvalCase::Context { forbidden, .. } => forbidden,
        RecallEvalCase::WriteGate { .. } => &EMPTY,
    }
}

fn all_unique(ids: &[String]) -> bool {
    ids.iter().collect::<BTreeSet<_>>().len() == ids.len()
}

fn rounded(value: f64, digits: i32) -> f64 {
    let factor = 10_f64.powi(digits);
    (value * factor).round() / factor
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::TempDir;

    use super::{
        RECALL_EVAL_CORPUS_VERSION, RawMetrics, RecallEvalCorpus, RecallEvalForbiddenIds,
        RecallEvalSurface, RecallEvalThresholds, evaluate_thresholds, nearest_rank_index,
        stage_proposal_fixture, validate_corpus,
    };

    #[test]
    fn v2_corpus_rejects_unknown_fields() {
        let yaml = format!(
            r#"
version: {RECALL_EVAL_CORPUS_VERSION}
name: invalid
evaluated_at: 2026-07-10T12:00:00Z
records: [one.md]
proposal_fixtures: []
runtime_fixtures: []
thresholds:
  min_mean_recall_at_k: 1.0
  min_mean_mrr: 1.0
  min_precheck_precision: 1.0
  min_precheck_recall: 1.0
  max_stale_leakage_rate: 0.0
  max_expired_leakage_rate: 0.0
  max_scope_leakage_rate: 0.0
  max_forbidden_hit_rate: 0.0
  min_citation_integrity: 1.0
  min_provenance_integrity: 1.0
  min_case_pass_rate: 1.0
cases: []
unexpected: true
"#
        );
        let error = serde_yaml::from_str::<RecallEvalCorpus>(&yaml)
            .expect_err("unknown corpus fields must be rejected");
        assert!(error.to_string().contains("unknown field `unexpected`"));
    }

    #[test]
    fn forbidden_categories_are_disjoint() {
        let forbidden = RecallEvalForbiddenIds {
            stale: vec!["same".to_owned()],
            expired: vec!["same".to_owned()],
            ..RecallEvalForbiddenIds::default()
        };
        assert_eq!(forbidden.all().len(), 1);
        assert_eq!(forbidden.count(), 2);
    }

    #[test]
    fn nearest_rank_percentile_is_stable() {
        assert_eq!(nearest_rank_index(1, 0.50), 0);
        assert_eq!(nearest_rank_index(4, 0.50), 1);
        assert_eq!(nearest_rank_index(4, 0.95), 3);
    }

    #[test]
    fn proposal_fixtures_preserve_relative_subdirectories() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let pending_root = temp.path().join("pending");

        let first = stage_proposal_fixture(
            &pending_root,
            Path::new("alpha/shared.md"),
            b"alpha fixture",
        )?;
        let second =
            stage_proposal_fixture(&pending_root, Path::new("beta/shared.md"), b"beta fixture")?;

        assert_eq!(first, pending_root.join("alpha/shared.md"));
        assert_eq!(second, pending_root.join("beta/shared.md"));
        assert_eq!(fs::read(&first)?, b"alpha fixture");
        assert_eq!(fs::read(second)?, b"beta fixture");
        let collision = stage_proposal_fixture(
            &pending_root,
            Path::new("alpha/shared.md"),
            b"replacement fixture",
        )
        .expect_err("fixture staging must not replace an existing destination");
        assert!(collision.to_string().contains("failed to stage"));
        assert_eq!(fs::read(first)?, b"alpha fixture");
        Ok(())
    }

    #[test]
    fn thresholds_use_unrounded_metric_values() {
        let thresholds = RecallEvalThresholds {
            min_mean_recall_at_k: 1.0,
            min_mean_mrr: 1.0,
            min_precheck_precision: 1.0,
            min_precheck_recall: 1.0,
            max_stale_leakage_rate: 0.0,
            max_expired_leakage_rate: 0.0,
            max_scope_leakage_rate: 0.0,
            max_forbidden_hit_rate: 0.0,
            min_citation_integrity: 1.0,
            min_provenance_integrity: 1.0,
            min_case_pass_rate: 1.0,
            max_estimated_usage: None,
            max_p95_latency_ms: None,
        };
        let raw = RawMetrics {
            mean_recall_at_k: 1.0,
            mean_mrr: 1.0,
            precheck_precision: 0.999_999_6,
            precheck_recall: 1.0,
            stale_leakage_rate: 0.0,
            expired_leakage_rate: 0.0,
            scope_leakage_rate: 0.0,
            forbidden_hit_rate: 0.0,
            citation_integrity: 1.0,
            provenance_integrity: 1.0,
            case_pass_rate: 1.0,
            max_estimated_usage: 0,
            p95_latency_ms: 0.0,
        };

        let results = evaluate_thresholds(&thresholds, &raw);
        assert!(!results.min_precheck_precision);
    }

    #[test]
    fn representative_corpus_is_valid() -> anyhow::Result<()> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../evals/recall/v2/corpus.yaml");
        if !path.exists() {
            return Ok(());
        }
        let corpus: RecallEvalCorpus = serde_yaml::from_str(&std::fs::read_to_string(path)?)?;
        validate_corpus(&corpus)?;
        assert!(
            corpus
                .cases
                .iter()
                .any(|case| super::case_surface(case) == RecallEvalSurface::WriteGate)
        );
        Ok(())
    }
}
