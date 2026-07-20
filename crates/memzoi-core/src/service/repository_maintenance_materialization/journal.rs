use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    CanonicalRevision, MAINTENANCE_MAX_INPUT_FILE_BYTES, MaintenanceAction, MaintenancePlan,
    MaterializationAction, MaterializationOutputRole, MemoryPaths,
    REPOSITORY_MAINTENANCE_MATERIALIZATION_JOURNAL_SCHEMA, RepositoryMaintenanceDecisionBinding,
    RepositoryMaintenanceMaterializationRequest, RepositoryWriteRoute,
    canonical_revision_for_okf_record, okf, repository_io,
};

use super::{PreparedMaintenanceMaterialization, path_text};
use crate::service::{
    repository_mutation::{
        AuthorizedRepositoryProjectionBatch, OwnedRepositoryProjection, RepositoryFileIdentity,
        borrowed_repository_projections, repository_transaction_path, repository_transaction_root,
    },
    safe_files::{remove_staged_file, sync_directory},
};

const JOURNAL_FILE: &str = "repository-maintenance-materialization-journal.json";
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 256;
const COMMIT_SCHEMA: &str = "memzoi/repository-maintenance-materialization-commit";
const COMMIT_EVENT: &str = "repository_maintenance.materialization_committed";

#[cfg(test)]
thread_local! {
    static INJECT_REWRITE_PARTIAL_WRITE: std::cell::Cell<bool> = const {
        std::cell::Cell::new(false)
    };
}

#[cfg(test)]
pub(super) fn inject_rewrite_partial_write_failure() {
    INJECT_REWRITE_PARTIAL_WRITE.with(|injected| injected.set(true));
}

#[cfg(test)]
fn take_rewrite_partial_write_failure() -> bool {
    INJECT_REWRITE_PARTIAL_WRITE.with(|injected| injected.replace(false))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MaintenanceMaterializationJournal {
    pub(super) schema: String,
    pub(super) route: String,
    pub(super) transaction_id: String,
    pub(super) project_context_digest: String,
    pub(super) plan: MaintenancePlan,
    pub(super) request: RepositoryMaintenanceMaterializationRequest,
    pub(super) selection_id: String,
    pub(super) selected_actions: Vec<MaintenanceAction>,
    pub(super) decision_id: String,
    pub(super) decision: RepositoryMaintenanceDecisionBinding,
    pub(super) policy_context_digest: String,
    pub(super) projection_digest: String,
    pub(super) authorization_digest: String,
    pub(super) safety_fields_digest: String,
    pub(super) outputs: Vec<MaintenanceJournalOutput>,
    pub(super) comparisons: Vec<MaintenanceJournalComparison>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MaintenanceJournalOutput {
    pub(super) action_id: String,
    pub(super) path: String,
    pub(super) record_id: String,
    pub(super) action: MaterializationAction,
    pub(super) role: MaterializationOutputRole,
    pub(super) prior_bytes: u64,
    pub(super) prior_hash: String,
    pub(super) prior_semantic_revision: CanonicalRevision,
    pub(super) post_bytes: u64,
    pub(super) post_hash: String,
    pub(super) post_semantic_revision: CanonicalRevision,
    pub(super) prior_device: u64,
    pub(super) prior_inode: u64,
    pub(super) post_device: u64,
    pub(super) post_inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MaintenanceJournalComparison {
    pub(super) path: String,
    pub(super) bytes: u64,
    pub(super) hash: String,
    pub(super) semantic_revision: CanonicalRevision,
}

pub(super) struct LoadedMaintenanceJournal {
    pub(super) journal: MaintenanceMaterializationJournal,
    pub(super) content_bytes: u64,
    pub(super) content_hash: String,
    pub(super) file_identity: RepositoryFileIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaintenanceCommitMarker {
    schema: String,
    transaction_id: String,
    plan_id: String,
    selection_id: String,
    decision_id: String,
    journal_digest: String,
}

pub(super) fn journal_path(paths: &MemoryPaths) -> PathBuf {
    paths.runtime_dir.join(JOURNAL_FILE)
}

fn journal_temporary_path(
    paths: &MemoryPaths,
    journal: &MaintenanceMaterializationJournal,
) -> PathBuf {
    paths
        .runtime_dir
        .join(format!(".{JOURNAL_FILE}.{}.tmp", journal.transaction_id))
}

pub(super) fn journal_exists(paths: &MemoryPaths) -> Result<bool> {
    match fs::symlink_metadata(journal_path(paths)) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("repository maintenance journal must be a regular file")
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("failed to inspect repository maintenance journal"),
    }
}

pub(super) fn build_journal(
    paths: &MemoryPaths,
    plan: &MaintenancePlan,
    prepared: &PreparedMaintenanceMaterialization,
    identities: &[RepositoryFileIdentity],
    authorization: &AuthorizedRepositoryProjectionBatch,
    projections: &[OwnedRepositoryProjection],
    safety_fields_digest: String,
) -> Result<MaintenanceMaterializationJournal> {
    ensure!(
        identities.len() == prepared.outputs.len(),
        "maintenance journal file-identity count does not match outputs"
    );
    let transaction_id = transaction_id_for_decision(&prepared.decision_id);
    let borrowed = borrowed_repository_projections(projections);
    let projection_digest = crate::repository_write_safety::projection_digest(&borrowed);
    let journal = MaintenanceMaterializationJournal {
        schema: REPOSITORY_MAINTENANCE_MATERIALIZATION_JOURNAL_SCHEMA.to_owned(),
        route: RepositoryWriteRoute::Maintenance.as_str().to_owned(),
        transaction_id,
        project_context_digest: project_context_digest(paths)?,
        plan: plan.clone(),
        request: RepositoryMaintenanceMaterializationRequest {
            schema: crate::REPOSITORY_MAINTENANCE_MATERIALIZATION_REQUEST_SCHEMA.to_owned(),
            plan_id: prepared.selection.plan_id.clone(),
            selected_action_ids: prepared
                .selection
                .selected_actions
                .iter()
                .map(|action| action.action_id.clone())
                .collect(),
            decision_at: prepared.decision.decision_at.clone(),
        },
        selection_id: prepared.selection.selection_id.clone(),
        selected_actions: prepared.selection.selected_actions.clone(),
        decision_id: prepared.decision_id.clone(),
        decision: prepared.decision.clone(),
        policy_context_digest: encode_digest(&authorization.policy_context_digest),
        projection_digest: encode_digest(&projection_digest),
        authorization_digest: authorization.digest(),
        safety_fields_digest,
        outputs: prepared
            .outputs
            .iter()
            .zip(identities)
            .map(|(output, identity)| {
                Ok(MaintenanceJournalOutput {
                    action_id: output.spec.action_id.clone(),
                    path: path_text(&output.spec.path)?,
                    record_id: output.spec.record_id.clone(),
                    action: output.spec.action,
                    role: output.spec.role,
                    prior_bytes: output.prior.bytes.len() as u64,
                    prior_hash: hash(&output.prior.bytes),
                    prior_semantic_revision: output.prior.semantic_revision.clone(),
                    post_bytes: output.markdown.len() as u64,
                    post_hash: hash(output.markdown.as_bytes()),
                    post_semantic_revision: output.intent.intended_semantic_revision.clone(),
                    prior_device: identity.device,
                    prior_inode: identity.inode,
                    post_device: 0,
                    post_inode: 0,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        comparisons: prepared
            .comparisons
            .iter()
            .map(|comparison| {
                Ok(MaintenanceJournalComparison {
                    path: path_text(&comparison.path)?,
                    bytes: comparison.bytes.len() as u64,
                    hash: hash(&comparison.bytes),
                    semantic_revision: comparison.expected_revision.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
    };
    validate_journal(&journal)?;
    Ok(journal)
}

pub(super) fn validate_recovery_context(
    paths: &MemoryPaths,
    journal: &MaintenanceMaterializationJournal,
) -> Result<()> {
    validate_journal(journal)?;
    ensure!(
        journal.project_context_digest == project_context_digest(paths)?,
        "repository maintenance journal belongs to another repository"
    );
    Ok(())
}

pub(super) fn stage_path(
    paths: &MemoryPaths,
    journal: &MaintenanceMaterializationJournal,
    entry: &MaintenanceJournalOutput,
) -> PathBuf {
    repository_transaction_path(
        paths,
        &paths.project_root.join(&entry.path),
        &journal.transaction_id,
        "write",
    )
}

pub(super) fn backup_path(
    paths: &MemoryPaths,
    journal: &MaintenanceMaterializationJournal,
    entry: &MaintenanceJournalOutput,
) -> PathBuf {
    repository_transaction_path(
        paths,
        &paths.project_root.join(&entry.path),
        &journal.transaction_id,
        "backup",
    )
}

pub(super) fn load_journal(paths: &MemoryPaths) -> Result<Option<LoadedMaintenanceJournal>> {
    let path = journal_path(paths);
    let Some((bytes, file_identity, mode)) =
        repository_io::read_bounded_direct_child_file_if_exists(
            &paths.runtime_dir,
            &path,
            MAX_JOURNAL_BYTES,
            "repository maintenance journal",
        )?
    else {
        return Ok(None);
    };
    ensure!(
        mode & 0o077 == 0,
        "repository maintenance journal permissions are not owner-only"
    );
    let journal: MaintenanceMaterializationJournal =
        serde_json::from_slice(&bytes).context("invalid repository maintenance journal")?;
    validate_journal(&journal)?;
    Ok(Some(LoadedMaintenanceJournal {
        journal,
        content_bytes: bytes.len() as u64,
        content_hash: hash(&bytes),
        file_identity,
    }))
}

pub(super) fn write_journal(
    paths: &MemoryPaths,
    journal: &MaintenanceMaterializationJournal,
) -> Result<()> {
    validate_journal(journal)?;
    fs::create_dir_all(&paths.runtime_dir)
        .context("failed to create repository maintenance journal directory")?;
    ensure!(
        !journal_exists(paths)?,
        "an interrupted repository maintenance transaction must be recovered first"
    );
    let bytes = journal_bytes(journal)?;
    let temporary = journal_temporary_path(paths, journal);
    remove_runtime_file_if_matching(
        paths,
        &temporary,
        bytes.len() as u64,
        &hash(&bytes),
        "orphan repository maintenance journal temporary",
    )?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .context("failed to stage repository maintenance journal")?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        let persistence =
            anyhow::Error::new(error).context("failed to persist repository maintenance journal");
        return match repository_io::remove_created_direct_child_file(
            &paths.runtime_dir,
            &temporary,
            &file,
            "incomplete repository maintenance journal",
        ) {
            Ok(()) => Err(persistence),
            Err(cleanup) => Err(persistence).context(format!(
                "additionally failed to clean incomplete maintenance journal: {cleanup:#}"
            )),
        };
    }
    drop(file);
    super::maintenance_transition("after_journal_temp_sync", 0);
    if let Err(error) = fs::hard_link(&temporary, journal_path(paths)) {
        let _ = remove_staged_file(&temporary);
        return Err(error).context("failed to install repository maintenance journal");
    }
    super::maintenance_transition("after_journal_link", 0);
    remove_staged_file(&temporary)?;
    sync_directory(&paths.runtime_dir)
        .context("failed to sync repository maintenance journal directory")
}

pub(super) fn rewrite_journal(
    paths: &MemoryPaths,
    loaded: &LoadedMaintenanceJournal,
    journal: &MaintenanceMaterializationJournal,
) -> Result<LoadedMaintenanceJournal> {
    validate_journal(journal)?;
    ensure!(
        loaded.journal.transaction_id == journal.transaction_id,
        "repository maintenance journal rewrite changed transaction identity"
    );
    verify_regular_file(
        paths,
        &journal_path(paths),
        loaded.content_bytes,
        &loaded.content_hash,
        Some(loaded.file_identity),
        "repository maintenance journal",
    )?;
    let bytes = journal_bytes(journal)?;
    let temporary = paths.runtime_dir.join(format!(
        ".{JOURNAL_FILE}.{}.{}.rewrite.tmp",
        journal.transaction_id,
        hash(&bytes)
    ));
    remove_runtime_file_if_matching(
        paths,
        &temporary,
        bytes.len() as u64,
        &hash(&bytes),
        "orphan repository maintenance journal rewrite",
    )?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .context("failed to stage repository maintenance journal rewrite")?;
    #[cfg(test)]
    let persistence = if take_rewrite_partial_write_failure() {
        let partial_len = bytes.len().min(1);
        file.write_all(&bytes[..partial_len]).and_then(|_| {
            Err(std::io::Error::other(
                "injected maintenance journal rewrite failure",
            ))
        })
    } else {
        file.write_all(&bytes).and_then(|_| file.sync_all())
    };
    #[cfg(not(test))]
    let persistence = file.write_all(&bytes).and_then(|_| file.sync_all());
    if let Err(error) = persistence {
        let persistence = anyhow::Error::new(error)
            .context("failed to persist repository maintenance journal rewrite");
        return match repository_io::remove_created_direct_child_file(
            &paths.runtime_dir,
            &temporary,
            &file,
            "incomplete repository maintenance journal rewrite",
        ) {
            Ok(()) => Err(persistence),
            Err(cleanup) => Err(persistence).context(format!(
                "additionally failed to clean incomplete maintenance journal rewrite: {cleanup:#}"
            )),
        };
    }
    drop(file);
    fs::rename(&temporary, journal_path(paths))
        .context("failed to install repository maintenance journal rewrite")?;
    sync_directory(&paths.runtime_dir)
        .context("failed to sync repository maintenance journal rewrite")?;
    let reloaded =
        load_journal(paths)?.context("rewritten repository maintenance journal disappeared")?;
    ensure!(
        reloaded.journal == *journal,
        "rewritten repository maintenance journal changed"
    );
    Ok(reloaded)
}

pub(super) fn read_stage(
    paths: &MemoryPaths,
    journal: &MaintenanceMaterializationJournal,
    entry: &MaintenanceJournalOutput,
) -> Result<Vec<u8>> {
    read_exact_transaction_file(
        paths,
        &stage_path(paths, journal, entry),
        entry.post_bytes,
        &entry.post_hash,
        "repository maintenance staged final",
    )
}

pub(super) fn read_backup(
    paths: &MemoryPaths,
    journal: &MaintenanceMaterializationJournal,
    entry: &MaintenanceJournalOutput,
) -> Result<Vec<u8>> {
    read_exact_transaction_file(
        paths,
        &backup_path(paths, journal, entry),
        entry.prior_bytes,
        &entry.prior_hash,
        "repository maintenance backup",
    )
}

pub(super) fn cleanup(paths: &MemoryPaths, loaded: &LoadedMaintenanceJournal) -> Result<()> {
    let journal = &loaded.journal;
    for entry in &journal.outputs {
        verify_transaction_file(
            paths,
            &stage_path(paths, journal, entry),
            entry.post_bytes,
            &entry.post_hash,
            "repository maintenance staged final",
        )?;
        verify_transaction_file(
            paths,
            &backup_path(paths, journal, entry),
            entry.prior_bytes,
            &entry.prior_hash,
            "repository maintenance backup",
        )?;
    }
    verify_regular_file(
        paths,
        &journal_path(paths),
        loaded.content_bytes,
        &loaded.content_hash,
        Some(loaded.file_identity),
        "repository maintenance journal",
    )?;
    remove_staged_file(&journal_path(paths))?;
    sync_directory(&paths.runtime_dir)?;
    super::maintenance_transition("after_journal_cleanup", journal.outputs.len());
    remove_runtime_file_if_matching(
        paths,
        &journal_temporary_path(paths, journal),
        loaded.content_bytes,
        &loaded.content_hash,
        "repository maintenance journal temporary",
    )?;
    for entry in &journal.outputs {
        remove_staged_file(&stage_path(paths, journal, entry))?;
        remove_staged_file(&backup_path(paths, journal, entry))?;
    }
    sync_directory(&repository_transaction_root(paths))
}

pub(super) fn cleanup_orphans_for_completed_decision(
    paths: &MemoryPaths,
    prepared: &PreparedMaintenanceMaterialization,
) -> Result<()> {
    let root = repository_transaction_root(paths);
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!("repository transaction root is unsafe")
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("failed to inspect repository transaction root"),
    }
    let transaction_id = transaction_id_for_decision(&prepared.decision_id);
    for output in &prepared.outputs {
        let destination = paths.project_root.join(&output.spec.path);
        remove_transaction_file_if_matching(
            paths,
            &repository_transaction_path(paths, &destination, &transaction_id, "write"),
            output.markdown.len() as u64,
            &hash(output.markdown.as_bytes()),
            "orphan repository maintenance staged final",
        )?;
        if !output.prior.bytes.is_empty() {
            remove_transaction_file_if_matching(
                paths,
                &repository_transaction_path(paths, &destination, &transaction_id, "backup"),
                output.prior.bytes.len() as u64,
                &hash(&output.prior.bytes),
                "orphan repository maintenance backup",
            )?;
        } else {
            remove_semantic_prior_backup_if_matching(
                paths,
                &repository_transaction_path(paths, &destination, &transaction_id, "backup"),
                &output.spec.path,
                &output.spec.record_id,
                &output.spec.expected_prior_revision,
            )?;
        }
    }
    sync_directory(&root)
}

pub(super) fn cleanup_artifacts_only(
    paths: &MemoryPaths,
    journal: &MaintenanceMaterializationJournal,
) -> Result<()> {
    let bytes = journal_bytes(journal)?;
    remove_runtime_file_if_matching(
        paths,
        &journal_temporary_path(paths, journal),
        bytes.len() as u64,
        &hash(&bytes),
        "repository maintenance journal temporary",
    )?;
    for entry in &journal.outputs {
        remove_transaction_file_if_matching(
            paths,
            &stage_path(paths, journal, entry),
            entry.post_bytes,
            &entry.post_hash,
            "repository maintenance staged final",
        )?;
        remove_transaction_file_if_matching(
            paths,
            &backup_path(paths, journal, entry),
            entry.prior_bytes,
            &entry.prior_hash,
            "repository maintenance backup",
        )?;
    }
    sync_directory(&repository_transaction_root(paths))
}

pub(super) fn append_commit_marker(
    conn: &Connection,
    journal: &MaintenanceMaterializationJournal,
) -> Result<()> {
    let marker = commit_marker(journal)?;
    let payload = serde_json::to_string(&marker)
        .context("failed to serialize repository maintenance commit marker")?;
    conn.execute(
        "INSERT INTO event_log (
           id, event_type, actor, data_class, payload_json, record_id, proposal_id, created_at
         ) VALUES (?1, ?2, 'memzoi-maintenance', 'private', ?3, NULL, NULL, ?4)",
        rusqlite::params![
            commit_event_id(journal),
            COMMIT_EVENT,
            payload,
            journal.request.decision_at,
        ],
    )
    .context("failed to append repository maintenance commit marker")?;
    Ok(())
}

pub(super) fn commit_marker_exists(
    conn: &Connection,
    journal: &MaintenanceMaterializationJournal,
) -> Result<bool> {
    let row = conn
        .query_row(
            "SELECT event_type, data_class, payload_json FROM event_log WHERE id = ?1",
            [commit_event_id(journal)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .context("failed to inspect repository maintenance commit marker")?;
    let Some((event_type, data_class, payload)) = row else {
        return Ok(false);
    };
    ensure!(
        event_type == COMMIT_EVENT && data_class == "private",
        "repository maintenance commit marker has an invalid envelope"
    );
    let actual: MaintenanceCommitMarker =
        serde_json::from_str(&payload).context("invalid repository maintenance commit marker")?;
    ensure!(
        actual == commit_marker(journal)?,
        "repository maintenance commit marker does not match its journal"
    );
    Ok(true)
}

pub(super) fn delete_commit_marker(
    conn: &Connection,
    journal: &MaintenanceMaterializationJournal,
) -> Result<()> {
    conn.execute(
        "DELETE FROM event_log WHERE id = ?1",
        [commit_event_id(journal)],
    )?;
    Ok(())
}

fn read_exact_transaction_file(
    paths: &MemoryPaths,
    path: &Path,
    expected_bytes: u64,
    expected_hash: &str,
    label: &str,
) -> Result<Vec<u8>> {
    let bytes = repository_io::read_transaction_file_if_exists(
        &repository_transaction_root(paths),
        path,
        expected_bytes,
        label,
    )?
    .with_context(|| format!("{label} is missing"))?;
    ensure!(
        hash(&bytes) == expected_hash,
        "{label} does not match its journal"
    );
    Ok(bytes)
}

fn verify_transaction_file(
    paths: &MemoryPaths,
    path: &Path,
    expected_bytes: u64,
    expected_hash: &str,
    label: &str,
) -> Result<()> {
    let _ = read_exact_transaction_file(paths, path, expected_bytes, expected_hash, label)?;
    Ok(())
}

fn remove_transaction_file_if_matching(
    paths: &MemoryPaths,
    path: &Path,
    expected_bytes: u64,
    expected_hash: &str,
    label: &str,
) -> Result<()> {
    let Some(bytes) = repository_io::read_transaction_file_if_exists(
        &repository_transaction_root(paths),
        path,
        expected_bytes,
        label,
    )?
    else {
        return Ok(());
    };
    ensure!(
        hash(&bytes) == expected_hash,
        "{label} does not match its journal"
    );
    remove_staged_file(path)
}

fn verify_regular_file(
    paths: &MemoryPaths,
    path: &Path,
    expected_bytes: u64,
    expected_hash: &str,
    expected_identity: Option<RepositoryFileIdentity>,
    label: &str,
) -> Result<()> {
    let (bytes, identity, _) = repository_io::read_bounded_direct_child_file_if_exists(
        &paths.runtime_dir,
        path,
        expected_bytes,
        label,
    )?
    .with_context(|| format!("{label} is missing"))?;
    ensure!(
        bytes.len() as u64 == expected_bytes
            && hash(&bytes) == expected_hash
            && expected_identity.is_none_or(|expected| expected == identity),
        "{label} does not match its journal"
    );
    Ok(())
}

fn remove_runtime_file_if_matching(
    paths: &MemoryPaths,
    path: &Path,
    expected_bytes: u64,
    expected_hash: &str,
    label: &str,
) -> Result<()> {
    let Some((bytes, _, mode)) = repository_io::read_bounded_direct_child_file_if_exists(
        &paths.runtime_dir,
        path,
        expected_bytes,
        label,
    )?
    else {
        return Ok(());
    };
    ensure!(
        bytes.len() as u64 == expected_bytes && hash(&bytes) == expected_hash && mode & 0o077 == 0,
        "{label} does not match its transaction"
    );
    remove_staged_file(path)
}

fn remove_semantic_prior_backup_if_matching(
    paths: &MemoryPaths,
    backup: &Path,
    canonical_path: &Path,
    record_id: &str,
    expected_revision: &CanonicalRevision,
) -> Result<()> {
    let transaction_root = repository_transaction_root(paths);
    let Some((bytes, _, mode)) = repository_io::read_bounded_direct_child_file_if_exists(
        &transaction_root,
        backup,
        MAINTENANCE_MAX_INPUT_FILE_BYTES as u64,
        "orphan repository maintenance backup",
    )?
    else {
        return Ok(());
    };
    ensure!(
        mode & 0o077 == 0,
        "orphan repository maintenance backup permissions are unsafe"
    );
    let markdown =
        std::str::from_utf8(&bytes).context("orphan repository maintenance backup is not UTF-8")?;
    let record = okf::parse_okf_record_markdown(
        paths.records_dir(),
        paths.project_root.join(canonical_path),
        markdown,
    )?
    .context("orphan repository maintenance backup is not canonical OKF")?;
    ensure!(
        record.concept_id == record_id
            && canonical_revision_for_okf_record(&record)? == *expected_revision
            && okf::render_okf_record_markdown(&record)?.as_bytes() == bytes,
        "orphan repository maintenance backup does not match the completed decision"
    );
    remove_staged_file(backup)
}

fn validate_journal(journal: &MaintenanceMaterializationJournal) -> Result<()> {
    ensure!(
        journal.schema == REPOSITORY_MAINTENANCE_MATERIALIZATION_JOURNAL_SCHEMA,
        "unsupported repository maintenance journal schema"
    );
    ensure!(
        journal.route == RepositoryWriteRoute::Maintenance.as_str(),
        "repository maintenance journal has the wrong route"
    );
    let transaction = Uuid::parse_str(&journal.transaction_id)
        .context("repository maintenance journal transaction ID is invalid")?;
    ensure!(
        transaction.to_string() == journal.transaction_id,
        "repository maintenance journal transaction ID is not canonical"
    );
    ensure!(
        journal.transaction_id == transaction_id_for_decision(&journal.decision_id),
        "repository maintenance journal transaction ID does not match its decision"
    );
    journal.plan.validate()?;
    journal.request.validate()?;
    journal.decision.validate()?;
    ensure!(
        !journal.outputs.is_empty() && journal.outputs.len() <= MAX_ENTRIES,
        "repository maintenance journal output count is invalid"
    );
    ensure!(
        journal
            .outputs
            .windows(2)
            .all(|window| window[0].path < window[1].path),
        "repository maintenance journal outputs are not uniquely ordered"
    );
    ensure!(
        journal
            .comparisons
            .windows(2)
            .all(|window| window[0].path < window[1].path),
        "repository maintenance journal comparisons are not uniquely ordered"
    );
    for value in [
        &journal.project_context_digest,
        &journal.policy_context_digest,
        &journal.projection_digest,
        &journal.authorization_digest,
        &journal.safety_fields_digest,
    ] {
        validate_hash(value)?;
    }
    for output in &journal.outputs {
        ensure!(
            output.prior_bytes > 0 && output.post_bytes > 0,
            "journal output is empty"
        );
        validate_hash(&output.prior_hash)?;
        validate_hash(&output.post_hash)?;
        output.prior_semantic_revision.validate()?;
        output.post_semantic_revision.validate()?;
        ensure!(
            (output.prior_device != 0 || output.prior_inode != 0)
                && ((output.post_device == 0 && output.post_inode == 0)
                    || (output.post_device != 0 && output.post_inode != 0)),
            "journal output file identities are invalid"
        );
    }
    ensure!(
        journal
            .outputs
            .iter()
            .all(|output| output.post_device == 0 && output.post_inode == 0)
            || journal
                .outputs
                .iter()
                .all(|output| output.post_device != 0 && output.post_inode != 0),
        "journal post-install identities must be all absent or all present"
    );
    for comparison in &journal.comparisons {
        ensure!(comparison.bytes > 0, "journal comparison is empty");
        validate_hash(&comparison.hash)?;
        comparison.semantic_revision.validate()?;
    }
    ensure!(
        journal_bytes(journal)?.len() as u64 <= MAX_JOURNAL_BYTES,
        "repository maintenance journal is too large"
    );
    Ok(())
}

fn journal_bytes(journal: &MaintenanceMaterializationJournal) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(journal).context("failed to serialize repository maintenance journal")
}

fn commit_marker(journal: &MaintenanceMaterializationJournal) -> Result<MaintenanceCommitMarker> {
    Ok(MaintenanceCommitMarker {
        schema: COMMIT_SCHEMA.to_owned(),
        transaction_id: journal.transaction_id.clone(),
        plan_id: journal.plan.plan_id.clone(),
        selection_id: journal.selection_id.clone(),
        decision_id: journal.decision_id.clone(),
        journal_digest: hash(&journal_bytes(journal)?),
    })
}

fn commit_event_id(journal: &MaintenanceMaterializationJournal) -> String {
    format!("evt_repository_maintenance_{}", journal.transaction_id)
}

fn transaction_id_for_decision(decision_id: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"memzoi.repository-maintenance.transaction-id\0");
    hasher.update(decision_id.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

fn validate_hash(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "repository maintenance journal digest is invalid"
    );
    Ok(())
}

fn project_context_digest(paths: &MemoryPaths) -> Result<String> {
    Ok(hash(&repository_io::repository_project_identity(
        &paths.project_root,
    )?))
}

pub(super) fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn encode_digest(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
