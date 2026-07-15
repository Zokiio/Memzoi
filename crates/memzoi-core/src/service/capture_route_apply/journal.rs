use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    CapturePlan, CaptureReview, MemoryPaths, OkfProposalSensitivity, RepositoryContentClass,
    RepositoryWriteRoute, ScopeKind, Visibility, okf, repository_io,
};

use super::super::{
    authorize_repository_projection_batch, borrowed_repository_projections,
    explicit_repository_provenance, okf_proposal_safety_values,
    proposal_packets::{ProposalPacketLifecycle, prepare_pending_proposal_root},
    repository_mutation::{
        AuthorizedRepositoryProjectionBatch, OwnedRepositoryProjection,
        repository_transaction_path, stage_authorized_file,
    },
    safe_files::{ensure_path_absent, remove_staged_file, sync_directory},
};

pub(super) const CAPTURE_APPLY_JOURNAL_SCHEMA: &str = "memzoi/capture-apply-journal-v2";
const CAPTURE_APPLY_COMMIT_SCHEMA: &str = "memzoi/capture-apply-commit-v2";
const CAPTURE_APPLY_JOURNAL_FILE: &str = "capture-apply-journal-v2.json";
const LEGACY_CAPTURE_APPLY_JOURNAL_FILE: &str = "capture-apply-journal-v1.json";
const CAPTURE_APPLY_COMMIT_EVENT: &str = "capture.apply_committed";
const MAX_CAPTURE_APPLY_JOURNAL_BYTES: u64 = 256 * 1024;
const MAX_CAPTURE_APPLY_JOURNAL_ENTRIES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureApplyJournal {
    pub(super) schema: String,
    pub(super) safety_contract_version: String,
    pub(super) detector_policy_version: String,
    pub(super) route: String,
    pub(super) authorization_digest: String,
    pub(super) project_context_digest: String,
    pub(super) journal_id: String,
    pub(super) plan_id: String,
    pub(super) review_id: String,
    pub(super) entries: Vec<CaptureApplyJournalEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureApplyJournalEntry {
    pub(super) candidate_id: String,
    pub(super) proposal_id: String,
    pub(super) content_bytes: u64,
    pub(super) content_hash: String,
    pub(super) projection_digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyCaptureApplyJournal {
    schema: String,
    journal_id: String,
    plan_id: String,
    review_id: String,
    entries: Vec<LegacyCaptureApplyJournalEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyCaptureApplyJournalEntry {
    proposal_id: String,
    content_bytes: u64,
    content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureApplyCommitMarker {
    schema: String,
    journal_id: String,
    plan_id: String,
    review_id: String,
    proposal_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CaptureApplyRecoveryOutcome {
    NoJournal,
    RolledBack,
    Committed,
}

struct LoadedCaptureApplyJournal {
    journal: CaptureApplyJournal,
    content_bytes: u64,
    content_hash: String,
}

pub(super) fn build_capture_apply_journal(
    paths: &MemoryPaths,
    plan: &CapturePlan,
    review: &CaptureReview,
    planned: &[(String, okf::OkfCreateProposalPlan)],
    authorization: &AuthorizedRepositoryProjectionBatch,
) -> Result<CaptureApplyJournal> {
    let journal = CaptureApplyJournal {
        schema: CAPTURE_APPLY_JOURNAL_SCHEMA.to_owned(),
        safety_contract_version: crate::REPOSITORY_WRITE_SAFETY_VERSION.to_owned(),
        detector_policy_version: crate::REPOSITORY_WRITE_DETECTOR_POLICY_VERSION.to_owned(),
        route: RepositoryWriteRoute::CaptureApply.as_str().to_owned(),
        authorization_digest: authorization.digest(),
        project_context_digest: blake3::hash(paths.project_root.as_os_str().as_encoded_bytes())
            .to_hex()
            .to_string(),
        journal_id: Uuid::now_v7().to_string(),
        plan_id: plan.plan_id.clone(),
        review_id: review.review_id.clone(),
        entries: planned
            .iter()
            .map(|(candidate_id, proposal)| CaptureApplyJournalEntry {
                candidate_id: candidate_id.clone(),
                proposal_id: proposal.proposal_id.clone(),
                content_bytes: proposal.markdown.len() as u64,
                content_hash: blake3::hash(proposal.markdown.as_bytes())
                    .to_hex()
                    .to_string(),
                projection_digest: {
                    let mut hasher = blake3::Hasher::new();
                    hasher.update(b"memzoi.capture.projection.v1\0");
                    hasher.update(proposal.path.as_os_str().as_encoded_bytes());
                    hasher.update(b"\0");
                    hasher.update(proposal.markdown.as_bytes());
                    hasher.finalize().to_hex().to_string()
                },
            })
            .collect(),
    };
    validate_capture_apply_journal(&journal)?;
    Ok(journal)
}

pub(super) fn capture_apply_journal_path(paths: &MemoryPaths) -> PathBuf {
    paths.runtime_dir.join(CAPTURE_APPLY_JOURNAL_FILE)
}

fn legacy_capture_apply_journal_path(paths: &MemoryPaths) -> PathBuf {
    paths.runtime_dir.join(LEGACY_CAPTURE_APPLY_JOURNAL_FILE)
}

pub(super) fn capture_apply_destination_path(
    paths: &MemoryPaths,
    entry: &CaptureApplyJournalEntry,
) -> PathBuf {
    paths
        .proposals_dir()
        .join("pending")
        .join(format!("{}.md", entry.proposal_id))
}

pub(super) fn capture_apply_stage_path(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
    entry: &CaptureApplyJournalEntry,
) -> PathBuf {
    repository_transaction_path(
        paths,
        &capture_apply_destination_path(paths, entry),
        &journal.journal_id,
        "write",
    )
}

fn capture_apply_commit_event_id(journal: &CaptureApplyJournal) -> String {
    format!("evt_capture_apply_{}", journal.journal_id)
}

fn capture_apply_commit_marker(journal: &CaptureApplyJournal) -> CaptureApplyCommitMarker {
    CaptureApplyCommitMarker {
        schema: CAPTURE_APPLY_COMMIT_SCHEMA.to_owned(),
        journal_id: journal.journal_id.clone(),
        plan_id: journal.plan_id.clone(),
        review_id: journal.review_id.clone(),
        proposal_ids: journal
            .entries
            .iter()
            .map(|entry| entry.proposal_id.clone())
            .collect(),
    }
}

fn validate_capture_apply_journal(journal: &CaptureApplyJournal) -> Result<()> {
    if journal.schema != CAPTURE_APPLY_JOURNAL_SCHEMA {
        bail!("unsupported capture apply journal schema");
    }
    if journal.safety_contract_version.is_empty()
        || journal.detector_policy_version.is_empty()
        || journal.route != RepositoryWriteRoute::CaptureApply.as_str()
    {
        bail!("capture apply journal safety decision is stale or unsupported");
    }
    for (value, label) in [
        (&journal.authorization_digest, "authorization digest"),
        (&journal.project_context_digest, "project context digest"),
    ] {
        validate_lower_hex_digest(value, label)?;
    }
    let journal_id =
        Uuid::parse_str(&journal.journal_id).context("capture apply journal id is invalid")?;
    if journal_id.to_string() != journal.journal_id {
        bail!("capture apply journal id must use canonical UUID syntax");
    }
    validate_capture_apply_journal_token(&journal.plan_id, "plan id")?;
    validate_capture_apply_journal_token(&journal.review_id, "review id")?;
    if journal.entries.is_empty() || journal.entries.len() > MAX_CAPTURE_APPLY_JOURNAL_ENTRIES {
        bail!("capture apply journal has an invalid entry count");
    }
    let mut proposal_ids = BTreeSet::new();
    for entry in &journal.entries {
        validate_capture_apply_journal_token(&entry.candidate_id, "candidate id")?;
        validate_capture_apply_proposal_id(&entry.proposal_id)?;
        if !proposal_ids.insert(entry.proposal_id.as_str()) {
            bail!("capture apply journal contains a duplicate proposal id");
        }
        if entry.content_bytes == 0 || entry.content_bytes > 8 * 1024 * 1024 {
            bail!("capture apply journal contains an invalid proposal size");
        }
        if entry.content_hash.len() != 64
            || !entry
                .content_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            bail!("capture apply journal contains an invalid content hash");
        }
        validate_lower_hex_digest(&entry.projection_digest, "projection digest")?;
    }
    Ok(())
}

fn validate_lower_hex_digest(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("capture apply journal contains an invalid {label}");
    }
    Ok(())
}

fn validate_capture_apply_journal_token(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("capture apply journal {label} is invalid");
    }
    Ok(())
}

fn validate_capture_apply_proposal_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        bail!("capture apply journal proposal id is invalid");
    }
    Ok(())
}

pub(super) fn capture_apply_journal_exists(paths: &MemoryPaths) -> Result<bool> {
    let path = capture_apply_journal_path(paths);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("capture apply journal must be a regular file")
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("failed to inspect capture apply journal"),
    }
}

pub(super) fn legacy_capture_apply_journal_exists(paths: &MemoryPaths) -> Result<bool> {
    let path = legacy_capture_apply_journal_path(paths);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("legacy capture apply journal must be a regular file")
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("failed to inspect legacy capture apply journal"),
    }
}

fn recover_legacy_capture_apply(paths: &MemoryPaths) -> Result<bool> {
    if !legacy_capture_apply_journal_exists(paths)? {
        return Ok(false);
    }
    if capture_apply_journal_exists(paths)? {
        bail!("current and legacy capture apply journals cannot coexist");
    }
    let path = legacy_capture_apply_journal_path(paths);
    let metadata =
        fs::symlink_metadata(&path).context("failed to inspect legacy capture apply journal")?;
    if metadata.len() == 0 || metadata.len() > MAX_CAPTURE_APPLY_JOURNAL_BYTES {
        bail!("legacy capture apply journal has an invalid size");
    }
    let bytes = fs::read(&path).context("failed to read legacy capture apply journal")?;
    let journal: LegacyCaptureApplyJournal =
        serde_json::from_slice(&bytes).context("failed to parse legacy capture apply journal")?;
    if journal.schema != "memzoi/capture-apply-journal-v1"
        || journal.entries.is_empty()
        || journal.entries.len() > MAX_CAPTURE_APPLY_JOURNAL_ENTRIES
    {
        bail!("legacy capture apply journal is invalid");
    }
    validate_capture_apply_journal_token(&journal.journal_id, "legacy journal id")?;
    validate_capture_apply_journal_token(&journal.plan_id, "legacy plan id")?;
    validate_capture_apply_journal_token(&journal.review_id, "legacy review id")?;
    prepare_pending_proposal_root(paths)?;
    for entry in &journal.entries {
        validate_capture_apply_proposal_id(&entry.proposal_id)?;
        validate_lower_hex_digest(&entry.content_hash, "legacy content hash")?;
        let destination = paths
            .proposals_dir()
            .join("pending")
            .join(format!("{}.md", entry.proposal_id));
        let staged = repository_transaction_path(paths, &destination, &journal.journal_id, "write");
        remove_capture_apply_file_if_matching(
            &destination,
            entry.content_bytes,
            &entry.content_hash,
            "legacy unverified capture proposal",
        )?;
        remove_capture_apply_file_if_matching(
            &staged,
            entry.content_bytes,
            &entry.content_hash,
            "legacy unverified staged capture proposal",
        )?;
    }
    remove_capture_apply_file_if_matching(
        &path,
        bytes.len() as u64,
        blake3::hash(&bytes).to_hex().as_ref(),
        "legacy capture apply journal",
    )?;
    sync_directory(&paths.proposals_dir().join("pending"))?;
    sync_directory(&paths.runtime_dir)?;
    Ok(true)
}

fn load_capture_apply_journal(paths: &MemoryPaths) -> Result<Option<LoadedCaptureApplyJournal>> {
    if !capture_apply_journal_exists(paths)? {
        return Ok(None);
    }
    let path = capture_apply_journal_path(paths);
    let metadata =
        fs::symlink_metadata(&path).context("failed to inspect capture apply journal")?;
    if metadata.len() == 0 || metadata.len() > MAX_CAPTURE_APPLY_JOURNAL_BYTES {
        bail!("capture apply journal has an invalid size");
    }
    let bytes = fs::read(&path).context("failed to read capture apply journal")?;
    if bytes.len() as u64 != metadata.len() {
        bail!("capture apply journal changed while it was being read");
    }
    let journal: CaptureApplyJournal =
        serde_json::from_slice(&bytes).context("failed to parse capture apply journal")?;
    validate_capture_apply_journal(&journal)?;
    Ok(Some(LoadedCaptureApplyJournal {
        journal,
        content_bytes: bytes.len() as u64,
        content_hash: blake3::hash(&bytes).to_hex().to_string(),
    }))
}

pub(super) fn write_capture_apply_journal(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
) -> Result<()> {
    validate_capture_apply_journal(journal)?;
    fs::create_dir_all(&paths.runtime_dir).context("failed to create capture journal directory")?;
    if capture_apply_journal_exists(paths)? || legacy_capture_apply_journal_exists(paths)? {
        bail!("an interrupted capture apply must be recovered before starting another one");
    }
    let mut bytes =
        serde_json::to_vec_pretty(journal).context("failed to serialize capture apply journal")?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_CAPTURE_APPLY_JOURNAL_BYTES {
        bail!("capture apply journal is too large");
    }
    let journal_path = capture_apply_journal_path(paths);
    let temp_path = paths.runtime_dir.join(format!(
        ".{CAPTURE_APPLY_JOURNAL_FILE}.{}.tmp",
        journal.journal_id
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp_path)
        .context("failed to stage capture apply journal")?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = remove_staged_file(&temp_path);
        return Err(error).context("failed to persist capture apply journal");
    }
    drop(file);
    if let Err(error) = fs::hard_link(&temp_path, &journal_path) {
        let _ = remove_staged_file(&temp_path);
        return Err(error).context("failed to install capture apply journal without replacement");
    }
    remove_staged_file(&temp_path).context("failed to finalize capture apply journal")?;
    sync_directory(&paths.runtime_dir).context("failed to sync capture journal directory")
}

pub(super) fn stage_capture_apply_proposals(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
    planned: &[(String, okf::OkfCreateProposalPlan)],
    authorization: &AuthorizedRepositoryProjectionBatch,
    projections: &[OwnedRepositoryProjection],
) -> Result<()> {
    validate_capture_apply_journal(journal)?;
    let borrowed = borrowed_repository_projections(projections);
    repository_io::verify_repository_batch(
        &paths.project_root,
        RepositoryWriteRoute::CaptureApply,
        &authorization.policy_context_digest,
        &authorization.capability,
        &borrowed,
    )?;
    if journal.authorization_digest != authorization.digest() {
        bail!("capture apply journal authorization digest does not match the current capability");
    }
    if journal.entries.len() != planned.len() {
        bail!("capture apply journal does not match the proposal batch");
    }
    for (entry, (_, proposal)) in journal.entries.iter().zip(planned) {
        let expected_destination = capture_apply_destination_path(paths, entry);
        let expected_hash = blake3::hash(proposal.markdown.as_bytes())
            .to_hex()
            .to_string();
        if proposal.proposal_id != entry.proposal_id
            || proposal.path != expected_destination
            || proposal.markdown.len() as u64 != entry.content_bytes
            || expected_hash != entry.content_hash
        {
            bail!("capture proposal batch changed after journaling");
        }
        let projection_digest = {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"memzoi.capture.projection.v1\0");
            hasher.update(proposal.path.as_os_str().as_encoded_bytes());
            hasher.update(b"\0");
            hasher.update(proposal.markdown.as_bytes());
            hasher.finalize().to_hex().to_string()
        };
        if projection_digest != entry.projection_digest {
            bail!("capture proposal projection digest changed after authorization");
        }
        let staged = stage_authorized_file(
            paths,
            RepositoryWriteRoute::CaptureApply,
            authorization,
            projections,
            &expected_destination,
            &proposal.markdown,
            &journal.journal_id,
        )?;
        if staged != capture_apply_stage_path(paths, journal, entry) {
            let _ = remove_staged_file(&staged);
            bail!("capture proposal staging path mismatch");
        }
    }
    sync_directory(&paths.proposals_dir().join("pending"))
        .context("failed to sync staged capture proposals")
}

pub(super) fn install_capture_apply_proposals(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
) -> Result<()> {
    validate_capture_apply_journal(journal)?;
    for entry in &journal.entries {
        let staged = capture_apply_stage_path(paths, journal, entry);
        let destination = capture_apply_destination_path(paths, entry);
        if !capture_apply_file_matches(
            &staged,
            entry.content_bytes,
            &entry.content_hash,
            "staged capture proposal",
        )? {
            bail!("staged capture proposal is missing");
        }
        ensure_path_absent(&destination, "capture proposal")?;
        fs::hard_link(&staged, &destination).with_context(|| {
            format!(
                "failed to install capture proposal {} without replacement",
                destination.display()
            )
        })?;
    }
    sync_directory(&paths.proposals_dir().join("pending"))
        .context("failed to sync installed capture proposals")
}

pub(super) fn append_capture_apply_commit_marker(
    conn: &Connection,
    journal: &CaptureApplyJournal,
    actor: &str,
    timestamp: &str,
) -> Result<()> {
    let payload = serde_json::to_string(&capture_apply_commit_marker(journal))
        .context("failed to serialize capture apply commit marker")?;
    conn.execute(
        "INSERT INTO event_log (
           id, event_type, actor, payload_json, record_id, proposal_id, created_at
         ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5)",
        rusqlite::params![
            capture_apply_commit_event_id(journal),
            CAPTURE_APPLY_COMMIT_EVENT,
            actor,
            payload,
            timestamp,
        ],
    )
    .context("failed to append capture apply commit marker")?;
    Ok(())
}

pub(super) fn capture_apply_commit_marker_exists(
    conn: &Connection,
    journal: &CaptureApplyJournal,
) -> Result<bool> {
    let row = conn
        .query_row(
            "SELECT event_type, payload_json FROM event_log WHERE id = ?1",
            [capture_apply_commit_event_id(journal)],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .context("failed to inspect capture apply commit marker")?;
    let Some((event_type, payload_json)) = row else {
        return Ok(false);
    };
    if event_type != CAPTURE_APPLY_COMMIT_EVENT {
        bail!("capture apply commit marker has an unexpected event type");
    }
    let marker: CaptureApplyCommitMarker = serde_json::from_str(&payload_json)
        .context("failed to parse capture apply commit marker")?;
    if marker != capture_apply_commit_marker(journal) {
        bail!("capture apply commit marker does not match its journal");
    }
    Ok(true)
}

fn capture_recovery_authorization_is_current(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
) -> Result<bool> {
    if journal.safety_contract_version != crate::REPOSITORY_WRITE_SAFETY_VERSION
        || journal.detector_policy_version != crate::REPOSITORY_WRITE_DETECTOR_POLICY_VERSION
        || journal.project_context_digest
            != blake3::hash(paths.project_root.as_os_str().as_encoded_bytes())
                .to_hex()
                .to_string()
    {
        return Ok(false);
    }
    let pending_root = paths.proposals_dir().join("pending");
    let mut projections = Vec::with_capacity(journal.entries.len());
    let mut values = Vec::new();
    for entry in &journal.entries {
        let destination = capture_apply_destination_path(paths, entry);
        let staged = capture_apply_stage_path(paths, journal, entry);
        let source = if capture_apply_file_matches(
            &staged,
            entry.content_bytes,
            &entry.content_hash,
            "staged capture proposal",
        )? {
            staged
        } else if capture_apply_file_matches(
            &destination,
            entry.content_bytes,
            &entry.content_hash,
            "installed capture proposal",
        )? {
            destination.clone()
        } else {
            return Ok(true);
        };
        let bytes = fs::read(&source).context("failed to read capture recovery projection")?;
        let markdown = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow!("capture recovery projection has invalid encoding"))?;
        let proposal = okf::parse_okf_proposal_markdown(&pending_root, &destination, markdown)
            .map_err(|_| anyhow!("capture recovery projection is malformed"))?
            .context("capture recovery projection was ignored")?;
        values.extend(okf_proposal_safety_values(
            &format!("candidate[{}]", entry.candidate_id),
            &proposal,
        ));
        projections.push(OwnedRepositoryProjection::from_absolute(
            paths,
            &destination,
            &bytes,
            None,
        )?);
    }
    let authorization = authorize_repository_projection_batch(
        paths,
        RepositoryWriteRoute::CaptureApply,
        OkfProposalSensitivity::RepoSafe,
        ScopeKind::Repo,
        None,
        Visibility::Repo,
        crate::AuthorizationProof::CaptureReview {
            plan_id: &journal.plan_id,
            review_id: &journal.review_id,
        },
        explicit_repository_provenance(
            RepositoryContentClass::GeneralRepoKnowledge,
            &journal.review_id,
        ),
        &values,
        &projections,
    );
    Ok(authorization
        .is_ok_and(|authorization| authorization.digest() == journal.authorization_digest))
}

pub(super) fn recover_capture_apply(
    paths: &MemoryPaths,
    conn: &Connection,
) -> Result<CaptureApplyRecoveryOutcome> {
    if recover_legacy_capture_apply(paths)? {
        return Ok(CaptureApplyRecoveryOutcome::RolledBack);
    }
    let Some(loaded) = load_capture_apply_journal(paths)? else {
        return Ok(CaptureApplyRecoveryOutcome::NoJournal);
    };
    let journal = &loaded.journal;
    let committed = capture_apply_commit_marker_exists(conn, journal)?
        && capture_recovery_authorization_is_current(paths, journal)?;
    ProposalPacketLifecycle::new(paths, conn).prepare_pending_root()?;

    if committed {
        for entry in &journal.entries {
            let destination = capture_apply_destination_path(paths, entry);
            if capture_apply_file_matches(
                &destination,
                entry.content_bytes,
                &entry.content_hash,
                "committed capture proposal",
            )? {
                continue;
            }
            let staged = capture_apply_stage_path(paths, journal, entry);
            if !capture_apply_file_matches(
                &staged,
                entry.content_bytes,
                &entry.content_hash,
                "staged capture proposal",
            )? {
                bail!("committed capture proposal and its staging file are both missing");
            }
            fs::hard_link(&staged, &destination).with_context(|| {
                format!(
                    "failed to finish committed capture proposal {}",
                    destination.display()
                )
            })?;
        }
        sync_directory(&paths.proposals_dir().join("pending"))
            .context("failed to sync recovered capture proposals")?;
        for entry in &journal.entries {
            let destination = capture_apply_destination_path(paths, entry);
            if !capture_apply_file_matches(
                &destination,
                entry.content_bytes,
                &entry.content_hash,
                "committed capture proposal",
            )? {
                bail!("committed capture proposal is missing after recovery");
            }
            remove_capture_apply_file_if_matching(
                &capture_apply_stage_path(paths, journal, entry),
                entry.content_bytes,
                &entry.content_hash,
                "staged capture proposal",
            )?;
        }
    } else {
        for entry in &journal.entries {
            remove_capture_apply_file_if_matching(
                &capture_apply_destination_path(paths, entry),
                entry.content_bytes,
                &entry.content_hash,
                "uncommitted capture proposal",
            )?;
            remove_capture_apply_file_if_matching(
                &capture_apply_stage_path(paths, journal, entry),
                entry.content_bytes,
                &entry.content_hash,
                "staged capture proposal",
            )?;
        }
    }
    sync_directory(&paths.proposals_dir().join("pending"))
        .context("failed to sync capture recovery cleanup")?;

    let journal_path = capture_apply_journal_path(paths);
    remove_capture_apply_file_if_matching(
        &journal_path,
        loaded.content_bytes,
        &loaded.content_hash,
        "capture apply journal",
    )?;
    sync_directory(&paths.runtime_dir).context("failed to sync capture journal cleanup")?;
    Ok(if committed {
        CaptureApplyRecoveryOutcome::Committed
    } else {
        CaptureApplyRecoveryOutcome::RolledBack
    })
}

fn capture_apply_file_matches(
    path: &Path,
    expected_bytes: u64,
    expected_hash: &str,
    label: &str,
) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("failed to inspect {label}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} is not a regular file; refusing recovery deletion");
    }
    if metadata.len() != expected_bytes {
        bail!("{label} does not match the recovery journal; refusing recovery deletion");
    }
    let bytes = fs::read(path).with_context(|| format!("failed to read {label}"))?;
    if bytes.len() as u64 != expected_bytes
        || blake3::hash(&bytes).to_hex().as_str() != expected_hash
    {
        bail!("{label} does not match the recovery journal; refusing recovery deletion");
    }
    Ok(true)
}

fn remove_capture_apply_file_if_matching(
    path: &Path,
    expected_bytes: u64,
    expected_hash: &str,
    label: &str,
) -> Result<()> {
    if capture_apply_file_matches(path, expected_bytes, expected_hash, label)? {
        fs::remove_file(path).with_context(|| format!("failed to remove {label}"))?;
    }
    Ok(())
}
