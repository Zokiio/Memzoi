use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    RecallCandidateArchitecture, RecallV3CandidateRecord, RecallV3CandidateReport,
    RecallV3CorpusKind, RecallV3Report,
};

pub const RECALL_MODEL_PROFILE_VERSION: &str = "memzoi-recall-model-profile/v1";
pub const RECALL_MODEL_INSTALL_VERSION: &str = "memzoi-recall-model-install/v1";
pub const RECALL_MATRIX_VERSION: &str = "memzoi-recall-development-matrix/v1";
pub const RECALL_DEVELOPMENT_LOG_V2: &str = "memzoi-recall-development-log/v2";
pub const RECALL_FREEZE_VERSION: &str = "memzoi-recall-development-freeze/v1";
pub const TITLE_BODY_TEMPLATE: &str = "title_body/v1";
pub const TYPE_TITLE_BODY_TEMPLATE: &str = "type_title_body/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallModelFile {
    pub path: PathBuf,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallModelProfile {
    pub version: String,
    pub id: String,
    pub repository: String,
    pub revision: String,
    pub license: String,
    pub dimensions: usize,
    pub pooling: String,
    pub normalization: String,
    pub query_prefix: String,
    pub document_prefix: String,
    pub max_length: usize,
    pub allowed_origins: Vec<String>,
    pub files: Vec<RecallModelFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallInstalledFile {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallModelInstallManifest {
    pub version: String,
    pub profile_id: String,
    pub profile_digest: String,
    pub files: Vec<RecallInstalledFile>,
}

impl RecallModelProfile {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = regular_file_bytes(path.as_ref())?;
        let profile: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid model profile {}", path.as_ref().display()))?;
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != RECALL_MODEL_PROFILE_VERSION
            || self.id.trim().is_empty()
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            || self.repository.trim().is_empty()
            || self.revision.trim().is_empty()
            || self.license.trim().is_empty()
            || self.dimensions == 0
            || self.max_length == 0
            || !matches!(self.pooling.as_str(), "cls" | "mean")
            || self.normalization != "l2"
            || self.files.is_empty()
            || self.allowed_origins.is_empty()
        {
            bail!("invalid recall model profile identity");
        }
        let mut paths = BTreeSet::new();
        for file in &self.files {
            validate_relative_path(&file.path)?;
            if !paths.insert(&file.path) || file.bytes == 0 || !is_sha256(&file.sha256) {
                bail!("invalid or duplicate model file {:?}", file.path);
            }
            let origin = url_origin(&file.url)?;
            if !self
                .allowed_origins
                .iter()
                .any(|allowed| allowed == &origin)
            {
                bail!("model file origin {origin:?} is not allowlisted");
            }
        }
        for required in [
            "model.onnx",
            "tokenizer.json",
            "config.json",
            "special_tokens_map.json",
            "tokenizer_config.json",
        ] {
            if !self
                .files
                .iter()
                .any(|file| file.path.file_name().is_some_and(|name| name == required))
            {
                bail!("model profile is missing {required}");
            }
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String> {
        canonical_digest(self)
    }

    pub fn permits_url(&self, url: &str) -> Result<bool> {
        let origin = url_origin(url)?;
        Ok(self
            .allowed_origins
            .iter()
            .any(|allowed| allowed == &origin))
    }
}

pub fn install_recall_model_with<F, R>(
    profile: &RecallModelProfile,
    model_root: &Path,
    force: bool,
    mut fetch: F,
) -> Result<PathBuf>
where
    F: FnMut(&str) -> Result<R>,
    R: Read,
{
    profile.validate()?;
    fs::create_dir_all(model_root)?;
    let root_metadata = fs::symlink_metadata(model_root)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        bail!("model root must be a non-symlink directory");
    }
    let destination = model_root.join(&profile.id);
    if destination.exists() && !force {
        inspect_recall_model(profile, &destination)?;
        return Ok(destination);
    }
    let staging = tempfile::Builder::new()
        .prefix(&format!(".{}.install-", profile.id))
        .tempdir_in(model_root)?;
    let mut installed = Vec::new();
    for file in &profile.files {
        let mut reader =
            fetch(&file.url).with_context(|| format!("failed to fetch {}", file.url))?;
        let target = staging.path().join(&file.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        write_verified_stream(&target, &mut reader, file)?;
        installed.push(RecallInstalledFile {
            path: file.path.clone(),
            sha256: file.sha256.clone(),
            bytes: file.bytes,
        });
    }
    installed.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = RecallModelInstallManifest {
        version: RECALL_MODEL_INSTALL_VERSION.into(),
        profile_id: profile.id.clone(),
        profile_digest: profile.digest()?,
        files: installed,
    };
    write_new_file(
        &staging.path().join("install.json"),
        &canonical_json(&manifest)?,
    )?;
    if destination.exists() {
        let metadata = fs::symlink_metadata(&destination)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("existing model destination must be a non-symlink directory");
        }
        fs::remove_dir_all(&destination)?;
    }
    fs::rename(staging.keep(), &destination)?;
    inspect_recall_model(profile, &destination)?;
    Ok(destination)
}

pub fn inspect_recall_model(
    profile: &RecallModelProfile,
    directory: &Path,
) -> Result<RecallModelInstallManifest> {
    let metadata = fs::symlink_metadata(directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("model install must be a non-symlink directory");
    }
    let manifest: RecallModelInstallManifest =
        serde_json::from_slice(&regular_file_bytes(&directory.join("install.json"))?)?;
    if manifest.version != RECALL_MODEL_INSTALL_VERSION
        || manifest.profile_id != profile.id
        || manifest.profile_digest != profile.digest()?
    {
        bail!("model install manifest does not match profile");
    }
    let expected = profile
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    let actual = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    if expected != actual {
        bail!("model install file set does not match profile");
    }
    for file in &profile.files {
        verify_regular_file(&directory.join(&file.path), file)?;
    }
    let mut disk = BTreeSet::new();
    collect_files(directory, directory, &mut disk)?;
    disk.remove(Path::new("install.json"));
    if disk != expected {
        bail!("model install contains missing or extra files");
    }
    Ok(manifest)
}

pub fn render_recall_document(template: &str, record: &RecallV3CandidateRecord) -> Result<String> {
    let title = normalize_lf(&record.title);
    let body = normalize_lf(&record.body);
    match template {
        TITLE_BODY_TEMPLATE => Ok(format!("title: {title}\nbody: {body}\n")),
        TYPE_TITLE_BODY_TEMPLATE => Ok(format!(
            "type: {}\ntitle: {title}\nbody: {body}\n",
            format!("{:?}", record.citation.memory_type).to_lowercase()
        )),
        _ => bail!("unsupported embedding document template {template:?}"),
    }
}

pub trait RecallEmbedder {
    fn embed_queries(&mut self, values: &[String], batch_size: usize) -> Result<Vec<Vec<f64>>>;
    fn embed_documents(&mut self, values: &[String], batch_size: usize) -> Result<Vec<Vec<f64>>>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEmbeddingInputs {
    pub queries: BTreeMap<String, String>,
    pub records: BTreeMap<String, RecallV3CandidateRecord>,
}

pub fn build_recall_vector_artifact<E: RecallEmbedder>(
    profile: &RecallModelProfile,
    template: &str,
    model_artifact_digest: &str,
    generation: &str,
    inputs: &RecallEmbeddingInputs,
    batch_size: usize,
    embedder: &mut E,
) -> Result<crate::RecallVectorArtifact> {
    if inputs.queries.is_empty() || inputs.records.is_empty() || batch_size == 0 {
        bail!("vector artifact inputs and batch size must be non-empty");
    }
    let query_ids = inputs.queries.keys().cloned().collect::<Vec<_>>();
    let queries = inputs.queries.values().cloned().collect::<Vec<_>>();
    let record_ids = inputs.records.keys().cloned().collect::<Vec<_>>();
    let documents = inputs
        .records
        .values()
        .map(|record| render_recall_document(template, record))
        .collect::<Result<Vec<_>>>()?;
    let query_vectors = embedder.embed_queries(&queries, batch_size)?;
    let record_vectors = embedder.embed_documents(&documents, batch_size)?;
    validate_vectors(&query_vectors, query_ids.len(), profile.dimensions)?;
    validate_vectors(&record_vectors, record_ids.len(), profile.dimensions)?;
    let document_digest = canonical_digest(&BTreeMap::from([("version", template)]))?;
    let content_fingerprint = canonical_digest(inputs)?;
    Ok(crate::RecallVectorArtifact {
        version: crate::RECALL_VECTOR_ARTIFACT_VERSION.into(),
        profile_id: profile.id.clone(),
        generation: generation.into(),
        model_artifact_digest: model_artifact_digest.into(),
        document_digest,
        content_fingerprint,
        dimensions: profile.dimensions,
        state: crate::RecallVectorState::Complete,
        queries: query_ids.into_iter().zip(query_vectors).collect(),
        records: record_ids.into_iter().zip(record_vectors).collect(),
    })
}

pub fn write_recall_vector_artifact(
    artifact: &crate::RecallVectorArtifact,
    artifact_root: &Path,
    relative_path: &Path,
) -> Result<String> {
    validate_relative_path(relative_path)?;
    fs::create_dir_all(artifact_root)?;
    let destination = artifact_root.join(relative_path);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if destination.exists() {
        bail!(
            "refusing to overwrite vector artifact {}",
            destination.display()
        );
    }
    let parent = destination.parent().unwrap_or(artifact_root);
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&canonical_json(artifact)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(&destination)?;
    crate::recall_vector_artifact_digest(artifact)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallMatrixParameters {
    pub lexical_candidates: usize,
    pub semantic_candidates: usize,
    pub final_results: usize,
    pub similarity_cutoff: f64,
    pub lexical_weight: f64,
    pub semantic_weight: f64,
    pub reciprocal_rank_k: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallDevelopmentMatrix {
    pub version: String,
    pub profiles: Vec<PathBuf>,
    pub templates: Vec<String>,
    pub architectures: Vec<RecallCandidateArchitecture>,
    pub parameters: RecallMatrixParameters,
}

impl RecallDevelopmentMatrix {
    pub fn validate(&self) -> Result<()> {
        if self.version != RECALL_MATRIX_VERSION
            || self.profiles.len() != 3
            || self.templates != [TITLE_BODY_TEMPLATE, TYPE_TITLE_BODY_TEMPLATE]
            || self.architectures.len() != 3
        {
            bail!(
                "development matrix must contain exactly 3 profiles, 2 ordered templates, and 3 architectures"
            );
        }
        let architectures = self.architectures.iter().copied().collect::<BTreeSet<_>>();
        if architectures
            != BTreeSet::from([
                RecallCandidateArchitecture::SemanticOnly,
                RecallCandidateArchitecture::LexicalRerank,
                RecallCandidateArchitecture::LexicalSemanticUnion,
            ])
        {
            bail!("development matrix must cover all candidate architectures exactly once");
        }
        if self.parameters.lexical_candidates == 0
            || self.parameters.semantic_candidates == 0
            || self.parameters.final_results == 0
            || ![
                self.parameters.similarity_cutoff,
                self.parameters.lexical_weight,
                self.parameters.semantic_weight,
                self.parameters.reciprocal_rank_k,
            ]
            .iter()
            .all(|v| v.is_finite())
        {
            bail!("invalid matrix parameters");
        }
        Ok(())
    }

    pub fn candidate_count(&self) -> Result<usize> {
        self.validate()?;
        Ok(self.profiles.len() * self.templates.len() * self.architectures.len())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallDevelopmentOutcome {
    Completed,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallDevelopmentAttemptV2 {
    pub attempted_at: String,
    pub candidate_id: String,
    pub candidate_digest: String,
    pub profile_id: String,
    pub template: String,
    pub architecture: RecallCandidateArchitecture,
    pub outcome: RecallDevelopmentOutcome,
    pub reason_code: Option<String>,
    pub report: Option<RecallV3CandidateReport>,
    pub report_digest: Option<String>,
    pub artifact_digest: Option<String>,
    pub environment_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallDevelopmentLogV2 {
    pub version: String,
    pub corpus_digest: String,
    pub judgment_digest: String,
    pub matrix_digest: String,
    pub runner_digest: String,
    pub attempts: Vec<RecallDevelopmentAttemptV2>,
}

impl RecallDevelopmentLogV2 {
    pub fn validate(&self) -> Result<()> {
        if self.version != RECALL_DEVELOPMENT_LOG_V2
            || [
                self.corpus_digest.as_str(),
                self.judgment_digest.as_str(),
                self.matrix_digest.as_str(),
                self.runner_digest.as_str(),
            ]
            .iter()
            .any(|v| v.trim().is_empty())
            || self.attempts.is_empty()
        {
            bail!("invalid development log identity");
        }
        for attempt in &self.attempts {
            let complete = attempt.report.is_some()
                && attempt
                    .report_digest
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                && attempt
                    .artifact_digest
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                && attempt.reason_code.is_none();
            let unsuccessful = attempt.report.is_none()
                && attempt.report_digest.is_none()
                && attempt.artifact_digest.is_none()
                && attempt
                    .reason_code
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty());
            if crate::FixedClock::from_rfc3339(&attempt.attempted_at).is_err()
                || attempt.candidate_id.trim().is_empty()
                || attempt.candidate_digest.trim().is_empty()
                || attempt.profile_id.trim().is_empty()
                || !matches!(
                    attempt.template.as_str(),
                    TITLE_BODY_TEMPLATE | TYPE_TITLE_BODY_TEMPLATE
                )
                || attempt.environment_digest.trim().is_empty()
                || match attempt.outcome {
                    RecallDevelopmentOutcome::Completed => !complete,
                    _ => !unsuccessful,
                }
            {
                bail!("invalid development attempt {:?}", attempt.candidate_id);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallFrozenCandidate {
    pub architecture: String,
    pub candidate_id: String,
    pub candidate_digest: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallDevelopmentFreeze {
    pub version: String,
    pub frozen_at: String,
    pub corpus_digest: String,
    pub judgment_digest: String,
    pub matrix_digest: String,
    pub runner_digest: String,
    pub log_digest: String,
    pub attempts_digest: String,
    pub finalists: Vec<RecallFrozenCandidate>,
    pub rejected: Vec<String>,
}

pub fn freeze_development(
    log: &RecallDevelopmentLogV2,
    frozen_at: &str,
) -> Result<RecallDevelopmentFreeze> {
    log.validate()?;
    crate::FixedClock::from_rfc3339(frozen_at).context("freeze timestamp must be valid RFC3339")?;
    let lexical_manifest = crate::recall_v3_lexical_candidate_manifest();
    let mut finalists = vec![RecallFrozenCandidate {
        architecture: "lexical_baseline".into(),
        candidate_id: lexical_manifest.id.clone(),
        candidate_digest: canonical_digest(&lexical_manifest)?,
        reason: "required baseline".into(),
    }];
    for architecture in [
        RecallCandidateArchitecture::SemanticOnly,
        RecallCandidateArchitecture::LexicalRerank,
        RecallCandidateArchitecture::LexicalSemanticUnion,
    ] {
        let best = log
            .attempts
            .iter()
            .filter(|a| {
                a.architecture == architecture
                    && a.outcome == RecallDevelopmentOutcome::Completed
                    && a.report.as_ref().is_some_and(|r| r.passed)
            })
            .min_by(|a, b| compare_attempts(a, b))
            .with_context(|| format!("no trust-safe completed finalist for {architecture:?}"))?;
        finalists.push(RecallFrozenCandidate {
            architecture: architecture_identifier(architecture).into(),
            candidate_id: best.candidate_id.clone(),
            candidate_digest: best.candidate_digest.clone(),
            reason: "highest development ranking under the documented deterministic tie-break"
                .into(),
        });
    }
    let selected = finalists
        .iter()
        .map(|f| f.candidate_id.as_str())
        .collect::<BTreeSet<_>>();
    let rejected = log
        .attempts
        .iter()
        .filter(|a| !selected.contains(a.candidate_id.as_str()))
        .map(|a| a.candidate_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(RecallDevelopmentFreeze {
        version: RECALL_FREEZE_VERSION.into(),
        frozen_at: frozen_at.into(),
        corpus_digest: log.corpus_digest.clone(),
        judgment_digest: log.judgment_digest.clone(),
        matrix_digest: log.matrix_digest.clone(),
        runner_digest: log.runner_digest.clone(),
        log_digest: canonical_digest(log)?,
        attempts_digest: canonical_digest(&log.attempts)?,
        finalists,
        rejected,
    })
}

fn architecture_identifier(architecture: RecallCandidateArchitecture) -> &'static str {
    match architecture {
        RecallCandidateArchitecture::SemanticOnly => "semantic_only",
        RecallCandidateArchitecture::LexicalRerank => "lexical_rerank",
        RecallCandidateArchitecture::LexicalSemanticUnion => "lexical_semantic_union",
    }
}

pub fn validate_development_report(report: &RecallV3Report) -> Result<()> {
    if report.corpus.kind != RecallV3CorpusKind::Development
        || report.network_required
        || !report.isolated_state
    {
        bail!("development reports must use isolated development data without network access");
    }
    Ok(())
}

#[cfg(feature = "recall-models")]
pub struct LocalRecallEmbedder {
    profile: RecallModelProfile,
    model: fastembed::TextEmbedding,
}

#[cfg(feature = "recall-models")]
impl LocalRecallEmbedder {
    pub fn load(profile: RecallModelProfile, directory: &Path) -> Result<Self> {
        inspect_recall_model(&profile, directory)?;
        let read = |name: &str| regular_file_bytes(&directory.join(name));
        let tokenizer = fastembed::TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: read("special_tokens_map.json")?,
            tokenizer_config_file: read("tokenizer_config.json")?,
        };
        let pooling = match profile.pooling.as_str() {
            "cls" => fastembed::Pooling::Cls,
            "mean" => fastembed::Pooling::Mean,
            other => bail!("unsupported pooling method {other:?}"),
        };
        let definition = fastembed::UserDefinedEmbeddingModel::new(read("model.onnx")?, tokenizer)
            .with_pooling(pooling);
        let options = fastembed::InitOptionsUserDefined::new().with_max_length(profile.max_length);
        let model = fastembed::TextEmbedding::try_new_from_user_defined(definition, options)?;
        Ok(Self { profile, model })
    }

    pub fn embed_queries(&mut self, values: &[String], batch_size: usize) -> Result<Vec<Vec<f64>>> {
        let prefixed = values
            .iter()
            .map(|value| format!("{}{}", self.profile.query_prefix, value))
            .collect::<Vec<_>>();
        self.embed(&prefixed, batch_size)
    }

    pub fn embed_documents(
        &mut self,
        values: &[String],
        batch_size: usize,
    ) -> Result<Vec<Vec<f64>>> {
        let prefixed = values
            .iter()
            .map(|value| format!("{}{}", self.profile.document_prefix, value))
            .collect::<Vec<_>>();
        self.embed(&prefixed, batch_size)
    }

    fn embed(&mut self, values: &[String], batch_size: usize) -> Result<Vec<Vec<f64>>> {
        if batch_size == 0 {
            bail!("embedding batch size must be non-zero");
        }
        let vectors = self.model.embed(values, Some(batch_size))?;
        if vectors.len() != values.len() {
            bail!("embedding result count does not match input count");
        }
        vectors
            .into_iter()
            .map(|vector| {
                if vector.len() != self.profile.dimensions || vector.iter().any(|v| !v.is_finite())
                {
                    bail!("embedding result has invalid dimensions or values");
                }
                let norm = vector
                    .iter()
                    .map(|v| (*v as f64) * (*v as f64))
                    .sum::<f64>()
                    .sqrt();
                if norm == 0.0 {
                    bail!("embedding result has zero norm");
                }
                Ok(vector.into_iter().map(|v| v as f64 / norm).collect())
            })
            .collect()
    }
}

#[cfg(feature = "recall-models")]
impl RecallEmbedder for LocalRecallEmbedder {
    fn embed_queries(&mut self, values: &[String], batch_size: usize) -> Result<Vec<Vec<f64>>> {
        LocalRecallEmbedder::embed_queries(self, values, batch_size)
    }

    fn embed_documents(&mut self, values: &[String], batch_size: usize) -> Result<Vec<Vec<f64>>> {
        LocalRecallEmbedder::embed_documents(self, values, batch_size)
    }
}

fn compare_attempts(
    a: &RecallDevelopmentAttemptV2,
    b: &RecallDevelopmentAttemptV2,
) -> std::cmp::Ordering {
    let ar = a.report.as_ref().unwrap();
    let br = b.report.as_ref().unwrap();
    br.aggregate
        .mean_ndcg_at_10
        .total_cmp(&ar.aggregate.mean_ndcg_at_10)
        .then_with(|| combined_slice(br).total_cmp(&combined_slice(ar)))
        .then_with(|| {
            br.aggregate
                .mean_recall_at_k
                .total_cmp(&ar.aggregate.mean_recall_at_k)
        })
        .then_with(|| br.aggregate.mean_mrr.total_cmp(&ar.aggregate.mean_mrr))
        .then_with(|| {
            ar.aggregate
                .latency_p95_ms
                .total_cmp(&br.aggregate.latency_p95_ms)
        })
        .then_with(|| a.candidate_digest.cmp(&b.candidate_digest))
}

fn combined_slice(report: &RecallV3CandidateReport) -> f64 {
    ["paraphrase", "zero_overlap"]
        .iter()
        .filter_map(|s| report.per_slice.get(*s))
        .map(|m| m.mean_recall_at_k)
        .sum()
}

fn validate_vectors(vectors: &[Vec<f64>], count: usize, dimensions: usize) -> Result<()> {
    if vectors.len() != count
        || vectors.iter().any(|vector| {
            vector.len() != dimensions || vector.iter().any(|value| !value.is_finite())
        })
    {
        bail!("embedding result count, dimensions, or values are invalid");
    }
    Ok(())
}

fn normalize_lf(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}
fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn url_origin(url: &str) -> Result<String> {
    let (scheme, rest) = url
        .split_once("://")
        .context("model URL must be absolute")?;
    if scheme != "https" {
        bail!("model URL must use https");
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        bail!("invalid model URL authority");
    }
    Ok(format!("https://{authority}"))
}
fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("path must be a scoped relative path: {}", path.display());
    }
    Ok(())
}
fn verify_regular_file(path: &Path, expected: &RecallModelFile) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{} must be a regular non-symlink file", path.display());
    }
    if metadata.len() != expected.bytes {
        bail!(
            "installed model file has the wrong size: {}",
            path.display()
        );
    }
    let mut reader = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if format!("{:x}", hasher.finalize()) != expected.sha256.to_ascii_lowercase() {
        bail!("installed model file digest mismatch: {}", path.display());
    }
    Ok(())
}
fn regular_file_bytes(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{} must be a regular non-symlink file", path.display());
    }
    fs::read(path).with_context(|| format!("failed to read {}", path.display()))
}
fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn write_verified_stream(
    path: &Path,
    reader: &mut impl Read,
    expected: &RecallModelFile,
) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .context("model file size overflow")?;
        if total > expected.bytes {
            bail!(
                "download exceeded declared size for {}",
                expected.path.display()
            );
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
    }
    let digest = format!("{:x}", hasher.finalize());
    if total != expected.bytes || digest != expected.sha256.to_ascii_lowercase() {
        bail!(
            "download verification failed for {}",
            expected.path.display()
        );
    }
    file.sync_all()?;
    Ok(())
}
fn collect_files(root: &Path, current: &Path, output: &mut BTreeSet<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            bail!("model install cannot contain symlinks");
        }
        if metadata.is_dir() {
            collect_files(root, &entry.path(), output)?;
        } else if metadata.is_file() {
            output.insert(entry.path().strip_prefix(root)?.to_owned());
        } else {
            bail!("model install contains a special file");
        }
    }
    Ok(())
}
fn canonical_json(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(serde_json_canonicalizer::to_vec(value)?)
}
fn canonical_digest(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&canonical_json(value)?).to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        MemoryCitation, MemoryDestination, MemoryPlane, MemoryType, ScopeKind, Visibility,
    };

    #[test]
    fn templates_are_exact_and_normalize_line_endings() {
        let record = RecallV3CandidateRecord {
            id: "r".into(),
            title: "Deploy\r\nnow".into(),
            body: "Step 1\rStep 2".into(),
            citation: MemoryCitation {
                record_id: "r".into(),
                memory_type: MemoryType::Procedure,
                scope_kind: ScopeKind::Repo,
                provenance: MemoryPlane::Git,
                destination: MemoryDestination::Repo,
                visibility: Visibility::Repo,
                source_kind: None,
                source_ref: None,
                path: None,
                capture: None,
            },
        };
        assert_eq!(
            render_recall_document(TITLE_BODY_TEMPLATE, &record).unwrap(),
            "title: Deploy\nnow\nbody: Step 1\nStep 2\n"
        );
        assert_eq!(
            render_recall_document(TYPE_TITLE_BODY_TEMPLATE, &record).unwrap(),
            "type: procedure\ntitle: Deploy\nnow\nbody: Step 1\nStep 2\n"
        );
    }

    #[test]
    fn matrix_requires_exactly_eighteen_candidates() {
        let matrix = RecallDevelopmentMatrix {
            version: RECALL_MATRIX_VERSION.into(),
            profiles: vec!["a".into(), "b".into(), "c".into()],
            templates: vec![TITLE_BODY_TEMPLATE.into(), TYPE_TITLE_BODY_TEMPLATE.into()],
            architectures: vec![
                RecallCandidateArchitecture::SemanticOnly,
                RecallCandidateArchitecture::LexicalRerank,
                RecallCandidateArchitecture::LexicalSemanticUnion,
            ],
            parameters: RecallMatrixParameters {
                lexical_candidates: 20,
                semantic_candidates: 20,
                final_results: 10,
                similarity_cutoff: 0.0,
                lexical_weight: 1.0,
                semantic_weight: 1.0,
                reciprocal_rank_k: 60.0,
            },
        };
        assert_eq!(matrix.candidate_count().unwrap(), 18);
    }

    #[test]
    fn installer_verifies_and_reuses_an_exact_install() {
        let root = tempfile::tempdir().unwrap();
        let names = [
            "model.onnx",
            "tokenizer.json",
            "config.json",
            "special_tokens_map.json",
            "tokenizer_config.json",
        ];
        let files = names
            .iter()
            .map(|name| {
                let bytes = name.as_bytes();
                RecallModelFile {
                    path: (*name).into(),
                    url: format!("https://models.example/{name}"),
                    sha256: format!("{:x}", Sha256::digest(bytes)),
                    bytes: bytes.len() as u64,
                }
            })
            .collect::<Vec<_>>();
        let profile = RecallModelProfile {
            version: RECALL_MODEL_PROFILE_VERSION.into(),
            id: "fixture".into(),
            repository: "fixture/model".into(),
            revision: "abc123".into(),
            license: "MIT".into(),
            dimensions: 3,
            pooling: "mean".into(),
            normalization: "l2".into(),
            query_prefix: String::new(),
            document_prefix: String::new(),
            max_length: 32,
            allowed_origins: vec!["https://models.example".into()],
            files,
        };
        let mut unsupported = profile.clone();
        unsupported.pooling = "maximum".into();
        assert!(unsupported.validate().is_err());
        unsupported = profile.clone();
        unsupported.normalization = "none".into();
        assert!(unsupported.validate().is_err());
        let mut fetches = 0;
        let installed = install_recall_model_with(&profile, root.path(), false, |url| {
            fetches += 1;
            Ok(std::io::Cursor::new(
                url.rsplit('/').next().unwrap().as_bytes().to_vec(),
            ))
        })
        .unwrap();
        assert_eq!(fetches, 5);
        install_recall_model_with(
            &profile,
            root.path(),
            false,
            |_| -> Result<std::io::Cursor<Vec<u8>>> { panic!("valid install must not fetch") },
        )
        .unwrap();
        fs::write(installed.join("config.json"), b"tampered").unwrap();
        assert!(inspect_recall_model(&profile, &installed).is_err());

        let mut oversized = profile.clone();
        oversized.id = "oversized-fixture".into();
        assert!(
            install_recall_model_with(&oversized, root.path(), false, |url| {
                let mut bytes = url.rsplit('/').next().unwrap().as_bytes().to_vec();
                bytes.push(0);
                Ok(std::io::Cursor::new(bytes))
            })
            .is_err()
        );
        assert!(!root.path().join("oversized-fixture").exists());
    }

    #[test]
    fn checked_in_profiles_and_matrix_are_strict_and_complete() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/v3");
        for profile in [
            "bge-small-en-v1.5.json",
            "multilingual-e5-small.json",
            "snowflake-arctic-embed-m-v2.0.json",
        ] {
            RecallModelProfile::load(root.join("profiles").join(profile)).unwrap();
        }
        let matrix: RecallDevelopmentMatrix =
            serde_json::from_slice(&fs::read(root.join("development-matrix.json")).unwrap())
                .unwrap();
        assert_eq!(matrix.candidate_count().unwrap(), 18);

        let corpus: crate::RecallV3Corpus =
            serde_yaml::from_slice(&fs::read(root.join("corpus.yaml")).unwrap()).unwrap();
        assert_eq!(corpus.cases.len(), 36);
        for required in [
            "paraphrase",
            "zero_overlap",
            "synonyms_abbreviation",
            "identifiers",
            "negation",
            "temporal_conflict",
            "procedure",
            "causal",
            "hard_negatives",
            "path",
            "scope",
            "privacy",
            "destination",
            "lifecycle",
            "ambiguous_short",
            "multi_relevant",
        ] {
            assert!(
                corpus
                    .cases
                    .iter()
                    .filter(|case| case.slices.iter().any(|slice| slice == required))
                    .count()
                    >= 3,
                "development slice {required:?} has fewer than three cases"
            );
        }
    }

    #[test]
    fn development_log_rejects_partial_unsuccessful_attempts() {
        let attempt = RecallDevelopmentAttemptV2 {
            attempted_at: "2026-07-12T12:00:00Z".into(),
            candidate_id: "candidate-a".into(),
            candidate_digest: "candidate-digest".into(),
            profile_id: "profile-a".into(),
            template: TITLE_BODY_TEMPLATE.into(),
            architecture: RecallCandidateArchitecture::SemanticOnly,
            outcome: RecallDevelopmentOutcome::Rejected,
            reason_code: Some("quality_gate".into()),
            report: None,
            report_digest: None,
            artifact_digest: None,
            environment_digest: "environment-digest".into(),
        };
        let log = RecallDevelopmentLogV2 {
            version: RECALL_DEVELOPMENT_LOG_V2.into(),
            corpus_digest: "corpus-digest".into(),
            judgment_digest: "judgment-digest".into(),
            matrix_digest: "matrix-digest".into(),
            runner_digest: "runner-digest".into(),
            attempts: vec![attempt],
        };
        log.validate().unwrap();
        assert!(
            freeze_development(&log, "not-a-timestamp")
                .unwrap_err()
                .to_string()
                .contains("RFC3339")
        );
        assert_eq!(
            architecture_identifier(RecallCandidateArchitecture::SemanticOnly),
            "semantic_only"
        );
        assert_eq!(
            architecture_identifier(RecallCandidateArchitecture::LexicalRerank),
            "lexical_rerank"
        );
        assert_eq!(
            architecture_identifier(RecallCandidateArchitecture::LexicalSemanticUnion),
            "lexical_semantic_union"
        );
        let lexical_manifest = crate::recall_v3_lexical_candidate_manifest();
        assert_eq!(lexical_manifest.id, "lexical-baseline");
        assert_ne!(
            canonical_digest(&lexical_manifest).unwrap(),
            log.runner_digest
        );

        let mut malformed = log.clone();
        malformed.attempts[0].artifact_digest = Some("unexpected-artifact".into());
        assert!(malformed.validate().is_err());
        malformed = log.clone();
        malformed.attempts[0].attempted_at = String::new();
        assert!(malformed.validate().is_err());
        malformed = log;
        malformed.attempts[0].template = "unknown/v1".into();
        assert!(malformed.validate().is_err());
    }
}
