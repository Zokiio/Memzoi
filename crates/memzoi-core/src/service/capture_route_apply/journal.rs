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
    proposal_packets::ProposalPacketLifecycle,
    repository_mutation::{
        AuthorizedRepositoryProjectionBatch, OwnedRepositoryProjection, RepositoryFileIdentity,
        RepositoryMutationAuthorization, authorize_repository_projection_batch,
        backup_repository_file_to_transaction_with_identity, borrowed_repository_projections,
        explicit_repository_provenance, install_verified_staged_file_no_replace,
        okf_proposal_safety_values, repository_transaction_path, repository_transaction_root,
        stage_authorized_file,
    },
    safe_files::{ensure_safe_directory, remove_staged_file, sync_directory},
};

pub(super) const CAPTURE_APPLY_JOURNAL_SCHEMA: &str = "memzoi/capture-apply-journal-v2";
const CAPTURE_APPLY_COMMIT_SCHEMA: &str = "memzoi/capture-apply-commit-v2";
const CAPTURE_APPLY_OWNERSHIP_SCHEMA: &str = "memzoi/capture-apply-ownership-v2";
const CAPTURE_APPLY_JOURNAL_FILE: &str = "capture-apply-journal-v2.json";
const CAPTURE_APPLY_OWNERSHIP_FILE: &str = "capture-apply-ownership-v2.json";
const LEGACY_CAPTURE_APPLY_JOURNAL_FILE: &str = "capture-apply-journal-v1.json";
const CAPTURE_APPLY_COMMIT_EVENT: &str = "capture.apply_committed";
const MAX_CAPTURE_APPLY_JOURNAL_BYTES: u64 = 256 * 1024;
const MAX_CAPTURE_APPLY_OWNERSHIP_BYTES: u64 = 64 * 1024;
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
    journal_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureApplyOwnershipManifest {
    schema: String,
    journal_digest: String,
    entries: Vec<CaptureApplyOwnershipEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureApplyOwnershipEntry {
    proposal_id: String,
    content_hash: String,
    device: u64,
    inode: u64,
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

struct LoadedCaptureApplyOwnership {
    manifest: CaptureApplyOwnershipManifest,
    content_bytes: u64,
    content_hash: String,
}

struct CaptureRecoveryAuthorization {
    authorization: AuthorizedRepositoryProjectionBatch,
    projections: Vec<OwnedRepositoryProjection>,
}

#[cfg(test)]
type AfterCaptureRecoveryBackupHook = Box<dyn FnOnce() -> Result<()>>;

#[cfg(test)]
type BeforeCommittedCaptureRecoveryHook = Box<dyn FnOnce() -> Result<()>>;

#[cfg(test)]
thread_local! {
    static AFTER_CAPTURE_RECOVERY_BACKUP_HOOK: std::cell::RefCell<
        Option<AfterCaptureRecoveryBackupHook>,
    > = std::cell::RefCell::new(None);
}

#[cfg(test)]
thread_local! {
    static BEFORE_COMMITTED_CAPTURE_RECOVERY_HOOK: std::cell::RefCell<
        Option<BeforeCommittedCaptureRecoveryHook>,
    > = std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(super) fn inject_after_capture_recovery_backup_hook(
    hook: impl FnOnce() -> Result<()> + 'static,
) {
    AFTER_CAPTURE_RECOVERY_BACKUP_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_capture_recovery_backup_hook() -> Result<()> {
    AFTER_CAPTURE_RECOVERY_BACKUP_HOOK.with(|slot| {
        let hook = slot.borrow_mut().take();
        hook.map_or(Ok(()), |hook| hook())
    })
}

#[cfg(test)]
pub(super) fn inject_before_committed_capture_recovery_hook(
    hook: impl FnOnce() -> Result<()> + 'static,
) {
    BEFORE_COMMITTED_CAPTURE_RECOVERY_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_before_committed_capture_recovery_hook() -> Result<()> {
    BEFORE_COMMITTED_CAPTURE_RECOVERY_HOOK.with(|slot| {
        let hook = slot.borrow_mut().take();
        hook.map_or(Ok(()), |hook| hook())
    })
}

fn capture_project_context_digest(paths: &MemoryPaths) -> Result<String> {
    Ok(blake3::hash(&repository_io::repository_project_identity(
        &paths.project_root,
    )?)
    .to_hex()
    .to_string())
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
        project_context_digest: capture_project_context_digest(paths)?,
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

fn capture_apply_journal_bytes(journal: &CaptureApplyJournal) -> Result<Vec<u8>> {
    let mut bytes =
        serde_json::to_vec_pretty(journal).context("failed to serialize capture apply journal")?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_CAPTURE_APPLY_JOURNAL_BYTES {
        bail!("capture apply journal is too large");
    }
    Ok(bytes)
}

fn capture_apply_journal_digest(journal: &CaptureApplyJournal) -> Result<String> {
    Ok(blake3::hash(&capture_apply_journal_bytes(journal)?)
        .to_hex()
        .to_string())
}

pub(super) fn capture_apply_ownership_path(paths: &MemoryPaths) -> PathBuf {
    paths.runtime_dir.join(CAPTURE_APPLY_OWNERSHIP_FILE)
}

fn capture_apply_ownership_exists(paths: &MemoryPaths) -> Result<bool> {
    match fs::symlink_metadata(capture_apply_ownership_path(paths)) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("capture apply ownership must be a regular file")
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("failed to inspect capture apply ownership"),
    }
}

fn validate_capture_apply_ownership(
    manifest: &CaptureApplyOwnershipManifest,
    journal: &CaptureApplyJournal,
    expected_journal_digest: &str,
) -> Result<()> {
    if manifest.schema != CAPTURE_APPLY_OWNERSHIP_SCHEMA {
        bail!("unsupported capture apply ownership schema");
    }
    validate_lower_hex_digest(&manifest.journal_digest, "ownership journal digest")?;
    if manifest.journal_digest != expected_journal_digest {
        bail!(
            "capture apply ownership does not match the exact journal; refusing recovery mutation"
        );
    }
    if manifest.entries.len() > journal.entries.len() {
        bail!("capture apply ownership contains too many entries");
    }
    let mut proposal_ids = BTreeSet::new();
    for ownership in &manifest.entries {
        validate_capture_apply_proposal_id(&ownership.proposal_id)?;
        validate_lower_hex_digest(&ownership.content_hash, "ownership content hash")?;
        if !proposal_ids.insert(ownership.proposal_id.as_str()) {
            bail!("capture apply ownership contains a duplicate proposal id");
        }
        if !journal.entries.iter().any(|entry| {
            entry.proposal_id == ownership.proposal_id
                && entry.content_hash == ownership.content_hash
        }) {
            bail!("capture apply ownership entry does not match its exact journal entry");
        }
    }
    Ok(())
}

fn load_capture_apply_ownership(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
    expected_journal_digest: &str,
) -> Result<Option<LoadedCaptureApplyOwnership>> {
    let path = capture_apply_ownership_path(paths);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("capture apply ownership must be a regular file")
        }
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to inspect capture apply ownership"),
    };
    if metadata.len() == 0 || metadata.len() > MAX_CAPTURE_APPLY_OWNERSHIP_BYTES {
        bail!("capture apply ownership has an invalid size");
    }
    let bytes = fs::read(&path).context("failed to read capture apply ownership")?;
    if bytes.len() as u64 != metadata.len() {
        bail!("capture apply ownership changed while it was being read");
    }
    let manifest: CaptureApplyOwnershipManifest =
        serde_json::from_slice(&bytes).context("failed to parse capture apply ownership")?;
    validate_capture_apply_ownership(&manifest, journal, expected_journal_digest)?;
    Ok(Some(LoadedCaptureApplyOwnership {
        manifest,
        content_bytes: bytes.len() as u64,
        content_hash: blake3::hash(&bytes).to_hex().to_string(),
    }))
}

fn write_capture_apply_ownership(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
    manifest: &CaptureApplyOwnershipManifest,
) -> Result<()> {
    let journal_digest = capture_apply_journal_digest(journal)?;
    validate_capture_apply_ownership(manifest, journal, &journal_digest)?;
    let mut bytes = serde_json::to_vec_pretty(manifest)
        .context("failed to serialize capture apply ownership")?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_CAPTURE_APPLY_OWNERSHIP_BYTES {
        bail!("capture apply ownership is too large");
    }
    let temp_path = paths.runtime_dir.join(format!(
        ".{CAPTURE_APPLY_OWNERSHIP_FILE}.{}.tmp",
        Uuid::now_v7()
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
        .context("failed to stage capture apply ownership")?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = remove_staged_file(&temp_path);
        return Err(error).context("failed to persist capture apply ownership");
    }
    drop(file);
    if let Err(error) = fs::rename(&temp_path, capture_apply_ownership_path(paths)) {
        let _ = remove_staged_file(&temp_path);
        return Err(error).context("failed to atomically install capture apply ownership");
    }
    sync_directory(&paths.runtime_dir).context("failed to sync capture apply ownership")
}

fn record_capture_apply_install_ownership(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
    entry: &CaptureApplyJournalEntry,
    identity: RepositoryFileIdentity,
) -> Result<()> {
    let journal_digest = capture_apply_journal_digest(journal)?;
    let mut manifest = load_capture_apply_ownership(paths, journal, &journal_digest)?
        .map(|loaded| loaded.manifest)
        .unwrap_or(CaptureApplyOwnershipManifest {
            schema: CAPTURE_APPLY_OWNERSHIP_SCHEMA.to_owned(),
            journal_digest,
            entries: Vec::new(),
        });
    let ownership = CaptureApplyOwnershipEntry {
        proposal_id: entry.proposal_id.clone(),
        content_hash: entry.content_hash.clone(),
        device: identity.device,
        inode: identity.inode,
    };
    if let Some(existing) = manifest
        .entries
        .iter_mut()
        .find(|existing| existing.proposal_id == entry.proposal_id)
    {
        *existing = ownership;
    } else {
        manifest.entries.push(ownership);
    }
    manifest.entries.sort_by_key(|ownership| {
        journal
            .entries
            .iter()
            .position(|entry| entry.proposal_id == ownership.proposal_id)
            .unwrap_or(usize::MAX)
    });
    write_capture_apply_ownership(paths, journal, &manifest)
}

fn capture_apply_commit_marker(
    journal: &CaptureApplyJournal,
    journal_digest: String,
) -> CaptureApplyCommitMarker {
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
        journal_digest,
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
    if capture_apply_ownership_exists(paths)? {
        bail!("legacy journal and current capture ownership cannot coexist");
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
    let mut recovery_entries = Vec::with_capacity(journal.entries.len());
    for entry in &journal.entries {
        validate_capture_apply_proposal_id(&entry.proposal_id)?;
        validate_lower_hex_digest(&entry.content_hash, "legacy content hash")?;
        if entry.content_bytes == 0 || entry.content_bytes > 8 * 1024 * 1024 {
            bail!("legacy capture apply journal contains an invalid proposal size");
        }
        let destination = paths
            .proposals_dir()
            .join("pending")
            .join(format!("{}.md", entry.proposal_id));
        let staged = repository_transaction_path(paths, &destination, &journal.journal_id, "write");
        match fs::symlink_metadata(&destination) {
            Ok(_) => bail!(
                "legacy capture recovery has no installed-file ownership proof; refusing recovery mutation"
            ),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("failed to inspect legacy capture destination");
            }
        }
        recovery_entries.push((staged, entry));
    }
    for (staged, entry) in recovery_entries {
        remove_capture_apply_transaction_file_if_matching(
            paths,
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
    if capture_apply_journal_exists(paths)?
        || legacy_capture_apply_journal_exists(paths)?
        || capture_apply_ownership_exists(paths)?
    {
        bail!("an interrupted capture apply must be recovered before starting another one");
    }
    let bytes = capture_apply_journal_bytes(journal)?;
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
    authorization: &AuthorizedRepositoryProjectionBatch,
    projections: &[OwnedRepositoryProjection],
) -> Result<()> {
    install_capture_apply_proposals_with_hook(paths, journal, authorization, projections, |_, _| {
        Ok(())
    })
}

pub(super) fn install_capture_apply_proposals_with_hook<BeforeInstall>(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
    authorization: &AuthorizedRepositoryProjectionBatch,
    projections: &[OwnedRepositoryProjection],
    before_install: BeforeInstall,
) -> Result<()>
where
    BeforeInstall: FnMut(usize, &Path) -> Result<()>,
{
    install_capture_apply_proposals_with_hooks(
        paths,
        journal,
        authorization,
        projections,
        before_install,
        |_, _| Ok(()),
    )
}

pub(super) fn install_capture_apply_proposals_with_hooks<BeforeInstall, AfterInstall>(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
    authorization: &AuthorizedRepositoryProjectionBatch,
    projections: &[OwnedRepositoryProjection],
    mut before_install: BeforeInstall,
    mut after_install_before_ownership: AfterInstall,
) -> Result<()>
where
    BeforeInstall: FnMut(usize, &Path) -> Result<()>,
    AfterInstall: FnMut(usize, &Path) -> Result<()>,
{
    validate_capture_apply_journal(journal)?;
    let journal_digest = capture_apply_journal_digest(journal)?;
    let persisted = load_capture_apply_journal(paths)?
        .context("capture apply journal must be durable before repository installation")?;
    if persisted.journal != *journal || persisted.content_hash != journal_digest {
        bail!("durable capture apply journal changed before repository installation");
    }
    if journal.authorization_digest != authorization.digest() {
        bail!("capture apply journal authorization digest does not match the current capability");
    }
    let mutation = RepositoryMutationAuthorization {
        route: RepositoryWriteRoute::CaptureApply,
        authorization,
        projections,
    };
    for (index, entry) in journal.entries.iter().enumerate() {
        let staged = capture_apply_stage_path(paths, journal, entry);
        let destination = capture_apply_destination_path(paths, entry);
        before_install(index, &staged)?;
        let installed_file = install_verified_staged_file_no_replace(
            paths,
            mutation,
            &staged,
            &destination,
            &entry.content_hash,
        )
        .with_context(|| {
            format!(
                "failed to install capture proposal {} without replacement",
                destination.display()
            )
        })?;
        let identity = installed_file.identity();
        after_install_before_ownership(index, &destination)?;
        record_capture_apply_install_ownership(paths, journal, entry, identity)?;
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
    let marker = capture_apply_commit_marker(journal, capture_apply_journal_digest(journal)?);
    let payload = serde_json::to_string(&marker)
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

#[cfg(test)]
pub(super) fn capture_apply_commit_marker_exists(
    conn: &Connection,
    journal: &CaptureApplyJournal,
) -> Result<bool> {
    let journal_digest = capture_apply_journal_digest(journal)?;
    capture_apply_commit_marker_exists_for_digest(conn, journal, &journal_digest)
}

fn capture_apply_commit_marker_exists_for_digest(
    conn: &Connection,
    journal: &CaptureApplyJournal,
    journal_digest: &str,
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
    if marker != capture_apply_commit_marker(journal, journal_digest.to_owned()) {
        bail!("capture apply commit marker does not match its journal");
    }
    Ok(true)
}

fn capture_recovery_authorization(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
) -> Result<Option<CaptureRecoveryAuthorization>> {
    if journal.safety_contract_version != crate::REPOSITORY_WRITE_SAFETY_VERSION
        || journal.detector_policy_version != crate::REPOSITORY_WRITE_DETECTOR_POLICY_VERSION
        || journal.project_context_digest != capture_project_context_digest(paths)?
    {
        return Ok(None);
    }
    let pending_root = paths.proposals_dir().join("pending");
    let mut projections = Vec::with_capacity(journal.entries.len());
    let mut values = Vec::new();
    for entry in &journal.entries {
        let destination = capture_apply_destination_path(paths, entry);
        let staged = capture_apply_stage_path(paths, journal, entry);
        let bytes = if let Some(bytes) = read_capture_apply_transaction_file_if_exists(
            paths,
            &staged,
            entry.content_bytes,
            "staged capture proposal",
        )? {
            bytes
        } else {
            let relative = destination
                .strip_prefix(&paths.project_root)
                .context("capture recovery destination is outside the project root")?;
            repository_io::read_repository_file_if_exists(
                &paths.project_root,
                relative,
                entry.content_bytes,
                "installed capture proposal",
            )?
            .context("committed capture proposal and its staging file are both missing")?
        };
        ensure_capture_apply_bytes_match(
            &bytes,
            &entry.content_hash,
            "capture recovery projection",
        )?;
        let markdown = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow!("capture recovery projection has invalid encoding"))?;
        let proposal = okf::parse_okf_proposal_markdown(&pending_root, &destination, markdown)
            .map_err(|_| anyhow!("capture recovery projection is malformed"))?
            .context("capture recovery projection was ignored")?;
        super::validate_capture_proposal_projection_values(
            proposal.scope_kind,
            proposal.scope_id.as_deref(),
            proposal.sensitivity,
            proposal.content_class,
        )?;
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
    Ok(authorization.ok().and_then(|authorization| {
        (authorization.digest() == journal.authorization_digest).then_some(
            CaptureRecoveryAuthorization {
                authorization,
                projections,
            },
        )
    }))
}

fn capture_ownership_identity(entry: &CaptureApplyOwnershipEntry) -> RepositoryFileIdentity {
    RepositoryFileIdentity {
        device: entry.device,
        inode: entry.inode,
    }
}

fn capture_apply_recovery_backup_path(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
    entry: &CaptureApplyJournalEntry,
) -> PathBuf {
    repository_transaction_path(
        paths,
        &capture_apply_destination_path(paths, entry),
        &journal.review_id,
        "recovery-cleanup",
    )
}

fn preflight_uncommitted_capture_recovery_backup(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
    entry: &CaptureApplyJournalEntry,
    has_ownership: bool,
) -> Result<()> {
    let backup = capture_apply_recovery_backup_path(paths, journal, entry);
    let Some(bytes) = read_capture_apply_transaction_file_if_exists(
        paths,
        &backup,
        entry.content_bytes,
        "capture recovery backup",
    )?
    else {
        return Ok(());
    };
    ensure_capture_apply_bytes_match(&bytes, &entry.content_hash, "capture recovery backup")?;
    if !has_ownership {
        bail!(
            "capture recovery backup has no installed-file ownership proof; refusing recovery mutation"
        );
    }
    Ok(())
}

fn preflight_uncommitted_capture_recovery(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
    ownership: Option<&CaptureApplyOwnershipManifest>,
) -> Result<Vec<Option<RepositoryFileIdentity>>> {
    let mut installed = Vec::with_capacity(journal.entries.len());
    for entry in &journal.entries {
        let expected_identity = ownership
            .and_then(|manifest| {
                manifest
                    .entries
                    .iter()
                    .find(|ownership| ownership.proposal_id == entry.proposal_id)
            })
            .map(capture_ownership_identity);
        preflight_uncommitted_capture_recovery_backup(
            paths,
            journal,
            entry,
            expected_identity.is_some(),
        )?;
        let destination = capture_apply_destination_path(paths, entry);
        match fs::symlink_metadata(&destination) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                installed.push(expected_identity);
                continue;
            }
            Err(error) => {
                return Err(error)
                    .context("failed to inspect uncommitted capture destination before recovery");
            }
        }
        let relative = destination
            .strip_prefix(&paths.project_root)
            .context("capture recovery destination is outside the project root")?;
        let snapshot = repository_io::read_repository_file_with_identity_if_exists(
            &paths.project_root,
            relative,
            entry.content_bytes,
            "uncommitted capture proposal",
        )
        .context("uncommitted capture destination is ambiguous; refusing recovery mutation")?;
        let Some((bytes, actual_identity)) = snapshot else {
            installed.push(expected_identity);
            continue;
        };
        ensure_capture_apply_bytes_match(
            &bytes,
            &entry.content_hash,
            "uncommitted capture proposal",
        )?;
        let expected_identity = expected_identity.context(
                "uncommitted capture destination has no installed-file ownership proof; refusing recovery mutation",
            )?;
        if actual_identity != expected_identity {
            bail!(
                "uncommitted capture destination does not match installed-file ownership; refusing recovery mutation"
            );
        }
        installed.push(Some(expected_identity));
    }
    Ok(installed)
}

pub(super) fn recover_capture_apply(
    paths: &MemoryPaths,
    conn: &Connection,
) -> Result<CaptureApplyRecoveryOutcome> {
    recover_capture_apply_with_hook(paths, conn, |_, _| Ok(()))
}

pub(super) fn recover_capture_apply_with_hook<BeforeInstall>(
    paths: &MemoryPaths,
    conn: &Connection,
    mut before_install: BeforeInstall,
) -> Result<CaptureApplyRecoveryOutcome>
where
    BeforeInstall: FnMut(usize, &Path) -> Result<()>,
{
    if recover_legacy_capture_apply(paths)? {
        return Ok(CaptureApplyRecoveryOutcome::RolledBack);
    }
    let Some(loaded) = load_capture_apply_journal(paths)? else {
        if capture_apply_ownership_exists(paths)? {
            bail!("capture apply ownership exists without its journal; refusing recovery mutation");
        }
        return Ok(CaptureApplyRecoveryOutcome::NoJournal);
    };
    let journal = &loaded.journal;
    if journal.project_context_digest != capture_project_context_digest(paths)? {
        bail!(
            "capture apply journal belongs to a different repository root; refusing recovery mutation"
        );
    }
    let ownership = load_capture_apply_ownership(paths, journal, &loaded.content_hash)?;
    let recovery_authorization = if capture_apply_commit_marker_exists_for_digest(
        conn,
        journal,
        &loaded.content_hash,
    )? {
        Some(
            capture_recovery_authorization(paths, journal)
                .context(
                    "failed to validate committed capture recovery authorization; refusing recovery mutation",
                )?
                .context(
                    "committed capture apply journal authorization is stale or invalid; refusing recovery mutation",
                )?,
        )
    } else {
        None
    };
    let committed = recovery_authorization.is_some();

    if let Some(recovery_authorization) = recovery_authorization.as_ref() {
        #[cfg(test)]
        run_before_committed_capture_recovery_hook()?;
        ProposalPacketLifecycle::new(paths, conn).prepare_pending_root()?;
        let mutation = RepositoryMutationAuthorization {
            route: RepositoryWriteRoute::CaptureApply,
            authorization: &recovery_authorization.authorization,
            projections: &recovery_authorization.projections,
        };
        for (index, entry) in journal.entries.iter().enumerate() {
            let destination = capture_apply_destination_path(paths, entry);
            let relative = destination
                .strip_prefix(&paths.project_root)
                .context("capture recovery destination is outside the project root")?;
            if let Some(bytes) = repository_io::read_repository_file_if_exists(
                &paths.project_root,
                relative,
                entry.content_bytes,
                "committed capture proposal",
            )? {
                ensure_capture_apply_bytes_match(
                    &bytes,
                    &entry.content_hash,
                    "committed capture proposal",
                )?;
                continue;
            }
            let staged = capture_apply_stage_path(paths, journal, entry);
            before_install(index, &staged)?;
            install_verified_staged_file_no_replace(
                paths,
                mutation,
                &staged,
                &destination,
                &entry.content_hash,
            )
            .with_context(|| {
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
            let relative = destination
                .strip_prefix(&paths.project_root)
                .context("capture recovery destination is outside the project root")?;
            let bytes = repository_io::read_repository_file_if_exists(
                &paths.project_root,
                relative,
                entry.content_bytes,
                "committed capture proposal",
            )?
            .context("committed capture proposal is missing after recovery")?;
            ensure_capture_apply_bytes_match(
                &bytes,
                &entry.content_hash,
                "committed capture proposal",
            )?;
            remove_capture_apply_transaction_file_if_matching(
                paths,
                &capture_apply_stage_path(paths, journal, entry),
                entry.content_bytes,
                &entry.content_hash,
                "staged capture proposal",
            )?;
        }
    } else {
        let installed = preflight_uncommitted_capture_recovery(
            paths,
            journal,
            ownership.as_ref().map(|loaded| &loaded.manifest),
        )?;
        for (entry, expected_identity) in journal.entries.iter().zip(installed) {
            if let Some(expected_identity) = expected_identity {
                remove_uncommitted_capture_destination(paths, journal, entry, expected_identity)?;
            }
            remove_capture_apply_transaction_file_if_matching(
                paths,
                &capture_apply_stage_path(paths, journal, entry),
                entry.content_bytes,
                &entry.content_hash,
                "staged capture proposal",
            )?;
        }
    }

    if let Some(ownership) = ownership {
        remove_capture_apply_file_if_matching(
            &capture_apply_ownership_path(paths),
            ownership.content_bytes,
            &ownership.content_hash,
            "capture apply ownership",
        )?;
    }

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

fn ensure_capture_apply_bytes_match(bytes: &[u8], expected_hash: &str, label: &str) -> Result<()> {
    if blake3::hash(bytes).to_hex().as_str() != expected_hash {
        bail!("{label} does not match the recovery journal; refusing recovery mutation");
    }
    Ok(())
}

fn remove_capture_apply_transaction_file_if_matching(
    paths: &MemoryPaths,
    path: &Path,
    expected_bytes: u64,
    expected_hash: &str,
    label: &str,
) -> Result<()> {
    let Some(bytes) =
        read_capture_apply_transaction_file_if_exists(paths, path, expected_bytes, label)?
    else {
        return Ok(());
    };
    ensure_capture_apply_bytes_match(&bytes, expected_hash, label)?;
    remove_staged_file(path)?;
    sync_directory(&repository_transaction_root(paths))
}

fn read_capture_apply_transaction_file_if_exists(
    paths: &MemoryPaths,
    path: &Path,
    expected_bytes: u64,
    label: &str,
) -> Result<Option<Vec<u8>>> {
    let transaction_root = repository_transaction_root(paths);
    match fs::symlink_metadata(&transaction_root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!("repository transaction root must be a real directory")
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to inspect repository transaction root"),
    }
    repository_io::read_transaction_file_if_exists(&transaction_root, path, expected_bytes, label)
}

fn remove_uncommitted_capture_destination(
    paths: &MemoryPaths,
    journal: &CaptureApplyJournal,
    entry: &CaptureApplyJournalEntry,
    expected_identity: RepositoryFileIdentity,
) -> Result<()> {
    let destination = capture_apply_destination_path(paths, entry);
    let backup = capture_apply_recovery_backup_path(paths, journal, entry);
    remove_capture_destination_with_recovery_authorization(
        paths,
        &destination,
        &backup,
        entry.content_bytes,
        &entry.content_hash,
        &journal.authorization_digest,
        &journal.review_id,
        "uncommitted capture proposal",
        expected_identity,
    )
}

#[allow(clippy::too_many_arguments)]
fn remove_capture_destination_with_recovery_authorization(
    paths: &MemoryPaths,
    destination: &Path,
    backup: &Path,
    expected_bytes: u64,
    expected_hash: &str,
    authorization_digest: &str,
    source_identity: &str,
    label: &str,
    expected_identity: RepositoryFileIdentity,
) -> Result<()> {
    let relative = destination
        .strip_prefix(&paths.project_root)
        .context("capture recovery destination is outside the project root")?;
    let Some((bytes, actual_identity)) =
        repository_io::read_repository_file_with_identity_if_exists(
            &paths.project_root,
            relative,
            expected_bytes,
            label,
        )
        .with_context(|| {
            format!("{label} does not match the recovery journal; refusing recovery deletion")
        })?
    else {
        return remove_capture_apply_transaction_file_if_matching(
            paths,
            backup,
            expected_bytes,
            expected_hash,
            "capture recovery backup",
        );
    };
    if actual_identity != expected_identity {
        bail!("{label} does not match installed-file ownership; refusing recovery deletion");
    }
    if blake3::hash(&bytes).to_hex().as_str() != expected_hash {
        bail!("{label} does not match the recovery journal; refusing recovery deletion");
    }
    remove_capture_apply_transaction_file_if_matching(
        paths,
        backup,
        expected_bytes,
        expected_hash,
        "capture recovery backup",
    )?;
    let transaction_root = repository_transaction_root(paths);
    ensure_safe_directory(
        &paths.runtime_dir,
        &transaction_root,
        true,
        "local repository transaction root",
    )?;
    let projections = vec![OwnedRepositoryProjection::existing_from_absolute(
        paths,
        destination,
        &bytes,
        expected_hash,
    )?];
    let authorization = authorize_repository_projection_batch(
        paths,
        RepositoryWriteRoute::Recovery,
        OkfProposalSensitivity::RepoSafe,
        ScopeKind::Repo,
        None,
        Visibility::Repo,
        crate::AuthorizationProof::Recovery {
            authorization_digest,
        },
        explicit_repository_provenance(
            RepositoryContentClass::GeneralRepoKnowledge,
            source_identity,
        ),
        &[],
        &projections,
    )?;
    backup_repository_file_to_transaction_with_identity(
        paths,
        RepositoryMutationAuthorization {
            route: RepositoryWriteRoute::Recovery,
            authorization: &authorization,
            projections: &projections,
        },
        destination,
        backup,
        expected_hash,
        Some(expected_identity),
    )?;
    #[cfg(test)]
    run_after_capture_recovery_backup_hook()?;
    remove_capture_apply_transaction_file_if_matching(
        paths,
        backup,
        expected_bytes,
        expected_hash,
        "capture recovery backup",
    )
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
