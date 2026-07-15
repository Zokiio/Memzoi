use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use crate::{
    CheckpointInput, FixedClock, InitRequest, LocalMemoryInput, MemoryCitation, MemoryDestination,
    MemoryLane, MemoryPaths, MemoryPlane, MemoryRecord, MemoryService, MemoryType, ScopeKind,
    SearchInput, search::SEARCH_RESULT_LIMIT_MAX,
};

pub const RECALL_V3_CORPUS_VERSION: &str = "memzoi-recall-corpus/v3";
pub const RECALL_V3_REPORT_VERSION: &str = "memzoi-recall-report/v3";
pub const RECALL_V3_METRICS_VERSION: &str = "memzoi-recall-metrics/v3";
pub const RECALL_V3_RUNNER_VERSION: &str = "memzoi-recall-runner/v3";
pub const RECALL_V3_COMMITMENT_VERSION: &str = "memzoi-recall-commitment/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallV3CorpusKind {
    Development,
    LockedTest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallV3ForbiddenReason {
    Stale,
    Expired,
    Scope,
    Path,
    Destination,
    Private,
    Prohibited,
    Tombstoned,
    Superseded,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallV3CaseProvenance {
    pub kind: RecallV3CaseProvenanceKind,
    pub reference: String,
    pub authoring_model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallV3CaseProvenanceKind {
    HumanWritten,
    RealFailureDerived,
    Generated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallV3Judgment {
    pub record_id: String,
    pub relevance: u8,
    pub eligible: bool,
    pub forbidden_reason: Option<RecallV3ForbiddenReason>,
    #[serde(default)]
    pub expected_citations: Vec<String>,
    #[serde(default)]
    pub hard_negative: bool,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallV3Case {
    pub id: String,
    pub query: String,
    pub slices: Vec<String>,
    pub provenance: RecallV3CaseProvenance,
    pub path: Option<String>,
    pub scope_kind: Option<ScopeKind>,
    pub scope_id: Option<String>,
    pub top_k: usize,
    pub context_budget: usize,
    pub judgments: Vec<RecallV3Judgment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallV3Thresholds {
    pub min_mean_ndcg_at_10: f64,
    pub min_mean_recall_at_k: f64,
    pub min_mean_mrr: f64,
    pub max_forbidden_hits: usize,
    pub min_citation_integrity: f64,
    pub min_fallback_parity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallV3Corpus {
    pub version: String,
    pub name: String,
    pub kind: RecallV3CorpusKind,
    pub evaluated_at: String,
    #[serde(default = "default_records_root")]
    pub records_root: PathBuf,
    pub records: Vec<PathBuf>,
    #[serde(default)]
    pub runtime_fixtures: Vec<RecallV3RuntimeFixture>,
    pub thresholds: RecallV3Thresholds,
    pub cases: Vec<RecallV3Case>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallV3RuntimeFixture {
    pub id: String,
    pub destination: MemoryDestination,
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallV3CandidateManifest {
    pub id: String,
    pub version: String,
    pub adapter: String,
    pub configuration_digest: String,
    pub offline: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallV3CandidateInput {
    pub case_id: String,
    pub query: String,
    pub path: Option<String>,
    pub scope_kind: Option<ScopeKind>,
    pub scope_id: Option<String>,
    pub top_k: usize,
    pub context_budget: usize,
    pub eligible_records: Vec<RecallV3CandidateRecord>,
    pub lexical_hits: Vec<RecallV3CandidateHit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallV3CandidateRecord {
    pub id: String,
    pub title: String,
    pub body: String,
    pub citation: MemoryCitation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallV3CandidateHit {
    pub record_id: String,
    pub score: f64,
    #[serde(default)]
    pub citations: Vec<MemoryCitation>,
    #[serde(default)]
    pub signals: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallV3CandidateOutput {
    pub hits: Vec<RecallV3CandidateHit>,
    pub fallback_reason: Option<String>,
    #[serde(default)]
    pub resource_observations: BTreeMap<String, f64>,
}

pub trait RecallV3Candidate {
    fn manifest(&self) -> RecallV3CandidateManifest;
    fn retrieve(&mut self, input: &RecallV3CandidateInput) -> Result<RecallV3CandidateOutput>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallV3Report {
    pub version: String,
    pub corpus: RecallV3CorpusMetadata,
    pub digests: RecallV3Digests,
    pub commitment: RecallV3Commitment,
    pub candidates: Vec<RecallV3CandidateReport>,
    pub isolated_state: bool,
    pub network_required: bool,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallV3CorpusMetadata {
    pub name: String,
    pub kind: RecallV3CorpusKind,
    pub version: String,
    pub evaluated_at: String,
    pub case_count: usize,
    pub record_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallV3Digests {
    pub corpus: String,
    pub judgments: String,
    pub metrics: String,
    pub runner: String,
    pub candidates: BTreeMap<String, String>,
    pub report: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallV3Commitment {
    pub version: String,
    pub corpus_kind: RecallV3CorpusKind,
    pub corpus_digest: String,
    pub judgment_digest: String,
    pub metrics_digest: String,
    pub runner_digest: String,
    pub candidate_digests: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallV3CandidateReport {
    pub manifest: RecallV3CandidateManifest,
    pub manifest_digest: String,
    pub cases: Vec<RecallV3CaseReport>,
    pub aggregate: RecallV3AggregateMetrics,
    pub per_slice: BTreeMap<String, RecallV3SliceMetrics>,
    pub paired_vs_lexical: RecallV3PairedComparison,
    pub resource_observations: BTreeMap<String, f64>,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallV3CaseReport {
    pub id: String,
    pub slices: Vec<String>,
    pub retrieved_ids: Vec<String>,
    pub suppressed_forbidden_ids: Vec<String>,
    pub ndcg_at_10: f64,
    pub recall_at_k: f64,
    pub mrr: f64,
    pub hard_negative_hits: usize,
    pub forbidden_hits: BTreeMap<RecallV3ForbiddenReason, usize>,
    pub citation_integrity: f64,
    pub fallback_reason: Option<String>,
    pub fallback_parity: f64,
    pub estimated_context_words: usize,
    pub latency_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallV3AggregateMetrics {
    pub mean_ndcg_at_10: f64,
    pub mean_recall_at_k: f64,
    pub mean_mrr: f64,
    pub hard_negative_hits: usize,
    pub forbidden_hits: BTreeMap<RecallV3ForbiddenReason, usize>,
    pub citation_integrity: f64,
    pub fallback_parity: f64,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallV3SliceMetrics {
    pub case_count: usize,
    pub mean_ndcg_at_10: f64,
    pub mean_recall_at_k: f64,
    pub mean_mrr: f64,
    pub forbidden_hits: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallV3PairedComparison {
    pub baseline_candidate_id: String,
    pub mean_ndcg_delta: f64,
    pub confidence_level: f64,
    pub lower_bound: f64,
    pub upper_bound: f64,
    pub method: String,
    pub samples: usize,
    pub seed: u64,
}

pub fn run_recall_v3_eval(path: impl AsRef<Path>) -> Result<RecallV3Report> {
    let loaded = load_corpus(path.as_ref())?;
    let temp = TempDir::new().context("failed to create isolated recall-v3 state")?;
    let paths =
        MemoryPaths::with_runtime_home(temp.path().join("project"), temp.path().join("runtime"));
    MemoryService::initialize_paths(paths.clone(), InitRequest { force: true })?;
    stage_records(&loaded.root, &loaded.corpus, &paths.records_dir())?;
    MemoryService::rebuild_paths_for_trusted_recall_eval(paths.clone())?;
    let clock = FixedClock::from_rfc3339(&loaded.corpus.evaluated_at)?;
    let service = MemoryService::open_paths_with_clock(paths.clone(), clock)?;
    let runtime_records = seed_runtime_fixtures(&service, &loaded.corpus.runtime_fixtures)?;
    let record_files = crate::read_okf_record_files(paths.records_dir())?;
    let record_paths = recall_record_paths(&record_files);
    let mut records = record_files
        .iter()
        .map(crate::okf::project_okf_record)
        .collect::<Vec<_>>();
    records.extend(runtime_records);
    validate_record_ids(&loaded.corpus, &records)?;
    let mut lexical = LexicalCandidate {
        service: &service,
        record_count: records.len(),
    };
    run_loaded(loaded, &records, &record_paths, &mut lexical, &mut [])
}

pub fn load_recall_v3_embedding_corpus(
    path: impl AsRef<Path>,
) -> Result<crate::RecallEmbeddingCorpus> {
    let loaded = load_corpus(path.as_ref())?;
    if loaded.corpus.kind != RecallV3CorpusKind::Development {
        bail!("embedding artifact builds require a development corpus");
    }
    let staged = TempDir::new().context("failed to stage declared embedding records")?;
    stage_records(&loaded.root, &loaded.corpus, staged.path())?;
    let record_files = crate::read_okf_record_files(staged.path())?;
    let record_paths = recall_record_paths(&record_files);
    let records = record_files
        .iter()
        .map(crate::okf::project_okf_record)
        .map(|record| {
            let path = record_paths
                .get(&record.id)
                .and_then(|paths| paths.first())
                .cloned();
            (
                record.id.clone(),
                RecallV3CandidateRecord {
                    id: record.id.clone(),
                    title: record.title.clone(),
                    body: record.body.clone(),
                    citation: MemoryCitation {
                        record_id: record.id,
                        memory_type: record.memory_type,
                        scope_kind: record.scope_kind,
                        provenance: MemoryPlane::Git,
                        destination: record.destination,
                        visibility: record.visibility,
                        source_kind: record.source_kind,
                        source_ref: record.source_ref,
                        path,
                        capture: record.capture,
                    },
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let queries = loaded
        .corpus
        .cases
        .iter()
        .map(|case| (case.id.clone(), case.query.clone()))
        .collect();
    Ok(crate::RecallEmbeddingCorpus {
        inputs: crate::RecallEmbeddingInputs { queries, records },
        corpus_digest: loaded.corpus_digest,
        judgment_digest: loaded.judgment_digest,
    })
}

pub fn run_recall_v3_eval_with_candidates(
    path: impl AsRef<Path>,
    candidates: &mut [&mut dyn RecallV3Candidate],
) -> Result<RecallV3Report> {
    let loaded = load_corpus(path.as_ref())?;
    let temp = TempDir::new().context("failed to create isolated recall-v3 state")?;
    let paths =
        MemoryPaths::with_runtime_home(temp.path().join("project"), temp.path().join("runtime"));
    MemoryService::initialize_paths(paths.clone(), InitRequest { force: true })?;
    stage_records(&loaded.root, &loaded.corpus, &paths.records_dir())?;
    MemoryService::rebuild_paths_for_trusted_recall_eval(paths.clone())?;
    let clock = FixedClock::from_rfc3339(&loaded.corpus.evaluated_at)?;
    let service = MemoryService::open_paths_with_clock(paths.clone(), clock)?;
    let runtime_records = seed_runtime_fixtures(&service, &loaded.corpus.runtime_fixtures)?;
    let record_files = crate::read_okf_record_files(paths.records_dir())?;
    let record_paths = recall_record_paths(&record_files);
    let mut records = record_files
        .iter()
        .map(crate::okf::project_okf_record)
        .collect::<Vec<_>>();
    records.extend(runtime_records);
    validate_record_ids(&loaded.corpus, &records)?;
    let mut lexical = LexicalCandidate {
        service: &service,
        record_count: records.len(),
    };
    run_loaded(loaded, &records, &record_paths, &mut lexical, candidates)
}

/// Prepares the immutable identity for a local locked-test bundle without
/// executing retrieval. The caller can write this value before a locked run
/// and verify it immediately before evaluation.
pub fn prepare_recall_v3_locked_commitment(
    path: impl AsRef<Path>,
    candidates: &[RecallV3CandidateManifest],
) -> Result<RecallV3Commitment> {
    let loaded = load_corpus(path.as_ref())?;
    if loaded.corpus.kind != RecallV3CorpusKind::LockedTest {
        bail!("locked commitment preparation requires a locked_test corpus");
    }
    let mut manifests = vec![recall_v3_lexical_candidate_manifest()];
    manifests.extend_from_slice(candidates);
    let candidate_digests = candidate_digests(&manifests)?;
    Ok(commitment_for(&loaded, candidate_digests))
}

/// Verifies that a local locked-test corpus and its frozen candidate manifests
/// still match a previously prepared commitment before evaluation begins.
pub fn verify_recall_v3_locked_commitment(
    corpus: impl AsRef<Path>,
    commitment_path: impl AsRef<Path>,
    candidates: &[RecallV3CandidateManifest],
) -> Result<RecallV3Commitment> {
    let actual = prepare_recall_v3_locked_commitment(corpus, candidates)?;
    let bytes = fs::read(commitment_path.as_ref()).with_context(|| {
        format!(
            "failed to read locked recall commitment {}",
            commitment_path.as_ref().display()
        )
    })?;
    let expected: RecallV3Commitment = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "failed to parse locked recall commitment {}",
            commitment_path.as_ref().display()
        )
    })?;
    if expected.version != RECALL_V3_COMMITMENT_VERSION {
        bail!(
            "unsupported locked recall commitment version {:?}",
            expected.version
        );
    }
    if actual != expected {
        bail!("locked recall commitment does not match the corpus or candidate manifests");
    }
    Ok(actual)
}

/// Fails when a non-lexical candidate did not execute for every evaluated case.
/// Production fallback remains valid; fixture and locked evaluation runs use
/// this gate to prove that the candidate itself was actually exercised.
pub fn require_recall_v3_candidates_ready(report: &RecallV3Report) -> Result<()> {
    for candidate in report.candidates.iter().skip(1) {
        if let Some(case) = candidate
            .cases
            .iter()
            .find(|case| case.fallback_reason.is_some())
        {
            let reason = case
                .fallback_reason
                .as_deref()
                .expect("fallback case has a fallback reason");
            bail!(
                "recall candidate {:?} fell back for case {:?}: {}",
                candidate.manifest.id,
                case.id,
                reason
            );
        }
    }
    Ok(())
}

fn recall_record_paths(records: &[crate::okf::OkfRecordFile]) -> BTreeMap<String, Vec<String>> {
    records
        .iter()
        .map(|record| {
            let mut paths = record.applies_to.clone();
            paths.sort();
            (record.concept_id.clone(), paths)
        })
        .collect()
}

fn recall_citation_path(paths: &[String], requested: Option<&str>) -> Option<String> {
    requested
        .and_then(|requested| {
            paths
                .iter()
                .find(|stored| crate::search::path_matches_request(stored, requested))
        })
        .or_else(|| paths.first())
        .cloned()
}

pub fn write_recall_v3_commitment(report: &RecallV3Report, path: impl AsRef<Path>) -> Result<()> {
    write_recall_v3_locked_commitment(&report.commitment, path)
}

/// Writes a prepared or verified local locked-test commitment artifact.
pub fn write_recall_v3_locked_commitment(
    commitment: &RecallV3Commitment,
    path: impl AsRef<Path>,
) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(commitment)?;
    fs::write(path.as_ref(), bytes)
        .with_context(|| format!("failed to write {}", path.as_ref().display()))
}

struct LoadedV3Corpus {
    corpus: RecallV3Corpus,
    root: PathBuf,
    corpus_digest: String,
    judgment_digest: String,
}

fn load_corpus(path: &Path) -> Result<LoadedV3Corpus> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let corpus: RecallV3Corpus = serde_yaml::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    validate_corpus(&corpus)?;
    let root = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut hasher = blake3::Hasher::new();
    hasher.update(&bytes);
    for record in &corpus.records {
        hasher.update(record.to_string_lossy().as_bytes());
        hasher.update(&fs::read(root.join(&corpus.records_root).join(record))?);
    }
    let corpus_digest = hasher.finalize().to_hex().to_string();
    let judgments = corpus
        .cases
        .iter()
        .map(|case| (&case.id, &case.judgments))
        .collect::<Vec<_>>();
    let judgment_digest = digest_json(&judgments)?;
    Ok(LoadedV3Corpus {
        corpus,
        root,
        corpus_digest,
        judgment_digest,
    })
}

fn validate_corpus(corpus: &RecallV3Corpus) -> Result<()> {
    if corpus.version != RECALL_V3_CORPUS_VERSION {
        bail!("unsupported recall-v3 corpus version {:?}", corpus.version);
    }
    if corpus.name.trim().is_empty() || corpus.cases.is_empty() || corpus.records.is_empty() {
        bail!("recall-v3 name, records, and cases must be non-empty");
    }
    FixedClock::from_rfc3339(&corpus.evaluated_at)?;
    for value in [
        corpus.thresholds.min_mean_ndcg_at_10,
        corpus.thresholds.min_mean_recall_at_k,
        corpus.thresholds.min_mean_mrr,
        corpus.thresholds.min_citation_integrity,
        corpus.thresholds.min_fallback_parity,
    ] {
        if !(0.0..=1.0).contains(&value) {
            bail!("recall-v3 ratio thresholds must be between zero and one");
        }
    }
    let mut ids = BTreeSet::new();
    for case in &corpus.cases {
        if !ids.insert(&case.id)
            || case.id.trim().is_empty()
            || case.query.trim().is_empty()
            || case.slices.is_empty()
            || case.context_budget == 0
            || case.judgments.is_empty()
        {
            bail!("invalid or duplicate recall-v3 case {:?}", case.id);
        }
        if !(1..=SEARCH_RESULT_LIMIT_MAX).contains(&case.top_k) {
            bail!(
                "case {:?} top_k must be between 1 and {SEARCH_RESULT_LIMIT_MAX}",
                case.id
            );
        }
        if case.provenance.reference.trim().is_empty() {
            bail!("case {:?} provenance reference is empty", case.id);
        }
        let mut slices = BTreeSet::new();
        if case
            .slices
            .iter()
            .any(|slice| slice.trim().is_empty() || !slices.insert(slice))
        {
            bail!("case {:?} has empty or duplicate slices", case.id);
        }
        let mut judged = BTreeSet::new();
        for judgment in &case.judgments {
            if judgment.record_id.trim().is_empty()
                || !judged.insert(&judgment.record_id)
                || judgment.relevance > 3
                || judgment.rationale.trim().is_empty()
            {
                bail!("invalid judgment in case {:?}", case.id);
            }
            if judgment.eligible == judgment.forbidden_reason.is_some() {
                bail!(
                    "case {:?} judgment {:?} must set forbidden_reason exactly when ineligible",
                    case.id,
                    judgment.record_id
                );
            }
            if !judgment.eligible && judgment.relevance > 0 {
                bail!(
                    "ineligible judgment {:?} in case {:?} cannot be relevant",
                    judgment.record_id,
                    case.id
                );
            }
        }
        if !case
            .judgments
            .iter()
            .any(|judgment| judgment.eligible && judgment.relevance > 0)
        {
            bail!(
                "case {:?} must have at least one eligible relevant judgment",
                case.id
            );
        }
    }
    validate_relative_path(&corpus.records_root)?;
    let mut record_paths = BTreeSet::new();
    for path in &corpus.records {
        validate_relative_path(path)?;
        if !record_paths.insert(path) {
            bail!("duplicate recall-v3 record path {}", path.display());
        }
    }
    let mut runtime_ids = BTreeSet::new();
    for fixture in &corpus.runtime_fixtures {
        if fixture.id.trim().is_empty()
            || !runtime_ids.insert(&fixture.id)
            || !matches!(
                fixture.destination,
                MemoryDestination::Local | MemoryDestination::Session
            )
            || fixture.title.trim().is_empty()
            || fixture.body.trim().is_empty()
        {
            bail!("invalid recall-v3 runtime fixture {:?}", fixture.id);
        }
    }
    Ok(())
}

fn seed_runtime_fixtures(
    service: &MemoryService,
    fixtures: &[RecallV3RuntimeFixture],
) -> Result<Vec<MemoryRecord>> {
    fixtures
        .iter()
        .map(|fixture| {
            let record = match fixture.destination {
                MemoryDestination::Local => service.create_local_memory(
                    "memzoi-recall-v3-eval",
                    LocalMemoryInput {
                        memory_type: fixture.memory_type,
                        lane: fixture.lane,
                        title: fixture.title.clone(),
                        body: fixture.body.clone(),
                    },
                )?,
                MemoryDestination::Session => service.create_checkpoint(
                    "memzoi-recall-v3-eval",
                    CheckpointInput {
                        task: fixture.title.clone(),
                        note: fixture.body.clone(),
                    },
                )?,
                _ => unreachable!("runtime fixture destination was validated"),
            };
            if record.id != fixture.id {
                bail!(
                    "runtime fixture {:?} produced unexpected id {:?}",
                    fixture.id,
                    record.id
                );
            }
            Ok(record)
        })
        .collect()
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("unsafe recall-v3 fixture path {}", path.display());
    }
    Ok(())
}

fn stage_records(root: &Path, corpus: &RecallV3Corpus, destination: &Path) -> Result<()> {
    for relative in &corpus.records {
        let source = root.join(&corpus.records_root).join(relative);
        let target = destination.join(relative);
        let metadata = fs::symlink_metadata(&source)
            .with_context(|| format!("failed to inspect {}", source.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!(
                "recall-v3 fixture must be a regular non-symlink file: {}",
                source.display()
            );
        }
        fs::create_dir_all(target.parent().context("record path has no parent")?)?;
        fs::copy(&source, &target)
            .with_context(|| format!("failed to stage {}", source.display()))?;
    }
    Ok(())
}

fn validate_record_ids(corpus: &RecallV3Corpus, records: &[MemoryRecord]) -> Result<()> {
    let ids = records
        .iter()
        .map(|r| r.id.as_str())
        .collect::<BTreeSet<_>>();
    for case in &corpus.cases {
        let judged = case
            .judgments
            .iter()
            .map(|judgment| judgment.record_id.as_str())
            .collect::<BTreeSet<_>>();
        if judged != ids {
            let missing = ids.difference(&judged).copied().collect::<Vec<_>>();
            let unknown = judged.difference(&ids).copied().collect::<Vec<_>>();
            bail!(
                "case {:?} must judge every staged record; missing={missing:?} unknown={unknown:?}",
                case.id
            );
        }
        for judgment in &case.judgments {
            if !ids.contains(judgment.record_id.as_str()) {
                bail!(
                    "case {:?} references missing record {:?}",
                    case.id,
                    judgment.record_id
                );
            }
        }
    }
    Ok(())
}

struct LexicalCandidate<'a> {
    service: &'a MemoryService,
    record_count: usize,
}
impl RecallV3Candidate for LexicalCandidate<'_> {
    fn manifest(&self) -> RecallV3CandidateManifest {
        recall_v3_lexical_candidate_manifest()
    }
    fn retrieve(&mut self, input: &RecallV3CandidateInput) -> Result<RecallV3CandidateOutput> {
        if input.eligible_records.is_empty() {
            return Ok(RecallV3CandidateOutput {
                hits: Vec::new(),
                fallback_reason: None,
                resource_observations: BTreeMap::new(),
            });
        }
        let ineligible_count = self
            .record_count
            .saturating_sub(input.eligible_records.len());
        let fetch_limit = lexical_fetch_limit(self.record_count, ineligible_count, input.top_k);
        let results = self.service.search_memory(SearchInput {
            query: input.query.clone(),
            scope_kind: input.scope_kind,
            scope_id: input.scope_id.clone(),
            path_prefix: input.path.clone(),
            limit: fetch_limit,
            ..SearchInput::default()
        })?;
        Ok(RecallV3CandidateOutput {
            hits: results
                .into_iter()
                .map(|result| RecallV3CandidateHit {
                    record_id: result.record.id,
                    score: result.score,
                    citations: result.citations,
                    signals: BTreeMap::new(),
                })
                .collect(),
            fallback_reason: None,
            resource_observations: BTreeMap::new(),
        })
    }
}

pub fn recall_v3_lexical_candidate_manifest() -> RecallV3CandidateManifest {
    RecallV3CandidateManifest {
        id: "lexical-baseline".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        adapter: "production-fts5".into(),
        configuration_digest: digest_bytes(
            b"production SearchInput defaults; exact policy filters; deterministic ties",
        ),
        offline: true,
    }
}

fn lexical_fetch_limit(record_count: usize, ineligible_count: usize, top_k: usize) -> usize {
    top_k.saturating_add(ineligible_count).min(record_count)
}

fn candidate_digests(manifests: &[RecallV3CandidateManifest]) -> Result<BTreeMap<String, String>> {
    let mut digests = BTreeMap::new();
    for manifest in manifests {
        if manifest.id.trim().is_empty()
            || manifest.version.trim().is_empty()
            || manifest.adapter.trim().is_empty()
            || manifest.configuration_digest.trim().is_empty()
        {
            bail!("candidate manifest identity fields must be non-empty");
        }
        if digests
            .insert(manifest.id.clone(), digest_json(manifest)?)
            .is_some()
        {
            bail!("duplicate candidate id {:?}", manifest.id);
        }
    }
    Ok(digests)
}

fn commitment_for(
    loaded: &LoadedV3Corpus,
    candidate_digests: BTreeMap<String, String>,
) -> RecallV3Commitment {
    RecallV3Commitment {
        version: RECALL_V3_COMMITMENT_VERSION.into(),
        corpus_kind: loaded.corpus.kind,
        corpus_digest: loaded.corpus_digest.clone(),
        judgment_digest: loaded.judgment_digest.clone(),
        metrics_digest: recall_v3_metrics_digest(),
        runner_digest: recall_v3_runner_digest(),
        candidate_digests,
    }
}

fn run_loaded(
    loaded: LoadedV3Corpus,
    records: &[MemoryRecord],
    record_paths: &BTreeMap<String, Vec<String>>,
    lexical: &mut dyn RecallV3Candidate,
    candidates: &mut [&mut dyn RecallV3Candidate],
) -> Result<RecallV3Report> {
    let mut manifests = vec![lexical.manifest()];
    manifests.extend(candidates.iter().map(|c| c.manifest()));
    let candidate_digests = candidate_digests(&manifests)?;
    let network_required = manifests.iter().any(|manifest| !manifest.offline);
    let metrics_digest = recall_v3_metrics_digest();
    let runner_digest = recall_v3_runner_digest();
    let mut digests = RecallV3Digests {
        corpus: loaded.corpus_digest.clone(),
        judgments: loaded.judgment_digest.clone(),
        metrics: metrics_digest.clone(),
        runner: runner_digest.clone(),
        candidates: candidate_digests.clone(),
        report: String::new(),
    };
    let mut all_cases = Vec::new();
    let mut all_resources = Vec::new();
    let (reports, resources) =
        evaluate_candidate(&loaded.corpus, records, record_paths, lexical, None)?;
    all_cases.push(reports);
    all_resources.push(resources);
    for candidate in candidates.iter_mut() {
        let (reports, resources) = evaluate_candidate(
            &loaded.corpus,
            records,
            record_paths,
            &mut **candidate,
            Some(lexical),
        )?;
        all_cases.push(reports);
        all_resources.push(resources);
    }
    let lexical_index = 0;
    let lexical_scores = all_cases[lexical_index]
        .iter()
        .map(|c| c.ndcg_at_10)
        .collect::<Vec<_>>();
    let lexical_cases = all_cases[lexical_index].clone();
    for (candidate_index, cases) in all_cases.iter_mut().enumerate() {
        if candidate_index == lexical_index {
            continue;
        }
        for (case, lexical) in cases.iter_mut().zip(&lexical_cases) {
            if case.fallback_reason.is_some() {
                case.fallback_parity = if case.retrieved_ids == lexical.retrieved_ids {
                    1.0
                } else {
                    0.0
                };
            }
        }
    }
    let mut candidate_reports = Vec::new();
    for (index, manifest) in manifests.into_iter().enumerate() {
        let cases = all_cases[index].clone();
        let aggregate = aggregate(&cases);
        let passed = aggregate.mean_ndcg_at_10 >= loaded.corpus.thresholds.min_mean_ndcg_at_10
            && aggregate.mean_recall_at_k >= loaded.corpus.thresholds.min_mean_recall_at_k
            && aggregate.mean_mrr >= loaded.corpus.thresholds.min_mean_mrr
            && aggregate.forbidden_hits.values().sum::<usize>()
                <= loaded.corpus.thresholds.max_forbidden_hits
            && aggregate.citation_integrity >= loaded.corpus.thresholds.min_citation_integrity
            && aggregate.fallback_parity >= loaded.corpus.thresholds.min_fallback_parity;
        candidate_reports.push(RecallV3CandidateReport {
            manifest_digest: candidate_digests[&manifest.id].clone(),
            per_slice: per_slice(&cases),
            paired_vs_lexical: paired_comparison(&cases, &lexical_scores),
            resource_observations: all_resources[index].clone(),
            manifest,
            aggregate,
            cases,
            passed,
        });
    }
    let passed = candidate_reports.iter().all(|candidate| candidate.passed);
    digests.report = stable_report_digest(&candidate_reports)?;
    let commitment = commitment_for(&loaded, candidate_digests);
    Ok(RecallV3Report {
        version: RECALL_V3_REPORT_VERSION.into(),
        corpus: RecallV3CorpusMetadata {
            name: loaded.corpus.name,
            kind: loaded.corpus.kind,
            version: loaded.corpus.version,
            evaluated_at: loaded.corpus.evaluated_at,
            case_count: loaded.corpus.cases.len(),
            record_count: records.len(),
        },
        digests,
        commitment,
        candidates: candidate_reports,
        isolated_state: true,
        network_required,
        passed,
    })
}

fn evaluate_candidate(
    corpus: &RecallV3Corpus,
    records: &[MemoryRecord],
    record_paths: &BTreeMap<String, Vec<String>>,
    candidate: &mut dyn RecallV3Candidate,
    mut lexical: Option<&mut dyn RecallV3Candidate>,
) -> Result<(Vec<RecallV3CaseReport>, BTreeMap<String, f64>)> {
    let mut reports = Vec::new();
    let mut resources = BTreeMap::new();
    for case in &corpus.cases {
        let eligible = case
            .judgments
            .iter()
            .filter(|j| j.eligible)
            .map(|j| j.record_id.as_str())
            .collect::<BTreeSet<_>>();
        let mut input = RecallV3CandidateInput {
            case_id: case.id.clone(),
            query: case.query.clone(),
            path: case.path.clone(),
            scope_kind: case.scope_kind,
            scope_id: case.scope_id.clone(),
            top_k: case.top_k,
            context_budget: case.context_budget,
            eligible_records: records
                .iter()
                .filter(|r| eligible.contains(r.id.as_str()))
                .map(|r| RecallV3CandidateRecord {
                    id: r.id.clone(),
                    title: r.title.clone(),
                    body: r.body.clone(),
                    citation: MemoryCitation {
                        record_id: r.id.clone(),
                        memory_type: r.memory_type,
                        scope_kind: r.scope_kind,
                        provenance: MemoryPlane::Git,
                        destination: r.destination,
                        visibility: r.visibility,
                        source_kind: r.source_kind.clone(),
                        source_ref: r.source_ref.clone(),
                        path: record_paths
                            .get(&r.id)
                            .and_then(|paths| recall_citation_path(paths, case.path.as_deref())),
                        capture: r.capture.clone(),
                    },
                })
                .collect(),
            lexical_hits: Vec::new(),
        };
        if let Some(baseline) = lexical.as_deref_mut() {
            let mut lexical_input = input.clone();
            lexical_input.top_k = SEARCH_RESULT_LIMIT_MAX;
            let lexical_output = baseline.retrieve(&lexical_input)?;
            let eligible_ids = input
                .eligible_records
                .iter()
                .map(|record| record.id.as_str())
                .collect::<BTreeSet<_>>();
            input.lexical_hits = lexical_output
                .hits
                .into_iter()
                .filter(|hit| eligible_ids.contains(hit.record_id.as_str()))
                .collect();
        }
        let started = Instant::now();
        let output = candidate.retrieve(&input)?;
        if output.fallback_reason.as_deref().is_some_and(|reason| {
            reason.is_empty()
                || reason.len() > 64
                || !reason
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        }) {
            bail!("candidate emitted an invalid fallback reason code");
        }
        if output.hits.iter().any(|hit| {
            !hit.score.is_finite() || hit.signals.values().any(|value| !value.is_finite())
        }) || output
            .resource_observations
            .values()
            .any(|value| !value.is_finite())
        {
            bail!(
                "candidate {:?} emitted non-finite observations",
                candidate.manifest().id
            );
        }
        let latency = started.elapsed().as_secs_f64() * 1000.0;
        for (key, value) in output.resource_observations {
            *resources.entry(key).or_insert(0.0) += value;
        }
        reports.push(score_case(
            case,
            records,
            output.hits,
            output.fallback_reason,
            latency,
        ));
    }
    Ok((reports, resources))
}

fn stable_report_digest(candidate_reports: &[RecallV3CandidateReport]) -> Result<String> {
    let mut stable = candidate_reports.to_vec();
    for candidate in &mut stable {
        candidate.resource_observations.clear();
        candidate.aggregate.latency_p50_ms = 0.0;
        candidate.aggregate.latency_p95_ms = 0.0;
        for case in &mut candidate.cases {
            case.latency_ms = 0.0;
        }
    }
    digest_json(&(RECALL_V3_REPORT_VERSION, stable))
}

fn score_case(
    case: &RecallV3Case,
    records: &[MemoryRecord],
    hits: Vec<RecallV3CandidateHit>,
    fallback_reason: Option<String>,
    latency_ms: f64,
) -> RecallV3CaseReport {
    let judgments = case
        .judgments
        .iter()
        .map(|j| (j.record_id.as_str(), j))
        .collect::<BTreeMap<_, _>>();
    let mut suppressed = Vec::new();
    let mut seen = BTreeSet::new();
    let eligible_hits = hits
        .into_iter()
        .filter(|hit| seen.insert(hit.record_id.clone()))
        .filter(|hit| match judgments.get(hit.record_id.as_str()) {
            Some(j) if j.eligible => true,
            _ => {
                suppressed.push(hit.record_id.clone());
                false
            }
        })
        .collect::<Vec<_>>();
    let record_words = records
        .iter()
        .map(|record| {
            (
                record.id.as_str(),
                record.title.split_whitespace().count() + record.body.split_whitespace().count(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut estimated_context_words = 0;
    let accepted = eligible_hits
        .into_iter()
        .filter(|hit| {
            let words = record_words
                .get(hit.record_id.as_str())
                .copied()
                .unwrap_or(0);
            if estimated_context_words + words > case.context_budget {
                false
            } else {
                estimated_context_words += words;
                true
            }
        })
        .take(case.top_k)
        .collect::<Vec<_>>();
    let retrieved_ids = accepted
        .iter()
        .map(|h| h.record_id.clone())
        .collect::<Vec<_>>();
    let relevant = case
        .judgments
        .iter()
        .filter(|j| j.eligible && j.relevance > 0)
        .collect::<Vec<_>>();
    let found = relevant
        .iter()
        .filter(|j| retrieved_ids.contains(&j.record_id))
        .count();
    let recall_at_k = ratio(found, relevant.len());
    let mrr = retrieved_ids
        .iter()
        .position(|id| judgments.get(id.as_str()).is_some_and(|j| j.relevance > 0))
        .map_or(0.0, |i| 1.0 / (i + 1) as f64);
    let dcg = accepted
        .iter()
        .take(10)
        .enumerate()
        .map(|(i, hit)| {
            (2_f64.powi(
                judgments
                    .get(hit.record_id.as_str())
                    .map_or(0, |j| j.relevance) as i32,
            ) - 1.0)
                / ((i + 2) as f64).log2()
        })
        .sum::<f64>();
    let mut grades = relevant.iter().map(|j| j.relevance).collect::<Vec<_>>();
    grades.sort_by(|a, b| b.cmp(a));
    let idcg = grades
        .iter()
        .take(10)
        .enumerate()
        .map(|(i, grade)| (2_f64.powi(*grade as i32) - 1.0) / ((i + 2) as f64).log2())
        .sum::<f64>();
    let citation_checked = accepted
        .iter()
        .filter(|h| {
            judgments
                .get(h.record_id.as_str())
                .is_some_and(|j| !j.expected_citations.is_empty())
        })
        .collect::<Vec<_>>();
    let citation_valid = citation_checked
        .iter()
        .filter(|hit| {
            let expected = &judgments[hit.record_id.as_str()].expected_citations;
            hit.citations.iter().any(|citation| {
                citation
                    .source_ref
                    .as_ref()
                    .is_some_and(|reference| expected.contains(reference))
                    && case.path.as_deref().is_none_or(|requested| {
                        citation.path.as_deref().is_some_and(|stored| {
                            crate::search::path_matches_request(stored, requested)
                        })
                    })
            })
        })
        .count();
    let hard_negative_hits = accepted
        .iter()
        .filter(|h| {
            judgments
                .get(h.record_id.as_str())
                .is_some_and(|j| j.hard_negative)
        })
        .count();
    let mut forbidden_hits = BTreeMap::new();
    for id in &suppressed {
        let reason = judgments
            .get(id.as_str())
            .and_then(|judgment| judgment.forbidden_reason)
            .unwrap_or(RecallV3ForbiddenReason::Other);
        *forbidden_hits.entry(reason).or_insert(0) += 1;
    }
    RecallV3CaseReport {
        id: case.id.clone(),
        slices: case.slices.clone(),
        retrieved_ids,
        suppressed_forbidden_ids: suppressed,
        ndcg_at_10: if idcg == 0.0 { 1.0 } else { dcg / idcg },
        recall_at_k,
        mrr,
        hard_negative_hits,
        forbidden_hits,
        citation_integrity: if citation_checked.is_empty() {
            1.0
        } else {
            ratio(citation_valid, citation_checked.len())
        },
        fallback_reason,
        fallback_parity: 1.0,
        estimated_context_words,
        latency_ms,
    }
}

fn aggregate(cases: &[RecallV3CaseReport]) -> RecallV3AggregateMetrics {
    let mut forbidden = BTreeMap::new();
    for case in cases {
        for (reason, count) in &case.forbidden_hits {
            *forbidden.entry(*reason).or_insert(0) += count;
        }
    }
    let latencies = cases.iter().map(|c| c.latency_ms).collect::<Vec<_>>();
    RecallV3AggregateMetrics {
        mean_ndcg_at_10: mean(cases.iter().map(|c| c.ndcg_at_10)),
        mean_recall_at_k: mean(cases.iter().map(|c| c.recall_at_k)),
        mean_mrr: mean(cases.iter().map(|c| c.mrr)),
        hard_negative_hits: cases.iter().map(|c| c.hard_negative_hits).sum(),
        forbidden_hits: forbidden,
        citation_integrity: mean(cases.iter().map(|c| c.citation_integrity)),
        fallback_parity: mean(cases.iter().map(|c| c.fallback_parity)),
        latency_p50_ms: percentile(&latencies, 0.5),
        latency_p95_ms: percentile(&latencies, 0.95),
    }
}

fn per_slice(cases: &[RecallV3CaseReport]) -> BTreeMap<String, RecallV3SliceMetrics> {
    let slices = cases
        .iter()
        .flat_map(|c| c.slices.iter().cloned())
        .collect::<BTreeSet<_>>();
    slices
        .into_iter()
        .map(|slice| {
            let selected = cases
                .iter()
                .filter(|c| c.slices.contains(&slice))
                .collect::<Vec<_>>();
            let metrics = RecallV3SliceMetrics {
                case_count: selected.len(),
                mean_ndcg_at_10: mean(selected.iter().map(|c| c.ndcg_at_10)),
                mean_recall_at_k: mean(selected.iter().map(|c| c.recall_at_k)),
                mean_mrr: mean(selected.iter().map(|c| c.mrr)),
                forbidden_hits: selected
                    .iter()
                    .map(|c| c.forbidden_hits.values().sum::<usize>())
                    .sum(),
            };
            (slice, metrics)
        })
        .collect()
}

fn paired_comparison(cases: &[RecallV3CaseReport], lexical: &[f64]) -> RecallV3PairedComparison {
    let deltas = cases
        .iter()
        .zip(lexical)
        .map(|(case, baseline)| case.ndcg_at_10 - baseline)
        .collect::<Vec<_>>();
    let mut state = 0x005e_ed00_56d3_u64;
    let samples = 10_000;
    let mut means = Vec::with_capacity(samples);
    for _ in 0..samples {
        let mut total = 0.0;
        for _ in 0..deltas.len() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            total += deltas[(state as usize) % deltas.len()];
        }
        means.push(total / deltas.len() as f64);
    }
    means.sort_by(f64::total_cmp);
    RecallV3PairedComparison {
        baseline_candidate_id: "lexical-baseline".into(),
        mean_ndcg_delta: mean(deltas.iter().copied()),
        confidence_level: 0.95,
        lower_bound: means[249],
        upper_bound: means[9749],
        method: "paired-bootstrap-v1".into(),
        samples,
        seed: 0x005e_ed00_56d3,
    }
}

fn default_records_root() -> PathBuf {
    PathBuf::from("records")
}
fn ratio(n: usize, d: usize) -> f64 {
    if d == 0 { 1.0 } else { n as f64 / d as f64 }
}
fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let values = values.collect::<Vec<_>>();
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}
fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[((sorted.len() as f64 * p).ceil() as usize).saturating_sub(1)]
}
fn digest_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn source_bound_digest(version: &str, source_digest: &str) -> String {
    digest_bytes(format!("{version}\n{source_digest}").as_bytes())
}

pub fn recall_v3_metrics_digest() -> String {
    source_bound_digest(
        RECALL_V3_METRICS_VERSION,
        env!("MEMZOI_RECALL_V3_METRICS_SOURCE_DIGEST"),
    )
}

pub fn recall_v3_runner_digest() -> String {
    source_bound_digest(
        RECALL_V3_RUNNER_VERSION,
        env!("MEMZOI_RECALL_V3_RUNNER_SOURCE_DIGEST"),
    )
}
fn digest_json(value: &impl Serialize) -> Result<String> {
    Ok(digest_bytes(&serde_json_canonicalizer::to_vec(value)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_fetch_limit_never_exceeds_staged_record_count() {
        assert_eq!(lexical_fetch_limit(4, 1, 2), 3);
        assert_eq!(lexical_fetch_limit(4, 4, 2), 4);
        assert_eq!(lexical_fetch_limit(4, 0, 100), 4);
    }

    #[test]
    fn corpus_top_k_matches_production_search_boundary() -> anyhow::Result<()> {
        let mut corpus: RecallV3Corpus =
            serde_yaml::from_str(include_str!("../../../evals/recall/v3/corpus.yaml"))?;
        corpus.cases[0].top_k = SEARCH_RESULT_LIMIT_MAX;
        validate_corpus(&corpus)?;
        corpus.cases[0].top_k = SEARCH_RESULT_LIMIT_MAX + 1;
        let error = validate_corpus(&corpus).unwrap_err();
        assert!(error.to_string().contains("top_k must be between"));
        Ok(())
    }

    #[test]
    fn citation_path_prefers_the_case_match_then_the_first_staged_path() {
        let paths = vec!["docs/default.md".to_owned(), "src/**".to_owned()];
        assert_eq!(
            recall_citation_path(&paths, Some("src/lib.rs")).as_deref(),
            Some("src/**")
        );
        assert_eq!(
            recall_citation_path(&paths, Some("other/file.md")).as_deref(),
            Some("docs/default.md")
        );
    }
}
