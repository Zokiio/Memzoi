use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{MemoryPaths, db, okf, repository_io};

use super::{
    canonical_write::{CanonicalFileWrite, FileWriteMode},
    runtime_records::{
        RuntimeRecordSnapshot, RuntimeRecords, lifecycle_generation, set_lifecycle_generation,
    },
    safe_files::{RepoLifecycleLock, sync_directory},
};

const SHARED_SYNC_JOURNAL_SCHEMA: &str = "memzoi/shared-sync";
const SHARED_SYNC_JOURNAL_FILE: &str = "shared-sync.json";
const SHARED_SYNC_MARKER_EVENT: &str = "memzoi.shared_sync.index_committed";
const SHARED_SYNC_MARKER_ACTOR: &str = "system:shared-sync";
const RUNTIME_MIRROR_STATE_SINGLETON: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProposalRow {
    id: String,
    operation: String,
    payload_json: String,
    status: String,
    actor: String,
    validation_json: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EventRow {
    id: String,
    event_type: String,
    actor: String,
    data_class: String,
    payload_json: String,
    record_id: Option<String>,
    proposal_id: Option<String>,
    created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CanonicalCreateProjection {
    relative_path: PathBuf,
    expected_length: u64,
    expected_hash: String,
    record_file: okf::OkfRecordFile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SharedSyncJournal {
    schema: String,
    operation_id: String,
    marker_id: String,
    source_project_root: PathBuf,
    source_index: PathBuf,
    payload: SharedSyncPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SharedSyncPayload {
    RuntimeRecords {
        records: Vec<RuntimeRecordSnapshot>,
        events: Vec<EventRow>,
        origin_outcomes: Vec<crate::OriginOutcome>,
    },
    ProposalApply {
        before: Box<ProposalRow>,
        after: Box<ProposalRow>,
        events: Vec<EventRow>,
        canonical_create: Box<CanonicalCreateProjection>,
    },
}

#[cfg(test)]
type BeforeSharedSyncRecoveryHook = Box<dyn FnOnce() -> Result<()>>;

#[cfg(test)]
type AfterSharedSyncMarkerCleanupHook = Box<dyn FnOnce() -> Result<()>>;

#[cfg(test)]
thread_local! {
    static BEFORE_SHARED_SYNC_RECOVERY_HOOK: std::cell::RefCell<
        Option<BeforeSharedSyncRecoveryHook>,
    > = std::cell::RefCell::new(None);
    static AFTER_SHARED_SYNC_MARKER_CLEANUP_HOOK: std::cell::RefCell<
        Option<AfterSharedSyncMarkerCleanupHook>,
    > = std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(super) fn inject_before_shared_sync_recovery_hook(hook: impl FnOnce() -> Result<()> + 'static) {
    BEFORE_SHARED_SYNC_RECOVERY_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_before_shared_sync_recovery_hook() -> Result<()> {
    BEFORE_SHARED_SYNC_RECOVERY_HOOK.with(|slot| {
        let hook = slot.borrow_mut().take();
        hook.map_or(Ok(()), |hook| hook())
    })
}

#[cfg(test)]
pub(super) fn inject_after_shared_sync_marker_cleanup_hook(
    hook: impl FnOnce() -> Result<()> + 'static,
) {
    AFTER_SHARED_SYNC_MARKER_CLEANUP_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_shared_sync_marker_cleanup_hook() -> Result<()> {
    AFTER_SHARED_SYNC_MARKER_CLEANUP_HOOK.with(|slot| {
        let hook = slot.borrow_mut().take();
        hook.map_or(Ok(()), |hook| hook())
    })
}

pub(super) fn refresh_index_mirrors(
    paths: &MemoryPaths,
    shared: &Connection,
    index: &Connection,
) -> Result<()> {
    if !shared_sync_journal_entry_exists(paths)? && runtime_mirror_revisions_match(shared, index)? {
        return Ok(());
    }
    let _lifecycle_lock = RepoLifecycleLock::acquire(paths)?;
    refresh_index_mirrors_locked(paths, shared, index)
}

pub(super) fn ensure_read_only_lifecycle_snapshot_ready(paths: &MemoryPaths) -> Result<()> {
    if shared_sync_journal_entry_exists(paths)? {
        bail!(
            "read-only lifecycle access requires shared runtime recovery; run a normal Memzoi command first"
        );
    }
    Ok(())
}

pub(super) fn ensure_read_only_mirror_ready(shared: &Connection, index: &Connection) -> Result<()> {
    if !runtime_mirror_revisions_match(shared, index)? {
        bail!("mirror refresh required before read-only access");
    }
    Ok(())
}

pub(super) fn refresh_index_mirrors_locked(
    paths: &MemoryPaths,
    shared: &Connection,
    index: &Connection,
) -> Result<()> {
    recover_pending_shared_sync_locked(paths, shared)?;
    if runtime_mirror_revisions_match(shared, index)? {
        return Ok(());
    }
    let shared_revision = ensure_runtime_mirror_revision(shared)?;
    let shared_lifecycle_generation = lifecycle_generation(shared)?;
    let shared_records = RuntimeRecords::new(shared).snapshots()?;
    let indexed_records = RuntimeRecords::new(index).snapshots()?;
    let shared_lifecycle_events = read_private_lifecycle_authority_events(shared)?;
    let indexed_lifecycle_events = read_private_lifecycle_authority_events(index)?;
    let non_runtime_ids = RuntimeRecords::new(index)
        .indexed_non_runtime_record_ids()?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let collisions = shared_records
        .iter()
        .filter_map(|snapshot| {
            non_runtime_ids
                .contains(snapshot.record().id.as_str())
                .then_some(snapshot.record().id.as_str())
        })
        .collect::<Vec<_>>();
    if !collisions.is_empty() {
        bail!(
            "worktree index refused shared local/session record id collision{}: {}",
            if collisions.len() == 1 { "" } else { "s" },
            collisions.join(", ")
        );
    }

    let shared_proposals = read_proposals(shared)?;
    let indexed_proposals = read_proposals(index)?;

    let tx = index.unchecked_transaction()?;
    // Runtime records may carry cross-record supersession/lineage foreign
    // keys. The complete authoritative snapshot is installed atomically, so
    // validate those references at commit rather than making ID sort order a
    // hidden materialization constraint.
    tx.pragma_update(None, "defer_foreign_keys", "ON")
        .context("failed to defer runtime mirror foreign keys")?;
    if shared_records != indexed_records {
        tx.execute(
            "DELETE FROM memory_record WHERE destination IN ('local', 'session')",
            [],
        )?;
        RuntimeRecords::new(&tx).restore_snapshots(&shared_records)?;
    }
    if shared_lifecycle_events != indexed_lifecycle_events {
        tx.execute(
            "DELETE FROM event_log
             WHERE event_type = 'memory.private_lifecycle_applied'",
            [],
        )?;
        insert_events_exact(&tx, &shared_lifecycle_events)?;
    }
    if shared_proposals != indexed_proposals {
        replace_proposals(&tx, &shared_proposals)?;
    }
    let current_shared_revision = runtime_mirror_revision(shared)?
        .context("shared runtime mirror revision disappeared during reconciliation")?;
    if current_shared_revision != shared_revision {
        bail!("shared runtime mirror revision changed during reconciliation");
    }
    if lifecycle_generation(shared)? != shared_lifecycle_generation {
        bail!("shared private lifecycle generation changed during reconciliation");
    }
    set_lifecycle_generation(&tx, shared_lifecycle_generation)?;
    set_runtime_mirror_revision(&tx, &shared_revision)?;
    tx.commit()?;
    Ok(())
}

fn shared_sync_journal_entry_exists(paths: &MemoryPaths) -> Result<bool> {
    let path = shared_sync_journal_path(paths);
    match fs::symlink_metadata(&path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect shared-sync journal {}", path.display())),
    }
}

fn runtime_mirror_revision(conn: &Connection) -> Result<Option<String>> {
    conn.query_row(
        "SELECT revision FROM runtime_mirror_state WHERE singleton = ?1",
        [RUNTIME_MIRROR_STATE_SINGLETON],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

fn runtime_mirror_revisions_match(shared: &Connection, index: &Connection) -> Result<bool> {
    let shared_revision = runtime_mirror_revision(shared)?;
    Ok(shared_revision.is_some()
        && shared_revision == runtime_mirror_revision(index)?
        && lifecycle_generations_match(shared, index)?)
}

pub(super) fn lifecycle_generations_match(shared: &Connection, index: &Connection) -> Result<bool> {
    Ok(lifecycle_generation(shared)? == lifecycle_generation(index)?)
}

fn ensure_runtime_mirror_revision(conn: &Connection) -> Result<String> {
    conn.execute(
        "INSERT OR IGNORE INTO runtime_mirror_state(singleton, revision)
         VALUES (?1, lower(hex(randomblob(16))))",
        [RUNTIME_MIRROR_STATE_SINGLETON],
    )?;
    runtime_mirror_revision(conn)?.context("runtime mirror revision was not initialized")
}

fn set_runtime_mirror_revision(conn: &Connection, revision: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO runtime_mirror_state(singleton, revision) VALUES (?1, ?2)
         ON CONFLICT(singleton) DO UPDATE SET revision = excluded.revision",
        rusqlite::params![RUNTIME_MIRROR_STATE_SINGLETON, revision],
    )?;
    Ok(())
}

pub(super) fn prepare_runtime_sync_journal(
    paths: &MemoryPaths,
    index: &Connection,
    record_ids: &[String],
) -> Result<()> {
    let record_ids = record_ids.iter().cloned().collect::<BTreeSet<_>>();
    let records = RuntimeRecords::new(index).snapshots_for_ids(&record_ids)?;
    if records.len() != record_ids.len() {
        let found = records
            .iter()
            .map(|snapshot| snapshot.record().id.as_str())
            .collect::<BTreeSet<_>>();
        let missing = record_ids
            .iter()
            .filter(|record_id| !found.contains(record_id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        bail!(
            "shared-sync journal could not snapshot runtime record{}: {}",
            if missing.len() == 1 { "" } else { "s" },
            missing.join(", ")
        );
    }
    let events = read_events_for_record_ids(index, &record_ids)?;
    for record_id in &record_ids {
        if !events
            .iter()
            .any(|event| event.record_id.as_deref() == Some(record_id.as_str()))
        {
            bail!("shared-sync journal found no event for runtime record {record_id}");
        }
    }
    let origin_outcomes = crate::origin::finalized_origin_outcomes(index, paths.repository_key())?;
    prepare_shared_sync_journal(
        paths,
        index,
        SharedSyncPayload::RuntimeRecords {
            records,
            events,
            origin_outcomes,
        },
    )
}

pub(super) fn prepare_proposal_apply_sync_journal(
    paths: &MemoryPaths,
    index: &Connection,
    shared: &Connection,
    proposal_id: &str,
    write: &CanonicalFileWrite,
) -> Result<()> {
    let before = read_proposal(shared, proposal_id)?
        .with_context(|| format!("shared proposal disappeared before apply: {proposal_id}"))?;
    let after = read_proposal(index, proposal_id)?
        .with_context(|| format!("indexed proposal disappeared during apply: {proposal_id}"))?;
    if before.status != "approved" || after.status != "applied" {
        bail!(
            "shared-sync proposal transition must be approved -> applied; found {} -> {}",
            before.status,
            after.status
        );
    }
    let events = read_proposal_apply_events(index, proposal_id)?;
    if events.is_empty() {
        bail!("shared-sync journal found no apply event for proposal {proposal_id}");
    }
    let canonical_create = canonical_create_projection(paths, write)?;
    prepare_shared_sync_journal(
        paths,
        index,
        SharedSyncPayload::ProposalApply {
            before: Box::new(before),
            after: Box::new(after),
            events,
            canonical_create: Box::new(canonical_create),
        },
    )
}

fn canonical_create_projection(
    paths: &MemoryPaths,
    write: &CanonicalFileWrite,
) -> Result<CanonicalCreateProjection> {
    if write.mode != FileWriteMode::CreateNew {
        bail!("proposal-apply shared sync requires a canonical create projection");
    }
    let relative_path = write
        .path
        .strip_prefix(&paths.project_root)
        .with_context(|| {
            format!(
                "canonical proposal projection {} is outside project root {}",
                write.path.display(),
                paths.project_root.display()
            )
        })?
        .to_path_buf();
    let projection = CanonicalCreateProjection {
        relative_path,
        expected_length: write.markdown.len() as u64,
        expected_hash: blake3::hash(write.markdown.as_bytes()).to_hex().to_string(),
        record_file: write.record_file.clone(),
    };
    let resolved = resolve_canonical_create_path(paths, &projection)?;
    if resolved != write.path {
        bail!("canonical proposal projection path changed while journaling");
    }
    Ok(projection)
}

fn resolve_canonical_create_path(
    paths: &MemoryPaths,
    projection: &CanonicalCreateProjection,
) -> Result<PathBuf> {
    if projection
        .relative_path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("shared-sync canonical projection contains an unsafe relative path");
    }
    let expected_name = format!("{}.md", projection.record_file.concept_id);
    let expected_name_path = Path::new(&expected_name);
    if !matches!(
        expected_name_path
            .components()
            .collect::<Vec<_>>()
            .as_slice(),
        [Component::Normal(_)]
    ) {
        bail!("shared-sync canonical projection has an unsafe record identifier");
    }
    let expected = paths.records_dir().join(expected_name_path);
    let expected_relative = expected
        .strip_prefix(&paths.project_root)
        .with_context(|| {
            format!(
                "canonical records root {} is outside project root {}",
                paths.records_dir().display(),
                paths.project_root.display()
            )
        })?;
    if projection.relative_path != expected_relative {
        bail!(
            "shared-sync canonical projection path does not match record {}",
            projection.record_file.concept_id
        );
    }
    super::safe_files::ensure_safe_path_parent(
        &paths.project_root,
        &paths.records_dir(),
        &expected,
        false,
        "shared-sync canonical record",
    )?;
    Ok(expected)
}

fn read_exact_canonical_create(
    paths: &MemoryPaths,
    projection: &CanonicalCreateProjection,
) -> Result<Option<Vec<u8>>> {
    let path = resolve_canonical_create_path(paths, projection)?;
    let Some(bytes) = repository_io::read_repository_file_if_exists(
        &paths.project_root,
        &projection.relative_path,
        projection.expected_length,
        "shared-sync canonical record",
    )?
    else {
        return Ok(None);
    };
    if blake3::hash(&bytes).to_hex().as_str() != projection.expected_hash {
        bail!(
            "shared-sync canonical record bytes do not match the authorized projection: {}",
            path.display()
        );
    }
    let markdown =
        std::str::from_utf8(&bytes).context("shared-sync canonical record is not valid UTF-8")?;
    let parsed = okf::parse_okf_record_markdown(paths.records_dir(), &path, markdown)?
        .context("shared-sync canonical record was ignored")?;
    if parsed != projection.record_file {
        bail!(
            "shared-sync canonical record does not match the exact journal projection: {}",
            path.display()
        );
    }
    Ok(Some(bytes))
}

pub(super) fn complete_pending_shared_sync_locked(
    paths: &MemoryPaths,
    shared: &Connection,
) -> Result<()> {
    #[cfg(test)]
    run_before_shared_sync_recovery_hook()?;
    recover_pending_shared_sync_locked(paths, shared)
}

pub(super) fn load_runtime_snapshots(db_path: &Path) -> Result<Vec<RuntimeRecordSnapshot>> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let conn = open_initialized_database(
        db_path,
        "local/session runtime memory could not be preserved from the shared database",
    )?;
    let snapshots = RuntimeRecords::new(&conn)
        .snapshots()
        .context("local/session runtime memory could not be loaded for preservation")?;
    read_proposals(&conn)
        .context("shared database proposals could not be validated before index rebuild")?;
    Ok(snapshots)
}

fn prepare_shared_sync_journal(
    paths: &MemoryPaths,
    index: &Connection,
    payload: SharedSyncPayload,
) -> Result<()> {
    let journal_path = shared_sync_journal_path(paths);
    if journal_path.exists() {
        bail!(
            "cannot prepare shared sync while pending journal exists: {}",
            journal_path.display()
        );
    }
    let operation_id = Uuid::now_v7().to_string();
    let journal = SharedSyncJournal {
        schema: SHARED_SYNC_JOURNAL_SCHEMA.to_owned(),
        marker_id: format!("evt_shared_sync_{operation_id}"),
        operation_id,
        source_project_root: canonical_source_project_root(paths)?,
        source_index: source_index_relative_path(paths)?,
        payload,
    };
    write_shared_sync_journal(paths, &journal)?;
    insert_shared_sync_marker(index, &journal)?;
    Ok(())
}

fn insert_shared_sync_marker(index: &Connection, journal: &SharedSyncJournal) -> Result<()> {
    index.execute(
        "INSERT INTO event_log (
           id, event_type, actor, data_class, payload_json, record_id, proposal_id, created_at
         ) VALUES (?1, ?2, ?3, 'repository', ?4, NULL, NULL, ?5)
         ON CONFLICT(id) DO NOTHING",
        rusqlite::params![
            journal.marker_id,
            SHARED_SYNC_MARKER_EVENT,
            SHARED_SYNC_MARKER_ACTOR,
            marker_payload_json(journal)?,
            crate::events::now_utc()?,
        ],
    )?;
    if !shared_sync_marker_is_committed(index, journal)? {
        bail!(
            "shared-sync marker id collision while preparing journal: {}",
            journal.marker_id
        );
    }
    Ok(())
}

fn recover_pending_shared_sync_locked(paths: &MemoryPaths, shared: &Connection) -> Result<()> {
    let Some(journal) = read_shared_sync_journal(paths)? else {
        return Ok(());
    };
    if journal.schema != SHARED_SYNC_JOURNAL_SCHEMA {
        bail!(
            "unsupported shared-sync journal schema {:?} in {}",
            journal.schema,
            shared_sync_journal_path(paths).display()
        );
    }
    let source_index = resolve_source_index(paths, &journal.source_index)?;
    let payload_applied = shared_sync_payload_is_applied(shared, &journal.payload)?;
    if !source_index.is_file() {
        if payload_applied {
            remove_shared_sync_journal(paths)?;
            return Ok(());
        }
        bail!(
            "pending shared sync cannot determine commit state because source index is missing: {}",
            source_index.display()
        );
    }

    let index = open_initialized_database(
        &source_index,
        "failed to inspect pending shared-sync source index",
    )?;
    let mut committed = shared_sync_marker_is_committed(&index, &journal)?;
    if payload_applied {
        if let SharedSyncPayload::ProposalApply {
            canonical_create, ..
        } = &journal.payload
            && !proposal_apply_index_is_applied(
                &index,
                &journal.payload,
                canonical_create.as_ref(),
            )?
        {
            bail!("applied proposal sync is missing its exact source-index projection");
        }
        if !committed {
            remove_shared_sync_journal(paths)?;
            return Ok(());
        }
    } else {
        let source_paths = match &journal.payload {
            SharedSyncPayload::ProposalApply { .. } => Some(resolve_source_worktree_paths(
                paths,
                &journal,
                &source_index,
            )?),
            SharedSyncPayload::RuntimeRecords { .. } => None,
        };
        let canonical_create = match &journal.payload {
            SharedSyncPayload::ProposalApply {
                canonical_create, ..
            } => Some((
                canonical_create.as_ref(),
                read_exact_canonical_create(
                    source_paths
                        .as_ref()
                        .context("proposal shared sync lost its source worktree binding")?,
                    canonical_create,
                )?,
            )),
            SharedSyncPayload::RuntimeRecords { .. } => None,
        };
        if !committed {
            match canonical_create {
                Some((_, None)) => {
                    remove_shared_sync_journal(paths)?;
                    return Ok(());
                }
                Some((projection, Some(_))) => {
                    roll_forward_markerless_proposal_apply(&index, &journal, projection)?;
                    committed = shared_sync_marker_is_committed(&index, &journal)?;
                    if !committed {
                        bail!(
                            "markerless proposal recovery committed without its shared-sync marker"
                        );
                    }
                }
                None => {
                    remove_shared_sync_journal(paths)?;
                    return Ok(());
                }
            }
        } else if canonical_create.is_some_and(|(_, bytes)| bytes.is_none()) {
            bail!("committed proposal apply is missing its canonical record");
        }

        apply_shared_sync_payload(shared, &journal.payload)?;
    }
    let removed = index.execute("DELETE FROM event_log WHERE id = ?1", [&journal.marker_id])?;
    if removed != 1 {
        bail!(
            "committed shared-sync marker disappeared before cleanup: {}",
            journal.marker_id
        );
    }
    #[cfg(test)]
    run_after_shared_sync_marker_cleanup_hook()?;
    remove_shared_sync_journal(paths)
}

fn proposal_apply_index_is_applied(
    index: &Connection,
    payload: &SharedSyncPayload,
    projection: &CanonicalCreateProjection,
) -> Result<bool> {
    if !shared_sync_payload_is_applied(index, payload)? {
        return Ok(false);
    }
    let projected = okf::project_okf_record(&projection.record_file);
    Ok(RuntimeRecords::new(index).get(&projected.id)?.as_ref() == Some(&projected))
}

fn roll_forward_markerless_proposal_apply(
    index: &Connection,
    journal: &SharedSyncJournal,
    projection: &CanonicalCreateProjection,
) -> Result<()> {
    let SharedSyncPayload::ProposalApply {
        before,
        after,
        events,
        canonical_create,
    } = &journal.payload
    else {
        bail!("markerless roll-forward requires a proposal-apply journal");
    };
    if canonical_create.as_ref() != projection {
        bail!("markerless proposal projection changed during recovery");
    }
    if projection.record_file.proposal_id.as_deref() != Some(after.id.as_str()) {
        bail!("markerless canonical record has the wrong proposal lineage");
    }
    if events.iter().any(|event| {
        event.proposal_id.as_deref() != Some(after.id.as_str())
            || event.record_id.as_deref() != Some(projection.record_file.concept_id.as_str())
    }) {
        bail!("markerless proposal events do not match the canonical projection");
    }

    let tx = index.unchecked_transaction()?;
    let current = read_proposal(&tx, &after.id)?.with_context(|| {
        format!(
            "indexed proposal disappeared before markerless recovery: {}",
            after.id
        )
    })?;
    if current != **before {
        bail!(
            "indexed proposal {} changed before markerless recovery",
            after.id
        );
    }
    if RuntimeRecords::new(&tx)
        .get(&projection.record_file.concept_id)?
        .is_some()
    {
        bail!(
            "markerless canonical record id already exists in the source index: {}",
            projection.record_file.concept_id
        );
    }
    okf::import_okf_records(&tx, std::slice::from_ref(&projection.record_file))?;
    let projected = okf::project_okf_record(&projection.record_file);
    if RuntimeRecords::new(&tx).get(&projected.id)?.as_ref() != Some(&projected) {
        bail!("markerless canonical record did not project exactly into the source index");
    }
    update_proposal_exact(&tx, after)?;
    insert_events_exact(&tx, events)?;
    insert_shared_sync_marker(&tx, journal)?;
    tx.commit()?;
    Ok(())
}

fn apply_shared_sync_payload(shared: &Connection, payload: &SharedSyncPayload) -> Result<()> {
    let tx = shared.unchecked_transaction()?;
    match payload {
        SharedSyncPayload::RuntimeRecords {
            records,
            events,
            origin_outcomes,
        } => {
            let record_ids = records
                .iter()
                .map(|snapshot| snapshot.record().id.clone())
                .collect::<BTreeSet<_>>();
            let existing = RuntimeRecords::new(&tx)
                .snapshots_for_ids(&record_ids)?
                .into_iter()
                .map(|snapshot| (snapshot.record().id.clone(), snapshot))
                .collect::<BTreeMap<_, _>>();
            for snapshot in records {
                match existing.get(snapshot.record().id.as_str()) {
                    Some(current) if current == snapshot => {}
                    Some(_) => bail!(
                        "shared runtime record {} changed before journal recovery",
                        snapshot.record().id
                    ),
                    None => RuntimeRecords::new(&tx)
                        .restore_snapshots(std::slice::from_ref(snapshot))?,
                }
            }
            insert_events_exact(&tx, events)?;
            for outcome in origin_outcomes {
                crate::record_origin_outcome(&tx, outcome)?;
            }
        }
        SharedSyncPayload::ProposalApply {
            before,
            after,
            events,
            ..
        } => {
            let current = read_proposal(&tx, &after.id)?.with_context(|| {
                format!(
                    "shared proposal disappeared during journal recovery: {}",
                    after.id
                )
            })?;
            if current == **before {
                update_proposal_exact(&tx, after.as_ref())?;
            } else if current != **after {
                bail!(
                    "shared proposal {} changed before journal recovery",
                    after.id
                );
            }
            insert_events_exact(&tx, events)?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn shared_sync_payload_is_applied(
    shared: &Connection,
    payload: &SharedSyncPayload,
) -> Result<bool> {
    let records_match = |expected: &[RuntimeRecordSnapshot]| -> Result<bool> {
        let record_ids = expected
            .iter()
            .map(|snapshot| snapshot.record().id.clone())
            .collect::<BTreeSet<_>>();
        let current = RuntimeRecords::new(shared)
            .snapshots_for_ids(&record_ids)?
            .into_iter()
            .map(|snapshot| (snapshot.record().id.clone(), snapshot))
            .collect::<BTreeMap<_, _>>();
        Ok(expected
            .iter()
            .all(|snapshot| current.get(snapshot.record().id.as_str()) == Some(snapshot)))
    };
    let events_match = |expected: &[EventRow]| -> Result<bool> {
        for event in expected {
            if read_event(shared, &event.id)?.as_ref() != Some(event) {
                return Ok(false);
            }
        }
        Ok(true)
    };
    match payload {
        SharedSyncPayload::RuntimeRecords {
            records,
            events,
            origin_outcomes,
        } => {
            let origins_match =
                origin_outcomes
                    .iter()
                    .try_fold(true, |matches, expected| -> Result<bool> {
                        if !matches {
                            return Ok(false);
                        }
                        Ok(matches!(
                            crate::lookup_origin(
                                shared,
                                &expected.identity,
                                &expected.input_fingerprint,
                            )?,
                            crate::OriginLookup::Replay(ref current) if current == expected
                        ))
                    })?;
            Ok(records_match(records)? && events_match(events)? && origins_match)
        }
        SharedSyncPayload::ProposalApply { after, events, .. } => {
            Ok(
                read_proposal(shared, &after.id)?.as_ref() == Some(after.as_ref())
                    && events_match(events)?,
            )
        }
    }
}

fn source_index_relative_path(paths: &MemoryPaths) -> Result<PathBuf> {
    let relative = paths
        .index_db_path
        .strip_prefix(&paths.repository_runtime_dir)
        .with_context(|| {
            format!(
                "worktree index {} is outside repository runtime {}",
                paths.index_db_path.display(),
                paths.repository_runtime_dir.display()
            )
        })?
        .to_path_buf();
    resolve_source_index(paths, &relative)?;
    Ok(relative)
}

fn canonical_source_project_root(paths: &MemoryPaths) -> Result<PathBuf> {
    let canonical = paths.project_root.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize shared-sync source checkout {}",
            paths.project_root.display()
        )
    })?;
    if canonical != paths.project_root {
        bail!(
            "shared-sync source checkout is not normalized: {}",
            paths.project_root.display()
        );
    }
    Ok(canonical)
}

fn resolve_source_worktree_paths(
    recovery_paths: &MemoryPaths,
    journal: &SharedSyncJournal,
    source_index: &Path,
) -> Result<MemoryPaths> {
    if !journal.source_project_root.is_absolute() {
        bail!("shared-sync source checkout must be an absolute path");
    }
    let canonical_source = journal
        .source_project_root
        .canonicalize()
        .with_context(|| {
            format!(
                "failed to resolve shared-sync source checkout {}",
                journal.source_project_root.display()
            )
        })?;
    if canonical_source != journal.source_project_root {
        bail!(
            "shared-sync source checkout changed after journaling: {}",
            journal.source_project_root.display()
        );
    }
    let runtime_home = recovery_paths
        .repository_runtime_dir
        .parent()
        .and_then(Path::parent)
        .context("repository runtime path has no runtime-home ancestor")?
        .to_path_buf();
    let source_paths = MemoryPaths::with_runtime_home(canonical_source, runtime_home);
    source_paths.validate_runtime_identity().with_context(|| {
        format!(
            "failed to validate shared-sync source checkout {}",
            journal.source_project_root.display()
        )
    })?;
    if source_paths.project_root != journal.source_project_root
        || source_paths.repository_runtime_dir != recovery_paths.repository_runtime_dir
        || source_paths.index_db_path != source_index
    {
        bail!(
            "shared-sync source checkout does not match its repository runtime and worktree index"
        );
    }
    Ok(source_paths)
}

fn resolve_source_index(paths: &MemoryPaths, relative: &Path) -> Result<PathBuf> {
    let components = relative.components().collect::<Vec<_>>();
    let valid = matches!(components.as_slice(), [
        Component::Normal(worktrees),
        Component::Normal(_),
        Component::Normal(index),
    ] if *worktrees == std::ffi::OsStr::new("worktrees") && *index == std::ffi::OsStr::new("index.db"));
    if !valid {
        bail!(
            "shared-sync source index has an invalid repository-runtime path: {}",
            relative.display()
        );
    }
    Ok(paths.repository_runtime_dir.join(relative))
}

fn marker_payload_json(journal: &SharedSyncJournal) -> Result<String> {
    Ok(serde_json::to_string(&json!({
        "schema": SHARED_SYNC_JOURNAL_SCHEMA,
        "operation_id": journal.operation_id,
    }))?)
}

fn shared_sync_marker_is_committed(
    index: &Connection,
    journal: &SharedSyncJournal,
) -> Result<bool> {
    let row = index
        .query_row(
            "SELECT event_type, actor, data_class, payload_json, record_id, proposal_id
             FROM event_log
             WHERE id = ?1",
            [&journal.marker_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((event_type, actor, data_class, payload_json, record_id, proposal_id)) = row else {
        return Ok(false);
    };
    if event_type != SHARED_SYNC_MARKER_EVENT
        || actor != SHARED_SYNC_MARKER_ACTOR
        || data_class != "repository"
        || payload_json != marker_payload_json(journal)?
        || record_id.is_some()
        || proposal_id.is_some()
    {
        bail!(
            "shared-sync marker {} does not match its journal",
            journal.marker_id
        );
    }
    Ok(true)
}

fn shared_sync_journal_path(paths: &MemoryPaths) -> PathBuf {
    paths.repository_runtime_dir.join(SHARED_SYNC_JOURNAL_FILE)
}

fn read_shared_sync_journal(paths: &MemoryPaths) -> Result<Option<SharedSyncJournal>> {
    let path = shared_sync_journal_path(paths);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read shared-sync journal {}", path.display()));
        }
    };
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse shared-sync journal {}", path.display()))
        .map(Some)
}

fn write_shared_sync_journal(paths: &MemoryPaths, journal: &SharedSyncJournal) -> Result<()> {
    fs::create_dir_all(&paths.repository_runtime_dir)?;
    let path = shared_sync_journal_path(paths);
    let bytes = serde_json::to_vec_pretty(journal)?;
    atomic_install_file(&path, &bytes, "shared-sync journal")
}

fn remove_shared_sync_journal(paths: &MemoryPaths) -> Result<()> {
    let path = shared_sync_journal_path(paths);
    match fs::remove_file(&path) {
        Ok(()) => sync_directory(&paths.repository_runtime_dir),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove shared-sync journal {}", path.display())),
    }
}

fn read_events_for_record_ids(
    conn: &Connection,
    record_ids: &BTreeSet<String>,
) -> Result<Vec<EventRow>> {
    let mut events = Vec::new();
    let mut statement = conn.prepare(
        "SELECT id, event_type, actor, data_class, payload_json, record_id, proposal_id, created_at
         FROM event_log
         WHERE record_id = ?1
         ORDER BY created_at, id",
    )?;
    for record_id in record_ids {
        let rows = statement.query_map([record_id], event_row)?;
        events.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
    }
    events.sort_by(|left, right| {
        (left.created_at.as_str(), left.id.as_str())
            .cmp(&(right.created_at.as_str(), right.id.as_str()))
    });
    Ok(events)
}

fn read_private_lifecycle_authority_events(conn: &Connection) -> Result<Vec<EventRow>> {
    let mut statement = conn.prepare(
        "SELECT id, event_type, actor, data_class, payload_json, record_id, proposal_id, created_at
         FROM event_log
         WHERE event_type = 'memory.private_lifecycle_applied'
           AND id IN (
             SELECT automatic_recall_event_id
             FROM private_lifecycle_state
             WHERE automatic_recall_until IS NOT NULL
             UNION
             SELECT validity_event_id
             FROM private_lifecycle_state
             WHERE validity_until IS NOT NULL
             UNION
             SELECT quarantine_event_id
             FROM private_lifecycle_state
             WHERE quarantined = 1
           )
         ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([], event_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn read_proposal_apply_events(conn: &Connection, proposal_id: &str) -> Result<Vec<EventRow>> {
    let mut statement = conn.prepare(
        "SELECT id, event_type, actor, data_class, payload_json, record_id, proposal_id, created_at
         FROM event_log
         WHERE proposal_id = ?1 AND event_type = 'memory.applied'
         ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([proposal_id], event_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn read_event(conn: &Connection, event_id: &str) -> Result<Option<EventRow>> {
    conn.query_row(
        "SELECT id, event_type, actor, data_class, payload_json, record_id, proposal_id, created_at
         FROM event_log
         WHERE id = ?1",
        [event_id],
        event_row,
    )
    .optional()
    .map_err(Into::into)
}

fn event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        id: row.get(0)?,
        event_type: row.get(1)?,
        actor: row.get(2)?,
        data_class: row.get(3)?,
        payload_json: row.get(4)?,
        record_id: row.get(5)?,
        proposal_id: row.get(6)?,
        created_at: row.get(7)?,
    })
}

fn insert_events_exact(conn: &Connection, events: &[EventRow]) -> Result<()> {
    for event in events {
        conn.execute(
            "INSERT OR IGNORE INTO event_log (
               id, event_type, actor, data_class, payload_json, record_id, proposal_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                event.id,
                event.event_type,
                event.actor,
                event.data_class,
                event.payload_json,
                event.record_id,
                event.proposal_id,
                event.created_at,
            ],
        )?;
        if read_event(conn, &event.id)?.as_ref() != Some(event) {
            bail!(
                "shared event id collision during journal recovery: {}",
                event.id
            );
        }
    }
    Ok(())
}

fn read_proposal(conn: &Connection, proposal_id: &str) -> Result<Option<ProposalRow>> {
    conn.query_row(
        "SELECT id, operation, payload_json, status, actor, validation_json, created_at, updated_at
         FROM proposal
         WHERE id = ?1",
        [proposal_id],
        |row| {
            Ok(ProposalRow {
                id: row.get(0)?,
                operation: row.get(1)?,
                payload_json: row.get(2)?,
                status: row.get(3)?,
                actor: row.get(4)?,
                validation_json: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn update_proposal_exact(conn: &Connection, proposal: &ProposalRow) -> Result<()> {
    let changed = conn.execute(
        "UPDATE proposal
         SET operation = ?2, payload_json = ?3, status = ?4, actor = ?5,
             validation_json = ?6, created_at = ?7, updated_at = ?8
         WHERE id = ?1",
        rusqlite::params![
            proposal.id,
            proposal.operation,
            proposal.payload_json,
            proposal.status,
            proposal.actor,
            proposal.validation_json,
            proposal.created_at,
            proposal.updated_at,
        ],
    )?;
    if changed != 1 {
        bail!(
            "shared proposal disappeared during journal update: {}",
            proposal.id
        );
    }
    Ok(())
}

fn read_proposals(conn: &Connection) -> Result<Vec<ProposalRow>> {
    let mut statement = conn.prepare(
        "SELECT id, operation, payload_json, status, actor, validation_json, created_at, updated_at
         FROM proposal
         ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(ProposalRow {
            id: row.get(0)?,
            operation: row.get(1)?,
            payload_json: row.get(2)?,
            status: row.get(3)?,
            actor: row.get(4)?,
            validation_json: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn replace_proposals(conn: &Connection, proposals: &[ProposalRow]) -> Result<()> {
    conn.execute("DELETE FROM proposal", [])?;
    for proposal in proposals {
        conn.execute(
            "INSERT INTO proposal (
               id, operation, payload_json, status, actor, validation_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                proposal.id,
                proposal.operation,
                proposal.payload_json,
                proposal.status,
                proposal.actor,
                proposal.validation_json,
                proposal.created_at,
                proposal.updated_at,
            ],
        )?;
    }
    Ok(())
}

fn open_initialized_database(path: &Path, context: &str) -> Result<Connection> {
    let conn = db::open_database(path).with_context(|| format!("{context}: {}", path.display()))?;
    db::init_database(&conn).with_context(|| format!("{context}: {}", path.display()))?;
    Ok(conn)
}

fn atomic_install_file(path: &Path, bytes: &[u8], label: &str) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{label} path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .with_context(|| format!("{label} path has no file name: {}", path.display()))?
        .to_string_lossy();
    let temp_path = parent.join(format!(".{file_name}.{}.tmp", Uuid::now_v7()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .with_context(|| format!("failed to create staged {label} {}", temp_path.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("failed to write staged {label} {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync staged {label} {}", temp_path.display()))?;
        drop(file);
        replace_file(&temp_path, path)
            .with_context(|| format!("failed to install {label} {}", path.display()))?;
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
    // SAFETY: both pointers reference NUL-terminated UTF-16 buffers that remain alive for the call.
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::TempDir;

    use crate::{
        ContextPackInput, InitRequest, LocalMemoryInput, MemoryDraft, MemoryLane, MemoryService,
        MemoryType, OkfProposalSensitivity, PrivateLifecycleApplyService, ProposalStatus,
        RepositoryContentClass, ScopeKind, Visibility, proposals,
    };

    use super::*;

    #[test]
    fn atomic_install_file_replaces_an_existing_target() -> Result<()> {
        let temp = TempDir::new()?;
        let target = temp.path().join("replace-me.toml");
        fs::write(&target, b"old")?;

        atomic_install_file(&target, b"new", "replacement fixture")?;

        assert_eq!(fs::read(&target)?, b"new");
        assert_eq!(fs::read_dir(temp.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn rebuild_recovers_committed_runtime_and_event_before_deleting_source_index() -> Result<()> {
        let temp = TempDir::new()?;
        let project = temp.path().join("project");
        fs::create_dir_all(&project)?;
        let paths = MemoryPaths::with_runtime_home(
            project.canonicalize()?,
            temp.path().join("runtime-home"),
        );
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;

        let shared = db::open_database(&paths.shared_db_path)?;
        db::init_database(&shared)?;
        let record_id = {
            let _lock = RepoLifecycleLock::acquire(&paths)?;
            let index = db::open_database(&paths.index_db_path)?;
            db::init_database(&index)?;
            let tx = index.unchecked_transaction()?;
            let record = RuntimeRecords::new(&tx).create_local(
                "agent:shared-sync-test",
                &LocalMemoryInput {
                    memory_type: MemoryType::Preference,
                    lane: MemoryLane::Semantic,
                    title: "Interrupted capture runtime".to_owned(),
                    body: "A committed worktree transaction must recover before rebuild."
                        .to_owned(),
                },
                "2026-07-16T12:00:00Z",
            )?;
            prepare_runtime_sync_journal(&paths, &tx, std::slice::from_ref(&record.id))?;
            tx.commit()?;
            record.id
        };
        let shared_count: i64 = shared.query_row(
            "SELECT COUNT(*) FROM memory_record WHERE id = ?1",
            [&record_id],
            |row| row.get(0),
        )?;
        assert_eq!(shared_count, 0);
        assert!(shared_sync_journal_path(&paths).is_file());
        drop(shared);

        MemoryService::rebuild_paths(paths.clone())?;
        let service = MemoryService::open_paths(paths.clone())?;
        assert_eq!(service.list_local_memory()?[0].id, record_id);
        let shared_event_count: i64 = service.shared_conn.query_row(
            "SELECT COUNT(*) FROM event_log
             WHERE record_id = ?1 AND event_type = 'memory.local_created'",
            [&record_id],
            |row| row.get(0),
        )?;
        assert_eq!(shared_event_count, 1);
        let marker_count: i64 = service.conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE event_type = ?1",
            [SHARED_SYNC_MARKER_EVENT],
            |row| row.get(0),
        )?;
        assert_eq!(marker_count, 0);
        assert!(!shared_sync_journal_path(&paths).exists());
        Ok(())
    }

    #[test]
    fn linked_worktree_recovers_committed_apply_from_source_checkout() -> Result<()> {
        let (_temp, source_paths, recovery_paths) = linked_worktree_paths_fixture()?;
        MemoryService::initialize_paths(source_paths.clone(), InitRequest { force: false })?;
        drop(MemoryService::open_paths(recovery_paths.clone())?);

        let source = MemoryService::open_paths(source_paths.clone())?;
        let proposal = source.propose_memory(
            "agent:cross-worktree-recovery",
            sample_memory_draft(
                "Committed source checkout projection",
                "Recovery must verify these bytes in the source checkout.",
            ),
        )?;
        source.validate_proposal(&proposal.id)?;
        source.approve_proposal(&proposal.id, "reviewer:cross-worktree-recovery")?;
        inject_before_shared_sync_recovery_hook(|| {
            bail!("injected committed shared-sync interruption")
        });
        let error = source
            .apply_proposal(&proposal.id, "agent:cross-worktree-recovery")
            .expect_err("the committed apply must stop before shared promotion");
        assert!(format!("{error:#}").contains("injected committed shared-sync interruption"));
        let record_id: String = source.conn.query_row(
            "SELECT id FROM memory_record WHERE proposal_id = ?1",
            [&proposal.id],
            |row| row.get(0),
        )?;
        assert!(
            source_paths
                .records_dir()
                .join(format!("{record_id}.md"))
                .is_file()
        );
        assert!(
            !recovery_paths
                .records_dir()
                .join(format!("{record_id}.md"))
                .exists()
        );
        drop(source);

        let recovered = MemoryService::open_paths(recovery_paths.clone())?;
        assert_eq!(
            recovered.show_proposal(&proposal.id)?.status,
            ProposalStatus::Applied
        );
        let apply_events: i64 = recovered.shared_conn.query_row(
            "SELECT COUNT(*) FROM event_log
             WHERE proposal_id = ?1 AND event_type = 'memory.applied'",
            [&proposal.id],
            |row| row.get(0),
        )?;
        assert_eq!(apply_events, 1);
        assert!(!shared_sync_journal_path(&recovery_paths).exists());
        Ok(())
    }

    #[test]
    fn linked_worktree_cleans_applied_sync_after_source_checkout_is_removed() -> Result<()> {
        let (_temp, recovery_paths, source_paths) = linked_worktree_paths_fixture()?;
        MemoryService::initialize_paths(recovery_paths.clone(), InitRequest { force: false })?;
        drop(MemoryService::open_paths(source_paths.clone())?);

        let source = MemoryService::open_paths(source_paths.clone())?;
        let proposal = source.propose_memory(
            "agent:cross-worktree-cleanup",
            sample_memory_draft(
                "Removed source checkout cleanup",
                "An exact applied payload must not retain a deleted source checkout dependency.",
            ),
        )?;
        source.validate_proposal(&proposal.id)?;
        source.approve_proposal(&proposal.id, "reviewer:cross-worktree-cleanup")?;
        inject_after_shared_sync_marker_cleanup_hook(|| {
            bail!("injected interruption after source marker cleanup")
        });
        let error = source
            .apply_proposal(&proposal.id, "agent:cross-worktree-cleanup")
            .expect_err("the cleanup interruption must leave the applied journal pending");
        assert!(format!("{error:#}").contains("injected interruption"));
        assert_eq!(
            proposals::load_proposal_public(&source.shared_conn, &proposal.id)?.status,
            ProposalStatus::Applied
        );
        let marker_count: i64 = source.conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE event_type = ?1",
            [SHARED_SYNC_MARKER_EVENT],
            |row| row.get(0),
        )?;
        assert_eq!(marker_count, 0);
        assert!(shared_sync_journal_path(&source_paths).is_file());
        drop(source);

        let removed_checkout = source_paths
            .project_root
            .to_str()
            .context("linked source checkout path must be UTF-8")?;
        run_git(
            &recovery_paths.project_root,
            &["worktree", "remove", "--force", removed_checkout],
        )?;
        assert!(!source_paths.project_root.exists());

        let recovered = MemoryService::open_paths(recovery_paths.clone())?;
        assert_eq!(
            recovered.show_proposal(&proposal.id)?.status,
            ProposalStatus::Applied
        );
        assert!(!shared_sync_journal_path(&recovery_paths).exists());
        Ok(())
    }

    #[test]
    fn linked_worktree_rolls_forward_markerless_apply_from_source_checkout() -> Result<()> {
        let (_temp, source_paths, recovery_paths) = linked_worktree_paths_fixture()?;
        MemoryService::initialize_paths(source_paths.clone(), InitRequest { force: false })?;
        drop(MemoryService::open_paths(recovery_paths.clone())?);

        let source = MemoryService::open_paths(source_paths.clone())?;
        let proposal = source.propose_memory(
            "agent:cross-worktree-recovery",
            sample_memory_draft(
                "Markerless source checkout projection",
                "Markerless recovery must read the durable source checkout bytes.",
            ),
        )?;
        source.validate_proposal(&proposal.id)?;
        source.approve_proposal(&proposal.id, "reviewer:cross-worktree-recovery")?;
        super::super::canonical_write::inject_after_canonical_install_hook(|| {
            panic!("injected cross-worktree crash before index commit")
        });
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            source.apply_proposal(&proposal.id, "agent:cross-worktree-recovery")
        }));
        assert!(crashed.is_err(), "the precommit crash hook must unwind");
        let canonical = okf::read_okf_record_files(source_paths.records_dir())?;
        assert_eq!(canonical.len(), 1);
        let record_id = canonical[0].concept_id.clone();
        assert_eq!(
            canonical[0].proposal_id.as_deref(),
            Some(proposal.id.as_str())
        );
        assert!(
            !recovery_paths
                .records_dir()
                .join(format!("{record_id}.md"))
                .exists()
        );
        drop(source);

        let recovered = MemoryService::open_paths(recovery_paths.clone())?;
        assert_eq!(
            recovered.show_proposal(&proposal.id)?.status,
            ProposalStatus::Applied
        );
        let source_index = db::open_database(&source_paths.index_db_path)?;
        db::init_database(&source_index)?;
        let source_record_count: i64 = source_index.query_row(
            "SELECT COUNT(*) FROM memory_record WHERE id = ?1 AND proposal_id = ?2",
            rusqlite::params![record_id, proposal.id],
            |row| row.get(0),
        )?;
        assert_eq!(source_record_count, 1);
        let apply_events: i64 = recovered.shared_conn.query_row(
            "SELECT COUNT(*) FROM event_log
             WHERE proposal_id = ?1 AND event_type = 'memory.applied'",
            [&proposal.id],
            |row| row.get(0),
        )?;
        assert_eq!(apply_events, 1);
        assert!(!shared_sync_journal_path(&recovery_paths).exists());
        Ok(())
    }

    #[test]
    fn refresh_discards_prepared_journal_when_index_transaction_did_not_commit() -> Result<()> {
        let temp = TempDir::new()?;
        let project = temp.path().join("project");
        fs::create_dir_all(&project)?;
        let paths = MemoryPaths::with_runtime_home(
            project.canonicalize()?,
            temp.path().join("runtime-home"),
        );
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let shared = db::open_database(&paths.shared_db_path)?;
        db::init_database(&shared)?;
        let index = db::open_database(&paths.index_db_path)?;
        db::init_database(&index)?;
        refresh_index_mirrors(&paths, &shared, &index)?;
        assert!(runtime_mirror_revisions_match(&shared, &index)?);

        let record_id = {
            let _lock = RepoLifecycleLock::acquire(&paths)?;
            let tx = index.unchecked_transaction()?;
            let record = RuntimeRecords::new(&tx).create_local(
                "agent:shared-sync-test",
                &LocalMemoryInput {
                    memory_type: MemoryType::Fact,
                    lane: MemoryLane::Semantic,
                    title: "Rolled back runtime".to_owned(),
                    body: "Prepared journals must not promote rolled-back index state.".to_owned(),
                },
                "2026-07-16T12:00:00Z",
            )?;
            prepare_runtime_sync_journal(&paths, &tx, std::slice::from_ref(&record.id))?;
            let record_id = record.id;
            drop(tx);
            record_id
        };

        refresh_index_mirrors(&paths, &shared, &index)?;
        let shared_count: i64 = shared.query_row(
            "SELECT COUNT(*) FROM memory_record WHERE id = ?1",
            [&record_id],
            |row| row.get(0),
        )?;
        assert_eq!(shared_count, 0);
        assert!(!shared_sync_journal_path(&paths).exists());
        Ok(())
    }

    #[test]
    fn runtime_sync_journal_snapshots_only_requested_records() -> Result<()> {
        let project = TempDir::new()?;
        let runtime_home = TempDir::new()?;
        let paths = MemoryPaths::with_runtime_home(
            project.path().canonicalize()?,
            runtime_home.path().to_path_buf(),
        );
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let index = db::open_database(&paths.index_db_path)?;
        db::init_database(&index)?;
        let records = RuntimeRecords::new(&index);
        let target = records.create_local(
            "agent:targeted-snapshot-test",
            &LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Journal target".to_owned(),
                body: "Only this runtime record belongs in the sync journal.".to_owned(),
            },
            "2026-07-16T12:00:00Z",
        )?;
        let unrelated = records.create_local(
            "agent:targeted-snapshot-test",
            &LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Unrelated malformed capture".to_owned(),
                body: "Targeted snapshotting must not inspect this record.".to_owned(),
            },
            "2026-07-16T12:00:00Z",
        )?;
        index.execute(
            "INSERT INTO memory_capture(record_id, provenance_json) VALUES (?1, '{}')",
            [&unrelated.id],
        )?;

        prepare_runtime_sync_journal(&paths, &index, std::slice::from_ref(&target.id))?;

        assert!(shared_sync_journal_path(&paths).is_file());
        Ok(())
    }

    #[test]
    fn mirror_revision_detects_shared_runtime_and_child_changes() -> Result<()> {
        let project = TempDir::new()?;
        let runtime_home = TempDir::new()?;
        let paths = MemoryPaths::with_runtime_home(
            project.path().canonicalize()?,
            runtime_home.path().to_path_buf(),
        );
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let shared = db::open_database(&paths.shared_db_path)?;
        db::init_database(&shared)?;
        let index = db::open_database(&paths.index_db_path)?;
        db::init_database(&index)?;
        refresh_index_mirrors(&paths, &shared, &index)?;
        assert!(runtime_mirror_revisions_match(&shared, &index)?);

        let record = RuntimeRecords::new(&shared).create_local(
            "agent:mirror-revision-test",
            &LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Revision-tracked runtime".to_owned(),
                body: "Shared runtime mutations must invalidate worktree mirrors.".to_owned(),
            },
            "2026-07-16T12:00:00Z",
        )?;
        assert!(!runtime_mirror_revisions_match(&shared, &index)?);

        refresh_index_mirrors(&paths, &shared, &index)?;
        assert!(runtime_mirror_revisions_match(&shared, &index)?);
        assert_eq!(
            RuntimeRecords::new(&index)
                .get(&record.id)?
                .context("refreshed runtime record is missing")?,
            record
        );

        shared.execute(
            "INSERT INTO memory_tag(record_id, tag) VALUES (?1, 'revision-tracked')",
            [&record.id],
        )?;
        assert!(!runtime_mirror_revisions_match(&shared, &index)?);

        refresh_index_mirrors(&paths, &shared, &index)?;
        assert!(runtime_mirror_revisions_match(&shared, &index)?);
        assert_eq!(
            RuntimeRecords::new(&index).tags(&record.id)?,
            vec!["revision-tracked"]
        );
        Ok(())
    }

    #[test]
    fn mirror_copies_lifecycle_state_and_relations_but_not_authority() -> Result<()> {
        use crate::service::runtime_records::{
            OwnerActionGrantRow, OwnerActionGrantState, PrivateLifecycleRelation,
            PrivateLifecycleRelationKind, PrivateLifecycleStorage,
        };

        let project = TempDir::new()?;
        let runtime_home = TempDir::new()?;
        let paths = MemoryPaths::with_runtime_home(
            project.path().canonicalize()?,
            runtime_home.path().to_path_buf(),
        );
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let shared = db::open_database(&paths.shared_db_path)?;
        db::init_database(&shared)?;
        let index = db::open_database(&paths.index_db_path)?;
        db::init_database(&index)?;

        let subject = RuntimeRecords::new(&shared).create_local(
            "agent:lifecycle-mirror-test",
            &LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Lifecycle mirror subject".to_owned(),
                body: "Lifecycle state and relations are authoritative shared facts.".to_owned(),
            },
            "2026-07-19T10:00:00Z",
        )?;
        let related = RuntimeRecords::new(&shared).create_local(
            "agent:lifecycle-mirror-test",
            &LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Lifecycle mirror successor".to_owned(),
                body: "The related endpoint must be restored before its relation.".to_owned(),
            },
            "2026-07-19T10:00:00Z",
        )?;
        refresh_index_mirrors(&paths, &shared, &index)?;

        shared.execute(
            "UPDATE private_lifecycle_state
             SET quarantined = 1,
                 quarantine_reason_code = 'owner_review',
                 quarantine_event_id = 'event-quarantine-1',
                 updated_at = '2026-07-19T10:30:00Z'
             WHERE record_id = ?1",
            [&subject.id],
        )?;
        let relation = PrivateLifecycleRelation {
            id: "relation-lifecycle-mirror".to_owned(),
            relation_kind: PrivateLifecycleRelationKind::SupersededBy,
            subject_record_id: subject.id.clone(),
            related_record_id: related.id.clone(),
            application_id: "application-lifecycle-mirror".to_owned(),
            created_at: "2026-07-19T10:30:00Z".to_owned(),
        };
        PrivateLifecycleStorage::new(&shared).insert_relation(&relation)?;
        assert!(!lifecycle_generations_match(&shared, &index)?);

        refresh_index_mirrors(&paths, &shared, &index)?;
        assert!(runtime_mirror_revisions_match(&shared, &index)?);
        let shared_store = PrivateLifecycleStorage::new(&shared);
        let index_store = PrivateLifecycleStorage::new(&index);
        assert_eq!(
            index_store.require_state(&subject.id)?,
            shared_store.require_state(&subject.id)?
        );
        assert_eq!(
            index_store.relations_for_record(&subject.id)?,
            vec![relation]
        );

        let generation_before_grant = lifecycle_generation(&shared)?;
        let revision_before_grant = runtime_mirror_revision(&shared)?;
        shared_store.insert_grant(&OwnerActionGrantRow {
            grant_id: "grant-shared-only".to_owned(),
            request_id: "request-shared-only".to_owned(),
            request_json: r#"{"request_id":"request-shared-only"}"#.to_owned(),
            state: OwnerActionGrantState::Active,
            authorized_at: "2026-07-19T10:30:00Z".to_owned(),
            expires_at: "2026-07-19T11:30:00Z".to_owned(),
            revoked_at: None,
            consumed_at: None,
            consumed_application_id: None,
        })?;
        assert_eq!(lifecycle_generation(&shared)?, generation_before_grant);
        assert_eq!(runtime_mirror_revision(&shared)?, revision_before_grant);
        refresh_index_mirrors(&paths, &shared, &index)?;
        assert!(index_store.grant("grant-shared-only")?.is_none());
        Ok(())
    }

    #[test]
    fn lifecycle_open_rejects_old_shared_or_index_schema_without_database_writes() -> Result<()> {
        for old_shared in [true, false] {
            let project = TempDir::new()?;
            let runtime_home = TempDir::new()?;
            let paths = MemoryPaths::with_runtime_home(
                project.path().canonicalize()?,
                runtime_home.path().to_path_buf(),
            );
            MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;

            let old_path = if old_shared {
                &paths.shared_db_path
            } else {
                &paths.index_db_path
            };
            let old = Connection::open(old_path)?;
            old.pragma_update(None, "user_version", 1_i64)?;
            old.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            drop(old);

            let shared_before = fs::read(&paths.shared_db_path)?;
            let index_before = fs::read(&paths.index_db_path)?;
            let error = match PrivateLifecycleApplyService::open_paths(paths.clone()) {
                Ok(_) => bail!("a lifecycle open accepted an old schema"),
                Err(error) => error,
            };
            assert!(
                format!("{error:#}").contains("user_version 1"),
                "unexpected schema rejection: {error:#}"
            );
            assert_eq!(fs::read(&paths.shared_db_path)?, shared_before);
            assert_eq!(fs::read(&paths.index_db_path)?, index_before);

            let unchanged = Connection::open(old_path)?;
            let actual: i64 =
                unchanged.pragma_query_value(None, "user_version", |row| row.get(0))?;
            assert_eq!(actual, 1);
        }
        Ok(())
    }

    #[test]
    fn linked_worktrees_share_grants_and_receipts_without_mirroring_authority() -> Result<()> {
        use crate::service::runtime_records::{
            OwnerActionGrantRow, OwnerActionGrantState, PrivateLifecycleApplicationRow,
            PrivateLifecycleStorage,
        };

        let (_temp, source_paths, sibling_paths) = linked_worktree_paths_fixture()?;
        MemoryService::initialize_paths(source_paths.clone(), InitRequest { force: false })?;
        let source = MemoryService::open_paths(source_paths)?;
        let sibling = MemoryService::open_paths(sibling_paths)?;
        assert_eq!(source.paths.shared_db_path, sibling.paths.shared_db_path);
        assert_ne!(source.paths.index_db_path, sibling.paths.index_db_path);

        RuntimeRecords::new(&source.shared_conn).create_local(
            "agent:linked-authority-test",
            &LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Shared lifecycle authority".to_owned(),
                body: "One authority database serves every linked worktree.".to_owned(),
            },
            "2026-07-19T10:00:00Z",
        )?;
        let source_authority = PrivateLifecycleStorage::new(&source.shared_conn);
        let grant = OwnerActionGrantRow {
            grant_id: "grant-linked-authority".to_owned(),
            request_id: "request-linked-authority".to_owned(),
            request_json: r#"{"request_id":"request-linked-authority"}"#.to_owned(),
            state: OwnerActionGrantState::Active,
            authorized_at: "2026-07-19T10:00:00Z".to_owned(),
            expires_at: "2026-07-19T11:00:00Z".to_owned(),
            revoked_at: None,
            consumed_at: None,
            consumed_application_id: None,
        };
        source_authority.insert_grant(&grant)?;
        let application = PrivateLifecycleApplicationRow {
            application_id: "application-linked-authority".to_owned(),
            operation_id: "operation-linked-authority".to_owned(),
            request_id: grant.request_id.clone(),
            grant_id: grant.grant_id.clone(),
            result_json: r#"{"applied":true}"#.to_owned(),
            lifecycle_generation: lifecycle_generation(&source.shared_conn)?,
            applied_at: "2026-07-19T10:05:00Z".to_owned(),
        };
        source_authority.insert_application(&application)?;
        assert!(source_authority.consume_active_grant(
            &grant.grant_id,
            &application.application_id,
            &application.applied_at,
        )?);

        let sibling_authority = PrivateLifecycleStorage::new(&sibling.shared_conn);
        assert_eq!(
            sibling_authority
                .grant(&grant.grant_id)?
                .map(|row| row.state),
            Some(OwnerActionGrantState::Consumed)
        );
        assert_eq!(
            sibling_authority.application_by_operation_id(&application.operation_id)?,
            Some(application.clone())
        );

        refresh_index_mirrors(&source.paths, &source.shared_conn, &source.conn)?;
        refresh_index_mirrors(&sibling.paths, &sibling.shared_conn, &sibling.conn)?;
        for index in [&source.conn, &sibling.conn] {
            let mirror = PrivateLifecycleStorage::new(index);
            assert!(mirror.grant(&grant.grant_id)?.is_none());
            assert!(
                mirror
                    .application_by_operation_id(&application.operation_id)?
                    .is_none()
            );
            assert_eq!(
                lifecycle_generation(index)?,
                lifecycle_generation(&source.shared_conn)?
            );
        }
        Ok(())
    }

    #[test]
    fn linked_worktree_read_refreshes_before_serving_committed_quarantine() -> Result<()> {
        let (_temp, source_paths, sibling_paths) = linked_worktree_paths_fixture()?;
        MemoryService::initialize_paths(source_paths.clone(), InitRequest { force: false })?;
        let source = MemoryService::open_paths(source_paths)?;
        let sibling = MemoryService::open_paths(sibling_paths)?;
        let record = source.create_local_memory(
            "agent:linked-quarantine-test",
            LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Linked quarantine sentinel".to_owned(),
                body: "A stale sibling mirror must never serve this private record.".to_owned(),
            },
        )?;

        let before = sibling.build_context_pack(ContextPackInput {
            task: "Linked quarantine sentinel".to_owned(),
            include_local: true,
            ..ContextPackInput::default()
        })?;
        assert!(
            before
                .records
                .iter()
                .any(|item| item.record.id == record.id),
            "the sibling did not converge the newly-created private record"
        );

        let event_id = "event-linked-quarantine";
        let payload = serde_json::json!({
            "grant_id": "grant-linked-quarantine",
            "application_id": "application-linked-quarantine",
            "operation_id": "operation-linked-quarantine",
            "action_kinds": ["quarantine"],
            "target_record_ids": [&record.id],
            "applied_at": "2026-07-19T10:30:00Z",
        });
        let tx = source.shared_conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO event_log(
               id, event_type, actor, data_class, payload_json, record_id, proposal_id, created_at
             ) VALUES (
               ?1, 'memory.private_lifecycle_applied', 'owner:local-cli', 'repository',
               ?2, NULL, NULL, ?3
             )",
            rusqlite::params![
                event_id,
                serde_json::to_string(&payload)?,
                "2026-07-19T10:30:00Z"
            ],
        )?;
        tx.execute(
            "UPDATE private_lifecycle_state
             SET quarantined = 1,
                 quarantine_reason_code = 'owner_review',
                 quarantine_event_id = ?1,
                 updated_at = ?2
             WHERE record_id = ?3",
            rusqlite::params![event_id, "2026-07-19T10:30:00Z", record.id],
        )?;
        tx.commit()?;
        assert!(!lifecycle_generations_match(
            &source.shared_conn,
            &sibling.conn
        )?);

        let after = sibling.build_context_pack(ContextPackInput {
            task: "Linked quarantine sentinel".to_owned(),
            include_local: true,
            ..ContextPackInput::default()
        })?;
        assert!(
            after.records.iter().all(|item| item.record.id != record.id),
            "the sibling served a quarantined record from its stale mirror"
        );
        assert!(lifecycle_generations_match(
            &source.shared_conn,
            &sibling.conn
        )?);
        let mirrored_event_count: i64 = sibling.conn.query_row(
            "SELECT COUNT(*) FROM event_log
             WHERE id = ?1 AND event_type = 'memory.private_lifecycle_applied'",
            [event_id],
            |row| row.get(0),
        )?;
        assert_eq!(mirrored_event_count, 1);
        Ok(())
    }

    fn linked_worktree_paths_fixture() -> Result<(TempDir, MemoryPaths, MemoryPaths)> {
        if Command::new("git").arg("--version").output().is_err() {
            bail!("Git is required for the linked-worktree fixture");
        }
        let temp = TempDir::new()?;
        let source = temp.path().join("source");
        let recovery = temp.path().join("recovery");
        fs::create_dir_all(&source)?;
        run_git(&source, &["init", "-q"])?;
        run_git(&source, &["config", "user.email", "fixture@example.test"])?;
        run_git(&source, &["config", "user.name", "Fixture"])?;
        fs::create_dir_all(source.join(".memzoi/records"))?;
        fs::write(source.join(".memzoi/records/.gitkeep"), "")?;
        fs::write(source.join("README.md"), "linked-worktree fixture\n")?;
        run_git(&source, &["add", "README.md", ".memzoi"])?;
        run_git(&source, &["commit", "-qm", "base"])?;
        run_git(
            &source,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "recovery-worktree",
                recovery
                    .to_str()
                    .context("linked-worktree path must be UTF-8")?,
            ],
        )?;
        let runtime_home = temp.path().join("runtime-home");
        let source_paths =
            MemoryPaths::with_runtime_home(source.canonicalize()?, runtime_home.clone());
        let recovery_paths = MemoryPaths::with_runtime_home(recovery.canonicalize()?, runtime_home);
        assert_eq!(
            source_paths.repository_runtime_dir,
            recovery_paths.repository_runtime_dir
        );
        assert_ne!(source_paths.index_db_path, recovery_paths.index_db_path);
        Ok((temp, source_paths, recovery_paths))
    }

    fn run_git(directory: &Path, args: &[&str]) -> Result<()> {
        let output = Command::new("git")
            .args(args)
            .current_dir(directory)
            .output()?;
        if !output.status.success() {
            bail!(
                "Git fixture command {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    fn sample_memory_draft(title: &str, body: &str) -> MemoryDraft {
        MemoryDraft {
            memory_type: MemoryType::Fact,
            lane: MemoryLane::Semantic,
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            visibility: Visibility::Repo,
            title: title.to_owned(),
            body: body.to_owned(),
            tags: vec!["migration".to_owned()],
            source_kind: Some("test".to_owned()),
            source_ref: Some("runtime-record".to_owned()),
            sensitivity: OkfProposalSensitivity::RepoSafe,
            content_class: RepositoryContentClass::GeneralRepoKnowledge,
            confidence: 1.0,
        }
    }
}
