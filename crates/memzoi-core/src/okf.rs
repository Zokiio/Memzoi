use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::{
    CaptureProvenance, MemoryDestination, MemoryDraft, MemoryLane, MemoryRecord, MemoryStatus,
    MemoryType, OriginDescriptor, OriginRoute, RecordLineage, RepositoryContentClass,
    RetentionFacts, ScopeKind, Visibility, capture,
    materialization::{MaterializationMetadata, canonical_revision_for_okf_record},
    proposals::title_to_concept_slug,
    retention::evaluate_retention,
};

pub const OKF_PROFILE: &str = "memzoi";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OkfRecordFile {
    pub concept_id: String,
    pub draft: MemoryDraft,
    pub status: MemoryStatus,
    pub applies_to: Vec<String>,
    pub created: String,
    pub updated: Option<String>,
    pub supersedes_id: Option<String>,
    pub retention: RetentionFacts,
    pub origin: OriginDescriptor,
    pub lineage: Option<RecordLineage>,
    pub proposal_id: Option<String>,
    pub capture: Option<CaptureProvenance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materialization: Option<MaterializationMetadata>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OkfRecordSnapshot {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub record: OkfRecordFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OkfProposalFile {
    pub file_id: String,
    pub id: String,
    pub kind: Option<String>,
    pub profile: String,
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub title: String,
    pub description: String,
    pub body: String,
    pub status: OkfProposalStatus,
    pub proposal: OkfProposalMetadata,
    pub scope_kind: ScopeKind,
    pub scope_id: Option<String>,
    pub applies_to: Vec<String>,
    pub tags: Vec<String>,
    pub timestamp: String,
    pub created_by: Option<String>,
    pub sources: Vec<OkfProposalSource>,
    pub supersedes: Vec<String>,
    pub sensitivity: OkfProposalSensitivity,
    pub content_class: RepositoryContentClass,
    pub resolution: Option<OkfProposalResolution>,
    pub capture: Option<CaptureProvenance>,
    pub retention: RetentionFacts,
    pub origin: OriginDescriptor,
    pub lineage: Option<RecordLineage>,
}

/// Minimal, content-free classification used before parsing any reviewable proposal fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OkfProposalPreflight {
    pub sensitivity: OkfProposalSensitivity,
    pub content_class: RepositoryContentClass,
    pub receipt_proposal: OkfProposalFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OkfProposalResolution {
    pub outcome: OkfProposalOutcome,
    pub resolved_by: String,
    pub resolved_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OkfProposalMetadata {
    pub action: OkfProposalAction,
    pub proposed_by: String,
    pub proposed_at: String,
    pub reason: Option<String>,
    pub confidence: Option<String>,
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OkfProposalSource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OkfCreateProposalDraft {
    pub(crate) proposal_id: String,
    pub(crate) memory_type: MemoryType,
    pub(crate) lane: MemoryLane,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) actor: String,
    pub(crate) timestamp: String,
    pub(crate) reason: Option<String>,
    pub(crate) scope_kind: ScopeKind,
    pub(crate) scope_id: Option<String>,
    pub(crate) applies_to: Vec<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) sources: Vec<OkfProposalSource>,
    pub(crate) sensitivity: OkfProposalSensitivity,
    pub(crate) content_class: RepositoryContentClass,
    pub(crate) capture: Option<CaptureProvenance>,
    pub(crate) retention: RetentionFacts,
    pub(crate) origin: OriginDescriptor,
    pub(crate) lineage: Option<RecordLineage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OkfCreateProposalPlan {
    pub(crate) proposal_id: String,
    pub(crate) path: PathBuf,
    pub(crate) markdown: String,
    pub(crate) parsed: OkfProposalFile,
}

pub(crate) fn reserve_okf_proposal_id(
    pending_root: &Path,
    base_id: &str,
    reserved_ids: &mut BTreeSet<String>,
) -> Result<String> {
    validate_proposal_identifier(base_id)?;
    for suffix in 1.. {
        let candidate = if suffix == 1 {
            base_id.to_owned()
        } else {
            format!("{base_id}-{suffix}")
        };
        let identity_token = okf_proposal_identity_token(&candidate);
        if reserved_ids.contains(&candidate) || reserved_ids.contains(&identity_token) {
            continue;
        }
        if !proposal_packet_id_exists(pending_root, &candidate)? {
            reserved_ids.insert(candidate.clone());
            reserved_ids.insert(identity_token);
            return Ok(candidate);
        }
    }
    unreachable!("unbounded proposal id search returns")
}

pub(crate) fn render_okf_create_proposal_markdown(
    draft: &OkfCreateProposalDraft,
) -> Result<String> {
    validate_okf_create_proposal_draft(draft)?;
    let frontmatter = OkfCreateProposalFrontmatter {
        id: draft.proposal_id.trim().to_owned(),
        kind: "proposal".to_owned(),
        profile: OKF_PROFILE.to_owned(),
        memory_type: draft.memory_type,
        lane: draft.lane,
        title: draft.title.trim().to_owned(),
        description: first_non_empty_line(&draft.body).to_owned(),
        status: "proposed".to_owned(),
        proposal: OkfCreateProposalMetadata {
            action: "create".to_owned(),
            proposed_by: draft.actor.trim().to_owned(),
            proposed_at: draft.timestamp.trim().to_owned(),
            reason: trimmed_optional(draft.reason.as_deref()),
        },
        scope: OkfCreateProposalScope {
            kind: draft.scope_kind,
            id: trimmed_optional(draft.scope_id.as_deref()),
            paths: draft
                .applies_to
                .iter()
                .map(|path| path.trim().to_owned())
                .collect(),
        },
        tags: draft.tags.iter().map(|tag| tag.trim().to_owned()).collect(),
        timestamp: draft.timestamp.trim().to_owned(),
        created_by: draft.actor.trim().to_owned(),
        sources: draft
            .sources
            .iter()
            .map(|source| OkfProposalSource {
                path: trimmed_optional(source.path.as_deref()),
                url: trimmed_optional(source.url.as_deref()),
                reference: trimmed_optional(source.reference.as_deref()),
            })
            .collect(),
        supersedes: Vec::new(),
        sensitivity: draft.sensitivity,
        content_class: draft.content_class,
        capture: draft.capture.clone(),
        retention: draft.retention.clone(),
        origin: draft.origin.clone(),
        lineage: draft.lineage.clone(),
    };
    let yaml =
        serde_yaml::to_string(&frontmatter).context("failed to render OKF proposal frontmatter")?;
    Ok(format!(
        "---\n{}---\n\n# {}\n\n{}\n",
        yaml,
        draft.title.trim(),
        draft.body.trim()
    ))
}

fn trimmed_optional(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

pub(crate) fn plan_okf_create_proposal(
    pending_root: &Path,
    draft: &OkfCreateProposalDraft,
) -> Result<OkfCreateProposalPlan> {
    validate_okf_create_proposal_draft(draft)?;
    let proposal_id = draft.proposal_id.trim();
    let path = pending_root.join(format!("{proposal_id}.md"));
    if proposal_packet_id_exists(pending_root, proposal_id)? {
        bail!("proposal packet id {proposal_id} already exists in pending or resolved state");
    }
    let markdown = render_okf_create_proposal_markdown(draft)?;
    let parsed = parse_okf_proposal_markdown(pending_root, &path, &markdown)?
        .with_context(|| format!("rendered OKF proposal {proposal_id} was ignored"))?;
    if parsed.id != proposal_id {
        bail!("rendered OKF proposal id does not match draft id {proposal_id}");
    }
    Ok(OkfCreateProposalPlan {
        proposal_id: proposal_id.to_owned(),
        path,
        markdown,
        parsed,
    })
}

fn proposal_packet_id_exists(pending_root: &Path, proposal_id: &str) -> Result<bool> {
    let pending_path = pending_root.join(format!("{proposal_id}.md"));
    if pending_path.try_exists().with_context(|| {
        format!(
            "failed to inspect pending proposal {}",
            pending_path.display()
        )
    })? {
        return Ok(true);
    }
    let Some(proposals_root) = pending_root.parent() else {
        return Ok(false);
    };
    for outcome in ["applied", "rejected"] {
        let resolved_path = proposals_root
            .join("resolved")
            .join(outcome)
            .join(format!("{proposal_id}.md"));
        if resolved_path.try_exists().with_context(|| {
            format!(
                "failed to inspect resolved proposal packet {}",
                resolved_path.display()
            )
        })? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
pub(crate) fn create_okf_proposal_file(plan: &OkfCreateProposalPlan) -> Result<PathBuf> {
    create_okf_proposal_file_with_writer(&plan.path, |file| {
        std::io::Write::write_all(file, plan.markdown.as_bytes())
    })
}

#[cfg(test)]
fn create_okf_proposal_file_with_writer(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
) -> Result<PathBuf> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create proposal directory {}", parent.display()))?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("failed to create OKF proposal {}", path.display()))?;
    if let Err(error) = write(&mut file) {
        let write_error = anyhow::Error::new(error)
            .context(format!("failed to create OKF proposal {}", path.display()));
        drop(file);
        return match fs::remove_file(path) {
            Ok(()) => Err(write_error),
            Err(cleanup_error) => Err(write_error.context(format!(
                "failed to remove incomplete OKF proposal {}: {cleanup_error}",
                path.display()
            ))),
        };
    }
    Ok(path.to_path_buf())
}

fn validate_okf_create_proposal_draft(draft: &OkfCreateProposalDraft) -> Result<()> {
    validate_proposal_identifier(draft.proposal_id.trim())?;
    if draft.title.trim().is_empty() {
        bail!("OKF proposal title cannot be empty");
    }
    if draft.body.trim().is_empty() {
        bail!("OKF proposal body cannot be empty");
    }
    if draft.actor.trim().is_empty() {
        bail!("OKF proposal actor cannot be empty");
    }
    if draft.timestamp.trim().is_empty() {
        bail!("OKF proposal timestamp cannot be empty");
    }
    ensure_timestampish(draft.timestamp.trim(), "timestamp")?;
    if draft
        .reason
        .as_deref()
        .is_some_and(|reason| reason.trim().is_empty())
    {
        bail!("OKF proposal reason cannot be empty");
    }
    if draft
        .scope_id
        .as_deref()
        .is_some_and(|id| id.trim().is_empty())
    {
        bail!("OKF proposal scope id cannot be empty");
    }
    validate_applies_to(draft.applies_to.clone())?;
    if draft.tags.iter().any(|tag| tag.trim().is_empty()) {
        bail!("OKF proposal tags cannot contain empty entries");
    }
    validate_proposal_sources(draft.sources.clone())?;
    if draft.sensitivity != OkfProposalSensitivity::RepoSafe {
        bail!("OKF create proposal sensitivity must be repo-safe");
    }
    validate_retention_facts(
        draft.proposal_id.trim(),
        draft.lane,
        &draft.retention,
        draft.timestamp.trim(),
    )?;
    validate_origin_descriptor(draft.proposal_id.trim(), &draft.origin)?;
    validate_record_lineage(draft.proposal_id.trim(), draft.lineage.as_ref())?;
    Ok(())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OkfProposalAction {
    Create,
    Supersede,
    Tombstone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OkfProposalStatus {
    Proposed,
    Applied,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OkfProposalOutcome {
    Applied,
    Rejected,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OkfProposalSensitivity {
    RepoSafe,
    LocalOnly,
    Sensitive,
    Secret,
    RawTranscript,
    PrivatePersonalData,
    TemporaryState,
    #[default]
    Unknown,
}

impl OkfProposalAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Supersede => "supersede",
            Self::Tombstone => "tombstone",
        }
    }
}

impl OkfProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
        }
    }
}

impl OkfProposalOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Rejected => "rejected",
        }
    }
}

impl OkfProposalSensitivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RepoSafe => "repo-safe",
            Self::LocalOnly => "local-only",
            Self::Sensitive => "sensitive",
            Self::Secret => "secret",
            Self::RawTranscript => "raw-transcript",
            Self::PrivatePersonalData => "private-personal-data",
            Self::TemporaryState => "temporary-state",
            Self::Unknown => "unknown",
        }
    }
}

impl FromStr for OkfProposalAction {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "create" => Ok(Self::Create),
            "supersede" => Ok(Self::Supersede),
            "tombstone" => Ok(Self::Tombstone),
            other => Err(format!("unknown OKF proposal action {other:?}")),
        }
    }
}

impl FromStr for OkfProposalStatus {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "proposed" => Ok(Self::Proposed),
            "applied" => Ok(Self::Applied),
            "rejected" => Ok(Self::Rejected),
            other => Err(format!("unknown OKF proposal status {other:?}")),
        }
    }
}

impl FromStr for OkfProposalOutcome {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "applied" => Ok(Self::Applied),
            "rejected" => Ok(Self::Rejected),
            other => Err(format!("unknown OKF proposal outcome {other:?}")),
        }
    }
}

impl FromStr for OkfProposalSensitivity {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "repo-safe" => Ok(Self::RepoSafe),
            "local-only" => Ok(Self::LocalOnly),
            "sensitive" => Ok(Self::Sensitive),
            "secret" => Ok(Self::Secret),
            "raw-transcript" => Ok(Self::RawTranscript),
            "private-personal-data" => Ok(Self::PrivatePersonalData),
            "temporary-state" => Ok(Self::TemporaryState),
            "unknown" => Ok(Self::Unknown),
            other => Err(format!("unknown OKF proposal sensitivity {other:?}")),
        }
    }
}

pub fn read_okf_record_files(bundle_root: impl AsRef<Path>) -> Result<Vec<OkfRecordFile>> {
    Ok(read_okf_record_snapshots(bundle_root)?
        .into_iter()
        .map(|snapshot| snapshot.record)
        .collect())
}

pub(crate) fn read_okf_record_snapshots(
    bundle_root: impl AsRef<Path>,
) -> Result<Vec<OkfRecordSnapshot>> {
    let bundle_root = bundle_root.as_ref();
    if !bundle_root.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    collect_markdown_files(bundle_root, &mut files)?;
    files.sort();
    let mut snapshots = Vec::new();
    for path in files {
        if is_reserved_record_file(&path) {
            continue;
        }
        let bytes = fs::read(&path)
            .with_context(|| format!("failed to read OKF record {}", path.display()))?;
        let markdown = std::str::from_utf8(&bytes)
            .with_context(|| format!("OKF record {} is not UTF-8", path.display()))?;
        if let Some(record) = parse_okf_record_markdown(bundle_root, &path, markdown)? {
            snapshots.push(OkfRecordSnapshot {
                path,
                bytes,
                record,
            });
        }
    }
    snapshots.sort_by(|left, right| left.record.concept_id.cmp(&right.record.concept_id));
    Ok(snapshots)
}

pub fn read_okf_proposal_files(proposals_root: impl AsRef<Path>) -> Result<Vec<OkfProposalFile>> {
    let proposals_root = proposals_root.as_ref();
    if !proposals_root.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    collect_markdown_files(proposals_root, &mut files)?;
    let mut proposals = Vec::new();
    for file in files {
        if let Some(proposal) = parse_okf_proposal_file(proposals_root, &file)? {
            proposals.push(proposal);
        }
    }
    proposals.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(proposals)
}

pub fn parse_okf_record_file(
    bundle_root: impl AsRef<Path>,
    file_path: impl AsRef<Path>,
) -> Result<Option<OkfRecordFile>> {
    let markdown = fs::read_to_string(file_path.as_ref())
        .with_context(|| format!("failed to read OKF record {}", file_path.as_ref().display()))?;
    parse_okf_record_markdown(bundle_root, file_path, &markdown)
}

pub fn parse_okf_proposal_file(
    proposals_root: impl AsRef<Path>,
    file_path: impl AsRef<Path>,
) -> Result<Option<OkfProposalFile>> {
    let markdown = fs::read_to_string(file_path.as_ref())
        .with_context(|| "failed to read proposal during safety preflight".to_owned())?;
    parse_okf_proposal_markdown(proposals_root, file_path, &markdown)
}

pub fn preflight_okf_proposal_file(
    proposals_root: impl AsRef<Path>,
    file_path: impl AsRef<Path>,
) -> Result<Option<OkfProposalPreflight>> {
    let markdown = fs::read_to_string(file_path.as_ref())
        .context("failed to read proposal during safety preflight")?;
    preflight_okf_proposal_markdown(proposals_root, file_path, &markdown)
}

pub fn redacted_okf_proposal_path(
    proposals_root: impl AsRef<Path>,
    file_path: impl AsRef<Path>,
) -> Result<PathBuf> {
    let proposals_root = proposals_root.as_ref();
    let raw_file_id = raw_proposal_file_id(proposals_root, file_path.as_ref())?;
    Ok(proposals_root.join(format!("{}.md", okf_proposal_identity_token(&raw_file_id))))
}

pub fn preflight_okf_proposal_markdown(
    proposals_root: impl AsRef<Path>,
    file_path: impl AsRef<Path>,
    markdown: &str,
) -> Result<Option<OkfProposalPreflight>> {
    let proposals_root = proposals_root.as_ref();
    let file_path = file_path.as_ref();
    if is_reserved_record_file(file_path) {
        return Ok(None);
    }

    let raw_file_id = raw_proposal_file_id(proposals_root, file_path)?;
    let preflight_frontmatter = proposal_preflight_frontmatter(markdown);
    let raw_id = unique_top_level_yaml_scalar(preflight_frontmatter, "id");
    let sensitivity: OkfProposalSensitivity =
        unique_top_level_yaml_scalar(preflight_frontmatter, "sensitivity")
            .as_deref()
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or_default();
    let raw_content_class = unique_top_level_yaml_scalar(preflight_frontmatter, "content_class");
    let content_class = raw_content_class
        .as_deref()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or_default();
    let mut classification_hasher = blake3::Hasher::new();
    classification_hasher.update(b"memzoi.rejected-proposal-classification.v1\0");
    classification_hasher.update(sensitivity.as_str().as_bytes());
    classification_hasher.update(b"\0");
    classification_hasher.update(
        raw_content_class
            .as_deref()
            .unwrap_or("<missing>")
            .as_bytes(),
    );
    let classification_digest = classification_hasher.finalize().to_hex();
    let digest = rejected_proposal_content_hash(raw_id.as_deref(), &raw_file_id, markdown);
    let receipt_id = raw_id
        .as_deref()
        .map(okf_proposal_identity_token)
        .unwrap_or_else(|| okf_proposal_identity_token(&format!("missing-id:{digest}")));
    let receipt_file_id = okf_proposal_identity_token(&raw_file_id);
    let timestamp = "1970-01-01T00:00:00Z".to_owned();
    let receipt_proposal = OkfProposalFile {
        file_id: receipt_file_id,
        id: receipt_id,
        kind: Some("proposal".to_owned()),
        profile: OKF_PROFILE.to_owned(),
        memory_type: MemoryType::Fact,
        lane: MemoryLane::Semantic,
        title: "Redacted non-repo-safe proposal".to_owned(),
        description: "Original proposal content and identity redacted before archival.".to_owned(),
        body: format!(
            "Original non-repo-safe proposal content, identity, and classification were redacted before archival.\n\nContent hash (BLAKE3): `{digest}`.\n\nOriginal classification hash (BLAKE3): `{classification_digest}`."
        ),
        status: OkfProposalStatus::Proposed,
        proposal: OkfProposalMetadata {
            action: OkfProposalAction::Create,
            proposed_by: "redacted".to_owned(),
            proposed_at: timestamp.clone(),
            reason: None,
            confidence: None,
            target: None,
        },
        scope_kind: ScopeKind::Repo,
        scope_id: None,
        applies_to: Vec::new(),
        tags: Vec::new(),
        timestamp,
        created_by: None,
        sources: Vec::new(),
        supersedes: Vec::new(),
        sensitivity: OkfProposalSensitivity::RepoSafe,
        content_class: RepositoryContentClass::GeneralRepoKnowledge,
        resolution: None,
        capture: None,
        retention: durable_retention_facts(),
        origin: OriginDescriptor::new(
            format!("rejected-proposal:{digest}"),
            OriginRoute::RepositoryProposal,
        ),
        lineage: None,
    };
    Ok(Some(OkfProposalPreflight {
        sensitivity,
        content_class,
        receipt_proposal,
    }))
}

fn proposal_preflight_frontmatter(markdown: &str) -> &str {
    let Some(rest) = markdown
        .strip_prefix("---\n")
        .or_else(|| markdown.strip_prefix("---\r\n"))
    else {
        return "";
    };
    rest.split_once("\n---")
        .map_or(rest, |(frontmatter, _)| frontmatter)
}

fn unique_top_level_yaml_scalar(frontmatter: &str, key: &str) -> Option<String> {
    let mut values = frontmatter.lines().filter_map(|line| {
        if line.starts_with(char::is_whitespace) {
            return None;
        }
        let (candidate_key, value) = line.trim_end_matches('\r').split_once(':')?;
        if candidate_key != key {
            return None;
        }
        serde_yaml::from_str::<serde_yaml::Value>(value.trim())
            .ok()?
            .as_str()
            .map(str::to_owned)
    });
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn durable_retention_facts() -> RetentionFacts {
    RetentionFacts {
        occurred_at: None,
        started_at: None,
        last_continued_at: None,
        closed_at: None,
        explicit_expires_at: None,
        episodic_extension: None,
    }
}

fn validate_schema_identity(
    id: Option<&str>,
    kind: Option<&str>,
    profile: Option<&str>,
    expected_kind: &str,
) -> Result<()> {
    let id = id
        .context("id is required by the current OKF profile")?
        .trim();
    validate_proposal_identifier(id).context("invalid current-profile OKF id")?;
    let kind = kind
        .context("kind is required by the current OKF profile")?
        .trim();
    if kind != expected_kind {
        bail!("unsupported OKF kind {kind:?}; expected {expected_kind:?}");
    }
    let profile = profile
        .context("profile is required by the current OKF profile")?
        .trim();
    if profile != OKF_PROFILE {
        bail!(
            "unsupported OKF profile {profile:?}; expected {OKF_PROFILE}; pre-1.0 artifacts must be manually upgraded or removed"
        );
    }
    Ok(())
}

fn validate_retention_facts(
    record_id: &str,
    lane: MemoryLane,
    retention: &RetentionFacts,
    evaluated_at: &str,
) -> Result<()> {
    let evaluated_at = time::OffsetDateTime::parse(
        evaluated_at.trim(),
        &time::format_description::well_known::Rfc3339,
    )
    .with_context(|| format!("record {record_id} has an invalid evaluation timestamp"))?;
    evaluate_retention(record_id, lane, retention, evaluated_at)?;
    Ok(())
}

fn validate_origin_descriptor(record_id: &str, origin: &OriginDescriptor) -> Result<()> {
    origin
        .validate()
        .with_context(|| format!("record {record_id} has invalid origin descriptor"))
}

fn validate_record_lineage(record_id: &str, lineage: Option<&RecordLineage>) -> Result<()> {
    let Some(lineage) = lineage else {
        return Ok(());
    };
    lineage
        .validate()
        .with_context(|| format!("record {record_id} has invalid lineage"))?;
    if lineage.predecessor_id == record_id {
        bail!("record {record_id} cannot name itself as its lineage predecessor");
    }
    Ok(())
}

pub fn parse_okf_record_markdown(
    bundle_root: impl AsRef<Path>,
    file_path: impl AsRef<Path>,
    markdown: &str,
) -> Result<Option<OkfRecordFile>> {
    let bundle_root = bundle_root.as_ref();
    let file_path = file_path.as_ref();
    if is_reserved_record_file(file_path) {
        return Ok(None);
    }

    let concept_id = concept_id(bundle_root, file_path)?;
    let (frontmatter, body) = split_frontmatter(markdown)?;
    let frontmatter: OkfFrontmatter = serde_yaml::from_str(frontmatter)
        .with_context(|| format!("failed to parse OKF frontmatter for {concept_id}"))?;

    validate_schema_identity(
        frontmatter.id.as_deref(),
        frontmatter.kind.as_deref(),
        frontmatter.profile.as_deref(),
        "memory",
    )?;
    let title = required_string(frontmatter.title, "title")?;
    let memory_type = parse_required_enum::<MemoryType>(frontmatter.memory_type, "type")?;
    let lane = parse_required_enum::<MemoryLane>(frontmatter.lane, "lane")?;
    let scope_kind = parse_required_enum::<ScopeKind>(frontmatter.scope, "scope")?;
    let scope_id = optional_string(frontmatter.scope_id, "scope_id")?;
    let visibility = parse_required_enum::<Visibility>(frontmatter.visibility, "visibility")?;
    let content_class =
        parse_required_enum::<RepositoryContentClass>(frontmatter.content_class, "content_class")?;
    let status = parse_required_enum::<MemoryStatus>(frontmatter.status, "status")?;
    let confidence = parse_confidence(frontmatter.confidence)?;
    let source_kind = optional_string(frontmatter.source, "source")?;
    let source_ref = optional_string(frontmatter.source_ref, "source_ref")?;
    let created = required_string(frontmatter.timestamp, "timestamp")?;
    ensure_timestampish(&created, "timestamp")?;
    let updated = optional_string(frontmatter.updated, "updated")?;
    if let Some(updated) = updated.as_deref() {
        ensure_timestampish(updated, "updated")?;
    }
    let applies_to = validate_applies_to(frontmatter.applies_to.unwrap_or_default())?;
    let supersedes_id = optional_string(frontmatter.supersedes, "supersedes")?;
    let retention = frontmatter.retention.ok_or_else(|| {
        anyhow::anyhow!("record {concept_id} retention is required by the current OKF profile")
    })?;
    validate_retention_facts(&concept_id, lane, &retention, &created)?;
    let origin = frontmatter.origin.ok_or_else(|| {
        anyhow::anyhow!("record {concept_id} origin is required by the current OKF profile")
    })?;
    validate_origin_descriptor(&concept_id, &origin)?;
    validate_record_lineage(&concept_id, frontmatter.lineage.as_ref())?;
    let proposal_id = optional_string(frontmatter.proposal_id, "proposal_id")?;
    let body = body_without_matching_h1(body, &title)?;

    let record = OkfRecordFile {
        concept_id: concept_id.clone(),
        draft: MemoryDraft {
            memory_type,
            lane,
            scope_kind,
            scope_id,
            visibility,
            title,
            body,
            tags: frontmatter.tags.unwrap_or_default(),
            source_kind,
            source_ref,
            sensitivity: OkfProposalSensitivity::RepoSafe,
            content_class,
            confidence,
        },
        status,
        applies_to,
        created,
        updated,
        supersedes_id,
        retention,
        origin,
        lineage: frontmatter.lineage,
        proposal_id,
        capture: frontmatter.capture,
        materialization: frontmatter.materialization,
    };
    validate_record_materialization(&record)?;
    Ok(Some(record))
}

fn validate_record_materialization(record: &OkfRecordFile) -> Result<()> {
    let Some(metadata) = record.materialization.as_ref() else {
        return Ok(());
    };
    metadata
        .validate()
        .with_context(|| format!("invalid materialization metadata for {}", record.concept_id))?;
    let expected_revision = canonical_revision_for_okf_record(record)?;
    if metadata.revision != expected_revision {
        bail!(
            "materialization revision does not match semantic record content for {}",
            record.concept_id
        );
    }
    Ok(())
}

pub fn parse_okf_proposal_markdown(
    proposals_root: impl AsRef<Path>,
    file_path: impl AsRef<Path>,
    markdown: &str,
) -> Result<Option<OkfProposalFile>> {
    let proposals_root = proposals_root.as_ref();
    let file_path = file_path.as_ref();
    if is_reserved_record_file(file_path) {
        return Ok(None);
    }

    let file_id = proposal_file_id(proposals_root, file_path)?;
    let (frontmatter, body) = split_frontmatter(markdown)?;
    let frontmatter: OkfProposalFrontmatter = serde_yaml::from_str(frontmatter)
        .with_context(|| format!("failed to parse OKF proposal frontmatter for {file_id}"))?;

    let id = required_string(frontmatter.id, "id")?;
    validate_proposal_identifier(&id)?;
    validate_schema_identity(
        Some(&id),
        frontmatter.kind.as_deref(),
        frontmatter.profile.as_deref(),
        "proposal",
    )?;
    let title = required_string(frontmatter.title, "title")?;
    let description = required_string(frontmatter.description, "description")?;
    let memory_type = parse_required_enum::<MemoryType>(frontmatter.memory_type, "type")?;
    let lane = parse_required_enum::<MemoryLane>(frontmatter.lane, "lane")?;
    let status = parse_required_enum::<OkfProposalStatus>(frontmatter.status, "status")?;
    let timestamp = required_string(frontmatter.timestamp, "timestamp")?;
    ensure_timestampish(&timestamp, "timestamp")?;
    let sensitivity =
        parse_required_enum::<OkfProposalSensitivity>(frontmatter.sensitivity, "sensitivity")?;
    let (scope_kind, scope_id, applies_to) = parse_proposal_scope(frontmatter.scope)?;
    let supersedes =
        validate_string_list(frontmatter.supersedes.unwrap_or_default(), "supersedes")?;
    let proposal = parse_proposal_metadata(frontmatter.proposal)?;
    let sources = validate_proposal_sources(frontmatter.sources.unwrap_or_default())?;
    let resolution = parse_proposal_resolution(frontmatter.resolution)?;
    validate_proposal_resolution(status, resolution.as_ref())?;
    let retention = frontmatter.retention.ok_or_else(|| {
        anyhow::anyhow!("record {id} retention is required by the current OKF proposal profile")
    })?;
    validate_retention_facts(&id, lane, &retention, &timestamp)?;
    let origin = frontmatter.origin.ok_or_else(|| {
        anyhow::anyhow!("record {id} origin is required by the current OKF proposal profile")
    })?;
    validate_origin_descriptor(&id, &origin)?;
    validate_record_lineage(&id, frontmatter.lineage.as_ref())?;
    let body = body_without_matching_h1(body, &title)?;
    if body.trim().is_empty() {
        bail!("OKF proposal body cannot be empty");
    }

    Ok(Some(OkfProposalFile {
        file_id,
        id,
        kind: frontmatter.kind,
        profile: OKF_PROFILE.to_owned(),
        memory_type,
        lane,
        title,
        description,
        body,
        status,
        proposal,
        scope_kind,
        scope_id,
        applies_to,
        tags: frontmatter.tags.unwrap_or_default(),
        timestamp,
        created_by: frontmatter.created_by,
        sources,
        supersedes,
        sensitivity,
        content_class: parse_required_enum::<RepositoryContentClass>(
            frontmatter.content_class,
            "content_class",
        )?,
        resolution,
        capture: frontmatter.capture,
        retention,
        origin,
        lineage: frontmatter.lineage,
    }))
}

pub fn import_okf_records(conn: &Connection, records: &[OkfRecordFile]) -> Result<usize> {
    for record in records {
        import_okf_record(conn, record)?;
    }
    Ok(records.len())
}

pub(crate) fn project_okf_create_proposal(proposal: &OkfProposalFile) -> Result<MemoryRecord> {
    validate_repo_apply_proposal(proposal)?;
    if proposal.proposal.action != OkfProposalAction::Create {
        bail!(
            "OKF proposal action {} is not supported by create projection",
            proposal.proposal.action.as_str()
        );
    }

    Ok(project_okf_new_record(proposal, None))
}

pub(crate) fn project_okf_supersede_proposal(
    proposal: &OkfProposalFile,
    target_id: &str,
) -> Result<MemoryRecord> {
    validate_repo_apply_proposal(proposal)?;
    if proposal.proposal.action != OkfProposalAction::Supersede {
        bail!(
            "OKF proposal action {} is not supported by supersede projection",
            proposal.proposal.action.as_str()
        );
    }
    Ok(project_okf_new_record(proposal, Some(target_id.to_owned())))
}

fn project_okf_new_record(
    proposal: &OkfProposalFile,
    supersedes_id: Option<String>,
) -> MemoryRecord {
    let body = proposal.body.trim().to_owned();
    let (source_kind, source_ref) = proposal_primary_evidence(&proposal.sources);
    MemoryRecord {
        id: title_to_concept_slug(&proposal.title)
            .unwrap_or_else(|| proposal.file_id.replace('_', "-")),
        memory_type: proposal.memory_type,
        lane: proposal.lane,
        destination: MemoryDestination::Repo,
        scope_kind: proposal.scope_kind,
        scope_id: proposal.scope_id.clone(),
        visibility: Visibility::Repo,
        title: proposal.title.trim().to_owned(),
        body,
        status: MemoryStatus::Active,
        confidence: 1.0,
        source_kind,
        source_ref,
        proposal_id: Some(proposal.id.clone()),
        capture: proposal
            .capture
            .as_ref()
            .map(CaptureProvenance::compact_for_record),
        content_hash: blake3::hash(proposal.body.trim().as_bytes())
            .to_hex()
            .to_string(),
        created_at: proposal.timestamp.clone(),
        updated_at: proposal.timestamp.clone(),
        supersedes_id,
        retention: proposal.retention.clone(),
        origin: proposal.origin.clone(),
        lineage: proposal.lineage.clone(),
    }
}

pub(crate) fn project_okf_record(record: &OkfRecordFile) -> MemoryRecord {
    MemoryRecord {
        id: record.concept_id.clone(),
        memory_type: record.draft.memory_type,
        lane: record.draft.lane,
        destination: MemoryDestination::Repo,
        scope_kind: record.draft.scope_kind,
        scope_id: record.draft.scope_id.clone(),
        visibility: record.draft.visibility,
        title: record.draft.title.clone(),
        body: record.draft.body.clone(),
        status: record.status,
        confidence: record.draft.confidence,
        source_kind: record.draft.source_kind.clone(),
        source_ref: record.draft.source_ref.clone(),
        proposal_id: record.proposal_id.clone(),
        capture: record.capture.clone(),
        content_hash: crate::import::content_hash(&record.draft.body),
        created_at: record.created.clone(),
        updated_at: record
            .updated
            .clone()
            .unwrap_or_else(|| record.created.clone()),
        supersedes_id: record.supersedes_id.clone(),
        retention: record.retention.clone(),
        origin: record.origin.clone(),
        lineage: record.lineage.clone(),
    }
}

pub(crate) fn validate_repo_apply_proposal(proposal: &OkfProposalFile) -> Result<()> {
    if proposal.status != OkfProposalStatus::Proposed {
        bail!(
            "OKF proposal {} must have status proposed before apply",
            proposal.id
        );
    }
    if proposal.sensitivity != OkfProposalSensitivity::RepoSafe {
        bail!(
            "OKF proposal sensitivity {} cannot be applied into repo records; {}",
            proposal.sensitivity.as_str(),
            repo_apply_sensitivity_guidance(proposal.sensitivity)
        );
    }
    validate_proposal_action_shape(
        proposal.proposal.action,
        &proposal.proposal.target,
        &proposal.proposal.reason,
        &proposal.supersedes,
    )?;
    Ok(())
}

pub(crate) fn render_resolved_okf_proposal_markdown(
    proposal: &OkfProposalFile,
    resolution: &OkfProposalResolution,
) -> Result<String> {
    if proposal.status != OkfProposalStatus::Proposed || proposal.resolution.is_some() {
        bail!("only pending proposed packets can be resolved");
    }
    let status = match resolution.outcome {
        OkfProposalOutcome::Applied => OkfProposalStatus::Applied,
        OkfProposalOutcome::Rejected => OkfProposalStatus::Rejected,
    };
    validate_proposal_resolution(status, Some(resolution))?;
    let frontmatter = OkfResolvedProposalFrontmatter {
        id: proposal.id.clone(),
        kind: proposal.kind.clone(),
        profile: proposal.profile.clone(),
        memory_type: proposal.memory_type,
        lane: proposal.lane,
        title: proposal.title.clone(),
        description: proposal.description.clone(),
        status,
        proposal: OkfResolvedProposalMetadata {
            action: proposal.proposal.action,
            proposed_by: proposal.proposal.proposed_by.clone(),
            proposed_at: proposal.proposal.proposed_at.clone(),
            reason: proposal.proposal.reason.clone(),
            confidence: proposal.proposal.confidence.clone(),
            target: proposal.proposal.target.clone(),
        },
        scope: OkfResolvedProposalScope {
            kind: proposal.scope_kind,
            id: proposal.scope_id.clone(),
            paths: proposal.applies_to.clone(),
        },
        tags: proposal.tags.clone(),
        timestamp: proposal.timestamp.clone(),
        created_by: proposal.created_by.clone(),
        sources: proposal.sources.clone(),
        supersedes: proposal.supersedes.clone(),
        sensitivity: proposal.sensitivity,
        content_class: proposal.content_class,
        resolution: resolution.clone(),
        capture: proposal.capture.clone(),
        retention: proposal.retention.clone(),
        origin: proposal.origin.clone(),
        lineage: proposal.lineage.clone(),
    };
    let yaml = serde_yaml::to_string(&frontmatter)
        .context("failed to render resolved OKF proposal frontmatter")?;
    Ok(format!(
        "---\n{}---\n\n# {}\n\n{}\n",
        yaml,
        proposal.title.trim(),
        proposal.body.trim()
    ))
}

fn rejected_proposal_content_hash(
    raw_id: Option<&str>,
    raw_file_id: &str,
    markdown: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proposal-id\0");
    hasher.update(raw_id.unwrap_or("<missing-or-non-string>").as_bytes());
    hasher.update(b"\0proposal-file-id\0");
    hasher.update(raw_file_id.as_bytes());
    hasher.update(b"\0proposal-markdown\0");
    hasher.update(markdown.as_bytes());
    hasher.finalize().to_hex().to_string()
}

pub fn okf_proposal_matches_identity(proposal: &OkfProposalFile, identity: &str) -> bool {
    proposal.id == identity
        || proposal.file_id == identity
        || proposal_identity_tokens(proposal).contains(&okf_proposal_identity_token(identity))
}

pub(crate) fn okf_proposals_share_identity(
    left: &OkfProposalFile,
    right: &OkfProposalFile,
) -> bool {
    !proposal_identity_tokens(left).is_disjoint(&proposal_identity_tokens(right))
}

pub(crate) fn proposal_identity_tokens(proposal: &OkfProposalFile) -> BTreeSet<String> {
    [proposal.id.as_str(), proposal.file_id.as_str()]
        .into_iter()
        .map(okf_proposal_identity_token)
        .collect()
}

pub(crate) fn okf_proposal_identity_token(identity: &str) -> String {
    const PREFIX: &str = "redacted-identity-";
    if identity
        .strip_prefix(PREFIX)
        .is_some_and(|digest| digest.len() == 64 && digest.chars().all(|ch| ch.is_ascii_hexdigit()))
    {
        return identity.to_owned();
    }
    format!("{PREFIX}{}", blake3::hash(identity.as_bytes()).to_hex())
}

pub(crate) fn render_memory_record_markdown(
    record: &MemoryRecord,
    tags: &[String],
    applies_to: &[String],
) -> String {
    render_memory_record(record, tags, applies_to)
}

/// Renders one fully typed canonical record, including optional materialization attestation.
pub fn render_okf_record_markdown(record: &OkfRecordFile) -> Result<String> {
    validate_record_materialization(record)?;

    let draft = &record.draft;
    let mut output = String::new();
    output.push_str("---\n");
    push_yaml_string(&mut output, "id", &record.concept_id);
    push_yaml_string(&mut output, "kind", "memory");
    push_yaml_string(&mut output, "profile", OKF_PROFILE);
    push_yaml_string(&mut output, "type", draft.memory_type.as_str());
    push_yaml_string(&mut output, "lane", draft.lane.as_str());
    push_yaml_string(&mut output, "title", &draft.title);
    push_yaml_string(
        &mut output,
        "description",
        first_non_empty_line(&draft.body),
    );
    push_yaml_string(&mut output, "timestamp", &record.created);
    if let Some(updated) = &record.updated {
        push_yaml_string(&mut output, "updated", updated);
    }
    push_yaml_string(&mut output, "status", record.status.as_str());
    push_yaml_string(&mut output, "scope", draft.scope_kind.as_str());
    if let Some(scope_id) = &draft.scope_id {
        push_yaml_string(&mut output, "scope_id", scope_id);
    }
    push_yaml_string(&mut output, "visibility", draft.visibility.as_str());
    push_yaml_string(&mut output, "content_class", draft.content_class.as_str());
    output.push_str(&format!("confidence: {}\n", draft.confidence));
    if let Some(source_kind) = &draft.source_kind {
        push_yaml_string(&mut output, "source", source_kind);
    }
    if let Some(source_ref) = &draft.source_ref {
        push_yaml_string(&mut output, "source_ref", source_ref);
    }
    if let Some(proposal_id) = &record.proposal_id {
        push_yaml_string(&mut output, "proposal_id", proposal_id);
    }
    if let Some(capture) = &record.capture {
        output.push_str("capture:\n");
        let yaml = serde_yaml::to_string(capture)
            .context("failed to serialize capture provenance for canonical record")?;
        for line in yaml.lines() {
            output.push_str("  ");
            output.push_str(line);
            output.push('\n');
        }
    }
    if !draft.tags.is_empty() {
        output.push_str("tags:\n");
        for tag in &draft.tags {
            output.push_str("  - ");
            output.push_str(&quote_yaml_string(tag));
            output.push('\n');
        }
    }
    if !record.applies_to.is_empty() {
        output.push_str("applies_to:\n");
        for path in &record.applies_to {
            output.push_str("  - ");
            output.push_str(&quote_yaml_string(path));
            output.push('\n');
        }
    }
    if let Some(supersedes_id) = &record.supersedes_id {
        push_yaml_string(&mut output, "supersedes", supersedes_id);
    }
    push_yaml_block(&mut output, "retention", &record.retention)
        .context("failed to serialize retention facts for canonical record")?;
    push_yaml_block(&mut output, "origin", &record.origin)
        .context("failed to serialize origin descriptor for canonical record")?;
    if let Some(lineage) = &record.lineage {
        push_yaml_block(&mut output, "lineage", lineage)
            .context("failed to serialize record lineage for canonical record")?;
    }
    if let Some(materialization) = &record.materialization {
        output.push_str("materialization:\n");
        let yaml = serde_yaml::to_string(materialization)
            .context("failed to serialize materialization metadata for canonical record")?;
        for line in yaml.lines() {
            output.push_str("  ");
            output.push_str(line);
            output.push('\n');
        }
    }
    output.push_str("---\n\n");
    output.push_str(&format!("# {}\n\n{}\n", draft.title, draft.body.trim()));
    Ok(output)
}

pub(crate) fn repo_apply_sensitivity_guidance(sensitivity: OkfProposalSensitivity) -> &'static str {
    match sensitivity {
        OkfProposalSensitivity::RepoSafe => "repo-safe proposals may be applied after review",
        OkfProposalSensitivity::Secret => "secret proposals must not become repo-shared memory",
        OkfProposalSensitivity::Sensitive => {
            "classify or sanitize sensitive content before applying it to the repo plane"
        }
        OkfProposalSensitivity::LocalOnly => {
            "local-only proposals belong in the future local/runtime memory plane"
        }
        OkfProposalSensitivity::RawTranscript => {
            "raw transcripts must not become repo-shared memory"
        }
        OkfProposalSensitivity::PrivatePersonalData => {
            "private personal data must not become repo-shared memory"
        }
        OkfProposalSensitivity::TemporaryState => {
            "temporary task state belongs in local or session memory, not canonical repo memory"
        }
        OkfProposalSensitivity::Unknown => {
            "classify the proposal sensitivity before applying it to repo records"
        }
    }
}

fn proposal_primary_evidence(sources: &[OkfProposalSource]) -> (Option<String>, Option<String>) {
    for source in sources {
        if let Some(path) = trimmed_optional(source.path.as_deref()) {
            return (Some("path".to_owned()), Some(path));
        }
        if let Some(url) = trimmed_optional(source.url.as_deref()) {
            return (Some("url".to_owned()), Some(url));
        }
        if let Some(reference) = trimmed_optional(source.reference.as_deref()) {
            return (Some("ref".to_owned()), Some(reference));
        }
    }
    (None, None)
}

#[derive(Debug, Serialize)]
struct OkfCreateProposalFrontmatter {
    id: String,
    kind: String,
    profile: String,
    #[serde(rename = "type")]
    memory_type: MemoryType,
    lane: MemoryLane,
    title: String,
    description: String,
    status: String,
    proposal: OkfCreateProposalMetadata,
    scope: OkfCreateProposalScope,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    timestamp: String,
    created_by: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    sources: Vec<OkfProposalSource>,
    supersedes: Vec<String>,
    sensitivity: OkfProposalSensitivity,
    content_class: RepositoryContentClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    capture: Option<CaptureProvenance>,
    retention: RetentionFacts,
    origin: OriginDescriptor,
    #[serde(skip_serializing_if = "Option::is_none")]
    lineage: Option<RecordLineage>,
}

#[derive(Debug, Serialize)]
struct OkfCreateProposalMetadata {
    action: String,
    proposed_by: String,
    proposed_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct OkfCreateProposalScope {
    kind: ScopeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OkfFrontmatter {
    id: Option<String>,
    kind: Option<String>,
    profile: Option<String>,
    #[serde(rename = "type")]
    memory_type: Option<String>,
    lane: Option<String>,
    title: Option<String>,
    #[serde(rename = "description")]
    _description: Option<String>,
    scope: Option<String>,
    scope_id: Option<String>,
    visibility: Option<String>,
    content_class: Option<String>,
    status: Option<String>,
    confidence: Option<ConfidenceValue>,
    source: Option<String>,
    source_ref: Option<String>,
    proposal_id: Option<String>,
    supersedes: Option<String>,
    retention: Option<RetentionFacts>,
    origin: Option<OriginDescriptor>,
    lineage: Option<RecordLineage>,
    timestamp: Option<String>,
    updated: Option<String>,
    applies_to: Option<Vec<String>>,
    tags: Option<Vec<String>>,
    capture: Option<CaptureProvenance>,
    materialization: Option<MaterializationMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OkfProposalFrontmatter {
    id: Option<String>,
    kind: Option<String>,
    profile: Option<String>,
    #[serde(rename = "type")]
    memory_type: Option<String>,
    lane: Option<String>,
    title: Option<String>,
    description: Option<String>,
    status: Option<String>,
    proposal: Option<OkfProposalMetadataFrontmatter>,
    scope: Option<OkfProposalScopeFrontmatter>,
    tags: Option<Vec<String>>,
    timestamp: Option<String>,
    created_by: Option<String>,
    sources: Option<Vec<OkfProposalSource>>,
    supersedes: Option<Vec<String>>,
    sensitivity: Option<String>,
    content_class: Option<String>,
    resolution: Option<OkfProposalResolutionFrontmatter>,
    capture: Option<CaptureProvenance>,
    retention: Option<RetentionFacts>,
    origin: Option<OriginDescriptor>,
    lineage: Option<RecordLineage>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OkfProposalResolutionFrontmatter {
    outcome: Option<String>,
    resolved_by: Option<String>,
    resolved_at: Option<String>,
    reason: Option<String>,
    record_id: Option<String>,
    target_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct OkfResolvedProposalFrontmatter {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    profile: String,
    #[serde(rename = "type")]
    memory_type: MemoryType,
    lane: MemoryLane,
    title: String,
    description: String,
    status: OkfProposalStatus,
    proposal: OkfResolvedProposalMetadata,
    scope: OkfResolvedProposalScope,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_by: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    sources: Vec<OkfProposalSource>,
    supersedes: Vec<String>,
    sensitivity: OkfProposalSensitivity,
    content_class: RepositoryContentClass,
    resolution: OkfProposalResolution,
    #[serde(skip_serializing_if = "Option::is_none")]
    capture: Option<CaptureProvenance>,
    retention: RetentionFacts,
    origin: OriginDescriptor,
    #[serde(skip_serializing_if = "Option::is_none")]
    lineage: Option<RecordLineage>,
}

#[derive(Debug, Serialize)]
struct OkfResolvedProposalMetadata {
    action: OkfProposalAction,
    proposed_by: String,
    proposed_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
}

#[derive(Debug, Serialize)]
struct OkfResolvedProposalScope {
    kind: ScopeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OkfProposalMetadataFrontmatter {
    action: Option<String>,
    proposed_by: Option<String>,
    proposed_at: Option<String>,
    reason: Option<String>,
    confidence: Option<String>,
    target: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OkfProposalScopeFrontmatter {
    kind: Option<String>,
    id: Option<String>,
    paths: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct ConfidenceValue(f64);

fn collect_markdown_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).context("failed to scan OKF Markdown directory")? {
        let entry = entry?;
        let path = entry.path();
        if is_hidden(&path) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_markdown_files(&path, files)?;
        } else if file_type.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md")
        {
            files.push(path);
        }
    }
    Ok(())
}

fn import_okf_record(conn: &Connection, record: &OkfRecordFile) -> Result<()> {
    let hash = crate::import::content_hash(&record.draft.body);
    let updated = record.updated.as_deref().unwrap_or(record.created.as_str());
    let retention_json = serde_json::to_string(&record.retention)
        .context("failed to serialize OKF retention facts for runtime projection")?;
    let origin_json = serde_json::to_string(&record.origin)
        .context("failed to serialize OKF origin descriptor for runtime projection")?;
    let lineage_json = record
        .lineage
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .context("failed to serialize OKF record lineage for runtime projection")?;
    let changed = conn.execute(
        "INSERT INTO memory_record (
          id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status, confidence,
          source_kind, source_ref, proposal_id, content_hash, created_at, updated_at, supersedes_id,
          retention_json, origin_json, lineage_json
        ) VALUES (?1, ?2, ?3, 'repo', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
        ON CONFLICT(id) DO UPDATE SET
          type = excluded.type,
          lane = excluded.lane,
          destination = excluded.destination,
          scope_kind = excluded.scope_kind,
          scope_id = excluded.scope_id,
          visibility = excluded.visibility,
          title = excluded.title,
          body = excluded.body,
          status = excluded.status,
          confidence = excluded.confidence,
          source_kind = excluded.source_kind,
          source_ref = excluded.source_ref,
          proposal_id = excluded.proposal_id,
          content_hash = excluded.content_hash,
          created_at = excluded.created_at,
          updated_at = excluded.updated_at,
          supersedes_id = excluded.supersedes_id,
          retention_json = excluded.retention_json,
          origin_json = excluded.origin_json,
          lineage_json = excluded.lineage_json
        WHERE memory_record.destination = 'repo'",
        params![
            record.concept_id,
            record.draft.memory_type.as_str(),
            record.draft.lane.as_str(),
            record.draft.scope_kind.as_str(),
            record.draft.scope_id,
            record.draft.visibility.as_str(),
            record.draft.title.trim(),
            record.draft.body.trim(),
            record.status.as_str(),
            record.draft.confidence,
            record.draft.source_kind,
            record.draft.source_ref,
            record.proposal_id,
            hash,
            record.created,
            updated,
            record.supersedes_id,
            retention_json,
            origin_json,
            lineage_json,
        ],
    )?;
    if changed != 1 {
        bail!("runtime record identity is already owned by non-repo memory");
    }
    conn.execute(
        "DELETE FROM memory_path WHERE record_id = ?1",
        [&record.concept_id],
    )?;
    conn.execute(
        "DELETE FROM memory_tag WHERE record_id = ?1",
        [&record.concept_id],
    )?;
    for (index, path) in record.applies_to.iter().enumerate() {
        conn.execute(
            "INSERT INTO memory_path(id, record_id, path, line_start, line_end) VALUES (?1, ?2, ?3, NULL, NULL)",
            params![format!("{}_path_{}", record.concept_id, index), record.concept_id, path],
        )?;
    }
    for tag in &record.draft.tags {
        conn.execute(
            "INSERT OR IGNORE INTO memory_tag(record_id, tag) VALUES (?1, ?2)",
            params![record.concept_id, tag],
        )?;
    }
    capture::store_capture_provenance(conn, &record.concept_id, record.capture.as_ref())?;
    Ok(())
}

fn render_memory_record(record: &MemoryRecord, tags: &[String], applies_to: &[String]) -> String {
    let mut output = String::new();
    output.push_str("---\n");
    push_yaml_string(&mut output, "id", &record.id);
    push_yaml_string(&mut output, "kind", "memory");
    push_yaml_string(&mut output, "profile", OKF_PROFILE);
    push_yaml_string(&mut output, "type", record.memory_type.as_str());
    push_yaml_string(&mut output, "lane", record.lane.as_str());
    push_yaml_string(&mut output, "title", &record.title);
    push_yaml_string(
        &mut output,
        "description",
        first_non_empty_line(&record.body),
    );
    push_yaml_string(&mut output, "timestamp", &record.created_at);
    push_yaml_string(&mut output, "updated", &record.updated_at);
    push_yaml_string(&mut output, "status", record.status.as_str());
    push_yaml_string(&mut output, "scope", record.scope_kind.as_str());
    if let Some(scope_id) = &record.scope_id {
        push_yaml_string(&mut output, "scope_id", scope_id);
    }
    push_yaml_string(&mut output, "visibility", record.visibility.as_str());
    push_yaml_string(
        &mut output,
        "content_class",
        RepositoryContentClass::GeneralRepoKnowledge.as_str(),
    );
    output.push_str(&format!("confidence: {}\n", record.confidence));
    if let Some(source_kind) = &record.source_kind {
        push_yaml_string(&mut output, "source", source_kind);
    }
    if let Some(source_ref) = &record.source_ref {
        push_yaml_string(&mut output, "source_ref", source_ref);
    }
    if let Some(proposal_id) = &record.proposal_id {
        push_yaml_string(&mut output, "proposal_id", proposal_id);
    }
    if let Some(capture) = &record.capture {
        output.push_str("capture:\n");
        let yaml = serde_yaml::to_string(capture)
            .expect("capture provenance must serialize as canonical record frontmatter");
        for line in yaml.lines() {
            output.push_str("  ");
            output.push_str(line);
            output.push('\n');
        }
    }
    if !tags.is_empty() {
        output.push_str("tags:\n");
        for tag in tags {
            output.push_str("  - ");
            output.push_str(&quote_yaml_string(tag));
            output.push('\n');
        }
    }
    if !applies_to.is_empty() {
        output.push_str("applies_to:\n");
        for path in applies_to {
            output.push_str("  - ");
            output.push_str(&quote_yaml_string(path));
            output.push('\n');
        }
    }
    if let Some(supersedes_id) = &record.supersedes_id {
        push_yaml_string(&mut output, "supersedes", supersedes_id);
    }
    push_yaml_block(&mut output, "retention", &record.retention)
        .expect("retention facts must serialize as canonical record frontmatter");
    push_yaml_block(&mut output, "origin", &record.origin)
        .expect("origin descriptor must serialize as canonical record frontmatter");
    if let Some(lineage) = &record.lineage {
        push_yaml_block(&mut output, "lineage", lineage)
            .expect("record lineage must serialize as canonical record frontmatter");
    }
    output.push_str("---\n\n");
    output.push_str(&format!("# {}\n\n{}\n", record.title, record.body.trim()));
    output
}

fn first_non_empty_line(body: &str) -> &str {
    body.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
}

fn push_yaml_string(output: &mut String, key: &str, value: &str) {
    output.push_str(key);
    output.push_str(": ");
    output.push_str(&quote_yaml_string(value));
    output.push('\n');
}

fn push_yaml_block<T: Serialize>(output: &mut String, key: &str, value: &T) -> Result<()> {
    output.push_str(key);
    output.push_str(":\n");
    let yaml = serde_yaml::to_string(value)
        .with_context(|| format!("failed to serialize {key} frontmatter"))?;
    for line in yaml.lines() {
        output.push_str("  ");
        output.push_str(line);
        output.push('\n');
    }
    Ok(())
}

fn quote_yaml_string(value: &str) -> String {
    if is_plain_yaml_scalar(value) {
        return value.to_owned();
    }
    serde_json::to_string(value).expect("serializing a YAML string scalar cannot fail")
}

fn is_plain_yaml_scalar(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/'))
}

fn is_reserved_record_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("index.md" | "log.md")
    )
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.') && name != ".")
}

fn concept_id(bundle_root: &Path, file_path: &Path) -> Result<String> {
    if file_path
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("md")
    {
        bail!(
            "OKF record files must use .md extension: {}",
            file_path.display()
        );
    }
    let relative = file_path.strip_prefix(bundle_root).with_context(|| {
        format!(
            "OKF record path {} is not under bundle root {}",
            file_path.display(),
            bundle_root.display()
        )
    })?;
    let without_extension = strip_md_extension(relative);
    for component in without_extension.components() {
        match component {
            Component::Normal(_) => {}
            _ => bail!("OKF concept path contains unsafe component"),
        }
    }
    let concept = without_extension
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    validate_concept_id(&concept)?;
    Ok(concept)
}

fn strip_md_extension(path: &Path) -> PathBuf {
    let mut output = path.to_path_buf();
    output.set_extension("");
    output
}

pub(crate) fn validate_concept_id(concept: &str) -> Result<()> {
    if concept.is_empty() {
        bail!("OKF concept id cannot be empty");
    }
    for segment in concept.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            bail!("OKF concept id contains invalid segment {segment:?}");
        }
        if !segment
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        {
            bail!("OKF concept id segments must use lowercase ASCII letters, digits, and hyphens");
        }
        let starts_and_ends_alnum = segment
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
            && segment
                .chars()
                .last()
                .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit());
        if !starts_and_ends_alnum {
            bail!("OKF concept id segments must start and end with a letter or digit");
        }
    }
    Ok(())
}

fn proposal_file_id(bundle_root: &Path, file_path: &Path) -> Result<String> {
    let file_id = raw_proposal_file_id(bundle_root, file_path)?;
    validate_proposal_identifier(&file_id)?;
    Ok(file_id)
}

fn raw_proposal_file_id(bundle_root: &Path, file_path: &Path) -> Result<String> {
    if file_path
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("md")
    {
        bail!("OKF proposal files must use .md extension");
    }
    let relative = file_path
        .strip_prefix(bundle_root)
        .context("OKF proposal path is not under proposal root")?;
    let without_extension = strip_md_extension(relative);
    for component in without_extension.components() {
        match component {
            Component::Normal(_) => {}
            _ => bail!("OKF proposal path contains unsafe component"),
        }
    }
    let file_id = without_extension
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    Ok(file_id)
}

fn validate_proposal_identifier(identifier: &str) -> Result<()> {
    if identifier.is_empty() {
        bail!("OKF proposal id cannot be empty");
    }
    for segment in identifier.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            bail!("OKF proposal id contains invalid segment {segment:?}");
        }
        if !segment
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
        {
            bail!(
                "OKF proposal id segments must use lowercase ASCII letters, digits, hyphens, and underscores"
            );
        }
        let starts_and_ends_alnum = segment
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
            && segment
                .chars()
                .last()
                .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit());
        if !starts_and_ends_alnum {
            bail!("OKF proposal id segments must start and end with a letter or digit");
        }
    }
    Ok(())
}

fn split_frontmatter(markdown: &str) -> Result<(&str, &str)> {
    let rest = markdown
        .strip_prefix("---\n")
        .or_else(|| markdown.strip_prefix("---\r\n"))
        .context("OKF Markdown must start with YAML frontmatter")?;
    let Some(separator_start) = rest.find("\n---") else {
        bail!("OKF Markdown frontmatter must be closed with ---");
    };
    let frontmatter = &rest[..separator_start];
    let after_separator = &rest[separator_start + "\n---".len()..];
    let body = after_separator
        .strip_prefix("\r\n")
        .or_else(|| after_separator.strip_prefix('\n'))
        .unwrap_or(after_separator);
    Ok((frontmatter, body))
}

fn required_string(value: Option<String>, key: &str) -> Result<String> {
    let value = value.with_context(|| format!("OKF frontmatter missing required field {key}"))?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("OKF frontmatter field {key} cannot be empty");
    }
    Ok(trimmed.to_owned())
}

fn optional_string(value: Option<String>, key: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("OKF frontmatter field {key} cannot be empty");
    }
    Ok(Some(trimmed.to_owned()))
}

fn parse_required_enum<T>(value: Option<String>, key: &str) -> Result<T>
where
    T: std::str::FromStr<Err = String>,
{
    let value = required_string(value, key)?;
    value.parse().map_err(anyhow::Error::msg)
}

fn parse_confidence(value: Option<ConfidenceValue>) -> Result<f64> {
    let confidence = value
        .context("OKF frontmatter missing required field confidence")?
        .0;
    if !(0.0..=1.0).contains(&confidence) {
        bail!("OKF confidence must be between 0.0 and 1.0");
    }
    Ok(confidence)
}

fn parse_proposal_metadata(
    metadata: Option<OkfProposalMetadataFrontmatter>,
) -> Result<OkfProposalMetadata> {
    let metadata = metadata.context("OKF proposal frontmatter missing required field proposal")?;
    let action = parse_required_enum::<OkfProposalAction>(metadata.action, "proposal.action")?;
    let proposed_by = required_string(metadata.proposed_by, "proposal.proposed_by")?;
    let proposed_at = required_string(metadata.proposed_at, "proposal.proposed_at")?;
    ensure_timestampish(&proposed_at, "proposal.proposed_at")?;
    let reason = optional_string(metadata.reason, "proposal.reason")?;
    let confidence = optional_string(metadata.confidence, "proposal.confidence")?;
    let target = optional_string(metadata.target, "proposal.target")?;
    Ok(OkfProposalMetadata {
        action,
        proposed_by,
        proposed_at,
        reason,
        confidence,
        target,
    })
}

fn parse_proposal_resolution(
    resolution: Option<OkfProposalResolutionFrontmatter>,
) -> Result<Option<OkfProposalResolution>> {
    let Some(resolution) = resolution else {
        return Ok(None);
    };
    let outcome =
        parse_required_enum::<OkfProposalOutcome>(resolution.outcome, "resolution.outcome")?;
    let resolved_by = required_string(resolution.resolved_by, "resolution.resolved_by")?;
    let resolved_at = required_string(resolution.resolved_at, "resolution.resolved_at")?;
    ensure_timestampish(&resolved_at, "resolution.resolved_at")?;
    Ok(Some(OkfProposalResolution {
        outcome,
        resolved_by,
        resolved_at,
        reason: optional_string(resolution.reason, "resolution.reason")?,
        record_id: optional_string(resolution.record_id, "resolution.record_id")?,
        target_id: optional_string(resolution.target_id, "resolution.target_id")?,
    }))
}

fn validate_proposal_resolution(
    status: OkfProposalStatus,
    resolution: Option<&OkfProposalResolution>,
) -> Result<()> {
    match (status, resolution) {
        (OkfProposalStatus::Proposed, None) => Ok(()),
        (OkfProposalStatus::Proposed, Some(_)) => {
            bail!("proposed OKF packets cannot include a resolution")
        }
        (OkfProposalStatus::Applied, Some(resolution))
            if resolution.outcome == OkfProposalOutcome::Applied
                && resolution.record_id.is_some() =>
        {
            Ok(())
        }
        (OkfProposalStatus::Rejected, Some(resolution))
            if resolution.outcome == OkfProposalOutcome::Rejected
                && resolution.reason.is_some()
                && resolution.record_id.is_none() =>
        {
            Ok(())
        }
        (OkfProposalStatus::Applied, None) | (OkfProposalStatus::Rejected, None) => {
            bail!("resolved OKF packets must include resolution metadata")
        }
        (OkfProposalStatus::Applied, Some(_)) => {
            bail!("applied OKF packets require an applied outcome and record_id")
        }
        (OkfProposalStatus::Rejected, Some(_)) => {
            bail!("rejected OKF packets require a rejected outcome and reason without record_id")
        }
    }
}

fn parse_proposal_scope(
    scope: Option<OkfProposalScopeFrontmatter>,
) -> Result<(ScopeKind, Option<String>, Vec<String>)> {
    let scope = scope.context("OKF proposal frontmatter missing required field scope")?;
    let scope_kind = parse_required_enum::<ScopeKind>(scope.kind, "scope.kind")?;
    let scope_id = optional_string(scope.id, "scope.id")?;
    let applies_to = validate_applies_to(scope.paths.unwrap_or_default())?;
    Ok((scope_kind, scope_id, applies_to))
}

fn validate_proposal_action_shape(
    action: OkfProposalAction,
    target: &Option<String>,
    reason: &Option<String>,
    supersedes: &[String],
) -> Result<()> {
    match action {
        OkfProposalAction::Create => {
            if target.is_some() || !supersedes.is_empty() {
                bail!("OKF create proposals cannot name a target or supersedes record");
            }
            Ok(())
        }
        OkfProposalAction::Supersede => {
            if supersedes.len() != 1 || target.is_some() {
                bail!(
                    "OKF supersede proposals must include exactly one supersedes target and no proposal.target"
                );
            }
            if reason.is_none() {
                bail!("OKF supersede proposals must include proposal.reason");
            }
            Ok(())
        }
        OkfProposalAction::Tombstone => {
            if target.as_deref().is_none_or(str::is_empty) || !supersedes.is_empty() {
                bail!(
                    "OKF tombstone proposals must include exactly one proposal.target and no supersedes records"
                );
            }
            if reason.is_none() {
                bail!("OKF tombstone proposals must include proposal.reason");
            }
            Ok(())
        }
    }
}

fn validate_string_list(values: Vec<String>, key: &str) -> Result<Vec<String>> {
    values
        .into_iter()
        .map(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                bail!("OKF frontmatter field {key} cannot contain empty entries");
            }
            Ok(trimmed.to_owned())
        })
        .collect()
}

fn validate_proposal_sources(sources: Vec<OkfProposalSource>) -> Result<Vec<OkfProposalSource>> {
    for source in &sources {
        let has_value = source
            .path
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || source
                .url
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            || source
                .reference
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
        if !has_value {
            bail!("proposal sources must include a non-empty path, url, or ref");
        }
        if let Some(path) = &source.path {
            validate_applies_to(vec![path.clone()])
                .with_context(|| format!("invalid proposal source path {path:?}"))?;
        }
        if source
            .url
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            bail!("proposal source url cannot be empty");
        }
        if source
            .reference
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            bail!("proposal source ref cannot be empty");
        }
    }
    Ok(sources)
}

fn ensure_timestampish(value: &str, key: &str) -> Result<()> {
    if value.len() < 10
        || !value
            .chars()
            .take(10)
            .all(|ch| ch.is_ascii_digit() || ch == '-')
    {
        bail!("OKF frontmatter field {key} must be timestamp-ish");
    }
    Ok(())
}

fn validate_applies_to(values: Vec<String>) -> Result<Vec<String>> {
    let mut output = Vec::with_capacity(values.len());
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            bail!("applies_to entries cannot be empty");
        }
        let path = Path::new(trimmed);
        if path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            bail!("applies_to entries must be relative and cannot contain traversal");
        }
        output.push(trimmed.to_owned());
    }
    Ok(output)
}

fn body_without_matching_h1(body: &str, title: &str) -> Result<String> {
    let body = body.trim_start_matches(['\r', '\n']);
    let Some(rest) = body.strip_prefix("# ") else {
        return Ok(body.trim().to_owned());
    };
    let line_end = rest.find('\n').unwrap_or(rest.len());
    let h1 = rest[..line_end].trim();
    if h1 != title {
        bail!("OKF H1 title must match frontmatter title");
    }
    Ok(rest[line_end..]
        .trim_start_matches(['\r', '\n'])
        .trim()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, io::ErrorKind, path::Path};

    use rusqlite::Connection;
    use tempfile::TempDir;

    use crate::{
        CANONICAL_REVISION_SCHEMA, CanonicalRevision, MATERIALIZATION_METADATA_SCHEMA,
        MaterializationAction, MaterializationMetadata, MemoryDestination, MemoryLane,
        MemoryRecord, MemoryStatus, MemoryType, OriginDescriptor, OriginRoute, RetentionFacts,
        ScopeKind, Visibility, canonical_revision_for_okf_record,
    };

    const EXAMPLE_MEMORY: &str = include_str!("../../../examples/example-memory.md");

    #[test]
    fn rejection_digest_and_alias_tokens_cover_raw_proposal_and_file_identities()
    -> anyhow::Result<()> {
        let markdown = "---\nid: secret-proposal-id\ntype: [malformed\nsensitivity: secret\n---\nsecret body\n";
        let base = super::rejected_proposal_content_hash(
            Some("secret-proposal-id"),
            "secret-file-id",
            markdown,
        );
        assert_ne!(
            base,
            super::rejected_proposal_content_hash(
                Some("different-proposal-id"),
                "secret-file-id",
                markdown,
            )
        );
        assert_ne!(
            base,
            super::rejected_proposal_content_hash(
                Some("secret-proposal-id"),
                "different-file-id",
                markdown,
            )
        );

        let root = Path::new("/bundle/pending");
        let preflight =
            super::preflight_okf_proposal_markdown(root, root.join("secret-file-id.md"), markdown)?
                .expect("malformed secret packet should still produce a safety receipt");
        assert_eq!(preflight.sensitivity, super::OkfProposalSensitivity::Secret);
        assert_eq!(
            preflight.content_class,
            crate::RepositoryContentClass::Unknown
        );
        assert!(super::okf_proposal_matches_identity(
            &preflight.receipt_proposal,
            "secret-proposal-id"
        ));
        assert!(super::okf_proposal_matches_identity(
            &preflight.receipt_proposal,
            "secret-file-id"
        ));
        assert!(!preflight.receipt_proposal.id.contains("secret-proposal-id"));
        assert!(
            !preflight
                .receipt_proposal
                .file_id
                .contains("secret-file-id")
        );
        assert_eq!(
            preflight.receipt_proposal.sensitivity,
            super::OkfProposalSensitivity::RepoSafe
        );
        assert_eq!(
            preflight.receipt_proposal.content_class,
            crate::RepositoryContentClass::GeneralRepoKnowledge
        );
        Ok(())
    }

    #[test]
    fn preflight_read_failure_does_not_echo_raw_file_identity() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let raw_file_id = "secret-invalid-utf8-file-identity";
        let path = temp.path().join(format!("{raw_file_id}.md"));
        fs::write(&path, [0xff, 0xfe])?;

        let error = super::preflight_okf_proposal_file(temp.path(), &path)
            .expect_err("invalid UTF-8 should fail before proposal parsing");
        let rendered = format!("{error:#}");
        assert!(!rendered.contains(raw_file_id), "{rendered}");
        assert!(
            rendered.contains("failed to read proposal during safety preflight"),
            "{rendered}"
        );
        Ok(())
    }

    #[test]
    fn internal_record_renderer_preserves_canonical_fields() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let records = temp.path().join("records");
        let record = MemoryRecord {
            id: "team/install-risk".to_owned(),
            memory_type: MemoryType::Risk,
            lane: MemoryLane::Semantic,
            destination: MemoryDestination::Repo,
            scope_kind: ScopeKind::Team,
            scope_id: Some("platform".to_owned()),
            visibility: Visibility::Team,
            title: "Risk: package install".to_owned(),
            body: "Package installs require review.".to_owned(),
            status: MemoryStatus::Superseded,
            confidence: 0.75,
            source_kind: Some("human-authored".to_owned()),
            source_ref: Some("issue://42".to_owned()),
            proposal_id: Some("prop_review_42".to_owned()),
            capture: None,
            content_hash: "hash".to_owned(),
            created_at: "2026-07-05T00:00:00Z".to_owned(),
            updated_at: "2026-07-06T00:00:00Z".to_owned(),
            supersedes_id: Some("team/old-install-risk".to_owned()),
            retention: RetentionFacts {
                occurred_at: None,
                started_at: None,
                last_continued_at: None,
                closed_at: None,
                explicit_expires_at: Some("2027-01-01T00:00:00Z".to_owned()),
                episodic_extension: None,
            },
            origin: OriginDescriptor::new(
                "test:team-install-risk",
                OriginRoute::RepositoryMaterialization,
            ),
            lineage: None,
        };

        let path = records.join(format!("{}.md", record.id));
        std::fs::create_dir_all(path.parent().expect("record path has parent"))?;
        let rendered = super::render_memory_record_markdown(&record, &[], &[]);
        std::fs::write(&path, &rendered)?;
        assert!(rendered.contains("type: risk\n"));
        assert!(rendered.contains("title: \"Risk: package install\"\n"));
        let parsed = super::parse_okf_record_file(&records, &path)?.expect("record parses");

        assert_eq!(parsed.concept_id, "team/install-risk");
        assert_eq!(parsed.draft.title, "Risk: package install");
        assert_eq!(parsed.draft.scope_kind, ScopeKind::Team);
        assert_eq!(parsed.draft.scope_id.as_deref(), Some("platform"));
        assert_eq!(parsed.status, MemoryStatus::Superseded);
        assert_eq!(parsed.draft.source_ref.as_deref(), Some("issue://42"));
        assert_eq!(parsed.proposal_id.as_deref(), Some("prop_review_42"));
        assert_eq!(
            parsed.supersedes_id.as_deref(),
            Some("team/old-install-risk")
        );
        assert_eq!(
            parsed.retention.explicit_expires_at.as_deref(),
            Some("2027-01-01T00:00:00Z")
        );
        Ok(())
    }

    #[test]
    fn proposal_id_reservation_never_reuses_resolved_audit_ids() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let pending = temp.path().join("proposals/pending");
        let applied = temp.path().join("proposals/resolved/applied");
        std::fs::create_dir_all(&pending)?;
        std::fs::create_dir_all(&applied)?;
        std::fs::write(applied.join("mem_example.md"), "resolved evidence")?;

        let reserved =
            super::reserve_okf_proposal_id(&pending, "mem_example", &mut BTreeSet::new())?;

        assert_eq!(reserved, "mem_example-2");
        Ok(())
    }

    #[test]
    fn failed_proposal_write_removes_partial_target_and_preserves_cause() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let proposal_dir = temp.path().join("proposals");
        std::fs::create_dir_all(&proposal_dir)?;
        let target = proposal_dir.join("new.md");
        let sibling = proposal_dir.join("untouched.md");
        let sentinel = b"untouched sibling sentinel";
        std::fs::write(&sibling, sentinel)?;

        let error = super::create_okf_proposal_file_with_writer(&target, |file| {
            std::io::Write::write_all(file, b"partial proposal bytes")?;
            Err(std::io::Error::other("forced writer failure"))
        })
        .expect_err("forced writer failure should fail proposal creation");

        assert!(!target.exists(), "partial proposal target was not removed");
        assert_eq!(std::fs::read(&sibling)?, sentinel);
        assert!(
            error.to_string().contains(&format!(
                "failed to create OKF proposal {}",
                target.display()
            )),
            "proposal path context missing from error: {error:#}"
        );
        let cause = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<std::io::Error>())
            .expect("original writer IO error should remain in anyhow chain");
        assert_eq!(cause.kind(), ErrorKind::Other);
        assert_eq!(cause.to_string(), "forced writer failure");

        Ok(())
    }

    #[test]
    fn nullable_evidence_round_trips_without_fabricated_defaults() -> anyhow::Result<()> {
        let root = Path::new("/bundle/records");
        let path = root.join("no-evidence.md");
        let markdown = r#"---
id: no-evidence
kind: memory
profile: memzoi
type: fact
lane: semantic
title: No evidence metadata
scope: repo
visibility: repo
content_class: general_repo_knowledge
status: active
confidence: 1.0
timestamp: 2026-07-04T00:00:00Z
retention: {}
origin:
  origin_key: test:no-evidence
  route: repository_materialization
---

# No evidence metadata

Nullable provenance remains nullable.
"#;
        let parsed = super::parse_okf_record_markdown(root, &path, markdown)?
            .expect("record should parse without evidence metadata");
        assert_eq!(parsed.draft.source_kind, None);
        assert_eq!(parsed.draft.source_ref, None);

        let record = super::project_okf_record(&parsed);
        let rendered = super::render_memory_record_markdown(&record, &[], &[]);
        assert!(!rendered.contains("source:"), "{rendered}");
        assert!(!rendered.contains("source_ref:"), "{rendered}");
        let reparsed = super::parse_okf_record_markdown(root, &path, &rendered)?
            .expect("rendered record should parse");
        assert_eq!(reparsed.draft.source_kind, None);
        assert_eq!(reparsed.draft.source_ref, None);

        Ok(())
    }

    #[test]
    fn record_projection_hashes_the_trimmed_body() -> anyhow::Result<()> {
        let root = Path::new("/bundle/records");
        let path = root.join("trimmed-hash.md");
        let mut parsed = super::parse_okf_record_markdown(root, &path, EXAMPLE_MEMORY)?
            .expect("example record should parse");
        parsed.draft.body = "\nCanonical body with edge whitespace.\n\n".to_owned();

        let expected_hash = crate::import::content_hash(&parsed.draft.body);
        let projected = super::project_okf_record(&parsed);
        assert_eq!(projected.content_hash, expected_hash);
        Ok(())
    }

    #[test]
    fn record_import_hashes_the_stored_trimmed_body() -> anyhow::Result<()> {
        let root = Path::new("/bundle/records");
        let path = root.join("trimmed-hash.md");
        let mut parsed = super::parse_okf_record_markdown(root, &path, EXAMPLE_MEMORY)?
            .expect("example record should parse");
        parsed.draft.body = "\nCanonical body with edge whitespace.\n\n".to_owned();

        let expected_body = parsed.draft.body.trim().to_owned();
        let expected_hash = crate::import::content_hash(&parsed.draft.body);
        let conn = Connection::open_in_memory()?;
        crate::init_database(&conn)?;
        super::import_okf_records(&conn, std::slice::from_ref(&parsed))?;
        let (stored_body, stored_hash): (String, String) = conn.query_row(
            "SELECT body, content_hash FROM memory_record WHERE id = ?1",
            [&parsed.concept_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        assert_eq!(stored_body, expected_body);
        assert_eq!(stored_hash, expected_hash);
        assert_eq!(stored_hash, crate::import::content_hash(&stored_body));
        Ok(())
    }

    #[test]
    fn parses_example_memory_into_importable_draft() -> anyhow::Result<()> {
        let bundle_root = Path::new("/bundle");
        let file_path = bundle_root.join("memories/repo/frontend/swedish-first.md");

        let parsed = super::parse_okf_record_markdown(bundle_root, &file_path, EXAMPLE_MEMORY)?
            .expect("example memory should be a concept record");

        assert_eq!(parsed.concept_id, "memories/repo/frontend/swedish-first");
        assert_eq!(parsed.status, MemoryStatus::Active);
        assert_eq!(parsed.applies_to, vec!["apps/web/**"]);
        assert_eq!(parsed.created, "2026-07-04T00:00:00Z");
        assert_eq!(parsed.updated, None);
        assert_eq!(parsed.draft.memory_type, MemoryType::Preference);
        assert_eq!(parsed.draft.lane, MemoryLane::Semantic);
        assert_eq!(parsed.draft.scope_kind, ScopeKind::Repo);
        assert_eq!(parsed.draft.visibility, Visibility::Team);
        assert_eq!(parsed.draft.title, "Swedish-first UI copy");
        assert_eq!(parsed.draft.tags, vec!["frontend", "i18n"]);
        assert_eq!(parsed.draft.source_kind.as_deref(), Some("human"));
        assert_eq!(
            parsed.draft.source_ref.as_deref(),
            Some("memories/repo/frontend/swedish-first")
        );
        assert_eq!(parsed.draft.confidence, 1.0);
        assert!(parsed.draft.body.contains("User-facing UI"));
        assert!(!parsed.draft.body.contains("# Swedish-first UI copy"));
        Ok(())
    }

    #[test]
    fn parses_supported_memory_lanes() -> anyhow::Result<()> {
        for (lane, memory_type, expected_lane, expected_type) in [
            (
                "session",
                "episode",
                MemoryLane::Session,
                MemoryType::Episode,
            ),
            (
                "semantic",
                "decision",
                MemoryLane::Semantic,
                MemoryType::Decision,
            ),
            (
                "episodic",
                "episode",
                MemoryLane::Episodic,
                MemoryType::Episode,
            ),
            (
                "procedural",
                "procedure",
                MemoryLane::Procedural,
                MemoryType::Procedure,
            ),
        ] {
            let parsed = super::parse_okf_record_markdown(
                Path::new("/bundle"),
                Path::new("/bundle/memories/lane-test.md"),
                &record_markdown(lane, memory_type),
            )?
            .expect("lane test record should parse");

            assert_eq!(parsed.draft.lane, expected_lane);
            assert_eq!(parsed.draft.memory_type, expected_type);
        }

        Ok(())
    }

    #[test]
    fn missing_lane_is_rejected_by_the_current_profile() {
        let error = super::parse_okf_record_markdown(
            Path::new("/bundle"),
            Path::new("/bundle/memories/missing-lane.md"),
            r#"---
id: missing-lane
kind: memory
profile: memzoi
type: decision
title: Missing lane
scope: repo
visibility: repo
content_class: general_repo_knowledge
source: human
status: active
confidence: 1.0
timestamp: 2026-07-04T00:00:00Z
retention: {}
origin:
  origin_key: test:missing-lane
  route: repository_materialization
---

# Missing lane

The current profile requires an explicit lane.
"#,
        )
        .expect_err("missing lane must fail closed");

        assert!(
            error
                .to_string()
                .contains("frontmatter missing required field lane"),
            "{error:#}"
        );
    }

    #[test]
    fn rejects_unknown_memory_lane() {
        let error = super::parse_okf_record_markdown(
            Path::new("/bundle"),
            Path::new("/bundle/memories/invalid-lane.md"),
            &record_markdown("mystery", "decision"),
        )
        .expect_err("unknown memory lane must be rejected");

        assert!(
            error.to_string().contains("unknown memory lane"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn rejects_applies_to_traversal() {
        let invalid = r#"---
id: unsafe
kind: memory
profile: memzoi
type: preference
lane: semantic
title: Unsafe path
scope: repo
visibility: team
content_class: general_repo_knowledge
source: human-authored
status: active
confidence: 1.0
timestamp: 2026-07-04T00:00:00Z
retention: {}
origin:
  origin_key: test:unsafe
  route: repository_materialization
applies_to:
  - ../secrets
---

# Unsafe path

Do not import this.
"#;

        let error = super::parse_okf_record_markdown(
            Path::new("/bundle"),
            Path::new("/bundle/memories/unsafe.md"),
            invalid,
        )
        .expect_err("applies_to traversal must be rejected");

        assert!(
            error
                .to_string()
                .contains("applies_to entries must be relative and cannot contain traversal"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn skips_reserved_index_and_log_files() -> anyhow::Result<()> {
        let bundle_root = Path::new("/bundle");

        assert!(
            super::parse_okf_record_markdown(
                bundle_root,
                bundle_root.join("memories/index.md"),
                EXAMPLE_MEMORY,
            )?
            .is_none()
        );
        assert!(
            super::parse_okf_record_markdown(
                bundle_root,
                bundle_root.join("memories/log.md"),
                EXAMPLE_MEMORY,
            )?
            .is_none()
        );
        Ok(())
    }

    #[test]
    fn typed_record_renderer_round_trips_materialization_attestation() -> anyhow::Result<()> {
        let root = Path::new("/bundle/records");
        let path = root.join("team/install-risk.md");
        let mut record =
            super::parse_okf_record_markdown(root, &path, &record_markdown("semantic", "risk"))?
                .expect("base record should parse");
        assert_eq!(
            record.materialization, None,
            "ordinary current-format records may be unattested"
        );

        record.materialization = Some(MaterializationMetadata {
            schema: MATERIALIZATION_METADATA_SCHEMA.to_owned(),
            action: MaterializationAction::Create,
            plan_id: format!("blake3:{}", "1".repeat(64)),
            candidate_id: format!("blake3:{}", "2".repeat(64)),
            decision_id: format!("blake3:{}", "3".repeat(64)),
            decision_at: "2026-07-16T12:00:00Z".to_owned(),
            safety_contract: crate::REPOSITORY_WRITE_SAFETY_SCHEMA.to_owned(),
            revision: CanonicalRevision {
                schema: CANONICAL_REVISION_SCHEMA.to_owned(),
                revision_hash: format!("blake3:{}", "4".repeat(64)),
            },
            target: None,
            reason: None,
        });
        let revision = canonical_revision_for_okf_record(&record)?;
        record
            .materialization
            .as_mut()
            .expect("test record has metadata")
            .revision = revision;

        let rendered = super::render_okf_record_markdown(&record)?;
        assert!(rendered.contains("materialization:\n"), "{rendered}");
        let reparsed = super::parse_okf_record_markdown(root, &path, &rendered)?
            .expect("rendered record should parse");
        assert_eq!(reparsed, record);

        let unsupported_schema = rendered.replacen(
            MATERIALIZATION_METADATA_SCHEMA,
            "incompatible/repository-materialization",
            1,
        );
        let error = super::parse_okf_record_markdown(root, &path, &unsupported_schema)
            .expect_err("unknown required materialization schemas must fail closed");
        assert!(
            error.chain().any(|cause| cause
                .to_string()
                .starts_with("unsupported materialization metadata schema")),
            "unexpected error: {error:#}"
        );
        Ok(())
    }

    fn record_markdown(lane: &str, memory_type: &str) -> String {
        let lane_retention = match lane {
            "session" => "retention:\n  started_at: 2026-07-04T00:00:00Z\n",
            "episodic" => "retention:\n  occurred_at: 2026-07-04T00:00:00Z\n",
            _ => "retention: {}\n",
        };
        format!(
            r#"---
id: lane-test
kind: memory
profile: memzoi
type: {memory_type}
lane: {lane}
title: Lane test
scope: repo
visibility: repo
content_class: general_repo_knowledge
source: human
status: active
confidence: 1.0
timestamp: 2026-07-04T00:00:00Z
{lane_retention}origin:
  origin_key: test:lane-test:{lane}
  route: repository_materialization
---

# Lane test

This record exercises lane parsing.
"#
        )
    }
}
