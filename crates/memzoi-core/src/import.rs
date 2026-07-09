use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use blake3::Hasher;
use serde::{Deserialize, Serialize};

use crate::{
    MemoryDestination, MemoryDestinationClassification, MemoryDestinationPolicy, MemoryLane,
    MemoryType, OkfProposalSensitivity, OkfProposalSource, ScopeKind, okf::OkfCreateProposalDraft,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportDocument {
    pub version: String,
    pub sources: Vec<OkfProposalSource>,
    pub candidates: Vec<ImportCandidateInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportCandidateInput {
    pub destination: MemoryDestination,
    pub reason: String,
    #[serde(rename = "type")]
    pub memory_type: Option<MemoryType>,
    pub lane: Option<MemoryLane>,
    pub title: String,
    pub body: String,
    pub sensitivity: Option<OkfProposalSensitivity>,
    pub scope: Option<ImportScope>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportScope {
    #[serde(default = "default_scope_kind")]
    pub kind: ScopeKind,
    pub id: Option<String>,
    #[serde(default)]
    pub paths: Vec<String>,
}

fn default_scope_kind() -> ScopeKind {
    ScopeKind::Repo
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportCandidate {
    pub index: usize,
    pub classification: MemoryDestinationClassification,
    pub policy: MemoryDestinationPolicy,
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub title: String,
    pub body: String,
    pub sensitivity: Option<OkfProposalSensitivity>,
    pub scope: ImportScope,
    pub tags: Vec<String>,
    pub content_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ImportDuplicateKind {
    CanonicalRecord,
    PendingProposal,
    RuntimeRecord,
    EarlierCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportDuplicate {
    pub kind: ImportDuplicateKind,
    pub id: String,
    pub destination: Option<MemoryDestination>,
    pub candidate_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImportCandidateAction {
    CreateProposal { proposal_id: String, path: PathBuf },
    Deferred { route: String, reason: String },
    Duplicate { matches: Vec<ImportDuplicate> },
    NoWrite { reason: String },
    Blocked { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPlanCandidate {
    pub index: usize,
    pub classification: MemoryDestinationClassification,
    pub policy: MemoryDestinationPolicy,
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub title: String,
    pub body: String,
    pub sensitivity: Option<OkfProposalSensitivity>,
    pub scope: ImportScope,
    pub tags: Vec<String>,
    pub content_hash: String,
    pub duplicates: Vec<ImportDuplicate>,
    pub action: ImportCandidateAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ImportPlanSummary {
    pub total: usize,
    pub create_proposals: usize,
    pub deferred_local: usize,
    pub deferred_session: usize,
    pub duplicates: usize,
    pub discarded: usize,
    pub needs_review: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPlan {
    pub schema: String,
    pub plan_id: String,
    pub sources: Vec<OkfProposalSource>,
    pub summary: ImportPlanSummary,
    pub candidates: Vec<ImportPlanCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportProposalWrite {
    pub index: usize,
    pub proposal_id: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportApplyResult {
    pub plan: ImportPlan,
    pub writes: Vec<ImportProposalWrite>,
}

pub fn parse_import_document(input: &str) -> Result<ImportDocument> {
    if input.trim().is_empty() {
        bail!("import manifest cannot be empty");
    }

    let mut doc: ImportDocument =
        serde_yaml::from_str(input).context("failed to parse import manifest as YAML")?;
    for source in &mut doc.sources {
        source.path = source.path.take().map(|v| v.trim().to_owned());
        source.url = source.url.take().map(|v| v.trim().to_owned());
        source.reference = source.reference.take().map(|v| v.trim().to_owned());
    }
    doc.sources
        .sort_by(|a, b| source_key(a).cmp(&source_key(b)));
    validate_document(&doc)?;
    Ok(doc)
}

pub(crate) fn validate_document(doc: &ImportDocument) -> Result<()> {
    if doc.version != "memzoi/import-v1" {
        bail!("unsupported import manifest version {:?}", doc.version);
    }

    if doc.sources.is_empty() {
        bail!("import manifest requires at least one source");
    }

    for (i, source) in doc.sources.iter().enumerate() {
        let has_locator = source
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
        if !has_locator {
            bail!("source {i} requires path, url, or ref");
        }

        if let Some(path) = source.path.as_deref() {
            validate_relative_path(path).with_context(|| format!("invalid source {i} path"))?;
        }
        if source
            .url
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            bail!("source {i} url cannot be empty");
        }
        if source
            .reference
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            bail!("source {i} ref cannot be empty");
        }
    }

    if doc.candidates.is_empty() {
        bail!("import manifest requires at least one candidate");
    }

    for (i, raw) in doc.candidates.iter().enumerate() {
        normalize_candidate(i, raw)?;
    }
    Ok(())
}

fn source_key(source: &OkfProposalSource) -> (&str, &str, &str) {
    (
        source.path.as_deref().unwrap_or(""),
        source.url.as_deref().unwrap_or(""),
        source.reference.as_deref().unwrap_or(""),
    )
}

fn validate_relative_path(path: &str) -> Result<()> {
    let trimmed = path.trim();
    if trimmed.is_empty()
        || trimmed.contains('\\')
        || trimmed.starts_with('/')
        || (trimmed.len() >= 2
            && trimmed.as_bytes()[1] == b':'
            && trimmed.as_bytes()[0].is_ascii_alphabetic())
    {
        bail!("path must be POSIX project-relative");
    }

    let p = Path::new(trimmed);
    if p.is_absolute() {
        bail!("path must be POSIX project-relative");
    }
    for component in p.components() {
        if matches!(
            component,
            Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            bail!("path must not contain . or .. components");
        }
    }
    Ok(())
}

pub(crate) fn normalize_document(doc: &ImportDocument) -> Result<Vec<ImportCandidate>> {
    doc.candidates
        .iter()
        .enumerate()
        .map(|(i, c)| normalize_candidate(i, c))
        .collect()
}

fn normalize_candidate(index: usize, raw: &ImportCandidateInput) -> Result<ImportCandidate> {
    let title = raw.title.trim().to_owned();
    if title.is_empty() {
        bail!("candidate {index} title is required");
    }

    let body = raw.body.trim().to_owned();
    if body.is_empty() {
        bail!("candidate {index} body is required");
    }

    let reason = raw.reason.trim().to_owned();
    if reason.is_empty() {
        bail!("candidate {index} reason is required");
    }

    if raw.destination == MemoryDestination::Repo
        && raw.sensitivity != Some(OkfProposalSensitivity::RepoSafe)
    {
        bail!("candidate {index} destination repo requires sensitivity repo-safe");
    }

    let mut tags = Vec::with_capacity(raw.tags.len());
    for tag in &raw.tags {
        let t = tag.trim().to_owned();
        if t.is_empty() {
            bail!("candidate {index} tags cannot contain empty values");
        }
        tags.push(t);
    }
    tags.sort();
    tags.dedup();

    let scope = normalize_scope(index, raw.scope.clone())?;
    let memory_type = raw
        .memory_type
        .or_else(|| match raw.lane {
            Some(MemoryLane::Session | MemoryLane::Episodic) => Some(MemoryType::Episode),
            Some(MemoryLane::Procedural) => Some(MemoryType::Procedure),
            _ if raw.destination == MemoryDestination::Session => Some(MemoryType::Episode),
            _ => None,
        })
        .ok_or_else(|| {
            anyhow::anyhow!("candidate {index} type is required or cannot be inferred")
        })?;
    let lane = raw.lane.unwrap_or_else(|| {
        if raw.destination == MemoryDestination::Session {
            MemoryLane::Session
        } else {
            match memory_type {
                MemoryType::Procedure => MemoryLane::Procedural,
                MemoryType::Episode => MemoryLane::Episodic,
                _ => MemoryLane::Semantic,
            }
        }
    });
    let (memory_type, lane) = if raw.destination == MemoryDestination::Session {
        (MemoryType::Episode, MemoryLane::Session)
    } else {
        (memory_type, lane)
    };
    let classification = MemoryDestinationClassification {
        destination: raw.destination,
        reason,
    };
    let policy = classification.destination.policy();

    Ok(ImportCandidate {
        index,
        classification,
        policy,
        memory_type,
        lane,
        title,
        body,
        sensitivity: raw.sensitivity,
        scope,
        tags,
        content_hash: content_hash(&raw.body),
    })
}

fn normalize_scope(index: usize, scope: Option<ImportScope>) -> Result<ImportScope> {
    let mut s = scope.unwrap_or(ImportScope {
        kind: ScopeKind::Repo,
        id: None,
        paths: Vec::new(),
    });
    if let Some(id) = &mut s.id {
        *id = id.trim().to_owned();
        if id.is_empty() {
            bail!("candidate {index} scope id cannot be empty");
        }
    }

    for p in &mut s.paths {
        *p = p.trim().to_owned();
        validate_relative_path(p).with_context(|| format!("candidate {index} scope path"))?;
    }
    s.paths.sort();
    s.paths.dedup();
    Ok(s)
}

pub(crate) fn content_hash(body: &str) -> String {
    blake3::hash(body.trim().as_bytes()).to_hex().to_string()
}

pub(crate) struct ExistingDuplicate {
    pub kind: ImportDuplicateKind,
    pub id: String,
    pub destination: Option<MemoryDestination>,
    pub hash: String,
}

pub(crate) fn build_plan(
    actor: &str,
    doc: &ImportDocument,
    existing: &[ExistingDuplicate],
    pending_root: &Path,
) -> Result<ImportPlan> {
    validate_document(doc)?;
    let normalized = normalize_document(doc)?;
    let mut sources = doc.sources.clone();
    for source in &mut sources {
        source.path = source.path.take().map(|v| v.trim().to_owned());
        source.url = source.url.take().map(|v| v.trim().to_owned());
        source.reference = source.reference.take().map(|v| v.trim().to_owned());
    }
    sources.sort_by(|a, b| source_key(a).cmp(&source_key(b)));

    let mut used_ids = BTreeSet::new();
    for e in existing {
        used_ids.insert(e.id.clone());
    }

    let mut by_hash: BTreeMap<String, Vec<ImportDuplicate>> = BTreeMap::new();
    for e in existing {
        by_hash
            .entry(e.hash.clone())
            .or_default()
            .push(ImportDuplicate {
                kind: e.kind,
                id: e.id.clone(),
                destination: e.destination,
                candidate_index: None,
            });
    }
    for values in by_hash.values_mut() {
        values.sort_by(|a, b| duplicate_sort_key(a).cmp(&duplicate_sort_key(b)));
        values.dedup_by(|a, b| a.kind == b.kind && a.id == b.id);
    }

    let mut candidates = Vec::new();
    let mut summary = ImportPlanSummary {
        total: normalized.len(),
        ..Default::default()
    };
    let mut prior: BTreeMap<String, usize> = BTreeMap::new();
    for c in normalized {
        let mut duplicates = by_hash.get(&c.content_hash).cloned().unwrap_or_default();
        if let Some(index) = prior.get(&c.content_hash).copied() {
            duplicates.push(ImportDuplicate {
                kind: ImportDuplicateKind::EarlierCandidate,
                id: format!("candidate-{index}"),
                destination: None,
                candidate_index: Some(index),
            });
        }
        duplicates.sort_by(|a, b| duplicate_sort_key(a).cmp(&duplicate_sort_key(b)));
        duplicates.dedup_by(|a, b| a.kind == b.kind && a.id == b.id);

        let action = if !duplicates.is_empty() {
            summary.duplicates += 1;
            ImportCandidateAction::Duplicate {
                matches: duplicates.clone(),
            }
        } else {
            match c.policy.write_route {
                crate::MemoryWriteRoute::FileBackedProposal => {
                    let base = format!("mem_import_{}", slug(&c.title));
                    let id =
                        crate::okf::reserve_okf_proposal_id(pending_root, &base, &mut used_ids)?;
                    summary.create_proposals += 1;
                    ImportCandidateAction::CreateProposal {
                        proposal_id: id.clone(),
                        path: PathBuf::from(".memzoi/proposals/pending").join(format!("{id}.md")),
                    }
                }
                crate::MemoryWriteRoute::RuntimeLocal => {
                    summary.deferred_local += 1;
                    ImportCandidateAction::Deferred {
                        route: "runtime_local".to_owned(),
                        reason: "runtime import writes are unavailable in this slice".to_owned(),
                    }
                }
                crate::MemoryWriteRoute::RuntimeSession => {
                    summary.deferred_session += 1;
                    ImportCandidateAction::Deferred {
                        route: "runtime_session".to_owned(),
                        reason: "runtime import writes are unavailable in this slice".to_owned(),
                    }
                }
                crate::MemoryWriteRoute::NoWrite
                    if c.policy.review == crate::MemoryReviewRequirement::HumanDecision =>
                {
                    summary.needs_review += 1;
                    ImportCandidateAction::Blocked {
                        reason: c.classification.reason.clone(),
                    }
                }
                crate::MemoryWriteRoute::NoWrite => {
                    summary.discarded += 1;
                    ImportCandidateAction::NoWrite {
                        reason: c.classification.reason.clone(),
                    }
                }
            }
        };
        prior.entry(c.content_hash.clone()).or_insert(c.index);
        candidates.push(ImportPlanCandidate {
            index: c.index,
            classification: c.classification,
            policy: c.policy,
            memory_type: c.memory_type,
            lane: c.lane,
            title: c.title,
            body: c.body,
            sensitivity: c.sensitivity,
            scope: c.scope,
            tags: c.tags,
            content_hash: c.content_hash,
            duplicates,
            action,
        });
    }

    let mut plan = ImportPlan {
        schema: "memzoi/import-plan-v1".to_owned(),
        plan_id: String::new(),
        sources,
        summary,
        candidates,
    };
    plan.plan_id = fingerprint_plan(actor, &plan);
    Ok(plan)
}

fn duplicate_sort_key(d: &ImportDuplicate) -> (u8, &str, Option<MemoryDestination>, Option<usize>) {
    (
        match d.kind {
            ImportDuplicateKind::CanonicalRecord => 0,
            ImportDuplicateKind::PendingProposal => 1,
            ImportDuplicateKind::RuntimeRecord => 2,
            ImportDuplicateKind::EarlierCandidate => 3,
        },
        d.id.as_str(),
        d.destination,
        d.candidate_index,
    )
}

fn slug(title: &str) -> String {
    let mut out = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }

    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 64 {
        out.truncate(64);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        "memory".to_owned()
    } else {
        out
    }
}

fn fingerprint_plan(actor: &str, plan: &ImportPlan) -> String {
    let mut h = Hasher::new();
    h.update(actor.trim().as_bytes());
    h.update(&[0]);
    let mut clone = plan.clone();
    clone.plan_id.clear();
    let bytes = serde_json::to_vec(&clone).expect("plan serializable");
    h.update(&bytes);
    format!("import_{}", h.finalize().to_hex())
}

pub(crate) fn proposal_draft(
    candidate: &ImportPlanCandidate,
    actor: &str,
    timestamp: &str,
    proposal_id: &str,
    sources: &[OkfProposalSource],
) -> OkfCreateProposalDraft {
    OkfCreateProposalDraft {
        proposal_id: proposal_id.to_owned(),
        memory_type: candidate.memory_type,
        lane: candidate.lane,
        title: candidate.title.clone(),
        body: candidate.body.clone(),
        actor: actor.to_owned(),
        timestamp: timestamp.to_owned(),
        reason: Some(candidate.classification.reason.clone()),
        scope_kind: candidate.scope.kind,
        scope_id: candidate.scope.id.clone(),
        applies_to: candidate.scope.paths.clone(),
        tags: candidate.tags.clone(),
        sources: sources.to_vec(),
        sensitivity: OkfProposalSensitivity::RepoSafe,
    }
}
