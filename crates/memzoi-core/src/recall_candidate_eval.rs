use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    MemoryDestination, RecallV3Candidate, RecallV3CandidateHit, RecallV3CandidateInput,
    RecallV3CandidateManifest, RecallV3CandidateOutput,
};

pub const RECALL_CANDIDATE_MANIFEST_VERSION: &str = "memzoi-recall-candidate/v1";
pub const RECALL_VECTOR_ARTIFACT_VERSION: &str = "memzoi-recall-vectors/v1";
pub const RECALL_DEVELOPMENT_LOG_VERSION: &str = "memzoi-recall-development-log/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallCandidateArchitecture {
    SemanticOnly,
    LexicalRerank,
    LexicalSemanticUnion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallFusionMethod {
    WeightedSum,
    ReciprocalRank,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallModelIdentity {
    pub id: String,
    pub revision: String,
    pub artifact_digest: String,
    pub license: String,
    pub dimensions: usize,
    pub pooling: String,
    pub normalization: String,
    pub query_prefix: String,
    pub document_prefix: String,
    pub explicitly_installed: bool,
    pub offline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEmbeddingDocument {
    pub version: String,
    pub template: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallCandidateRetrieval {
    pub architecture: RecallCandidateArchitecture,
    pub lexical_candidates: usize,
    pub semantic_candidates: usize,
    pub final_results: usize,
    pub similarity_cutoff: f64,
    pub fusion: RecallFusionMethod,
    pub lexical_weight: f64,
    pub semantic_weight: f64,
    pub reciprocal_rank_k: f64,
    pub path_weight: f64,
    pub type_weight: f64,
    pub lane_weight: f64,
    pub destination_weight: f64,
    pub confidence_weight: f64,
    pub tie_break: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallCandidateStorage {
    pub profile_id: String,
    pub generation: String,
    pub vector_artifact: PathBuf,
    pub content_fingerprint: String,
    pub exact_search: bool,
    pub destination: MemoryDestination,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallCandidateEnvironment {
    pub target_os: String,
    pub target_arch: String,
    pub cpu_features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallRetrievalCandidateManifest {
    pub version: String,
    pub id: String,
    pub revision: String,
    pub model: RecallModelIdentity,
    pub document: RecallEmbeddingDocument,
    pub retrieval: RecallCandidateRetrieval,
    pub storage: RecallCandidateStorage,
    pub environment: RecallCandidateEnvironment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallVectorState {
    Complete,
    Stale,
    Incomplete,
    Incompatible,
    Corrupt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallVectorArtifact {
    pub version: String,
    pub profile_id: String,
    pub generation: String,
    pub model_artifact_digest: String,
    pub document_digest: String,
    pub content_fingerprint: String,
    pub dimensions: usize,
    pub state: RecallVectorState,
    pub queries: BTreeMap<String, Vec<f64>>,
    pub records: BTreeMap<String, Vec<f64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallDevelopmentAttemptOutcome {
    Completed,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallDevelopmentAttempt {
    pub candidate_id: String,
    pub candidate_digest: String,
    pub outcome: RecallDevelopmentAttemptOutcome,
    pub reason_code: Option<String>,
    pub report_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallDevelopmentLog {
    pub version: String,
    pub corpus_digest: String,
    pub runner_digest: String,
    pub attempts: Vec<RecallDevelopmentAttempt>,
}

enum CandidateState {
    Ready(RecallVectorArtifact),
    Fallback(String),
}

pub struct ManifestDrivenRecallCandidate {
    manifest: RecallRetrievalCandidateManifest,
    manifest_digest: String,
    state: CandidateState,
}

impl ManifestDrivenRecallCandidate {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect candidate manifest {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("candidate manifest must be a regular non-symlink file");
        }
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read candidate manifest {}", path.display()))?;
        let manifest: RecallRetrievalCandidateManifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse candidate manifest {}", path.display()))?;
        validate_manifest(&manifest)?;
        let manifest_digest = digest_json(&manifest)?;
        let artifact_path = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&manifest.storage.vector_artifact);
        let state = if (manifest.environment.target_os == std::env::consts::OS
            && manifest.environment.target_arch == std::env::consts::ARCH)
            || (manifest.environment.target_os == "portable-fixture"
                && manifest.environment.target_arch == "portable-fixture")
        {
            load_artifact(&manifest, &artifact_path)
        } else {
            CandidateState::Fallback("unsupported_platform".into())
        };
        Ok(Self {
            manifest,
            manifest_digest,
            state,
        })
    }

    pub fn retrieval_manifest(&self) -> &RecallRetrievalCandidateManifest {
        &self.manifest
    }
}

impl RecallV3Candidate for ManifestDrivenRecallCandidate {
    fn manifest(&self) -> RecallV3CandidateManifest {
        RecallV3CandidateManifest {
            id: self.manifest.id.clone(),
            version: self.manifest.revision.clone(),
            adapter: "manifest-driven-exact-vector".into(),
            configuration_digest: self.manifest_digest.clone(),
            offline: self.manifest.model.offline,
        }
    }

    fn retrieve(&mut self, input: &RecallV3CandidateInput) -> Result<RecallV3CandidateOutput> {
        let CandidateState::Ready(artifact) = &self.state else {
            let CandidateState::Fallback(reason) = &self.state else {
                unreachable!()
            };
            return Ok(lexical_fallback(input, reason));
        };
        let Some(query) = artifact.queries.get(&input.case_id) else {
            return Ok(lexical_fallback(input, "query_embedding_failure"));
        };
        let eligible = input
            .eligible_records
            .iter()
            .map(|record| (record.id.as_str(), record))
            .collect::<BTreeMap<_, _>>();
        let mut semantic = Vec::with_capacity(eligible.len());
        for (id, record) in &eligible {
            let Some(vector) = artifact.records.get(*id) else {
                return Ok(lexical_fallback(input, "incomplete_index"));
            };
            let score = cosine(query, vector)?;
            if score >= self.manifest.retrieval.similarity_cutoff {
                semantic.push(RecallV3CandidateHit {
                    record_id: (*id).to_owned(),
                    score,
                    citations: vec![record.citation.clone()],
                    signals: BTreeMap::from([
                        ("eligibility".into(), 1.0),
                        ("lexical".into(), 0.0),
                        ("semantic".into(), score),
                        ("fusion".into(), score),
                        ("structural".into(), 0.0),
                        ("suppression".into(), 0.0),
                    ]),
                });
            }
        }
        semantic.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.record_id.cmp(&right.record_id))
        });
        semantic.truncate(self.manifest.retrieval.semantic_candidates);

        let hits = match self.manifest.retrieval.architecture {
            RecallCandidateArchitecture::SemanticOnly => semantic,
            RecallCandidateArchitecture::LexicalRerank => {
                rerank_lexical(input, &semantic, &self.manifest.retrieval)
            }
            RecallCandidateArchitecture::LexicalSemanticUnion => {
                union(input, &semantic, &self.manifest.retrieval)
            }
        };
        Ok(RecallV3CandidateOutput {
            hits,
            fallback_reason: None,
            resource_observations: BTreeMap::from([
                ("vector_dimensions".into(), artifact.dimensions as f64),
                ("indexed_records".into(), artifact.records.len() as f64),
                ("exact_distance_comparisons".into(), eligible.len() as f64),
            ]),
        })
    }
}

pub fn validate_development_log(log: &RecallDevelopmentLog) -> Result<()> {
    if log.version != RECALL_DEVELOPMENT_LOG_VERSION
        || log.corpus_digest.trim().is_empty()
        || log.runner_digest.trim().is_empty()
        || log.attempts.is_empty()
    {
        bail!("invalid recall development log identity");
    }
    let mut candidates = BTreeSet::new();
    for attempt in &log.attempts {
        if !candidates.insert(&attempt.candidate_id)
            || attempt.candidate_digest.trim().is_empty()
            || matches!(
                attempt.outcome,
                RecallDevelopmentAttemptOutcome::Rejected | RecallDevelopmentAttemptOutcome::Failed
            ) != attempt.reason_code.is_some()
            || matches!(attempt.outcome, RecallDevelopmentAttemptOutcome::Completed)
                != attempt.report_digest.is_some()
        {
            bail!(
                "invalid or duplicate development attempt {:?}",
                attempt.candidate_id
            );
        }
    }
    Ok(())
}

fn validate_manifest(manifest: &RecallRetrievalCandidateManifest) -> Result<()> {
    if manifest.version != RECALL_CANDIDATE_MANIFEST_VERSION {
        bail!(
            "unsupported recall candidate manifest version {:?}",
            manifest.version
        );
    }
    let required = [
        manifest.id.as_str(),
        manifest.revision.as_str(),
        manifest.model.id.as_str(),
        manifest.model.revision.as_str(),
        manifest.model.artifact_digest.as_str(),
        manifest.model.license.as_str(),
        manifest.model.pooling.as_str(),
        manifest.model.normalization.as_str(),
        manifest.document.version.as_str(),
        manifest.document.template.as_str(),
        manifest.document.digest.as_str(),
        manifest.storage.profile_id.as_str(),
        manifest.storage.generation.as_str(),
        manifest.storage.content_fingerprint.as_str(),
        manifest.environment.target_os.as_str(),
        manifest.environment.target_arch.as_str(),
        manifest.retrieval.tie_break.as_str(),
    ];
    if required.iter().any(|value| value.trim().is_empty()) {
        bail!("candidate manifest required fields must be non-empty");
    }
    if !manifest.model.explicitly_installed || !manifest.model.offline {
        bail!("v0.5 candidates must be explicitly installed and offline");
    }
    if !manifest.storage.exact_search
        || manifest.storage.destination != MemoryDestination::Repo
        || manifest.storage.vector_artifact.is_absolute()
        || manifest
            .storage
            .vector_artifact
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        bail!("v0.5 candidate storage must be repo-only exact search with a relative artifact");
    }
    let retrieval = &manifest.retrieval;
    if manifest.model.dimensions == 0
        || retrieval.lexical_candidates == 0
        || retrieval.semantic_candidates == 0
        || retrieval.final_results == 0
        || ![
            retrieval.similarity_cutoff,
            retrieval.lexical_weight,
            retrieval.semantic_weight,
            retrieval.reciprocal_rank_k,
            retrieval.path_weight,
            retrieval.type_weight,
            retrieval.lane_weight,
            retrieval.destination_weight,
            retrieval.confidence_weight,
        ]
        .iter()
        .all(|value| value.is_finite())
    {
        bail!("invalid candidate retrieval parameters");
    }
    Ok(())
}

fn load_artifact(manifest: &RecallRetrievalCandidateManifest, path: &Path) -> CandidateState {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return CandidateState::Fallback("missing_index".into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return CandidateState::Fallback("corrupt_index".into());
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return CandidateState::Fallback("missing_index".into()),
    };
    let artifact: RecallVectorArtifact = match serde_json::from_slice(&bytes) {
        Ok(artifact) => artifact,
        Err(_) => return CandidateState::Fallback("corrupt_index".into()),
    };
    if artifact.version != RECALL_VECTOR_ARTIFACT_VERSION
        || artifact.profile_id != manifest.storage.profile_id
        || artifact.generation != manifest.storage.generation
        || artifact.model_artifact_digest != manifest.model.artifact_digest
        || artifact.document_digest != manifest.document.digest
        || artifact.content_fingerprint != manifest.storage.content_fingerprint
        || artifact.dimensions != manifest.model.dimensions
        || artifact
            .queries
            .values()
            .chain(artifact.records.values())
            .any(|vector| {
                vector.len() != artifact.dimensions || vector.iter().any(|value| !value.is_finite())
            })
    {
        return CandidateState::Fallback("incompatible_index".into());
    }
    match artifact.state {
        RecallVectorState::Complete => CandidateState::Ready(artifact),
        RecallVectorState::Stale => CandidateState::Fallback("stale_index".into()),
        RecallVectorState::Incomplete => CandidateState::Fallback("incomplete_index".into()),
        RecallVectorState::Incompatible => CandidateState::Fallback("incompatible_index".into()),
        RecallVectorState::Corrupt => CandidateState::Fallback("corrupt_index".into()),
    }
}

fn rerank_lexical(
    input: &RecallV3CandidateInput,
    semantic: &[RecallV3CandidateHit],
    retrieval: &RecallCandidateRetrieval,
) -> Vec<RecallV3CandidateHit> {
    let semantic_scores = semantic
        .iter()
        .map(|hit| (hit.record_id.as_str(), hit.score))
        .collect::<BTreeMap<_, _>>();
    let records = input
        .eligible_records
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect::<BTreeMap<_, _>>();
    let mut hits = input
        .lexical_hits
        .iter()
        .take(retrieval.lexical_candidates)
        .enumerate()
        .filter_map(|(rank, lexical)| {
            let record = records.get(lexical.record_id.as_str())?;
            let semantic_score = semantic_scores
                .get(lexical.record_id.as_str())
                .copied()
                .unwrap_or(0.0);
            let lexical_score = 1.0 / (rank + 1) as f64;
            let score = retrieval.lexical_weight * lexical_score
                + retrieval.semantic_weight * semantic_score;
            Some(candidate_hit(record, lexical_score, semantic_score, score))
        })
        .collect::<Vec<_>>();
    sort_and_truncate(&mut hits, retrieval.final_results);
    hits
}

fn union(
    input: &RecallV3CandidateInput,
    semantic: &[RecallV3CandidateHit],
    retrieval: &RecallCandidateRetrieval,
) -> Vec<RecallV3CandidateHit> {
    let records = input
        .eligible_records
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect::<BTreeMap<_, _>>();
    let lexical_ranks = input
        .lexical_hits
        .iter()
        .take(retrieval.lexical_candidates)
        .enumerate()
        .map(|(rank, hit)| (hit.record_id.as_str(), rank + 1))
        .collect::<BTreeMap<_, _>>();
    let semantic_ranks = semantic
        .iter()
        .enumerate()
        .map(|(rank, hit)| (hit.record_id.as_str(), (rank + 1, hit.score)))
        .collect::<BTreeMap<_, _>>();
    let ids = lexical_ranks
        .keys()
        .chain(semantic_ranks.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut hits = ids
        .into_iter()
        .filter_map(|id| {
            let record = records.get(id)?;
            let lexical_rank = lexical_ranks.get(id).copied();
            let (semantic_rank, semantic_score) = semantic_ranks
                .get(id)
                .copied()
                .map_or((None, 0.0), |(rank, score)| (Some(rank), score));
            let lexical_score = lexical_rank.map_or(0.0, |rank| 1.0 / rank as f64);
            let score = match retrieval.fusion {
                RecallFusionMethod::WeightedSum => {
                    retrieval.lexical_weight * lexical_score
                        + retrieval.semantic_weight * semantic_score
                }
                RecallFusionMethod::ReciprocalRank => {
                    lexical_rank.map_or(0.0, |rank| {
                        retrieval.lexical_weight / (retrieval.reciprocal_rank_k + rank as f64)
                    }) + semantic_rank.map_or(0.0, |rank| {
                        retrieval.semantic_weight / (retrieval.reciprocal_rank_k + rank as f64)
                    })
                }
            };
            Some(candidate_hit(record, lexical_score, semantic_score, score))
        })
        .collect::<Vec<_>>();
    sort_and_truncate(&mut hits, retrieval.final_results);
    hits
}

fn candidate_hit(
    record: &&crate::RecallV3CandidateRecord,
    lexical: f64,
    semantic: f64,
    fusion: f64,
) -> RecallV3CandidateHit {
    RecallV3CandidateHit {
        record_id: record.id.clone(),
        score: fusion,
        citations: vec![record.citation.clone()],
        signals: BTreeMap::from([
            ("eligibility".into(), 1.0),
            ("lexical".into(), lexical),
            ("semantic".into(), semantic),
            ("fusion".into(), fusion),
            ("structural".into(), 0.0),
            ("suppression".into(), 0.0),
        ]),
    }
}

fn sort_and_truncate(hits: &mut Vec<RecallV3CandidateHit>, limit: usize) {
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.record_id.cmp(&right.record_id))
    });
    hits.truncate(limit);
}

fn lexical_fallback(input: &RecallV3CandidateInput, reason: &str) -> RecallV3CandidateOutput {
    let mut hits = input.lexical_hits.clone();
    for hit in &mut hits {
        hit.signals.insert("fallback".into(), 1.0);
    }
    RecallV3CandidateOutput {
        hits,
        fallback_reason: Some(reason.into()),
        resource_observations: BTreeMap::new(),
    }
}

fn cosine(left: &[f64], right: &[f64]) -> Result<f64> {
    if left.len() != right.len() || left.is_empty() {
        bail!("vector dimensions do not match");
    }
    let dot = left.iter().zip(right).map(|(a, b)| a * b).sum::<f64>();
    let left_norm = left.iter().map(|value| value * value).sum::<f64>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f64>().sqrt();
    if left_norm == 0.0 || right_norm == 0.0 {
        bail!("zero-length vector cannot be searched");
    }
    Ok(dot / (left_norm * right_norm))
}

fn digest_json(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json_canonicalizer::to_vec(value)?)
        .to_hex()
        .to_string())
}
