use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use crate::{
    FixedClock, InitRequest, MemoryCitation, MemoryLane, MemoryPaths, MemoryService, MemoryType,
    ScopeKind, SearchInput,
};

pub const RECALL_EVAL_CORPUS_VERSION: &str = "memzoi-recall-corpus/v1";
pub const RECALL_EVAL_REPORT_VERSION: &str = "memzoi-recall-report/v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEvalCorpus {
    pub version: String,
    pub name: String,
    pub evaluated_at: String,
    #[serde(default = "default_records_root")]
    pub records_root: PathBuf,
    pub records: Vec<PathBuf>,
    pub thresholds: RecallEvalThresholds,
    pub cases: Vec<RecallEvalCase>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEvalCase {
    pub id: String,
    pub query: String,
    pub relevant_ids: Vec<String>,
    #[serde(default)]
    pub forbidden_ids: Vec<String>,
    pub scope_kind: Option<ScopeKind>,
    pub scope_id: Option<String>,
    #[serde(rename = "type")]
    pub memory_type: Option<MemoryType>,
    pub lane: Option<MemoryLane>,
    #[serde(rename = "path")]
    pub path_prefix: Option<String>,
    pub k: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEvalThresholds {
    pub min_mean_recall_at_k: f64,
    pub min_mean_mrr: f64,
    pub max_forbidden_hits: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_mean_latency_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalReport {
    pub version: String,
    pub corpus_name: String,
    pub corpus_version: String,
    pub evaluated_at: String,
    pub fixture_record_count: usize,
    pub cases: Vec<RecallEvalCaseReport>,
    pub aggregate: RecallEvalAggregate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalCaseReport {
    pub id: String,
    pub query: String,
    pub k: usize,
    pub relevant_ids: Vec<String>,
    pub forbidden_ids: Vec<String>,
    pub retrieved_ids: Vec<String>,
    pub retrieved: Vec<RecallEvalRetrievedRecord>,
    pub recall_at_k: f64,
    pub mrr: f64,
    pub forbidden_hits: Vec<String>,
    pub forbidden_hit_count: usize,
    pub latency_ms: f64,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalRetrievedRecord {
    pub rank: usize,
    pub record_id: String,
    pub citations: Vec<MemoryCitation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvalAggregate {
    pub case_count: usize,
    pub mean_recall_at_k: f64,
    pub mean_mrr: f64,
    pub total_forbidden_hits: usize,
    pub mean_latency_ms: f64,
    pub thresholds: RecallEvalThresholds,
    pub threshold_results: RecallEvalThresholdResults,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallEvalThresholdResults {
    pub min_mean_recall_at_k: bool,
    pub min_mean_mrr: bool,
    pub max_forbidden_hits: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_mean_latency_ms: Option<bool>,
}

pub fn run_recall_eval(corpus_path: impl AsRef<Path>) -> Result<RecallEvalReport> {
    let corpus_path = corpus_path.as_ref();
    let (corpus, corpus_root) = load_corpus(corpus_path)?;
    validate_corpus(&corpus)?;

    let temp = TempDir::new().context("failed to create isolated recall evaluation state")?;
    let project_root = temp
        .path()
        .canonicalize()
        .context("failed to canonicalize isolated recall evaluation root")?;
    let paths = MemoryPaths::with_runtime_home(project_root, temp.path().join("runtime-home"));
    MemoryService::initialize_paths(paths.clone(), InitRequest { force: true })?;
    stage_fixture_records(&corpus, &corpus_root, &paths.records_dir())?;

    let rebuilt = MemoryService::rebuild_paths(paths.clone())?;
    let fixture_ids = rebuilt.record_ids.into_iter().collect::<BTreeSet<_>>();
    validate_case_record_ids(&corpus, &fixture_ids)?;

    let clock = FixedClock::from_rfc3339(&corpus.evaluated_at)
        .context("invalid recall evaluation evaluated_at")?;
    let service = MemoryService::open_paths_with_clock(paths, clock)?;
    let mut cases = Vec::with_capacity(corpus.cases.len());
    for case in &corpus.cases {
        cases.push(evaluate_case(&service, case)?);
    }

    let aggregate = aggregate(&cases, corpus.thresholds.clone());
    Ok(RecallEvalReport {
        version: RECALL_EVAL_REPORT_VERSION.to_owned(),
        corpus_name: corpus.name,
        corpus_version: corpus.version,
        evaluated_at: corpus.evaluated_at,
        fixture_record_count: fixture_ids.len(),
        cases,
        aggregate,
    })
}

fn default_records_root() -> PathBuf {
    PathBuf::from(".")
}

fn load_corpus(path: &Path) -> Result<(RecallEvalCorpus, PathBuf)> {
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
    let yaml = fs::read_to_string(&path)
        .with_context(|| format!("failed to read recall corpus {}", path.display()))?;
    let corpus = serde_yaml::from_str(&yaml)
        .with_context(|| format!("failed to parse recall corpus {}", path.display()))?;
    Ok((corpus, root))
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
    if corpus.cases.is_empty() {
        bail!("recall corpus must define at least one case");
    }
    validate_ratio(
        corpus.thresholds.min_mean_recall_at_k,
        "thresholds.min_mean_recall_at_k",
    )?;
    validate_ratio(corpus.thresholds.min_mean_mrr, "thresholds.min_mean_mrr")?;
    if let Some(max_latency) = corpus.thresholds.max_mean_latency_ms
        && (!max_latency.is_finite() || max_latency < 0.0)
    {
        bail!("thresholds.max_mean_latency_ms must be a finite non-negative number");
    }

    let mut case_ids = BTreeSet::new();
    for case in &corpus.cases {
        if case.id.trim().is_empty() {
            bail!("recall case id cannot be empty");
        }
        if !case_ids.insert(case.id.as_str()) {
            bail!("duplicate recall case id {:?}", case.id);
        }
        if case.query.trim().is_empty() {
            bail!("recall case {:?} query cannot be empty", case.id);
        }
        if case.k == 0 || case.k > 100 {
            bail!("recall case {:?} k must be between 1 and 100", case.id);
        }
        if case.relevant_ids.is_empty() {
            bail!("recall case {:?} must declare relevant_ids", case.id);
        }
        validate_unique_ids(&case.id, "relevant_ids", &case.relevant_ids)?;
        validate_unique_ids(&case.id, "forbidden_ids", &case.forbidden_ids)?;
        let relevant = case.relevant_ids.iter().collect::<BTreeSet<_>>();
        if let Some(overlap) = case.forbidden_ids.iter().find(|id| relevant.contains(id)) {
            bail!(
                "recall case {:?} record {:?} cannot be both relevant and forbidden",
                case.id,
                overlap
            );
        }
        if case
            .scope_id
            .as_deref()
            .is_some_and(|id| id.trim().is_empty())
        {
            bail!("recall case {:?} scope_id cannot be empty", case.id);
        }
        if case
            .path_prefix
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            bail!("recall case {:?} path cannot be empty", case.id);
        }
    }
    Ok(())
}

fn validate_ratio(value: f64, label: &str) -> Result<()> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        bail!("{label} must be a finite number between 0 and 1");
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

fn stage_fixture_records(
    corpus: &RecallEvalCorpus,
    corpus_root: &Path,
    destination_root: &Path,
) -> Result<()> {
    validate_records_root(&corpus.records_root)?;
    let fixture_root = corpus_root.join(&corpus.records_root);
    ensure_directory_without_symlinks(corpus_root, &corpus.records_root, &fixture_root)?;
    let fixture_root = fixture_root
        .canonicalize()
        .with_context(|| format!("failed to resolve records_root {}", fixture_root.display()))?;

    let mut seen = BTreeSet::new();
    for record_path in &corpus.records {
        validate_record_fixture_path(record_path)?;
        if !seen.insert(record_path.clone()) {
            bail!("duplicate record fixture path {}", record_path.display());
        }
        let source = fixture_root.join(record_path);
        ensure_regular_file_without_symlinks(&fixture_root, record_path, &source)?;
        let source = source.canonicalize().with_context(|| {
            format!("failed to resolve record fixture {}", record_path.display())
        })?;
        if !source.starts_with(&fixture_root) {
            bail!(
                "record fixture escapes records_root: {}",
                record_path.display()
            );
        }
        let bytes = fs::read(&source)
            .with_context(|| format!("failed to read record fixture {}", record_path.display()))?;
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

fn validate_records_root(path: &Path) -> Result<()> {
    if path.is_absolute() {
        bail!("records_root must be relative to the corpus file");
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_) | Component::CurDir) {
            bail!("records_root contains traversal or an unsafe component");
        }
    }
    Ok(())
}

fn validate_record_fixture_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("record fixture paths must be non-empty and relative");
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!(
            "record fixture path contains traversal or an unsafe component: {}",
            path.display()
        );
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
        bail!(
            "record fixture must use the .md extension: {}",
            path.display()
        );
    }
    if matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("index.md" | "log.md")
    ) {
        bail!(
            "record fixture uses a reserved file name: {}",
            path.display()
        );
    }
    Ok(())
}

fn ensure_directory_without_symlinks(root: &Path, relative: &Path, path: &Path) -> Result<()> {
    ensure_path_chain_without_symlinks(root, relative)?;
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect records_root {}", path.display()))?;
    if !metadata.is_dir() {
        bail!("records_root is not a directory: {}", path.display());
    }
    Ok(())
}

fn ensure_regular_file_without_symlinks(root: &Path, relative: &Path, path: &Path) -> Result<()> {
    ensure_path_chain_without_symlinks(root, relative)?;
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect record fixture {}", relative.display()))?;
    if !metadata.is_file() {
        bail!(
            "record fixture is not a regular file: {}",
            relative.display()
        );
    }
    Ok(())
}

fn ensure_path_chain_without_symlinks(root: &Path, relative: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if let Component::Normal(component) = component {
            current.push(component);
            let metadata = fs::symlink_metadata(&current).with_context(|| {
                format!("failed to inspect fixture path {}", relative.display())
            })?;
            if metadata.file_type().is_symlink() {
                bail!(
                    "fixture path must not contain symlinks: {}",
                    relative.display()
                );
            }
        }
    }
    Ok(())
}

fn validate_case_record_ids(
    corpus: &RecallEvalCorpus,
    fixture_ids: &BTreeSet<String>,
) -> Result<()> {
    for case in &corpus.cases {
        for (label, ids) in [
            ("relevant", case.relevant_ids.as_slice()),
            ("forbidden", case.forbidden_ids.as_slice()),
        ] {
            for id in ids {
                if !fixture_ids.contains(id) {
                    bail!(
                        "recall case {:?} references missing {label} record id {:?}",
                        case.id,
                        id
                    );
                }
            }
        }
    }
    Ok(())
}

fn evaluate_case(service: &MemoryService, case: &RecallEvalCase) -> Result<RecallEvalCaseReport> {
    let started = Instant::now();
    let results = service.search_memory(SearchInput {
        query: case.query.clone(),
        scope_kind: case.scope_kind,
        scope_id: case.scope_id.clone(),
        memory_type: case.memory_type,
        lane: case.lane,
        path_prefix: case.path_prefix.clone(),
        limit: case.k,
        ..SearchInput::default()
    })?;
    let latency_ms = rounded(started.elapsed().as_secs_f64() * 1_000.0, 3);
    let retrieved_ids = results
        .iter()
        .map(|result| result.record.id.clone())
        .collect::<Vec<_>>();
    let relevant = case.relevant_ids.iter().collect::<BTreeSet<_>>();
    let forbidden = case.forbidden_ids.iter().collect::<BTreeSet<_>>();
    let relevant_hits = retrieved_ids
        .iter()
        .filter(|id| relevant.contains(id))
        .count();
    let recall_at_k = rounded(relevant_hits as f64 / relevant.len() as f64, 6);
    let mrr = retrieved_ids
        .iter()
        .position(|id| relevant.contains(id))
        .map_or(0.0, |index| 1.0 / (index + 1) as f64);
    let mrr = rounded(mrr, 6);
    let forbidden_hits = retrieved_ids
        .iter()
        .filter(|id| forbidden.contains(id))
        .cloned()
        .collect::<Vec<_>>();
    let retrieved = results
        .into_iter()
        .enumerate()
        .map(|(index, result)| RecallEvalRetrievedRecord {
            rank: index + 1,
            record_id: result.record.id,
            citations: result.citations,
        })
        .collect();
    let passed = relevant_hits == relevant.len() && forbidden_hits.is_empty();

    Ok(RecallEvalCaseReport {
        id: case.id.clone(),
        query: case.query.clone(),
        k: case.k,
        relevant_ids: case.relevant_ids.clone(),
        forbidden_ids: case.forbidden_ids.clone(),
        retrieved_ids,
        retrieved,
        recall_at_k,
        mrr,
        forbidden_hit_count: forbidden_hits.len(),
        forbidden_hits,
        latency_ms,
        passed,
    })
}

fn aggregate(
    cases: &[RecallEvalCaseReport],
    thresholds: RecallEvalThresholds,
) -> RecallEvalAggregate {
    let count = cases.len() as f64;
    let mean_recall_at_k = rounded(
        cases.iter().map(|case| case.recall_at_k).sum::<f64>() / count,
        6,
    );
    let mean_mrr = rounded(cases.iter().map(|case| case.mrr).sum::<f64>() / count, 6);
    let total_forbidden_hits = cases.iter().map(|case| case.forbidden_hit_count).sum();
    let mean_latency_ms = rounded(
        cases.iter().map(|case| case.latency_ms).sum::<f64>() / count,
        3,
    );
    let threshold_results = RecallEvalThresholdResults {
        min_mean_recall_at_k: mean_recall_at_k >= thresholds.min_mean_recall_at_k,
        min_mean_mrr: mean_mrr >= thresholds.min_mean_mrr,
        max_forbidden_hits: total_forbidden_hits <= thresholds.max_forbidden_hits,
        max_mean_latency_ms: thresholds
            .max_mean_latency_ms
            .map(|maximum| mean_latency_ms <= maximum),
    };
    let passed = threshold_results.min_mean_recall_at_k
        && threshold_results.min_mean_mrr
        && threshold_results.max_forbidden_hits
        && threshold_results.max_mean_latency_ms.unwrap_or(true);

    RecallEvalAggregate {
        case_count: cases.len(),
        mean_recall_at_k,
        mean_mrr,
        total_forbidden_hits,
        mean_latency_ms,
        thresholds,
        threshold_results,
        passed,
    }
}

fn rounded(value: f64, digits: i32) -> f64 {
    let factor = 10_f64.powi(digits);
    (value * factor).round() / factor
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, path::Path};

    use super::{
        RECALL_EVAL_CORPUS_VERSION, RecallEvalCorpus, validate_case_record_ids, validate_corpus,
        validate_record_fixture_path,
    };

    #[test]
    fn corpus_rejects_unknown_fields() {
        let yaml = format!(
            r#"
version: {RECALL_EVAL_CORPUS_VERSION}
name: invalid
evaluated_at: 2026-07-10T12:00:00Z
records: [one.md]
thresholds:
  min_mean_recall_at_k: 1.0
  min_mean_mrr: 1.0
  max_forbidden_hits: 0
cases: []
unexpected: true
"#
        );
        let error = serde_yaml::from_str::<RecallEvalCorpus>(&yaml)
            .expect_err("unknown corpus fields must be rejected");
        assert!(error.to_string().contains("unknown field `unexpected`"));
    }

    #[test]
    fn corpus_rejects_overlapping_relevant_and_forbidden_ids() {
        let yaml = format!(
            r#"
version: {RECALL_EVAL_CORPUS_VERSION}
name: invalid
evaluated_at: 2026-07-10T12:00:00Z
records: [one.md]
thresholds:
  min_mean_recall_at_k: 1.0
  min_mean_mrr: 1.0
  max_forbidden_hits: 0
cases:
  - id: overlap
    query: token
    relevant_ids: [one]
    forbidden_ids: [one]
    scope_kind: repo
    scope_id: null
    type: fact
    lane: semantic
    path: null
    k: 1
"#
        );
        let corpus: RecallEvalCorpus = serde_yaml::from_str(&yaml).expect("parse corpus");
        let error = validate_corpus(&corpus).expect_err("overlap must be rejected");
        assert!(error.to_string().contains("both relevant and forbidden"));
    }

    #[test]
    fn corpus_rejects_missing_expected_fixture_ids() {
        let yaml = format!(
            r#"
version: {RECALL_EVAL_CORPUS_VERSION}
name: invalid
evaluated_at: 2026-07-10T12:00:00Z
records: [one.md]
thresholds:
  min_mean_recall_at_k: 1.0
  min_mean_mrr: 1.0
  max_forbidden_hits: 0
cases:
  - id: missing
    query: token
    relevant_ids: [missing-record]
    forbidden_ids: []
    k: 1
"#
        );
        let corpus: RecallEvalCorpus = serde_yaml::from_str(&yaml).expect("parse corpus");
        let error = validate_case_record_ids(&corpus, &BTreeSet::from(["one".to_owned()]))
            .expect_err("missing expected ids must be rejected");
        assert!(error.to_string().contains("missing relevant record id"));
    }

    #[test]
    fn record_fixture_paths_reject_traversal_and_absolute_paths() {
        let traversal = validate_record_fixture_path(Path::new("../outside.md"))
            .expect_err("traversal must be rejected");
        assert!(traversal.to_string().contains("unsafe component"));

        let absolute = validate_record_fixture_path(Path::new("/tmp/outside.md"))
            .expect_err("absolute paths must be rejected");
        assert!(absolute.to_string().contains("relative"));
    }
}
