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
pub const RECALL_DEVELOPMENT_ENVIRONMENT_VERSION: &str = "memzoi-recall-development-environment/v1";
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
    pub provenance: RecallModelProvenance,
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
pub struct RecallModelProvenance {
    pub upstream_repository: String,
    pub upstream_revision: String,
    pub artifact_repository: String,
    pub artifact_revision: String,
    pub conversion: String,
    pub upstream_license: String,
    pub artifact_license: String,
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
            || [
                self.provenance.upstream_repository.as_str(),
                self.provenance.upstream_revision.as_str(),
                self.provenance.artifact_repository.as_str(),
                self.provenance.artifact_revision.as_str(),
                self.provenance.conversion.as_str(),
                self.provenance.upstream_license.as_str(),
                self.provenance.artifact_license.as_str(),
            ]
            .iter()
            .any(|value| value.trim().is_empty())
            || self.repository != self.provenance.artifact_repository
            || self.revision != self.provenance.artifact_revision
            || self.license != self.provenance.artifact_license
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
    let backup = model_root.join(format!(".{}.backup-{}", profile.id, uuid::Uuid::now_v7()));
    let had_previous = destination.exists();
    if had_previous {
        let metadata = fs::symlink_metadata(&destination)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("existing model destination must be a non-symlink directory");
        }
        fs::rename(&destination, &backup)?;
    }
    // TempDir::keep returns the retained directory path (NamedTempFile::keep is
    // the tempfile API that returns a Result).
    let staged: PathBuf = staging.keep();
    if let Err(error) = fs::rename(&staged, &destination) {
        if had_previous {
            fs::rename(&backup, &destination)
                .context("failed to restore the previous model after promotion failed")?;
        }
        return Err(error).context("failed to promote verified model installation");
    }
    if let Err(error) = inspect_recall_model(profile, &destination) {
        fs::remove_dir_all(&destination)
            .context("failed to remove invalid promoted model installation")?;
        if had_previous {
            fs::rename(&backup, &destination)
                .context("failed to restore the previous model after verification failed")?;
        }
        return Err(error).context("promoted model installation failed verification");
    }
    if had_previous {
        fs::remove_dir_all(&backup)?;
    }
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
    let mut expected_files = profile
        .files
        .iter()
        .map(|file| RecallInstalledFile {
            path: file.path.clone(),
            sha256: file.sha256.clone(),
            bytes: file.bytes,
        })
        .collect::<Vec<_>>();
    expected_files.sort_by(|left, right| left.path.cmp(&right.path));
    if manifest.files != expected_files {
        bail!("model install file metadata does not match profile");
    }
    let expected = expected_files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
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
            record.citation.memory_type.as_str()
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEmbeddingCorpus {
    pub inputs: RecallEmbeddingInputs,
    pub corpus_digest: String,
    pub judgment_digest: String,
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
    pub lexical_rerank_fusion: crate::RecallFusionMethod,
    pub lexical_semantic_union_fusion: crate::RecallFusionMethod,
    pub path_weight: f64,
    pub type_weight: f64,
    pub lane_weight: f64,
    pub destination_weight: f64,
    pub confidence_weight: f64,
    pub tie_break: String,
    pub batch_size: usize,
    pub threads: usize,
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
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let matrix: Self = serde_json::from_slice(&regular_file_bytes(path.as_ref())?)
            .with_context(|| format!("invalid development matrix {}", path.as_ref().display()))?;
        matrix.validate()?;
        Ok(matrix)
    }

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
                self.parameters.path_weight,
                self.parameters.type_weight,
                self.parameters.lane_weight,
                self.parameters.destination_weight,
                self.parameters.confidence_weight,
            ]
            .iter()
            .all(|v| v.is_finite())
        {
            bail!("invalid matrix parameters");
        }
        if self.parameters.lexical_rerank_fusion != crate::RecallFusionMethod::WeightedSum
            || [
                self.parameters.path_weight,
                self.parameters.type_weight,
                self.parameters.lane_weight,
                self.parameters.destination_weight,
                self.parameters.confidence_weight,
            ]
            .iter()
            .any(|weight| *weight != 0.0)
            || self.parameters.tie_break != "record_id_ascending"
            || self.parameters.batch_size == 0
            || self.parameters.threads == 0
        {
            bail!("matrix execution and ranking parameters are invalid");
        }
        Ok(())
    }

    pub fn candidate_count(&self) -> Result<usize> {
        self.validate()?;
        Ok(self.profiles.len() * self.templates.len() * self.architectures.len())
    }
}

pub fn recall_development_matrix_digest(matrix: &RecallDevelopmentMatrix) -> Result<String> {
    matrix.validate()?;
    canonical_digest(matrix)
}

pub fn recall_model_install_digest(install: &RecallModelInstallManifest) -> Result<String> {
    canonical_digest(install)
}

pub fn recall_candidate_manifest_digest(
    manifest: &crate::RecallRetrievalCandidateManifest,
) -> Result<String> {
    canonical_digest(manifest)
}

pub fn recall_candidate_report_digest(report: &RecallV3CandidateReport) -> Result<String> {
    canonical_digest(report)
}

pub struct RecallCandidateArtifactBinding<'a> {
    pub artifact: &'a crate::RecallVectorArtifact,
    pub digest: &'a str,
    pub path: PathBuf,
}

pub fn build_recall_candidate_manifest(
    profile: &RecallModelProfile,
    template: &str,
    architecture: RecallCandidateArchitecture,
    parameters: &RecallMatrixParameters,
    environment: crate::RecallCandidateEnvironment,
    binding: RecallCandidateArtifactBinding<'_>,
) -> Result<crate::RecallRetrievalCandidateManifest> {
    validate_relative_path(&binding.path)?;
    let artifact = binding.artifact;
    let fusion = match architecture {
        RecallCandidateArchitecture::SemanticOnly => crate::RecallFusionMethod::WeightedSum,
        RecallCandidateArchitecture::LexicalRerank => parameters.lexical_rerank_fusion,
        RecallCandidateArchitecture::LexicalSemanticUnion => {
            parameters.lexical_semantic_union_fusion
        }
    };
    Ok(crate::RecallRetrievalCandidateManifest {
        version: crate::RECALL_CANDIDATE_MANIFEST_VERSION.into(),
        id: recall_candidate_id(&profile.id, template, architecture),
        revision: "development-v1".into(),
        model: crate::RecallModelIdentity {
            id: profile.repository.clone(),
            revision: profile.revision.clone(),
            artifact_digest: artifact.model_artifact_digest.clone(),
            license: profile.license.clone(),
            dimensions: profile.dimensions,
            pooling: profile.pooling.clone(),
            normalization: profile.normalization.clone(),
            query_prefix: profile.query_prefix.clone(),
            document_prefix: profile.document_prefix.clone(),
            explicitly_installed: true,
            offline: true,
        },
        document: crate::RecallEmbeddingDocument {
            version: "memzoi-recall-document/v1".into(),
            template: template.into(),
            digest: artifact.document_digest.clone(),
        },
        retrieval: crate::RecallCandidateRetrieval {
            architecture,
            lexical_candidates: parameters.lexical_candidates,
            semantic_candidates: parameters.semantic_candidates,
            final_results: parameters.final_results,
            similarity_cutoff: parameters.similarity_cutoff,
            fusion,
            lexical_weight: parameters.lexical_weight,
            semantic_weight: parameters.semantic_weight,
            reciprocal_rank_k: parameters.reciprocal_rank_k,
            path_weight: parameters.path_weight,
            type_weight: parameters.type_weight,
            lane_weight: parameters.lane_weight,
            destination_weight: parameters.destination_weight,
            confidence_weight: parameters.confidence_weight,
            tie_break: parameters.tie_break.clone(),
        },
        storage: crate::RecallCandidateStorage {
            profile_id: profile.id.clone(),
            generation: artifact.generation.clone(),
            vector_artifact: binding.path,
            vector_artifact_digest: binding.digest.into(),
            content_fingerprint: artifact.content_fingerprint.clone(),
            exact_search: true,
            destination: crate::MemoryDestination::Repo,
        },
        environment,
    })
}

pub fn recall_candidate_id(
    profile_id: &str,
    template: &str,
    architecture: RecallCandidateArchitecture,
) -> String {
    format!(
        "{}-{}-{}",
        profile_id,
        template.replace('/', "-"),
        architecture_identifier(architecture)
    )
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
    pub attempt_id: String,
    pub attempted_at: String,
    pub candidate_id: String,
    pub candidate_digest: Option<String>,
    pub profile_id: String,
    pub template: String,
    pub architecture: RecallCandidateArchitecture,
    pub candidate_manifest: Option<PathBuf>,
    pub vector_artifact: Option<PathBuf>,
    pub outcome: RecallDevelopmentOutcome,
    pub reason_code: Option<String>,
    pub report: Option<RecallV3CandidateReport>,
    pub report_digest: Option<String>,
    pub artifact_digest: Option<String>,
    pub environment_digest: String,
    pub trust_eligible: bool,
    pub development_quality_passed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallDevelopmentLogV2 {
    pub version: String,
    pub corpus_digest: String,
    pub judgment_digest: String,
    pub matrix_digest: String,
    pub metrics_digest: String,
    pub runner_digest: String,
    pub selected_attempt_ids: Vec<String>,
    pub attempts: Vec<RecallDevelopmentAttemptV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallDevelopmentEnvironment {
    pub version: String,
    pub git_commit: String,
    pub cargo_lock_digest: String,
    pub rustc_version: String,
    pub target_triple: String,
    pub target_os: String,
    pub target_arch: String,
    pub build_profile: String,
    pub fastembed_version: String,
    pub ort_version: String,
    pub cpu_model: String,
    pub cpu_features: Vec<String>,
    pub embedding_threads: usize,
    pub network_mode: String,
    pub model_install_digests: BTreeMap<String, Option<String>>,
}

impl RecallDevelopmentEnvironment {
    pub fn validate(&self) -> Result<()> {
        let mut sorted_features = self.cpu_features.clone();
        sorted_features.sort();
        sorted_features.dedup();
        if self.version != RECALL_DEVELOPMENT_ENVIRONMENT_VERSION
            || [
                self.git_commit.as_str(),
                self.cargo_lock_digest.as_str(),
                self.rustc_version.as_str(),
                self.target_triple.as_str(),
                self.target_os.as_str(),
                self.target_arch.as_str(),
                self.build_profile.as_str(),
                self.fastembed_version.as_str(),
                self.ort_version.as_str(),
                self.cpu_model.as_str(),
            ]
            .iter()
            .any(|value| value.trim().is_empty())
            || self.embedding_threads == 0
            || self.network_mode != "application_offline"
            || self.model_install_digests.len() != 3
            || self.model_install_digests.iter().any(|(profile, digest)| {
                profile.trim().is_empty()
                    || digest
                        .as_deref()
                        .is_some_and(|value| value.trim().is_empty())
            })
            || sorted_features != self.cpu_features
        {
            bail!("invalid recall development environment");
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String> {
        self.validate()?;
        canonical_digest(self)
    }
}

impl RecallDevelopmentLogV2 {
    pub fn validate(&self) -> Result<()> {
        if self.version != RECALL_DEVELOPMENT_LOG_V2
            || [
                self.corpus_digest.as_str(),
                self.judgment_digest.as_str(),
                self.matrix_digest.as_str(),
                self.metrics_digest.as_str(),
                self.runner_digest.as_str(),
            ]
            .iter()
            .any(|v| v.trim().is_empty())
            || self.attempts.is_empty()
        {
            bail!("invalid development log identity");
        }
        let attempt_ids = self
            .attempts
            .iter()
            .map(|attempt| attempt.attempt_id.as_str())
            .collect::<BTreeSet<_>>();
        let selected_ids = self
            .selected_attempt_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if attempt_ids.len() != self.attempts.len()
            || selected_ids.len() != self.selected_attempt_ids.len()
            || !selected_ids.is_subset(&attempt_ids)
        {
            bail!("development attempt identities are invalid");
        }
        for attempt in &self.attempts {
            let complete = attempt.report.is_some()
                && attempt
                    .candidate_digest
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                && attempt
                    .candidate_manifest
                    .as_deref()
                    .is_some_and(|path| validate_relative_path(path).is_ok())
                && attempt
                    .vector_artifact
                    .as_deref()
                    .is_some_and(|path| validate_relative_path(path).is_ok())
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
                && attempt.candidate_manifest.is_none()
                && attempt.vector_artifact.is_none()
                && attempt.report_digest.is_none()
                && attempt.artifact_digest.is_none()
                && attempt
                    .reason_code
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty());
            if crate::FixedClock::from_rfc3339(&attempt.attempted_at).is_err()
                || attempt.attempt_id.trim().is_empty()
                || attempt.candidate_id.trim().is_empty()
                || attempt.profile_id.trim().is_empty()
                || !matches!(
                    attempt.template.as_str(),
                    TITLE_BODY_TEMPLATE | TYPE_TITLE_BODY_TEMPLATE
                )
                || !is_sha256(&attempt.environment_digest)
                || (attempt.outcome != RecallDevelopmentOutcome::Completed
                    && (attempt.trust_eligible || attempt.development_quality_passed))
                || match attempt.outcome {
                    RecallDevelopmentOutcome::Completed => !complete,
                    _ => !unsuccessful || attempt.candidate_digest.is_some(),
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
    pub attempt_id: Option<String>,
    pub candidate_id: String,
    pub candidate_digest: String,
    pub reason: String,
    pub development_quality_passed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallDevelopmentFreeze {
    pub version: String,
    pub frozen_at: String,
    pub corpus_digest: String,
    pub judgment_digest: String,
    pub matrix_digest: String,
    pub metrics_digest: String,
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
        attempt_id: None,
        candidate_id: lexical_manifest.id.clone(),
        candidate_digest: canonical_digest(&lexical_manifest)?,
        reason: "required baseline".into(),
        development_quality_passed: true,
    }];
    let selected_attempt_ids = log
        .selected_attempt_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
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
                    && selected_attempt_ids.contains(a.attempt_id.as_str())
                    && a.outcome == RecallDevelopmentOutcome::Completed
                    && a.trust_eligible
            })
            .min_by(|a, b| compare_attempts(a, b))
            .with_context(|| format!("no trust-safe completed finalist for {architecture:?}"))?;
        finalists.push(RecallFrozenCandidate {
            architecture: architecture_identifier(architecture).into(),
            attempt_id: Some(best.attempt_id.clone()),
            candidate_id: best.candidate_id.clone(),
            candidate_digest: best.candidate_digest.clone().unwrap(),
            reason: "highest development ranking under the documented deterministic tie-break"
                .into(),
            development_quality_passed: best.development_quality_passed,
        });
    }
    let selected = finalists
        .iter()
        .map(|f| f.candidate_id.as_str())
        .collect::<BTreeSet<_>>();
    let rejected = log
        .attempts
        .iter()
        .filter(|a| {
            selected_attempt_ids.contains(a.attempt_id.as_str())
                && !selected.contains(a.candidate_id.as_str())
        })
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
        metrics_digest: log.metrics_digest.clone(),
        runner_digest: log.runner_digest.clone(),
        log_digest: canonical_digest(log)?,
        attempts_digest: canonical_digest(&log.attempts)?,
        finalists,
        rejected,
    })
}

pub fn recall_candidate_trust_eligible(report: &RecallV3CandidateReport) -> bool {
    report.manifest.offline
        && report.aggregate.forbidden_hits.values().sum::<usize>() == 0
        && report.aggregate.citation_integrity == 1.0
        && report.aggregate.fallback_parity == 1.0
        && report
            .cases
            .iter()
            .all(|case| case.fallback_reason.is_none())
}

pub fn verify_development_evidence(
    log: &RecallDevelopmentLogV2,
    matrix: &RecallDevelopmentMatrix,
    report: &RecallV3Report,
    evidence_root: &Path,
    current_corpus: &RecallEmbeddingCorpus,
    profiles: &BTreeMap<String, RecallModelProfile>,
    environment: &RecallDevelopmentEnvironment,
) -> Result<()> {
    log.validate()?;
    matrix.validate()?;
    environment.validate()?;
    validate_development_report(report)?;
    let environment_digest = environment.digest()?;
    let selected_attempt_ids = log
        .selected_attempt_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let selected_attempts = log
        .attempts
        .iter()
        .filter(|attempt| selected_attempt_ids.contains(attempt.attempt_id.as_str()))
        .collect::<Vec<_>>();
    for digest in log
        .attempts
        .iter()
        .map(|attempt| attempt.environment_digest.as_str())
        .collect::<BTreeSet<_>>()
    {
        let historical: RecallDevelopmentEnvironment =
            serde_json::from_slice(&regular_file_bytes(
                &evidence_root
                    .join("environments")
                    .join(format!("{digest}.json")),
            )?)?;
        if historical.digest()? != digest {
            bail!("development attempt references an invalid environment artifact");
        }
    }
    if selected_attempts.len() != 18
        || log.corpus_digest != current_corpus.corpus_digest
        || log.judgment_digest != current_corpus.judgment_digest
        || log.corpus_digest != report.digests.corpus
        || log.judgment_digest != report.digests.judgments
        || log.metrics_digest != report.digests.metrics
        || log.metrics_digest != crate::recall_v3_metrics_digest()
        || log.runner_digest != report.digests.runner
        || log.runner_digest != crate::recall_v3_runner_digest()
        || log.matrix_digest != recall_development_matrix_digest(matrix)?
        || environment.embedding_threads != matrix.parameters.threads
    {
        bail!("development evidence does not match current sources or selected matrix");
    }
    let combinations = selected_attempts
        .iter()
        .map(|attempt| {
            (
                attempt.profile_id.clone(),
                attempt.template.clone(),
                attempt.architecture,
            )
        })
        .collect::<BTreeSet<_>>();
    let expected_combinations = matrix
        .profiles
        .iter()
        .filter_map(|profile| profile.file_stem().and_then(|stem| stem.to_str()))
        .flat_map(|profile| {
            matrix.templates.iter().flat_map(move |template| {
                matrix
                    .architectures
                    .iter()
                    .copied()
                    .map(move |architecture| (profile.to_owned(), template.clone(), architecture))
            })
        })
        .collect::<BTreeSet<_>>();
    let expected_profiles = expected_combinations
        .iter()
        .map(|(profile, _, _)| profile.as_str())
        .collect::<BTreeSet<_>>();
    if combinations != expected_combinations
        || profiles.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected_profiles
        || report.candidates.len() != 19
        || selected_attempts
            .iter()
            .map(|attempt| attempt.environment_digest.as_str())
            .collect::<BTreeSet<_>>()
            != BTreeSet::from([environment_digest.as_str()])
        || environment
            .model_install_digests
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != expected_profiles
    {
        bail!("development attempts do not form one complete environment-bound matrix");
    }
    let reports = report
        .candidates
        .iter()
        .skip(1)
        .map(|candidate| (candidate.manifest.id.as_str(), candidate))
        .collect::<BTreeMap<_, _>>();
    for attempt in selected_attempts {
        if attempt.outcome != RecallDevelopmentOutcome::Completed {
            bail!("selected matrix attempts must be completed");
        }
        let manifest_path = evidence_root.join(
            attempt
                .candidate_manifest
                .as_deref()
                .context("completed attempt omitted candidate manifest")?,
        );
        let artifact_path = evidence_root.join(
            attempt
                .vector_artifact
                .as_deref()
                .context("completed attempt omitted vector artifact")?,
        );
        let manifest: crate::RecallRetrievalCandidateManifest =
            serde_json::from_slice(&regular_file_bytes(&manifest_path)?)?;
        let artifact: crate::RecallVectorArtifact =
            serde_json::from_slice(&regular_file_bytes(&artifact_path)?)?;
        let candidate_report = reports
            .get(attempt.candidate_id.as_str())
            .context("matrix report omitted a development attempt")?;
        let profile = profiles
            .get(&attempt.profile_id)
            .context("selected attempt references an unknown current profile")?;
        let expected_manifest = build_recall_candidate_manifest(
            profile,
            &attempt.template,
            attempt.architecture,
            &matrix.parameters,
            crate::RecallCandidateEnvironment {
                target_os: environment.target_os.clone(),
                target_arch: environment.target_arch.clone(),
                cpu_features: environment.cpu_features.clone(),
            },
            RecallCandidateArtifactBinding {
                artifact: &artifact,
                digest: attempt.artifact_digest.as_deref().unwrap_or_default(),
                path: manifest.storage.vector_artifact.clone(),
            },
        )?;
        let candidate_digest = attempt.candidate_digest.as_deref().unwrap_or_default();
        if manifest != expected_manifest
            || environment
                .model_install_digests
                .get(&attempt.profile_id)
                .and_then(Option::as_deref)
                != Some(artifact.model_artifact_digest.as_str())
            || recall_candidate_manifest_digest(&manifest)? != candidate_digest
            || crate::recall_vector_artifact_digest(&artifact)?
                != attempt.artifact_digest.as_deref().unwrap_or_default()
            || recall_candidate_report_digest(candidate_report)?
                != attempt.report_digest.as_deref().unwrap_or_default()
            || attempt.report.as_ref() != Some(*candidate_report)
            || candidate_report.manifest.id != attempt.candidate_id
            || candidate_report.manifest.configuration_digest != candidate_digest
            || recall_candidate_trust_eligible(candidate_report) != attempt.trust_eligible
            || candidate_report.passed != attempt.development_quality_passed
            || manifest.storage.vector_artifact_digest
                != attempt.artifact_digest.as_deref().unwrap_or_default()
        {
            bail!("development attempt evidence failed digest or trust verification");
        }
    }
    Ok(())
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
        Self::load_with_threads(profile, directory, 1)
    }

    pub fn load_with_threads(
        profile: RecallModelProfile,
        directory: &Path,
        threads: usize,
    ) -> Result<Self> {
        if threads == 0 {
            bail!("embedding thread count must be non-zero");
        }
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
        let options = fastembed::InitOptionsUserDefined::new()
            .with_max_length(profile.max_length)
            .with_intra_threads(threads);
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

        let mut failed_attempt = record.clone();
        failed_attempt.citation.memory_type = MemoryType::FailedAttempt;
        assert!(
            render_recall_document(TYPE_TITLE_BODY_TEMPLATE, &failed_attempt)
                .unwrap()
                .starts_with("type: failed_attempt\n")
        );

        let mut instruction = record;
        instruction.citation.memory_type = MemoryType::InstructionProjection;
        assert!(
            render_recall_document(TYPE_TITLE_BODY_TEMPLATE, &instruction)
                .unwrap()
                .starts_with("type: instruction_projection\n")
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
                lexical_rerank_fusion: crate::RecallFusionMethod::WeightedSum,
                lexical_semantic_union_fusion: crate::RecallFusionMethod::ReciprocalRank,
                path_weight: 0.0,
                type_weight: 0.0,
                lane_weight: 0.0,
                destination_weight: 0.0,
                confidence_weight: 0.0,
                tie_break: "record_id_ascending".into(),
                batch_size: 32,
                threads: 4,
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
            provenance: RecallModelProvenance {
                upstream_repository: "fixture/model".into(),
                upstream_revision: "abc123".into(),
                artifact_repository: "fixture/model".into(),
                artifact_revision: "abc123".into(),
                conversion: "fixture conversion v1".into(),
                upstream_license: "MIT".into(),
                artifact_license: "MIT".into(),
            },
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
        let install_manifest_path = installed.join("install.json");
        let original_manifest = fs::read(&install_manifest_path).unwrap();
        let mut tampered_manifest: RecallModelInstallManifest =
            serde_json::from_slice(&original_manifest).unwrap();
        tampered_manifest.files[0].bytes += 1;
        fs::write(
            &install_manifest_path,
            serde_json::to_vec(&tampered_manifest).unwrap(),
        )
        .unwrap();
        assert!(inspect_recall_model(&profile, &installed).is_err());
        fs::write(&install_manifest_path, original_manifest).unwrap();
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
        let queries = corpus
            .cases
            .iter()
            .map(|case| case.query.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(queries.len(), 36);
        assert!(
            queries
                .iter()
                .all(|query| !query.contains("development example"))
        );
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
        for case in &corpus.cases {
            let has_forbidden = |reason| {
                case.judgments
                    .iter()
                    .any(|judgment| judgment.forbidden_reason == Some(reason))
            };
            if case.slices.iter().any(|slice| slice == "privacy") {
                assert!(has_forbidden(crate::RecallV3ForbiddenReason::Private));
            }
            if case.slices.iter().any(|slice| slice == "destination") {
                assert!(has_forbidden(crate::RecallV3ForbiddenReason::Destination));
            }
            if case.slices.iter().any(|slice| slice == "path") {
                assert!(has_forbidden(crate::RecallV3ForbiddenReason::Path));
            }
            if case.slices.iter().any(|slice| slice == "lifecycle") {
                assert!(
                    has_forbidden(crate::RecallV3ForbiddenReason::Expired)
                        && has_forbidden(crate::RecallV3ForbiddenReason::Tombstoned)
                        && has_forbidden(crate::RecallV3ForbiddenReason::Superseded)
                );
            }
            if case.slices.iter().any(|slice| slice == "multi_relevant") {
                assert!(
                    case.judgments
                        .iter()
                        .filter(|judgment| judgment.eligible && judgment.relevance > 0)
                        .count()
                        >= 2
                );
            }
            if case.slices.iter().any(|slice| slice == "hard_negatives") {
                assert!(
                    case.judgments
                        .iter()
                        .any(|judgment| judgment.eligible && judgment.hard_negative)
                );
            }
        }

        let observed = root.join("observed");
        let log: RecallDevelopmentLogV2 =
            serde_json::from_slice(&fs::read(observed.join("development-log.json")).unwrap())
                .unwrap();
        let report: RecallV3Report =
            serde_json::from_slice(&fs::read(observed.join("matrix-report.json")).unwrap())
                .unwrap();
        let freeze: RecallDevelopmentFreeze =
            serde_json::from_slice(&fs::read(observed.join("frozen-candidates.json")).unwrap())
                .unwrap();
        assert_eq!(log.attempts.len(), 18);
        assert_eq!(report.candidates.len(), 19);
        assert_eq!(freeze.finalists.len(), 4);
        assert_eq!(freeze_development(&log, &freeze.frozen_at).unwrap(), freeze);
        let mut quality_failure = report.candidates[1].clone();
        quality_failure.passed = false;
        assert!(recall_candidate_trust_eligible(&quality_failure));
    }

    #[test]
    fn development_log_rejects_partial_unsuccessful_attempts() {
        let attempt = RecallDevelopmentAttemptV2 {
            attempt_id: "attempt-a".into(),
            attempted_at: "2026-07-12T12:00:00Z".into(),
            candidate_id: "candidate-a".into(),
            candidate_digest: None,
            profile_id: "profile-a".into(),
            template: TITLE_BODY_TEMPLATE.into(),
            architecture: RecallCandidateArchitecture::SemanticOnly,
            candidate_manifest: None,
            vector_artifact: None,
            outcome: RecallDevelopmentOutcome::Rejected,
            reason_code: Some("quality_gate".into()),
            report: None,
            report_digest: None,
            artifact_digest: None,
            environment_digest: "e".repeat(64),
            trust_eligible: false,
            development_quality_passed: false,
        };
        let log = RecallDevelopmentLogV2 {
            version: RECALL_DEVELOPMENT_LOG_V2.into(),
            corpus_digest: "corpus-digest".into(),
            judgment_digest: "judgment-digest".into(),
            matrix_digest: "matrix-digest".into(),
            metrics_digest: "metrics-digest".into(),
            runner_digest: "runner-digest".into(),
            selected_attempt_ids: Vec::new(),
            attempts: vec![attempt],
        };
        log.validate().unwrap();
        let mut unknown_selection = log.clone();
        unknown_selection.selected_attempt_ids = vec!["missing-attempt".into()];
        assert!(unknown_selection.validate().is_err());
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
        malformed = log.clone();
        malformed.attempts[0].environment_digest = "../escape".into();
        assert!(malformed.validate().is_err());
        malformed = log;
        malformed.attempts[0].template = "unknown/v1".into();
        assert!(malformed.validate().is_err());
    }

    #[test]
    fn development_environment_digest_binds_reproducibility_fields() {
        let environment = RecallDevelopmentEnvironment {
            version: RECALL_DEVELOPMENT_ENVIRONMENT_VERSION.into(),
            git_commit: "abc123".into(),
            cargo_lock_digest: "cargo-lock".into(),
            rustc_version: "rustc 1.97.0".into(),
            target_triple: "aarch64-apple-darwin".into(),
            target_os: "macos".into(),
            target_arch: "aarch64".into(),
            build_profile: "release".into(),
            fastembed_version: "5.17.2".into(),
            ort_version: "2.0.0-rc.12".into(),
            cpu_model: "fixture-cpu".into(),
            cpu_features: vec!["neon".into()],
            embedding_threads: 4,
            network_mode: "application_offline".into(),
            model_install_digests: BTreeMap::from([
                ("a".into(), Some("install-a".into())),
                ("b".into(), Some("install-b".into())),
                ("c".into(), Some("install-c".into())),
            ]),
        };
        let digest = environment.digest().unwrap();
        let mut changed = environment;
        changed.embedding_threads += 1;
        assert_ne!(digest, changed.digest().unwrap());
    }
}
