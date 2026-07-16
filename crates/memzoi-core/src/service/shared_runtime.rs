use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{MemoryPath, MemoryPaths, db, okf, repository_io, search};

use super::{
    canonical_write::{CanonicalFileWrite, FileWriteMode},
    default_config,
    runtime_records::{RuntimeRecordSnapshot, RuntimeRecords},
    safe_files::{RepoLifecycleLock, sync_directory},
};

const MIGRATION_RECEIPT_SCHEMA: &str = "memzoi/runtime-migration-v1";
const MIGRATION_RECEIPT_FILE: &str = "migration-v1.json";
const LEGACY_DATABASE_DIGESTS_FIELD: &str = "legacy_database_digests";
const MIGRATION_IDENTITY_BASELINES_FIELD: &str = "identity_baselines";
const SHARED_SYNC_JOURNAL_SCHEMA: &str = "memzoi/shared-sync-v1";
const SHARED_SYNC_JOURNAL_FILE: &str = "shared-sync-v1.json";
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
    payload_json: String,
    record_id: Option<String>,
    proposal_id: Option<String>,
    created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MigrationReceiptRequirements {
    legacy_sources: BTreeSet<PathBuf>,
    legacy_database_digests: Option<BTreeMap<PathBuf, String>>,
    runtime_record_ids: BTreeSet<String>,
    proposal_ids: BTreeSet<String>,
    event_ids: BTreeSet<String>,
    identity_baselines: Option<MigrationIdentityBaselines>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyDatabaseDigest {
    path: PathBuf,
    input_digest: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct MigrationIdentityDigests {
    runtime_records: BTreeMap<String, String>,
    proposals: BTreeMap<String, String>,
    events: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyDatabaseIdentityDigests {
    path: PathBuf,
    identities: MigrationIdentityDigests,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigrationIdentityBaselines {
    shared: MigrationIdentityDigests,
    legacy_databases: Vec<LegacyDatabaseIdentityDigests>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationIdentityKind {
    RuntimeRecord,
    Proposal,
    Event,
}

impl MigrationIdentityKind {
    fn label(self) -> &'static str {
        match self {
            Self::RuntimeRecord => "runtime record",
            Self::Proposal => "proposal",
            Self::Event => "event",
        }
    }
}

impl MigrationIdentityDigests {
    fn for_kind(&self, kind: MigrationIdentityKind) -> &BTreeMap<String, String> {
        match kind {
            MigrationIdentityKind::RuntimeRecord => &self.runtime_records,
            MigrationIdentityKind::Proposal => &self.proposals,
            MigrationIdentityKind::Event => &self.events,
        }
    }
}

impl MigrationIdentityBaselines {
    fn legacy_database(&self, path: &Path) -> Option<&LegacyDatabaseIdentityDigests> {
        self.legacy_databases
            .iter()
            .find(|database| database.path == path)
    }
}

trait MigrationIdentityRow: Clone + PartialEq + Serialize {
    fn migration_id(&self) -> &str;
}

impl MigrationIdentityRow for RuntimeRecordSnapshot {
    fn migration_id(&self) -> &str {
        &self.record().id
    }
}

impl MigrationIdentityRow for ProposalRow {
    fn migration_id(&self) -> &str {
        &self.id
    }
}

impl MigrationIdentityRow for EventRow {
    fn migration_id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct LegacyDatabaseLogicalState {
    runtime_records: Vec<RuntimeRecordSnapshot>,
    proposals: Vec<ProposalRow>,
    events: Vec<EventRow>,
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

pub(super) fn migrate_legacy_runtime_if_needed(paths: &MemoryPaths) -> Result<bool> {
    if migration_receipt_is_valid(paths)? {
        return Ok(false);
    }
    let prior_requirements = migration_receipt_requirements(&paths.repository_runtime_dir)?;
    let discovered_sources = migration_sources(paths, prior_requirements.as_ref());
    if discovered_sources.is_empty() {
        if paths
            .repository_runtime_dir
            .join(MIGRATION_RECEIPT_FILE)
            .is_file()
        {
            bail!(
                "repository runtime migration receipt is invalid and no retained legacy source is available"
            );
        }
        return Ok(false);
    }

    let _lifecycle_lock = RepoLifecycleLock::acquire(paths)?;
    if migration_receipt_is_valid(paths)? {
        return Ok(false);
    }
    let mut prior_requirements = migration_receipt_requirements(&paths.repository_runtime_dir)?;

    let discovered_sources = migration_sources(paths, prior_requirements.as_ref());
    if discovered_sources.is_empty() {
        bail!("repository runtime migration cannot recover without a retained legacy source");
    }
    let sources = remove_subsumed_legacy_sources(&discovered_sources)?;
    if sources.is_empty() {
        bail!("legacy runtime migration found a cyclic or invalid receipt ancestry");
    }

    fs::create_dir_all(&paths.repository_runtime_dir).with_context(|| {
        format!(
            "failed to create repository runtime directory {}",
            paths.repository_runtime_dir.display()
        )
    })?;

    let selected_config = select_migration_config(paths, &sources)?;
    let mut receipt_sources = legacy_sources(paths).into_iter().collect::<BTreeSet<_>>();
    if let Some(requirements) = &prior_requirements {
        receipt_sources.extend(requirements.legacy_sources.iter().cloned());
    }
    let legacy_database_states = read_legacy_database_states(&receipt_sources)?;
    let mut existing_snapshots = Vec::new();
    let mut existing_proposals = Vec::new();
    let mut existing_shared_events = Vec::new();

    if paths.shared_db_path.is_file() {
        let shared = open_initialized_database(
            &paths.shared_db_path,
            "failed to inspect existing repository-shared runtime during migration",
        )?;
        existing_snapshots = RuntimeRecords::new(&shared).snapshots()?;
        existing_proposals = read_proposals(&shared)?;
        existing_shared_events = read_events(&shared)?;
    }

    let upgraded_baselines = prior_requirements
        .as_ref()
        .filter(|requirements| requirements.identity_baselines.is_none())
        .map(|requirements| {
            upgrade_migration_identity_baselines(
                requirements,
                &legacy_database_states,
                &existing_snapshots,
                &existing_proposals,
                &existing_shared_events,
            )
        })
        .transpose()?;
    if let Some(upgraded_baselines) = upgraded_baselines {
        prior_requirements
            .as_mut()
            .context("migration receipt disappeared while upgrading identity baselines")?
            .identity_baselines = Some(upgraded_baselines);
    }

    let mut migration_databases = Vec::new();
    for source in &sources {
        for db_path in [source.join("memory.db"), source.join("shared.db")] {
            let Some(state) = legacy_database_states.get(&db_path) else {
                continue;
            };
            migration_databases.push((db_path, state));
        }
    }

    let runtime_records = reconcile_migration_rows(
        MigrationIdentityKind::RuntimeRecord,
        &paths.shared_db_path,
        existing_snapshots.clone(),
        migration_databases
            .iter()
            .map(|(path, state)| (path.clone(), state.runtime_records.clone()))
            .collect(),
        prior_requirements.as_ref(),
        true,
    )?;
    let proposals = reconcile_migration_rows(
        MigrationIdentityKind::Proposal,
        &paths.shared_db_path,
        existing_proposals.clone(),
        migration_databases
            .iter()
            .map(|(path, state)| (path.clone(), state.proposals.clone()))
            .collect(),
        prior_requirements.as_ref(),
        true,
    )?;
    let runtime_record_ids = runtime_records.keys().cloned().collect::<BTreeSet<_>>();
    let proposal_ids = proposals.keys().cloned().collect::<BTreeSet<_>>();
    let events = reconcile_migration_rows(
        MigrationIdentityKind::Event,
        &paths.shared_db_path,
        events_associated_with(&existing_shared_events, &runtime_record_ids, &proposal_ids),
        migration_databases
            .iter()
            .map(|(path, state)| {
                (
                    path.clone(),
                    events_associated_with(&state.events, &runtime_record_ids, &proposal_ids),
                )
            })
            .collect(),
        prior_requirements.as_ref(),
        false,
    )?;
    let event_ids = events.keys().cloned().collect::<BTreeSet<_>>();
    if let Some(requirements) = &prior_requirements {
        require_migration_identity_floor(
            "runtime record",
            &requirements.runtime_record_ids,
            &runtime_record_ids,
        )?;
        require_migration_identity_floor("proposal", &requirements.proposal_ids, &proposal_ids)?;
        require_migration_identity_floor("event", &requirements.event_ids, &event_ids)?;
    }

    let shared = db::open_database(&paths.shared_db_path)?;
    db::init_database(&shared)?;
    let tx = shared.unchecked_transaction()?;
    let snapshots = runtime_records
        .values()
        .map(|(_, snapshot)| snapshot.clone())
        .collect::<Vec<_>>();
    let existing_snapshots_by_id = existing_snapshots
        .iter()
        .map(|snapshot| (snapshot.record().id.clone(), snapshot))
        .collect::<BTreeMap<_, _>>();
    let imported_snapshots = snapshots
        .iter()
        .filter(|snapshot| !existing_snapshots_by_id.contains_key(snapshot.record().id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    RuntimeRecords::new(&tx).restore_snapshots(&imported_snapshots)?;
    let runtime_record_store = RuntimeRecords::new(&tx);
    for snapshot in snapshots.iter().filter(|snapshot| {
        existing_snapshots_by_id
            .get(snapshot.record().id.as_str())
            .is_some_and(|existing| *existing != *snapshot)
    }) {
        runtime_record_store.replace_snapshot_exact(snapshot)?;
    }
    let existing_proposals_by_id = existing_proposals
        .iter()
        .map(|proposal| (proposal.id.as_str(), proposal))
        .collect::<BTreeMap<_, _>>();
    for (_, proposal) in proposals.values() {
        match existing_proposals_by_id.get(proposal.id.as_str()) {
            Some(existing) if *existing != proposal => update_proposal_exact(&tx, proposal)?,
            Some(_) => {}
            None => insert_proposal_if_absent(&tx, proposal)?,
        }
    }
    let event_rows = events
        .values()
        .map(|(_, event)| event.clone())
        .collect::<Vec<_>>();
    insert_events_exact(&tx, &event_rows)?;
    tx.commit()?;

    if read_optional_file(&paths.config_path, "repository runtime config")?.as_deref()
        != Some(selected_config.as_slice())
    {
        atomic_install_file(
            &paths.config_path,
            &selected_config,
            "migrated runtime config",
        )?;
    }

    let migration_input = json!({
        "config_blake3": blake3::hash(&selected_config).to_hex().to_string(),
        "runtime_records": snapshots,
        "proposals": proposals.values().map(|(_, proposal)| proposal).collect::<Vec<_>>(),
        "events": event_rows,
    });
    let input_digest = blake3::hash(&serde_json::to_vec(&migration_input)?)
        .to_hex()
        .to_string();
    let legacy_database_digests =
        legacy_database_digests(&legacy_database_states, &runtime_record_ids, &proposal_ids)?;
    let identity_baselines = migration_identity_baselines(
        &runtime_records,
        &proposals,
        &events,
        &legacy_database_states,
        &runtime_record_ids,
        &proposal_ids,
        prior_requirements
            .as_ref()
            .and_then(|requirements| requirements.identity_baselines.as_ref()),
    )?;
    let receipt = json!({
        "schema": MIGRATION_RECEIPT_SCHEMA,
        "input_digest": format!("blake3:{input_digest}"),
        "legacy_sources": receipt_sources,
        "legacy_database_digests": legacy_database_digests,
        "identity_baselines": identity_baselines,
        "runtime_record_count": runtime_records.len(),
        "runtime_record_ids": runtime_record_ids,
        "proposal_count": proposals.len(),
        "proposal_ids": proposal_ids,
        "event_count": events.len(),
        "event_ids": event_ids,
        "legacy_directories_retained": true,
    });
    write_migration_receipt(paths, &receipt)?;
    Ok(true)
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
    let shared_records = RuntimeRecords::new(shared).snapshots()?;
    let indexed_records = RuntimeRecords::new(index).snapshots()?;
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
    if shared_records != indexed_records {
        tx.execute(
            "DELETE FROM memory_record WHERE destination IN ('local', 'session')",
            [],
        )?;
        RuntimeRecords::new(&tx).restore_snapshots(&shared_records)?;
    }
    if shared_proposals != indexed_proposals {
        replace_proposals(&tx, &shared_proposals)?;
    }
    let current_shared_revision = runtime_mirror_revision(shared)?
        .context("shared runtime mirror revision disappeared during reconciliation")?;
    if current_shared_revision != shared_revision {
        bail!("shared runtime mirror revision changed during reconciliation");
    }
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
    Ok(shared_revision.is_some() && shared_revision == runtime_mirror_revision(index)?)
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
    prepare_shared_sync_journal(
        paths,
        index,
        SharedSyncPayload::RuntimeRecords { records, events },
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
           id, event_type, actor, payload_json, record_id, proposal_id, created_at
         ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5)
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
        SharedSyncPayload::RuntimeRecords { records, events } => {
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
        SharedSyncPayload::RuntimeRecords { records, events } => {
            Ok(records_match(records)? && events_match(events)?)
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
            "SELECT event_type, actor, payload_json, record_id, proposal_id
             FROM event_log
             WHERE id = ?1",
            [&journal.marker_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((event_type, actor, payload_json, record_id, proposal_id)) = row else {
        return Ok(false);
    };
    if event_type != SHARED_SYNC_MARKER_EVENT
        || actor != SHARED_SYNC_MARKER_ACTOR
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
        "SELECT id, event_type, actor, payload_json, record_id, proposal_id, created_at
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

fn read_proposal_apply_events(conn: &Connection, proposal_id: &str) -> Result<Vec<EventRow>> {
    let mut statement = conn.prepare(
        "SELECT id, event_type, actor, payload_json, record_id, proposal_id, created_at
         FROM event_log
         WHERE proposal_id = ?1 AND event_type = 'memory.applied'
         ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([proposal_id], event_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn read_event(conn: &Connection, event_id: &str) -> Result<Option<EventRow>> {
    conn.query_row(
        "SELECT id, event_type, actor, payload_json, record_id, proposal_id, created_at
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
        payload_json: row.get(3)?,
        record_id: row.get(4)?,
        proposal_id: row.get(5)?,
        created_at: row.get(6)?,
    })
}

fn insert_events_exact(conn: &Connection, events: &[EventRow]) -> Result<()> {
    for event in events {
        conn.execute(
            "INSERT OR IGNORE INTO event_log (
               id, event_type, actor, payload_json, record_id, proposal_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                event.id,
                event.event_type,
                event.actor,
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

fn legacy_sources(paths: &MemoryPaths) -> Vec<PathBuf> {
    paths
        .legacy_runtime_dirs
        .iter()
        .filter(|path| legacy_source_is_retained(path))
        .cloned()
        .collect()
}

fn legacy_source_is_retained(path: &Path) -> bool {
    path.join("config.toml").is_file()
        || path.join("memory.db").is_file()
        || path.join("shared.db").is_file()
}

fn migration_sources(
    paths: &MemoryPaths,
    prior_requirements: Option<&MigrationReceiptRequirements>,
) -> Vec<PathBuf> {
    let mut sources = legacy_sources(paths).into_iter().collect::<BTreeSet<_>>();
    if let Some(requirements) = prior_requirements {
        sources.extend(
            requirements
                .legacy_sources
                .iter()
                .filter(|source| legacy_source_is_retained(source))
                .cloned(),
        );
    }
    sources.into_iter().collect()
}

fn migration_receipt_is_valid(paths: &MemoryPaths) -> Result<bool> {
    let Some(receipt) = valid_migration_receipt(&paths.repository_runtime_dir)? else {
        return Ok(false);
    };
    let recorded_sources = receipt
        .get("legacy_sources")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(PathBuf::from))
        .collect::<BTreeSet<_>>();
    Ok(legacy_sources(paths)
        .iter()
        .all(|source| recorded_sources.contains(source)))
}

fn remove_subsumed_legacy_sources(sources: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let candidates = sources.iter().cloned().collect::<BTreeSet<_>>();
    let mut subsumed = BTreeSet::new();
    for source in sources {
        let Some(receipt) = valid_migration_receipt(source)? else {
            continue;
        };
        let Some(predecessors) = receipt
            .get("legacy_sources")
            .and_then(|value| value.as_array())
        else {
            continue;
        };
        for predecessor in predecessors {
            let Some(predecessor) = predecessor.as_str().map(PathBuf::from) else {
                continue;
            };
            if predecessor != *source && candidates.contains(&predecessor) {
                subsumed.insert(predecessor);
            }
        }
    }
    Ok(sources
        .iter()
        .filter(|source| !subsumed.contains(*source))
        .cloned()
        .collect())
}

fn valid_migration_receipt(runtime_dir: &Path) -> Result<Option<serde_json::Value>> {
    let Some(receipt) = read_migration_receipt_shape(runtime_dir)? else {
        return Ok(None);
    };
    let runtime_record_ids =
        receipt_identity_set(&receipt, "runtime_record_ids", "runtime_record_count")
            .context("validated migration receipt lost runtime record identities")?;
    let proposal_ids = receipt_identity_set(&receipt, "proposal_ids", "proposal_count")
        .context("validated migration receipt lost proposal identities")?;
    let event_ids = receipt_identity_set(&receipt, "event_ids", "event_count")
        .context("validated migration receipt lost event identities")?;
    if receipt
        .get(MIGRATION_IDENTITY_BASELINES_FIELD)
        .and_then(|value| {
            parse_migration_identity_baselines(
                value,
                &receipt_legacy_sources(&receipt)?,
                &runtime_record_ids,
                &proposal_ids,
                &event_ids,
            )
        })
        .is_none()
    {
        return Ok(None);
    }
    if read_optional_file(&runtime_dir.join("config.toml"), "migrated runtime config")?.is_none() {
        return Ok(None);
    }

    let shared_db_path = runtime_dir.join("shared.db");
    let shared_metadata = match fs::metadata(&shared_db_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect migrated shared database {}",
                    shared_db_path.display()
                )
            });
        }
    };
    if !shared_metadata.is_file()
        || inspect_shared_database_read_only(
            &shared_db_path,
            &runtime_record_ids,
            &proposal_ids,
            &event_ids,
        )
        .is_err()
    {
        return Ok(None);
    }
    if !legacy_database_digests_are_current(&receipt, &runtime_record_ids, &proposal_ids)? {
        return Ok(None);
    }
    Ok(Some(receipt))
}

fn read_migration_receipt_shape(runtime_dir: &Path) -> Result<Option<serde_json::Value>> {
    let receipt_path = runtime_dir.join(MIGRATION_RECEIPT_FILE);
    let bytes = match fs::read(&receipt_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to read migration receipt {}",
                    receipt_path.display()
                )
            });
        }
    };
    let Ok(receipt) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(None);
    };
    let legacy_sources = receipt_legacy_sources(&receipt);
    let runtime_record_ids =
        receipt_identity_set(&receipt, "runtime_record_ids", "runtime_record_count");
    let proposal_ids = receipt_identity_set(&receipt, "proposal_ids", "proposal_count");
    let event_ids = receipt_identity_set(&receipt, "event_ids", "event_count");
    let shape_is_valid = receipt.get("schema").and_then(|value| value.as_str())
        == Some(MIGRATION_RECEIPT_SCHEMA)
        && receipt
            .get("input_digest")
            .and_then(|value| value.as_str())
            .is_some_and(|digest| digest.starts_with("blake3:"))
        && legacy_sources.is_some()
        && receipt
            .get("legacy_directories_retained")
            .and_then(|value| value.as_bool())
            == Some(true)
        && runtime_record_ids.is_some()
        && proposal_ids.is_some()
        && event_ids.is_some();
    if !shape_is_valid {
        return Ok(None);
    }
    Ok(Some(receipt))
}

fn receipt_legacy_sources(receipt: &serde_json::Value) -> Option<BTreeSet<PathBuf>> {
    let sources = receipt
        .get("legacy_sources")?
        .as_array()?
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|source| !source.is_empty())
                .map(PathBuf::from)
        })
        .collect::<Option<Vec<_>>>()?;
    let unique = sources.iter().cloned().collect::<BTreeSet<_>>();
    (unique.len() == sources.len()).then_some(unique)
}

fn parse_legacy_database_digests(
    value: &serde_json::Value,
    legacy_sources: &BTreeSet<PathBuf>,
) -> Option<BTreeMap<PathBuf, String>> {
    let entries = serde_json::from_value::<Vec<LegacyDatabaseDigest>>(value.clone()).ok()?;
    let mut digests = BTreeMap::new();
    for entry in entries {
        let belongs_to_recorded_source = legacy_sources.iter().any(|source| {
            entry.path == source.join("memory.db") || entry.path == source.join("shared.db")
        });
        if !belongs_to_recorded_source
            || !is_blake3_digest(&entry.input_digest)
            || digests.insert(entry.path, entry.input_digest).is_some()
        {
            return None;
        }
    }
    Some(digests)
}

fn parse_migration_identity_baselines(
    value: &serde_json::Value,
    legacy_sources: &BTreeSet<PathBuf>,
    runtime_record_ids: &BTreeSet<String>,
    proposal_ids: &BTreeSet<String>,
    event_ids: &BTreeSet<String>,
) -> Option<MigrationIdentityBaselines> {
    let baselines = serde_json::from_value::<MigrationIdentityBaselines>(value.clone()).ok()?;
    if baselines
        .shared
        .runtime_records
        .keys()
        .collect::<BTreeSet<_>>()
        != runtime_record_ids.iter().collect::<BTreeSet<_>>()
        || baselines.shared.proposals.keys().collect::<BTreeSet<_>>()
            != proposal_ids.iter().collect::<BTreeSet<_>>()
        || baselines.shared.events.keys().collect::<BTreeSet<_>>()
            != event_ids.iter().collect::<BTreeSet<_>>()
        || !identity_digests_are_valid(
            &baselines.shared,
            runtime_record_ids,
            proposal_ids,
            event_ids,
        )
    {
        return None;
    }

    let mut database_paths = BTreeSet::new();
    for database in &baselines.legacy_databases {
        let belongs_to_recorded_source = legacy_sources.iter().any(|source| {
            database.path == source.join("memory.db") || database.path == source.join("shared.db")
        });
        if !belongs_to_recorded_source
            || !database_paths.insert(database.path.clone())
            || !identity_digests_are_valid(
                &database.identities,
                runtime_record_ids,
                proposal_ids,
                event_ids,
            )
        {
            return None;
        }
    }
    Some(baselines)
}

fn identity_digests_are_valid(
    digests: &MigrationIdentityDigests,
    runtime_record_ids: &BTreeSet<String>,
    proposal_ids: &BTreeSet<String>,
    event_ids: &BTreeSet<String>,
) -> bool {
    [
        (&digests.runtime_records, runtime_record_ids),
        (&digests.proposals, proposal_ids),
        (&digests.events, event_ids),
    ]
    .into_iter()
    .all(|(values, allowed_ids)| {
        values
            .iter()
            .all(|(id, digest)| allowed_ids.contains(id) && is_blake3_digest(digest))
    })
}

fn is_blake3_digest(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("blake3:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn legacy_database_digests_are_current(
    receipt: &serde_json::Value,
    runtime_record_ids: &BTreeSet<String>,
    proposal_ids: &BTreeSet<String>,
) -> Result<bool> {
    let legacy_sources = receipt_legacy_sources(receipt)
        .context("validated migration receipt lost its legacy source ancestry")?;
    let current_states = read_legacy_database_states(&legacy_sources)?;
    let Some(recorded_digests) = receipt
        .get(LEGACY_DATABASE_DIGESTS_FIELD)
        .and_then(|value| parse_legacy_database_digests(value, &legacy_sources))
    else {
        return Ok(current_states.is_empty());
    };
    for (path, state) in &current_states {
        let Some(recorded_digest) = recorded_digests.get(path) else {
            return Ok(false);
        };
        if recorded_digest
            != &legacy_database_input_digest(state, runtime_record_ids, proposal_ids)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn receipt_identity_set(
    receipt: &serde_json::Value,
    ids_field: &str,
    count_field: &str,
) -> Option<BTreeSet<String>> {
    let ids = receipt
        .get(ids_field)?
        .as_array()?
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        })
        .collect::<Option<Vec<_>>>()?;
    let unique = ids.iter().cloned().collect::<BTreeSet<_>>();
    let count = usize::try_from(receipt.get(count_field)?.as_u64()?).ok()?;
    (unique.len() == ids.len() && count == ids.len()).then_some(unique)
}

fn migration_receipt_requirements(
    runtime_dir: &Path,
) -> Result<Option<MigrationReceiptRequirements>> {
    let Some(receipt) = read_migration_receipt_shape(runtime_dir)? else {
        return Ok(None);
    };
    let legacy_sources = receipt_legacy_sources(&receipt)
        .context("migration receipt lost its legacy source ancestry")?;
    let runtime_record_ids =
        receipt_identity_set(&receipt, "runtime_record_ids", "runtime_record_count")
            .context("migration receipt lost its runtime record identities")?;
    let proposal_ids = receipt_identity_set(&receipt, "proposal_ids", "proposal_count")
        .context("migration receipt lost its proposal identities")?;
    let event_ids = receipt_identity_set(&receipt, "event_ids", "event_count")
        .context("migration receipt lost its event identities")?;
    let identity_baselines = receipt
        .get(MIGRATION_IDENTITY_BASELINES_FIELD)
        .and_then(|value| {
            parse_migration_identity_baselines(
                value,
                &legacy_sources,
                &runtime_record_ids,
                &proposal_ids,
                &event_ids,
            )
        });
    let legacy_database_digests = receipt
        .get(LEGACY_DATABASE_DIGESTS_FIELD)
        .and_then(|value| parse_legacy_database_digests(value, &legacy_sources));
    Ok(Some(MigrationReceiptRequirements {
        legacy_sources,
        legacy_database_digests,
        runtime_record_ids,
        proposal_ids,
        event_ids,
        identity_baselines,
    }))
}

fn require_migration_identity_floor(
    label: &str,
    required: &BTreeSet<String>,
    recovered: &BTreeSet<String>,
) -> Result<()> {
    let missing = required.difference(recovered).cloned().collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "migration recovery would lose previously recorded {label} identit{}: {}",
            if missing.len() == 1 { "y" } else { "ies" },
            missing.join(", ")
        );
    }
    Ok(())
}

fn select_migration_config(paths: &MemoryPaths, sources: &[PathBuf]) -> Result<Vec<u8>> {
    let mut legacy_config: Option<(PathBuf, Vec<u8>)> = None;
    for source in sources {
        let config_path = source.join("config.toml");
        if !config_path.is_file() {
            continue;
        }
        let bytes = fs::read(&config_path)
            .with_context(|| format!("failed to read legacy config {}", config_path.display()))?;
        if let Some((previous_path, previous_bytes)) = &legacy_config
            && previous_bytes != &bytes
        {
            bail!(
                "legacy runtime config conflict between {} and {}",
                previous_path.display(),
                config_path.display()
            );
        }
        legacy_config = Some((config_path, bytes));
    }

    let installed = read_optional_file(&paths.config_path, "repository runtime config")?;
    let has_completed_migration_receipt =
        read_migration_receipt_shape(&paths.repository_runtime_dir)?.is_some();
    match (installed, legacy_config) {
        (Some(installed), Some((_, legacy)))
            if installed != legacy && has_completed_migration_receipt =>
        {
            Ok(installed)
        }
        (Some(installed), Some((legacy_path, legacy)))
            if installed != legacy && installed != default_config().as_bytes() =>
        {
            bail!(
                "repository runtime config {} conflicts with legacy config {}",
                paths.config_path.display(),
                legacy_path.display()
            );
        }
        (Some(installed), Some((_, legacy))) if installed != legacy => Ok(legacy),
        (Some(installed), _) => Ok(installed),
        (None, Some((_, legacy))) => Ok(legacy),
        (None, None) => Ok(default_config().as_bytes().to_vec()),
    }
}

fn open_initialized_database(path: &Path, context: &str) -> Result<Connection> {
    let conn = db::open_database(path).with_context(|| format!("{context}: {}", path.display()))?;
    db::init_database(&conn).with_context(|| format!("{context}: {}", path.display()))?;
    Ok(conn)
}

fn open_database_read_only(path: &Path, context: &str) -> Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("{context}: {}", path.display()))
}

fn read_legacy_database_states(
    legacy_sources: &BTreeSet<PathBuf>,
) -> Result<BTreeMap<PathBuf, LegacyDatabaseLogicalState>> {
    let mut states = BTreeMap::new();
    for source in legacy_sources {
        for path in [source.join("memory.db"), source.join("shared.db")] {
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to inspect retained legacy database {}",
                            path.display()
                        )
                    });
                }
            };
            if !metadata.is_file() {
                bail!(
                    "retained legacy database path is not a file: {}",
                    path.display()
                );
            }
            states.insert(path.clone(), read_legacy_database_state(&path)?);
        }
    }
    Ok(states)
}

fn read_legacy_database_state(path: &Path) -> Result<LegacyDatabaseLogicalState> {
    let conn =
        open_database_read_only(path, "failed to inspect retained legacy database contents")?;
    conn.execute_batch("BEGIN")
        .with_context(|| format!("failed to snapshot legacy database {}", path.display()))?;
    let state = (|| {
        Ok(LegacyDatabaseLogicalState {
            runtime_records: read_legacy_runtime_snapshots(&conn)?,
            proposals: read_proposals(&conn)?,
            events: read_events(&conn)?,
        })
    })();
    conn.execute_batch("ROLLBACK").with_context(|| {
        format!(
            "failed to release read-only legacy database snapshot {}",
            path.display()
        )
    })?;
    state
}

fn legacy_database_digests(
    states: &BTreeMap<PathBuf, LegacyDatabaseLogicalState>,
    runtime_record_ids: &BTreeSet<String>,
    proposal_ids: &BTreeSet<String>,
) -> Result<Vec<LegacyDatabaseDigest>> {
    states
        .iter()
        .map(|(path, state)| {
            Ok(LegacyDatabaseDigest {
                path: path.clone(),
                input_digest: legacy_database_input_digest(
                    state,
                    runtime_record_ids,
                    proposal_ids,
                )?,
            })
        })
        .collect()
}

fn migration_identity_baselines(
    runtime_records: &BTreeMap<String, (PathBuf, RuntimeRecordSnapshot)>,
    proposals: &BTreeMap<String, (PathBuf, ProposalRow)>,
    events: &BTreeMap<String, (PathBuf, EventRow)>,
    legacy_states: &BTreeMap<PathBuf, LegacyDatabaseLogicalState>,
    runtime_record_ids: &BTreeSet<String>,
    proposal_ids: &BTreeSet<String>,
    prior: Option<&MigrationIdentityBaselines>,
) -> Result<MigrationIdentityBaselines> {
    let mut shared = MigrationIdentityDigests::default();
    for (id, (_, snapshot)) in runtime_records {
        shared.runtime_records.insert(
            id.clone(),
            migration_identity_digest(MigrationIdentityKind::RuntimeRecord, snapshot)?,
        );
    }
    for (id, (_, proposal)) in proposals {
        shared.proposals.insert(
            id.clone(),
            migration_identity_digest(MigrationIdentityKind::Proposal, proposal)?,
        );
    }
    for (id, (_, event)) in events {
        shared.events.insert(
            id.clone(),
            migration_identity_digest(MigrationIdentityKind::Event, event)?,
        );
    }

    let mut legacy_databases = prior
        .map(|baselines| {
            baselines
                .legacy_databases
                .iter()
                .map(|database| (database.path.clone(), database.identities.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    for (path, state) in legacy_states {
        let mut identities = MigrationIdentityDigests::default();
        for snapshot in &state.runtime_records {
            if runtime_record_ids.contains(snapshot.migration_id()) {
                identities.runtime_records.insert(
                    snapshot.migration_id().to_owned(),
                    migration_identity_digest(MigrationIdentityKind::RuntimeRecord, snapshot)?,
                );
            }
        }
        for proposal in &state.proposals {
            if proposal_ids.contains(proposal.migration_id()) {
                identities.proposals.insert(
                    proposal.migration_id().to_owned(),
                    migration_identity_digest(MigrationIdentityKind::Proposal, proposal)?,
                );
            }
        }
        for event in events_associated_with(&state.events, runtime_record_ids, proposal_ids) {
            if events.contains_key(event.migration_id()) {
                identities.events.insert(
                    event.migration_id().to_owned(),
                    migration_identity_digest(MigrationIdentityKind::Event, &event)?,
                );
            }
        }
        legacy_databases.insert(path.clone(), identities);
    }

    Ok(MigrationIdentityBaselines {
        shared,
        legacy_databases: legacy_databases
            .into_iter()
            .map(|(path, identities)| LegacyDatabaseIdentityDigests { path, identities })
            .collect(),
    })
}

fn upgrade_migration_identity_baselines(
    requirements: &MigrationReceiptRequirements,
    legacy_states: &BTreeMap<PathBuf, LegacyDatabaseLogicalState>,
    shared_runtime_records: &[RuntimeRecordSnapshot],
    shared_proposals: &[ProposalRow],
    shared_events: &[EventRow],
) -> Result<MigrationIdentityBaselines> {
    let recorded_database_digests = requirements
        .legacy_database_digests
        .as_ref()
        .context(
            "cannot safely upgrade a baseline-less migration receipt without exact legacy database digests",
        )?;
    let current_database_paths = legacy_states.keys().cloned().collect::<BTreeSet<_>>();
    let recorded_database_paths = recorded_database_digests
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if current_database_paths != recorded_database_paths {
        bail!(
            "cannot safely upgrade a baseline-less migration receipt after a retained legacy database appeared or disappeared"
        );
    }
    for (path, state) in legacy_states {
        let current_digest = legacy_database_input_digest(
            state,
            &requirements.runtime_record_ids,
            &requirements.proposal_ids,
        )?;
        if recorded_database_digests.get(path) != Some(&current_digest) {
            bail!(
                "cannot safely upgrade a baseline-less migration receipt because retained legacy database {} changed",
                path.display()
            );
        }
    }

    let present_runtime_ids = shared_runtime_records
        .iter()
        .map(|snapshot| snapshot.migration_id().to_owned())
        .collect::<BTreeSet<_>>();
    let present_proposal_ids = shared_proposals
        .iter()
        .map(|proposal| proposal.migration_id().to_owned())
        .collect::<BTreeSet<_>>();
    let present_event_ids = shared_events
        .iter()
        .map(|event| event.migration_id().to_owned())
        .collect::<BTreeSet<_>>();
    require_migration_identity_floor(
        "runtime record",
        &requirements.runtime_record_ids,
        &present_runtime_ids,
    )?;
    require_migration_identity_floor(
        "proposal",
        &requirements.proposal_ids,
        &present_proposal_ids,
    )?;
    require_migration_identity_floor("event", &requirements.event_ids, &present_event_ids)?;

    let mut shared = MigrationIdentityDigests::default();
    for snapshot in shared_runtime_records {
        if requirements
            .runtime_record_ids
            .contains(snapshot.migration_id())
        {
            shared.runtime_records.insert(
                snapshot.migration_id().to_owned(),
                migration_identity_digest(MigrationIdentityKind::RuntimeRecord, snapshot)?,
            );
        }
    }
    for proposal in shared_proposals {
        if requirements.proposal_ids.contains(proposal.migration_id()) {
            shared.proposals.insert(
                proposal.migration_id().to_owned(),
                migration_identity_digest(MigrationIdentityKind::Proposal, proposal)?,
            );
        }
    }
    for event in shared_events {
        if requirements.event_ids.contains(event.migration_id()) {
            shared.events.insert(
                event.migration_id().to_owned(),
                migration_identity_digest(MigrationIdentityKind::Event, event)?,
            );
        }
    }

    let mut legacy_databases = Vec::with_capacity(legacy_states.len());
    for (path, state) in legacy_states {
        let mut identities = MigrationIdentityDigests::default();
        for snapshot in &state.runtime_records {
            if requirements
                .runtime_record_ids
                .contains(snapshot.migration_id())
            {
                identities.runtime_records.insert(
                    snapshot.migration_id().to_owned(),
                    migration_identity_digest(MigrationIdentityKind::RuntimeRecord, snapshot)?,
                );
            }
        }
        for proposal in &state.proposals {
            if requirements.proposal_ids.contains(proposal.migration_id()) {
                identities.proposals.insert(
                    proposal.migration_id().to_owned(),
                    migration_identity_digest(MigrationIdentityKind::Proposal, proposal)?,
                );
            }
        }
        for event in &state.events {
            if requirements.event_ids.contains(event.migration_id()) {
                identities.events.insert(
                    event.migration_id().to_owned(),
                    migration_identity_digest(MigrationIdentityKind::Event, event)?,
                );
            }
        }
        legacy_databases.push(LegacyDatabaseIdentityDigests {
            path: path.clone(),
            identities,
        });
    }
    Ok(MigrationIdentityBaselines {
        shared,
        legacy_databases,
    })
}

fn legacy_database_input_digest(
    state: &LegacyDatabaseLogicalState,
    runtime_record_ids: &BTreeSet<String>,
    proposal_ids: &BTreeSet<String>,
) -> Result<String> {
    let mut associated_runtime_ids = runtime_record_ids.clone();
    associated_runtime_ids.extend(
        state
            .runtime_records
            .iter()
            .map(|snapshot| snapshot.record().id.clone()),
    );
    let mut associated_proposal_ids = proposal_ids.clone();
    associated_proposal_ids.extend(state.proposals.iter().map(|proposal| proposal.id.clone()));
    let input = json!({
        "runtime_records": &state.runtime_records,
        "proposals": &state.proposals,
        "events": events_associated_with(
            &state.events,
            &associated_runtime_ids,
            &associated_proposal_ids,
        ),
    });
    let digest = blake3::hash(&serde_json::to_vec(&input)?)
        .to_hex()
        .to_string();
    Ok(format!("blake3:{digest}"))
}

fn inspect_shared_database_read_only(
    path: &Path,
    runtime_record_ids: &BTreeSet<String>,
    proposal_ids: &BTreeSet<String>,
    event_ids: &BTreeSet<String>,
) -> Result<()> {
    let conn = open_database_read_only(path, "failed to inspect migrated shared database")?;
    let quick_check: String = conn.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if quick_check != "ok" {
        bail!("migrated shared database integrity check failed: {quick_check}");
    }
    let present_runtime_ids = RuntimeRecords::new(&conn)
        .snapshots()?
        .into_iter()
        .map(|snapshot| snapshot.record().id.clone())
        .collect::<BTreeSet<_>>();
    let present_proposal_ids = read_proposals(&conn)?
        .into_iter()
        .map(|proposal| proposal.id)
        .collect::<BTreeSet<_>>();
    let present_event_ids = read_event_ids(&conn)?;
    if !runtime_record_ids.is_subset(&present_runtime_ids) {
        bail!("one or more migrated runtime record identities are missing");
    }
    if !proposal_ids.is_subset(&present_proposal_ids) {
        bail!("one or more migrated proposal identities are missing");
    }
    if !event_ids.is_subset(&present_event_ids) {
        bail!("one or more migrated event identities are missing");
    }
    Ok(())
}

fn read_optional_file(path: &Path, label: &str) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("failed to read {label} {}", path.display()))
        }
    }
}

fn reconcile_migration_rows<T>(
    kind: MigrationIdentityKind,
    shared_source: &Path,
    shared_rows: Vec<T>,
    legacy_databases: Vec<(PathBuf, Vec<T>)>,
    prior_requirements: Option<&MigrationReceiptRequirements>,
    allow_legacy_updates: bool,
) -> Result<BTreeMap<String, (PathBuf, T)>>
where
    T: MigrationIdentityRow,
{
    let mut shared_by_id = BTreeMap::new();
    for row in shared_rows {
        let id = row.migration_id().to_owned();
        if shared_by_id.insert(id.clone(), row).is_some() {
            bail!("shared {} identity is duplicated: {id}", kind.label());
        }
    }

    let mut legacy_by_id = BTreeMap::<String, Vec<(PathBuf, T, String)>>::new();
    for (database_path, rows) in legacy_databases {
        for row in rows {
            let id = row.migration_id().to_owned();
            let digest = migration_identity_digest(kind, &row)?;
            legacy_by_id
                .entry(id)
                .or_default()
                .push((database_path.clone(), row, digest));
        }
    }

    let ids = shared_by_id
        .keys()
        .chain(legacy_by_id.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let prior_baselines =
        prior_requirements.and_then(|requirements| requirements.identity_baselines.as_ref());
    let mut reconciled = BTreeMap::new();

    for id in ids {
        let shared_row = shared_by_id.remove(&id);
        let legacy_rows = legacy_by_id.remove(&id).unwrap_or_default();
        let prior_shared_digest =
            prior_baselines.and_then(|baselines| baselines.shared.for_kind(kind).get(&id));

        let Some(prior_shared_digest) = prior_shared_digest else {
            let selected = reconcile_new_migration_identity(
                kind,
                &id,
                shared_source,
                shared_row,
                legacy_rows,
            )?;
            reconciled.insert(id, selected);
            continue;
        };

        let shared_digest = shared_row
            .as_ref()
            .map(|row| migration_identity_digest(kind, row))
            .transpose()?;
        let shared_changed = shared_digest.as_deref() != Some(prior_shared_digest.as_str());
        let changed_legacy_rows = legacy_rows
            .iter()
            .filter(|(database_path, _, digest)| {
                prior_baselines
                    .and_then(|baselines| baselines.legacy_database(database_path))
                    .and_then(|database| database.identities.for_kind(kind).get(&id))
                    .map(String::as_str)
                    != Some(digest.as_str())
            })
            .collect::<Vec<_>>();

        let changed_legacy_row = changed_legacy_rows.first().copied();
        if let Some((first_path, first_row, _)) = changed_legacy_row {
            for (other_path, other_row, _) in changed_legacy_rows.iter().skip(1).copied() {
                if other_row != first_row {
                    bail_migration_identity_conflict(kind, &id, first_path, other_path)?;
                }
            }
        }

        let selected = match (shared_row, changed_legacy_row) {
            (Some(shared), None) => (shared_source.to_path_buf(), shared),
            (Some(shared), Some((legacy_path, legacy, _))) if &shared == legacy => {
                (shared_source.to_path_buf(), shared)
            }
            (Some(_), Some((legacy_path, _, _))) if !allow_legacy_updates => {
                bail!(
                    "retained legacy {} {id} changed after migration in {}; existing identities are immutable",
                    kind.label(),
                    legacy_path.display()
                );
            }
            (Some(_), Some((legacy_path, _, _))) if shared_changed => {
                bail!(
                    "shared and retained legacy {} {id} diverged after migration at {}",
                    kind.label(),
                    legacy_path.display()
                );
            }
            (Some(_), Some((legacy_path, legacy, _))) => {
                let every_changed_source_started_from_shared =
                    changed_legacy_rows.iter().all(|(database_path, _, _)| {
                        prior_baselines
                            .and_then(|baselines| baselines.legacy_database(database_path))
                            .and_then(|database| database.identities.for_kind(kind).get(&id))
                            == Some(prior_shared_digest)
                    });
                if !every_changed_source_started_from_shared {
                    bail!(
                        "retained legacy {} {id} changed from a stale or untracked baseline at {}",
                        kind.label(),
                        legacy_path.display()
                    );
                }
                (legacy_path.clone(), legacy.clone())
            }
            (None, Some((legacy_path, legacy, digest))) if digest == prior_shared_digest => {
                (legacy_path.clone(), legacy.clone())
            }
            (None, Some((legacy_path, _, _))) => {
                bail!(
                    "shared {} {id} is missing while its retained legacy copy changed at {}",
                    kind.label(),
                    legacy_path.display()
                );
            }
            (None, None) => {
                let Some((legacy_path, legacy, _)) = legacy_rows
                    .iter()
                    .find(|(_, _, digest)| digest == prior_shared_digest)
                else {
                    bail!(
                        "migration recovery cannot restore the prior shared {} identity {id} exactly",
                        kind.label()
                    );
                };
                (legacy_path.clone(), legacy.clone())
            }
        };
        reconciled.insert(id, selected);
    }

    Ok(reconciled)
}

fn reconcile_new_migration_identity<T>(
    kind: MigrationIdentityKind,
    id: &str,
    shared_source: &Path,
    shared_row: Option<T>,
    legacy_rows: Vec<(PathBuf, T, String)>,
) -> Result<(PathBuf, T)>
where
    T: MigrationIdentityRow,
{
    if let Some(shared) = shared_row {
        for (legacy_path, legacy, _) in &legacy_rows {
            if legacy != &shared {
                bail_migration_identity_conflict(kind, id, shared_source, legacy_path)?;
            }
        }
        return Ok((shared_source.to_path_buf(), shared));
    }

    let Some((first_path, first_row, _)) = legacy_rows.first() else {
        bail!("migration {} identity disappeared: {id}", kind.label());
    };
    for (other_path, other_row, _) in legacy_rows.iter().skip(1) {
        if other_row != first_row {
            bail_migration_identity_conflict(kind, id, first_path, other_path)?;
        }
    }
    Ok((first_path.clone(), first_row.clone()))
}

fn bail_migration_identity_conflict(
    kind: MigrationIdentityKind,
    id: &str,
    first_source: &Path,
    second_source: &Path,
) -> Result<()> {
    if kind == MigrationIdentityKind::Event {
        bail!(
            "legacy event id collision for {id} between {} and {}",
            first_source.display(),
            second_source.display()
        );
    }
    bail!(
        "legacy {} {id} conflicts between {} and {}",
        kind.label(),
        first_source.display(),
        second_source.display()
    )
}

fn migration_identity_digest<T>(kind: MigrationIdentityKind, row: &T) -> Result<String>
where
    T: Serialize + ?Sized,
{
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"memzoi/migration-identity-v1\0");
    hasher.update(kind.label().as_bytes());
    hasher.update(&[0]);
    hasher.update(&serde_json::to_vec(row)?);
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn read_legacy_runtime_snapshots(conn: &Connection) -> Result<Vec<RuntimeRecordSnapshot>> {
    if !table_exists(conn, "memory_record")?
        || !table_has_column(conn, "memory_record", "destination")?
    {
        return Ok(Vec::new());
    }

    let lane = if table_has_column(conn, "memory_record", "lane")? {
        "lane"
    } else {
        "'semantic'"
    };
    let proposal_id = if table_has_column(conn, "memory_record", "proposal_id")? {
        "proposal_id"
    } else {
        "NULL"
    };
    let mut statement = conn.prepare(&format!(
        "SELECT id, type, {lane}, destination, scope_kind, scope_id, visibility, title, body,
                status, confidence, source_kind, source_ref, content_hash, created_at, updated_at,
                supersedes_id, expires_at, {proposal_id}
         FROM memory_record
         WHERE destination IN ('local', 'session')
         ORDER BY updated_at DESC, id ASC"
    ))?;
    let rows = statement.query_map([], search::record_from_row)?;
    let mut records = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);

    let has_capture = table_exists(conn, "memory_capture")?;
    let mut snapshots = Vec::with_capacity(records.len());
    for mut record in records.drain(..) {
        if has_capture {
            record.capture = crate::capture::load_capture_provenance(conn, &record.id)?;
        }
        let tags = read_legacy_record_tags(conn, &record.id)?;
        let paths = read_legacy_record_paths(conn, &record.id)?;
        snapshots.push(RuntimeRecordSnapshot::from_parts(record, tags, paths));
    }
    Ok(snapshots)
}

fn read_legacy_record_tags(conn: &Connection, record_id: &str) -> Result<Vec<String>> {
    let mut statement =
        conn.prepare("SELECT tag FROM memory_tag WHERE record_id = ?1 ORDER BY tag ASC")?;
    let rows = statement.query_map([record_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn read_legacy_record_paths(conn: &Connection, record_id: &str) -> Result<Vec<MemoryPath>> {
    let mut statement = conn.prepare(
        "SELECT path, symbol, line_start, line_end
         FROM memory_path
         WHERE record_id = ?1
         ORDER BY path ASC, COALESCE(symbol, '') ASC, COALESCE(line_start, 0) ASC",
    )?;
    let rows = statement.query_map([record_id], |row| {
        Ok(MemoryPath {
            path: row.get(0)?,
            symbol: row.get(1)?,
            line_start: row.get(2)?,
            line_end: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn events_associated_with(
    events: &[EventRow],
    runtime_record_ids: &BTreeSet<String>,
    proposal_ids: &BTreeSet<String>,
) -> Vec<EventRow> {
    events
        .iter()
        .filter(|event| {
            event
                .record_id
                .as_ref()
                .is_some_and(|id| runtime_record_ids.contains(id))
                || event
                    .proposal_id
                    .as_ref()
                    .is_some_and(|id| proposal_ids.contains(id))
        })
        .cloned()
        .collect()
}

fn read_events(conn: &Connection) -> Result<Vec<EventRow>> {
    if !table_exists(conn, "event_log")? {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare(
        "SELECT id, event_type, actor, payload_json, record_id, proposal_id, created_at
         FROM event_log
         ORDER BY created_at, id",
    )?;
    let rows = statement.query_map([], event_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn read_event_ids(conn: &Connection) -> Result<BTreeSet<String>> {
    if !table_exists(conn, "event_log")? {
        return Ok(BTreeSet::new());
    }
    let mut statement = conn.prepare("SELECT id FROM event_log ORDER BY id")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .collect())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1
         )",
        [table],
        |row| row.get(0),
    )?)
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names.iter().any(|name| name == column))
}

fn read_proposals(conn: &Connection) -> Result<Vec<ProposalRow>> {
    if !table_exists(conn, "proposal")? {
        return Ok(Vec::new());
    }
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
        insert_proposal_if_absent(conn, proposal)?;
    }
    Ok(())
}

fn insert_proposal_if_absent(conn: &Connection, proposal: &ProposalRow) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO proposal (
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
    Ok(())
}

fn write_migration_receipt(paths: &MemoryPaths, receipt: &serde_json::Value) -> Result<()> {
    let receipt_path = paths.repository_runtime_dir.join(MIGRATION_RECEIPT_FILE);
    atomic_install_file(
        &receipt_path,
        &serde_json::to_vec_pretty(receipt)?,
        "migration receipt",
    )
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
        InitRequest, LocalMemoryInput, MemoryDraft, MemoryLane, MemoryService, MemoryType,
        OkfProposalSensitivity, ProposalStatus, RepositoryContentClass, ScopeKind, Visibility,
        proposals,
    };

    use super::*;

    #[derive(Debug, Clone, Copy)]
    enum ReleasedSchema {
        V020,
        V030,
        V031,
    }

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
    fn released_v020_migrates_config_proposal_and_event_without_mutating_source() -> Result<()> {
        let (_temp, paths, legacy_dir) = legacy_git_paths_fixture()?;
        let source_db = legacy_dir.join("memory.db");
        fs::write(legacy_dir.join("config.toml"), default_config())?;
        create_released_legacy_database(&source_db, ReleasedSchema::V020)?;
        let source_before = fs::read(&source_db)?;
        let schema_before = database_schema_snapshot(&source_db)?;

        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;

        assert_eq!(fs::read(&paths.config_path)?, default_config().as_bytes());
        let shared = open_database_read_only(&paths.shared_db_path, "test shared database")?;
        assert!(RuntimeRecords::new(&shared).snapshots()?.is_empty());
        assert_eq!(
            read_proposal(&shared, "prop-released")?,
            Some(released_proposal_row())
        );
        assert_eq!(
            read_event(&shared, "evt-proposal")?,
            Some(released_event(
                "evt-proposal",
                "memory.proposed",
                None,
                Some("prop-released")
            ))
        );
        assert!(read_event(&shared, "evt-repo-record")?.is_none());
        drop(shared);

        assert_eq!(fs::read(&source_db)?, source_before);
        assert_eq!(database_schema_snapshot(&source_db)?, schema_before);
        assert!(!database_has_column(
            &source_db,
            "memory_record",
            "destination"
        )?);
        assert!(!database_has_column(
            &source_db,
            "memory_record",
            "proposal_id"
        )?);
        assert!(!database_has_table(&source_db, "memory_capture")?);
        Ok(())
    }

    #[test]
    fn released_v030_and_v031_migrate_runtime_rows_and_exact_events_read_only() -> Result<()> {
        for release in [ReleasedSchema::V030, ReleasedSchema::V031] {
            let (_temp, paths, legacy_dir) = legacy_git_paths_fixture()?;
            let source_db = legacy_dir.join("memory.db");
            fs::write(legacy_dir.join("config.toml"), default_config())?;
            create_released_legacy_database(&source_db, release)?;
            let source_before = fs::read(&source_db)?;
            let schema_before = database_schema_snapshot(&source_db)?;

            MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
            let shared = open_database_read_only(&paths.shared_db_path, "test shared database")?;
            let snapshots = RuntimeRecords::new(&shared).snapshots()?;
            assert_eq!(snapshots.len(), 2, "release {release:?}");
            let records = snapshots
                .iter()
                .map(|snapshot| (snapshot.record().id.as_str(), snapshot.record()))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(
                records["rec-local"].proposal_id.as_deref(),
                matches!(release, ReleasedSchema::V031).then_some("prop-released")
            );
            assert!(records["rec-local"].capture.is_none());
            assert!(records["rec-session"].capture.is_none());
            assert_eq!(
                read_event(&shared, "evt-local")?,
                Some(released_event(
                    "evt-local",
                    "memory.local_created",
                    Some("rec-local"),
                    None
                ))
            );
            assert_eq!(
                read_event(&shared, "evt-session")?,
                Some(released_event(
                    "evt-session",
                    "memory.session_created",
                    Some("rec-session"),
                    None
                ))
            );
            assert_eq!(
                read_event(&shared, "evt-proposal")?,
                Some(released_event(
                    "evt-proposal",
                    "memory.proposed",
                    None,
                    Some("prop-released")
                ))
            );
            assert!(read_event(&shared, "evt-unrelated")?.is_none());
            drop(shared);

            assert_eq!(fs::read(&source_db)?, source_before, "release {release:?}");
            assert_eq!(
                database_schema_snapshot(&source_db)?,
                schema_before,
                "release {release:?}"
            );
            assert_eq!(
                database_has_column(&source_db, "memory_record", "proposal_id")?,
                matches!(release, ReleasedSchema::V031)
            );
            assert!(!database_has_table(&source_db, "memory_capture")?);
        }
        Ok(())
    }

    #[test]
    fn legacy_event_id_collision_fails_closed_without_a_receipt() -> Result<()> {
        let (_temp, paths, legacy_dir) = legacy_git_paths_fixture()?;
        let source_db = legacy_dir.join("memory.db");
        fs::write(legacy_dir.join("config.toml"), default_config())?;
        create_released_legacy_database(&source_db, ReleasedSchema::V031)?;
        fs::create_dir_all(&paths.repository_runtime_dir)?;
        fs::write(&paths.config_path, default_config())?;
        let shared = db::open_database(&paths.shared_db_path)?;
        db::init_database(&shared)?;
        shared.execute(
            "INSERT INTO event_log (
               id, event_type, actor, payload_json, record_id, proposal_id, created_at
             ) VALUES ('evt-local', 'memory.conflicting', 'agent:other', '{}', NULL, NULL,
                       '2026-07-01T00:00:00Z')",
            [],
        )?;
        drop(shared);

        let error = migrate_legacy_runtime_if_needed(&paths)
            .expect_err("a legacy event id collision must fail closed");
        assert!(
            format!("{error:#}").contains("event id collision"),
            "unexpected collision error: {error:#}"
        );
        assert!(
            !paths
                .repository_runtime_dir
                .join(MIGRATION_RECEIPT_FILE)
                .exists()
        );
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
    fn initialize_migrates_legacy_runtime_before_creating_the_new_layout() -> Result<()> {
        let (_temp, paths, local_id, proposal_id) = legacy_git_fixture()?;

        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let service = MemoryService::open_paths(paths.clone())?;

        assert_eq!(service.list_local_memory()?[0].id, local_id);
        assert_eq!(service.show_proposal(&proposal_id)?.id, proposal_id);
        assert!(
            paths
                .repository_runtime_dir
                .join(MIGRATION_RECEIPT_FILE)
                .is_file()
        );
        assert!(paths.legacy_runtime_dirs[0].is_dir());
        Ok(())
    }

    #[test]
    fn missing_shared_database_invalidates_receipt_and_recovers_retained_runtime() -> Result<()> {
        let (_temp, paths, local_id, proposal_id) = legacy_git_fixture()?;
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        assert!(migration_receipt_is_valid(&paths)?);

        fs::remove_file(&paths.shared_db_path)?;
        assert!(!migration_receipt_is_valid(&paths)?);
        assert!(migrate_legacy_runtime_if_needed(&paths)?);

        let service = MemoryService::open_paths(paths)?;
        assert_eq!(service.list_local_memory()?[0].id, local_id);
        assert_eq!(service.show_proposal(&proposal_id)?.id, proposal_id);
        Ok(())
    }

    #[test]
    fn rebuild_recovers_retained_runtime_when_shared_database_is_missing() -> Result<()> {
        let (_temp, paths, local_id, proposal_id) = legacy_git_fixture()?;
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        fs::remove_file(&paths.shared_db_path)?;

        MemoryService::rebuild_paths(paths.clone())?;

        let service = MemoryService::open_paths(paths)?;
        assert_eq!(service.list_local_memory()?[0].id, local_id);
        assert_eq!(service.show_proposal(&proposal_id)?.id, proposal_id);
        Ok(())
    }

    #[test]
    fn edited_migrated_config_remains_authoritative_during_shared_recovery() -> Result<()> {
        let (_temp, paths, local_id, proposal_id) = legacy_git_fixture()?;
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let edited_config =
            default_config().replace("okf = \"exports/okf\"", "okf = \"exports/custom-okf\"");
        assert_ne!(edited_config, default_config());
        fs::write(&paths.config_path, &edited_config)?;

        let reopened = MemoryService::open_paths(paths.clone())?;
        assert_eq!(reopened.list_local_memory()?[0].id, local_id);
        drop(reopened);
        assert_eq!(fs::read_to_string(&paths.config_path)?, edited_config);

        fs::remove_file(&paths.shared_db_path)?;
        MemoryService::rebuild_paths(paths.clone())?;

        assert_eq!(fs::read_to_string(&paths.config_path)?, edited_config);
        let recovered = MemoryService::open_paths(paths)?;
        assert_eq!(recovered.list_local_memory()?[0].id, local_id);
        assert_eq!(recovered.show_proposal(&proposal_id)?.id, proposal_id);
        Ok(())
    }

    #[test]
    fn receipt_requires_exact_runtime_proposal_and_event_identities_before_rebuild() -> Result<()> {
        let (_temp, paths, legacy_dir) = legacy_git_paths_fixture()?;
        fs::write(legacy_dir.join("config.toml"), default_config())?;
        create_released_legacy_database(&legacy_dir.join("memory.db"), ReleasedSchema::V031)?;
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        assert!(migration_receipt_is_valid(&paths)?);

        let shared = db::open_database(&paths.shared_db_path)?;
        db::init_database(&shared)?;
        shared.execute_batch(
            r#"
            DELETE FROM event_log WHERE id IN ('evt-local', 'evt-session', 'evt-proposal');
            DELETE FROM proposal WHERE id = 'prop-released';
            DELETE FROM memory_record WHERE id IN ('rec-local', 'rec-session');
            INSERT INTO memory_record (
              id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
              confidence, source_kind, source_ref, proposal_id, content_hash, created_at,
              updated_at, supersedes_id, expires_at
            ) VALUES
              ('rec-unrelated-a', 'preference', 'semantic', 'local', 'agent', NULL, 'private',
               'Unrelated A', 'Unrelated body A', 'active', 1.0, 'test', 'unrelated', NULL,
               'hash-unrelated-a', '2026-07-02T00:00:00Z', '2026-07-02T00:00:00Z', NULL, NULL),
              ('rec-unrelated-b', 'episode', 'session', 'session', 'agent', NULL, 'private',
               'Unrelated B', 'Unrelated body B', 'active', 1.0, 'test', 'unrelated', NULL,
               'hash-unrelated-b', '2026-07-02T00:00:00Z', '2026-07-02T00:00:00Z', NULL, NULL);
            INSERT INTO proposal (
              id, operation, payload_json, status, actor, validation_json, created_at, updated_at
            ) VALUES (
              'prop-unrelated', 'create', '{}', 'pending', 'agent:unrelated', NULL,
              '2026-07-02T00:00:00Z', '2026-07-02T00:00:00Z'
            );
            INSERT INTO event_log (
              id, event_type, actor, payload_json, record_id, proposal_id, created_at
            ) VALUES
              ('evt-unrelated-a', 'memory.local_created', 'agent:unrelated', '{}',
               'rec-unrelated-a', NULL, '2026-07-02T00:00:00Z'),
              ('evt-unrelated-b', 'memory.session_created', 'agent:unrelated', '{}',
               'rec-unrelated-b', NULL, '2026-07-02T00:00:00Z'),
              ('evt-unrelated-proposal', 'memory.proposed', 'agent:unrelated', '{}',
               NULL, 'prop-unrelated', '2026-07-02T00:00:00Z');
            "#,
        )?;
        drop(shared);

        assert!(
            !migration_receipt_is_valid(&paths)?,
            "same-count unrelated identities must not satisfy a migration receipt"
        );
        MemoryService::rebuild_paths(paths.clone())?;

        let shared = open_database_read_only(&paths.shared_db_path, "test recovered shared")?;
        for record_id in ["rec-local", "rec-session"] {
            let present: bool = shared.query_row(
                "SELECT EXISTS(SELECT 1 FROM memory_record WHERE id = ?1)",
                [record_id],
                |row| row.get(0),
            )?;
            assert!(present, "missing recovered runtime record {record_id}");
        }
        assert!(read_proposal(&shared, "prop-released")?.is_some());
        for event_id in ["evt-local", "evt-session", "evt-proposal"] {
            assert!(
                read_event(&shared, event_id)?.is_some(),
                "missing recovered event {event_id}"
            );
        }
        assert!(migration_receipt_is_valid(&paths)?);
        Ok(())
    }

    #[test]
    fn receipt_does_not_ignore_a_later_discovered_legacy_source() -> Result<()> {
        let (temp, mut paths, _legacy_dir) = legacy_git_paths_fixture()?;
        fs::write(
            paths.legacy_runtime_dirs[0].join("config.toml"),
            default_config(),
        )?;
        create_released_legacy_database(
            &paths.legacy_runtime_dirs[0].join("memory.db"),
            ReleasedSchema::V020,
        )?;
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        assert!(migration_receipt_is_valid(&paths)?);

        let later_source = temp.path().join("later-linked-runtime");
        fs::create_dir_all(&later_source)?;
        fs::write(later_source.join("config.toml"), default_config())?;
        paths.legacy_runtime_dirs.push(later_source.clone());

        assert!(!migration_receipt_is_valid(&paths)?);
        assert!(migrate_legacy_runtime_if_needed(&paths)?);
        assert!(migration_receipt_is_valid(&paths)?);
        let receipt = valid_migration_receipt(&paths.repository_runtime_dir)?
            .context("migration receipt should be valid after merging the later source")?;
        assert!(receipt["legacy_sources"].as_array().is_some_and(|sources| {
            sources
                .iter()
                .any(|source| source.as_str() == later_source.to_str())
        }));
        Ok(())
    }

    #[test]
    fn receipt_detects_later_legacy_writes_and_reimports_them_read_only() -> Result<()> {
        let (_temp, paths, legacy_dir) = legacy_git_paths_fixture()?;
        fs::write(legacy_dir.join("config.toml"), default_config())?;
        let source_db = legacy_dir.join("memory.db");
        let legacy = db::open_database(&source_db)?;
        db::init_database(&legacy)?;
        RuntimeRecords::new(&legacy).create_local(
            "agent:migration-test",
            &LocalMemoryInput {
                memory_type: MemoryType::Preference,
                lane: MemoryLane::Semantic,
                title: "Initially migrated local memory".to_owned(),
                body: "The first migration establishes the source digest.".to_owned(),
            },
            "2026-07-16T10:00:00Z",
        )?;
        let initially_migrated_proposal = proposals::propose_memory(
            &legacy,
            "agent:migration-test",
            sample_memory_draft(
                "Initially migrated proposal",
                "Shared proposal state may legitimately advance after migration.",
            ),
        )?;
        drop(legacy);

        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        assert!(migration_receipt_is_valid(&paths)?);
        let service = MemoryService::open_paths(paths.clone())?;
        let rejected = service.reject_proposal(
            &initially_migrated_proposal.id,
            "reviewer:migration-test",
            "advance the authoritative shared proposal state",
        )?;
        assert_eq!(rejected.status, ProposalStatus::Rejected);
        drop(service);
        assert!(migration_receipt_is_valid(&paths)?);
        let edited_config =
            default_config().replace("okf = \"exports/okf\"", "okf = \"exports/custom-okf\"");
        fs::write(&paths.config_path, &edited_config)?;

        let legacy = db::open_database(&source_db)?;
        db::init_database(&legacy)?;
        let event_ids_before = read_event_ids(&legacy)?;
        let later_local = RuntimeRecords::new(&legacy).create_local(
            "agent:old-memzoi",
            &LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Later legacy local memory".to_owned(),
                body: "A retained legacy writer appended this after migration.".to_owned(),
            },
            "2026-07-16T11:00:00Z",
        )?;
        let later_proposal = proposals::propose_memory(
            &legacy,
            "agent:old-memzoi",
            sample_memory_draft(
                "Later legacy proposal",
                "Proposal writes after migration must also invalidate the receipt.",
            ),
        )?;
        let later_event_ids = read_event_ids(&legacy)?
            .difference(&event_ids_before)
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(later_event_ids.len(), 2);
        drop(legacy);
        let source_after_append = fs::read(&source_db)?;
        let schema_after_append = database_schema_snapshot(&source_db)?;

        assert!(!migration_receipt_is_valid(&paths)?);
        let reopened = MemoryService::open_paths(paths.clone())?;
        assert!(
            reopened
                .list_local_memory()?
                .iter()
                .any(|record| record.id == later_local.id)
        );
        assert_eq!(
            reopened.show_proposal(&later_proposal.id)?.id,
            later_proposal.id
        );
        assert_eq!(
            reopened
                .show_proposal(&initially_migrated_proposal.id)?
                .status,
            ProposalStatus::Rejected
        );
        for event_id in later_event_ids {
            assert!(read_event(&reopened.shared_conn, &event_id)?.is_some());
        }
        drop(reopened);

        assert_eq!(fs::read_to_string(&paths.config_path)?, edited_config);
        assert_eq!(fs::read(&source_db)?, source_after_append);
        assert_eq!(database_schema_snapshot(&source_db)?, schema_after_append);
        assert!(migration_receipt_is_valid(&paths)?);

        fs::remove_dir_all(legacy_dir)?;
        assert!(migration_receipt_is_valid(&paths)?);
        Ok(())
    }

    #[test]
    fn receipt_three_way_imports_legacy_only_updates_exactly() -> Result<()> {
        let (_temp, paths, legacy_dir) = legacy_git_paths_fixture()?;
        fs::write(legacy_dir.join("config.toml"), default_config())?;
        let source_db = legacy_dir.join("memory.db");
        let legacy = db::open_database(&source_db)?;
        db::init_database(&legacy)?;
        let local = RuntimeRecords::new(&legacy).create_local(
            "agent:migration-test",
            &LocalMemoryInput {
                memory_type: MemoryType::Preference,
                lane: MemoryLane::Semantic,
                title: "Legacy-updated runtime baseline".to_owned(),
                body: "This runtime record will change only in the retained source.".to_owned(),
            },
            "2026-07-16T10:00:00Z",
        )?;
        legacy.execute(
            "INSERT INTO memory_tag(record_id, tag) VALUES (?1, 'old-tag')",
            [&local.id],
        )?;
        legacy.execute(
            "INSERT INTO memory_path(id, record_id, path, symbol, line_start, line_end)
             VALUES ('old-path', ?1, 'crates/old/**', NULL, NULL, NULL)",
            [&local.id],
        )?;
        let proposal = proposals::propose_memory(
            &legacy,
            "agent:migration-test",
            sample_memory_draft(
                "Legacy-updated proposal baseline",
                "This proposal will advance only in the retained source.",
            ),
        )?;
        drop(legacy);

        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        assert!(migration_receipt_is_valid(&paths)?);

        let legacy = db::open_database(&source_db)?;
        db::init_database(&legacy)?;
        let updated_body = "The retained source owns this exact updated runtime snapshot.";
        legacy.execute(
            "UPDATE memory_record
             SET title = 'Legacy-only runtime update', body = ?2, content_hash = ?3,
                 source_ref = 'legacy-only-update', updated_at = '2026-07-16T11:00:00Z'
             WHERE id = ?1",
            rusqlite::params![
                local.id,
                updated_body,
                blake3::hash(updated_body.as_bytes()).to_hex().to_string(),
            ],
        )?;
        legacy.execute("DELETE FROM memory_tag WHERE record_id = ?1", [&local.id])?;
        legacy.execute("DELETE FROM memory_path WHERE record_id = ?1", [&local.id])?;
        legacy.execute(
            "INSERT INTO memory_tag(record_id, tag) VALUES (?1, 'new-tag')",
            [&local.id],
        )?;
        legacy.execute(
            "INSERT INTO memory_path(id, record_id, path, symbol, line_start, line_end)
             VALUES ('new-path', ?1, 'crates/new/**', 'updated_symbol', 4, 8)",
            [&local.id],
        )?;
        let event_ids_before = read_event_ids(&legacy)?;
        let approved = proposals::approve_proposal(&legacy, &proposal.id, "agent:old-memzoi")?;
        assert_eq!(approved.status, ProposalStatus::Approved);
        let approval_event_ids = read_event_ids(&legacy)?
            .difference(&event_ids_before)
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(approval_event_ids.len(), 1);
        let expected_snapshot = read_legacy_runtime_snapshots(&legacy)?
            .into_iter()
            .find(|snapshot| snapshot.record().id == local.id)
            .context("updated legacy runtime snapshot should exist")?;
        drop(legacy);
        let source_after_update = fs::read(&source_db)?;

        assert!(!migration_receipt_is_valid(&paths)?);
        let reopened = MemoryService::open_paths(paths.clone())?;
        let actual_snapshot = RuntimeRecords::new(&reopened.shared_conn)
            .snapshots()?
            .into_iter()
            .find(|snapshot| snapshot.record().id == local.id)
            .context("shared runtime should contain the legacy-only update")?;
        assert_eq!(actual_snapshot, expected_snapshot);
        assert_eq!(
            reopened.show_proposal(&proposal.id)?.status,
            ProposalStatus::Approved
        );
        for event_id in approval_event_ids {
            assert!(read_event(&reopened.shared_conn, &event_id)?.is_some());
        }
        drop(reopened);

        assert_eq!(fs::read(&source_db)?, source_after_update);
        assert!(migration_receipt_is_valid(&paths)?);
        Ok(())
    }

    #[test]
    fn receipt_three_way_rejects_divergent_shared_and_legacy_updates() -> Result<()> {
        let (_temp, paths, legacy_dir) = legacy_git_paths_fixture()?;
        fs::write(legacy_dir.join("config.toml"), default_config())?;
        let source_db = legacy_dir.join("memory.db");
        let legacy = db::open_database(&source_db)?;
        db::init_database(&legacy)?;
        let proposal = proposals::propose_memory(
            &legacy,
            "agent:migration-test",
            sample_memory_draft(
                "Divergent migration proposal",
                "Concurrent terminal decisions must fail closed.",
            ),
        )?;
        drop(legacy);

        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let service = MemoryService::open_paths(paths.clone())?;
        let rejected = service.reject_proposal(
            &proposal.id,
            "reviewer:migration-test",
            "the shared authority rejects this proposal",
        )?;
        assert_eq!(rejected.status, ProposalStatus::Rejected);
        drop(service);

        let legacy = db::open_database(&source_db)?;
        db::init_database(&legacy)?;
        let event_ids_before = read_event_ids(&legacy)?;
        proposals::approve_proposal(&legacy, &proposal.id, "agent:old-memzoi")?;
        let legacy_approval_events = read_event_ids(&legacy)?
            .difference(&event_ids_before)
            .cloned()
            .collect::<BTreeSet<_>>();
        drop(legacy);

        assert!(!migration_receipt_is_valid(&paths)?);
        let error = MemoryService::open_paths(paths.clone())
            .err()
            .context("divergent shared and retained legacy updates must fail closed")?;
        assert!(format!("{error:#}").contains("diverged after migration"));

        let shared = open_database_read_only(&paths.shared_db_path, "test divergent shared")?;
        assert_eq!(
            read_proposal(&shared, &proposal.id)?
                .context("shared proposal should remain present")?
                .status,
            "rejected"
        );
        for event_id in legacy_approval_events {
            assert!(read_event(&shared, &event_id)?.is_none());
        }
        Ok(())
    }

    #[test]
    fn receipt_provenance_rejects_a_new_sibling_database_identity_collision() -> Result<()> {
        let (_temp, paths, legacy_dir) = legacy_git_paths_fixture()?;
        fs::write(legacy_dir.join("config.toml"), default_config())?;
        let memory_db = legacy_dir.join("memory.db");
        let legacy = db::open_database(&memory_db)?;
        db::init_database(&legacy)?;
        let proposal = proposals::propose_memory(
            &legacy,
            "agent:migration-test",
            sample_memory_draft(
                "Per-database provenance",
                "A new sibling database must not inherit another database's identities.",
            ),
        )?;
        drop(legacy);

        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let legacy = open_database_read_only(&memory_db, "test original legacy proposal")?;
        let mut colliding = read_proposal(&legacy, &proposal.id)?
            .context("original legacy proposal should exist")?;
        drop(legacy);
        colliding.status = "approved".to_owned();
        colliding.updated_at = "2026-07-16T12:00:00Z".to_owned();
        let sibling_db = legacy_dir.join("shared.db");
        let sibling = db::open_database(&sibling_db)?;
        db::init_database(&sibling)?;
        insert_proposal_if_absent(&sibling, &colliding)?;
        drop(sibling);

        assert!(!migration_receipt_is_valid(&paths)?);
        let error = MemoryService::open_paths(paths.clone())
            .err()
            .context("a new sibling database collision must fail closed")?;
        assert!(format!("{error:#}").contains("stale or untracked baseline"));

        let shared = open_database_read_only(&paths.shared_db_path, "test collision shared")?;
        assert_eq!(
            read_proposal(&shared, &proposal.id)?
                .context("shared proposal should remain present")?
                .status,
            "pending"
        );
        Ok(())
    }

    #[test]
    fn baseline_less_receipt_rebases_only_unchanged_legacy_databases() -> Result<()> {
        let (_temp, paths, legacy_dir) = legacy_git_paths_fixture()?;
        fs::write(legacy_dir.join("config.toml"), default_config())?;
        let source_db = legacy_dir.join("memory.db");
        let legacy = db::open_database(&source_db)?;
        db::init_database(&legacy)?;
        let proposal = proposals::propose_memory(
            &legacy,
            "agent:migration-test",
            sample_memory_draft(
                "Baseline-less receipt upgrade",
                "An unchanged legacy source can be safely rebased after shared state evolves.",
            ),
        )?;
        drop(legacy);

        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let service = MemoryService::open_paths(paths.clone())?;
        service.reject_proposal(
            &proposal.id,
            "reviewer:migration-test",
            "advance shared state before upgrading the receipt",
        )?;
        drop(service);

        let receipt_path = paths.repository_runtime_dir.join(MIGRATION_RECEIPT_FILE);
        let mut receipt: serde_json::Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
        receipt
            .as_object_mut()
            .context("migration receipt should be an object")?
            .remove(MIGRATION_IDENTITY_BASELINES_FIELD);
        fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt)?)?;

        assert!(!migration_receipt_is_valid(&paths)?);
        assert!(migrate_legacy_runtime_if_needed(&paths)?);
        let reopened = MemoryService::open_paths(paths.clone())?;
        assert_eq!(
            reopened.show_proposal(&proposal.id)?.status,
            ProposalStatus::Rejected
        );
        drop(reopened);
        let upgraded: serde_json::Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
        assert!(upgraded.get(MIGRATION_IDENTITY_BASELINES_FIELD).is_some());
        assert!(migration_receipt_is_valid(&paths)?);
        Ok(())
    }

    #[test]
    fn baseline_less_receipt_refuses_changed_legacy_databases() -> Result<()> {
        let (_temp, paths, legacy_dir) = legacy_git_paths_fixture()?;
        fs::write(legacy_dir.join("config.toml"), default_config())?;
        let source_db = legacy_dir.join("memory.db");
        let legacy = db::open_database(&source_db)?;
        db::init_database(&legacy)?;
        RuntimeRecords::new(&legacy).create_local(
            "agent:migration-test",
            &LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Baseline-less source baseline".to_owned(),
                body: "The source will change after its identity baselines are removed.".to_owned(),
            },
            "2026-07-16T10:00:00Z",
        )?;
        drop(legacy);

        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let receipt_path = paths.repository_runtime_dir.join(MIGRATION_RECEIPT_FILE);
        let mut receipt: serde_json::Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
        receipt
            .as_object_mut()
            .context("migration receipt should be an object")?
            .remove(MIGRATION_IDENTITY_BASELINES_FIELD);
        let baseline_less_receipt = serde_json::to_vec_pretty(&receipt)?;
        fs::write(&receipt_path, &baseline_less_receipt)?;

        let legacy = db::open_database(&source_db)?;
        db::init_database(&legacy)?;
        RuntimeRecords::new(&legacy).create_local(
            "agent:old-memzoi",
            &LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Unattributable later legacy row".to_owned(),
                body: "Without per-identity baselines this append must fail closed.".to_owned(),
            },
            "2026-07-16T11:00:00Z",
        )?;
        drop(legacy);

        let error = migrate_legacy_runtime_if_needed(&paths)
            .expect_err("a changed baseline-less source cannot be reconciled safely");
        assert!(format!("{error:#}").contains("baseline-less migration receipt"));
        assert_eq!(fs::read(&receipt_path)?, baseline_less_receipt);
        Ok(())
    }

    #[test]
    fn present_database_deletion_removes_identity_provenance() -> Result<()> {
        let (_temp, paths, legacy_dir) = legacy_git_paths_fixture()?;
        fs::write(legacy_dir.join("config.toml"), default_config())?;
        let source_db = legacy_dir.join("memory.db");
        let legacy = db::open_database(&source_db)?;
        db::init_database(&legacy)?;
        let proposal = proposals::propose_memory(
            &legacy,
            "agent:migration-test",
            sample_memory_draft(
                "Deleted legacy identity",
                "Recreating this identity later must be treated as a new collision.",
            ),
        )?;
        let original_row =
            read_proposal(&legacy, &proposal.id)?.context("original proposal row should exist")?;
        drop(legacy);

        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let legacy = db::open_database(&source_db)?;
        db::init_database(&legacy)?;
        legacy.execute(
            "DELETE FROM event_log WHERE proposal_id = ?1",
            [&proposal.id],
        )?;
        legacy.execute("DELETE FROM proposal WHERE id = ?1", [&proposal.id])?;
        let later = proposals::propose_memory(
            &legacy,
            "agent:old-memzoi",
            sample_memory_draft(
                "Later distinct proposal",
                "This row forces a receipt rewrite while the old identity is absent.",
            ),
        )?;
        drop(legacy);

        let service = MemoryService::open_paths(paths.clone())?;
        assert_eq!(service.show_proposal(&later.id)?.id, later.id);
        assert_eq!(
            service.show_proposal(&proposal.id)?.status,
            ProposalStatus::Pending
        );
        drop(service);
        let requirements = migration_receipt_requirements(&paths.repository_runtime_dir)?
            .context("rewritten receipt should retain migration requirements")?;
        let source_baseline = requirements
            .identity_baselines
            .as_ref()
            .and_then(|baselines| baselines.legacy_database(&source_db))
            .context("present source database should have an identity baseline")?;
        assert!(
            !source_baseline
                .identities
                .proposals
                .contains_key(&proposal.id)
        );

        let sibling = db::open_database(&source_db)?;
        db::init_database(&sibling)?;
        let mut recreated = original_row;
        recreated.status = "approved".to_owned();
        recreated.updated_at = "2026-07-16T12:00:00Z".to_owned();
        insert_proposal_if_absent(&sibling, &recreated)?;
        drop(sibling);

        let error = MemoryService::open_paths(paths.clone())
            .err()
            .context("a recreated deleted identity must be an untracked collision")?;
        assert!(format!("{error:#}").contains("stale or untracked baseline"));
        let shared = open_database_read_only(&paths.shared_db_path, "test recreated collision")?;
        assert_eq!(
            read_proposal(&shared, &proposal.id)?
                .context("shared proposal should remain present")?
                .status,
            "pending"
        );
        Ok(())
    }

    #[test]
    fn invalid_receipt_recovery_refuses_to_forget_missing_legacy_source_identities() -> Result<()> {
        let (temp, mut paths, legacy_a) = legacy_git_paths_fixture()?;
        let legacy_b = temp.path().join("retained-legacy-b");
        fs::create_dir_all(&legacy_b)?;
        paths.legacy_runtime_dirs.push(legacy_b.clone());

        let seed =
            |runtime_dir: &Path, title: &str| -> Result<(String, String, BTreeSet<String>)> {
                fs::write(runtime_dir.join("config.toml"), default_config())?;
                let conn = db::open_database(&runtime_dir.join("memory.db"))?;
                db::init_database(&conn)?;
                let local = RuntimeRecords::new(&conn).create_local(
                    "agent:receipt-ancestry-test",
                    &LocalMemoryInput {
                        memory_type: MemoryType::Preference,
                        lane: MemoryLane::Semantic,
                        title: format!("{title} runtime"),
                        body: format!("{title} runtime body"),
                    },
                    "2026-07-16T12:00:00Z",
                )?;
                let proposal = proposals::propose_memory(
                    &conn,
                    "agent:receipt-ancestry-test",
                    sample_memory_draft(
                        &format!("{title} proposal"),
                        &format!("{title} proposal body"),
                    ),
                )?;
                let event_ids = read_event_ids(&conn)?;
                Ok((local.id, proposal.id, event_ids))
            };
        let (_runtime_a, _proposal_a, _events_a) = seed(&legacy_a, "Legacy A")?;
        let (runtime_b, proposal_b, events_b) = seed(&legacy_b, "Legacy B")?;

        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        assert!(migration_receipt_is_valid(&paths)?);
        let receipt_path = paths.repository_runtime_dir.join(MIGRATION_RECEIPT_FILE);
        let receipt_before = fs::read(&receipt_path)?;
        let receipt: serde_json::Value = serde_json::from_slice(&receipt_before)?;
        assert!(receipt["legacy_sources"].as_array().is_some_and(|sources| {
            sources
                .iter()
                .any(|source| source.as_str() == legacy_b.to_str())
        }));

        fs::remove_dir_all(&legacy_b)?;
        let shared = db::open_database(&paths.shared_db_path)?;
        db::init_database(&shared)?;
        for event_id in &events_b {
            shared.execute("DELETE FROM event_log WHERE id = ?1", [event_id])?;
        }
        shared.execute("DELETE FROM proposal WHERE id = ?1", [&proposal_b])?;
        shared.execute("DELETE FROM memory_record WHERE id = ?1", [&runtime_b])?;
        drop(shared);
        assert!(!migration_receipt_is_valid(&paths)?);

        let error = migrate_legacy_runtime_if_needed(&paths)
            .expect_err("recovery must not replace the receipt with only Legacy A identities");
        assert!(
            format!("{error:#}").contains("previously recorded"),
            "unexpected ancestry error: {error:#}"
        );
        assert_eq!(fs::read(&receipt_path)?, receipt_before);
        assert!(!migration_receipt_is_valid(&paths)?);
        Ok(())
    }

    #[test]
    fn migration_merges_after_init_created_repository_runtime_without_a_receipt() -> Result<()> {
        let (_temp, paths, local_id, proposal_id) = legacy_git_fixture()?;
        fs::create_dir_all(paths.records_dir())?;
        fs::create_dir_all(&paths.repository_runtime_dir)?;
        fs::write(&paths.config_path, default_config())?;
        assert!(paths.repository_runtime_dir.is_dir());
        assert!(
            !paths
                .repository_runtime_dir
                .join(MIGRATION_RECEIPT_FILE)
                .exists()
        );
        let existing_shared = db::open_database(&paths.shared_db_path)?;
        db::init_database(&existing_shared)?;
        let existing_local = RuntimeRecords::new(&existing_shared).create_local(
            "agent:existing-runtime-test",
            &LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Existing repository runtime memory".to_owned(),
                body: "In-place migration must preserve existing shared rows.".to_owned(),
            },
            "2026-07-14T12:30:00Z",
        )?;
        existing_shared.execute(
            "INSERT INTO memory_path(id, record_id, path) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "existing-runtime-path",
                existing_local.id,
                "crates/existing/**"
            ],
        )?;
        drop(existing_shared);

        assert!(migrate_legacy_runtime_if_needed(&paths)?);
        assert!(!migrate_legacy_runtime_if_needed(&paths)?);
        let service = MemoryService::open_paths(paths.clone())?;

        let local_ids = service
            .list_local_memory()?
            .into_iter()
            .map(|record| record.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            local_ids,
            BTreeSet::from([local_id, existing_local.id.clone()])
        );
        let existing_path_count: i64 = service.shared_conn.query_row(
            "SELECT COUNT(*) FROM memory_path WHERE record_id = ?1",
            [existing_local.id],
            |row| row.get(0),
        )?;
        assert_eq!(existing_path_count, 1);
        assert_eq!(service.show_proposal(&proposal_id)?.id, proposal_id);
        assert!(paths.legacy_runtime_dirs[0].is_dir());
        Ok(())
    }

    #[test]
    fn migration_preserves_runtime_when_a_non_git_project_becomes_git() -> Result<()> {
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        let temp = TempDir::new()?;
        let project = temp.path().join("project");
        fs::create_dir_all(&project)?;
        let runtime_home = temp.path().join("runtime-home");
        let non_git_paths =
            MemoryPaths::with_runtime_home(project.canonicalize()?, runtime_home.clone());
        MemoryService::initialize_paths(non_git_paths.clone(), InitRequest { force: false })?;
        let service = MemoryService::open_paths(non_git_paths.clone())?;
        let local = service.create_local_memory(
            "agent:migration-test",
            LocalMemoryInput {
                memory_type: MemoryType::Preference,
                lane: MemoryLane::Semantic,
                title: "Pre-Git local memory".to_owned(),
                body: "Initializing Git later must not orphan this runtime memory.".to_owned(),
            },
        )?;
        let proposal = service.propose_memory(
            "agent:migration-test",
            sample_memory_draft(
                "Pre-Git proposal",
                "Initializing Git later must preserve shared proposal state.",
            ),
        )?;
        drop(service);

        run_git(&project, &["init", "-q"])?;
        run_git(&project, &["config", "user.email", "fixture@example.test"])?;
        run_git(&project, &["config", "user.name", "Fixture"])?;
        fs::write(project.join(".memzoi/records/.gitkeep"), "")?;
        run_git(&project, &["add", ".memzoi"])?;
        run_git(&project, &["commit", "-qm", "initialize repository"])?;

        let git_paths =
            MemoryPaths::with_runtime_home(project.canonicalize()?, runtime_home.clone());
        assert_ne!(
            git_paths.repository_runtime_dir,
            non_git_paths.repository_runtime_dir
        );
        assert!(
            git_paths
                .legacy_runtime_dirs
                .contains(&non_git_paths.repository_runtime_dir)
        );

        let migrated = MemoryService::open_paths(git_paths)?;
        assert_eq!(migrated.list_local_memory()?[0].id, local.id);
        assert_eq!(migrated.show_proposal(&proposal.id)?.id, proposal.id);
        assert!(non_git_paths.repository_runtime_dir.is_dir());
        Ok(())
    }

    #[test]
    fn git_migration_prefers_non_git_runtime_that_already_subsumed_legacy_state() -> Result<()> {
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        let temp = TempDir::new()?;
        let project = temp.path().join("project");
        fs::create_dir_all(&project)?;
        let runtime_home = temp.path().join("runtime-home");
        let non_git_paths =
            MemoryPaths::with_runtime_home(project.canonicalize()?, runtime_home.clone());
        let legacy_dir = non_git_paths
            .legacy_runtime_dirs
            .first()
            .context("non-Git project should expose its path-keyed legacy runtime")?
            .clone();
        fs::create_dir_all(&legacy_dir)?;
        fs::write(legacy_dir.join("config.toml"), default_config())?;
        let legacy = db::open_database(&legacy_dir.join("memory.db"))?;
        db::init_database(&legacy)?;
        let local = RuntimeRecords::new(&legacy).create_local(
            "agent:migration-test",
            &LocalMemoryInput {
                memory_type: MemoryType::Preference,
                lane: MemoryLane::Semantic,
                title: "Legacy non-Git memory".to_owned(),
                body: "A later Git migration must use the already-migrated runtime.".to_owned(),
            },
            "2026-07-14T12:00:00Z",
        )?;
        let proposal = proposals::propose_memory(
            &legacy,
            "agent:migration-test",
            sample_memory_draft(
                "Legacy non-Git proposal",
                "The migrated proposal will advance before Git is initialized.",
            ),
        )?;
        drop(legacy);

        MemoryService::initialize_paths(non_git_paths.clone(), InitRequest { force: false })?;
        let service = MemoryService::open_paths(non_git_paths.clone())?;
        let rejected = service.reject_proposal(
            &proposal.id,
            "reviewer:migration-test",
            "advance proposal state in the migrated runtime",
        )?;
        assert_eq!(rejected.status, ProposalStatus::Rejected);
        drop(service);

        run_git(&project, &["init", "-q"])?;
        run_git(&project, &["config", "user.email", "fixture@example.test"])?;
        run_git(&project, &["config", "user.name", "Fixture"])?;
        fs::create_dir_all(project.join(".memzoi/records"))?;
        fs::write(project.join(".memzoi/records/.gitkeep"), "")?;
        run_git(&project, &["add", ".memzoi"])?;
        run_git(&project, &["commit", "-qm", "initialize repository"])?;

        let git_paths =
            MemoryPaths::with_runtime_home(project.canonicalize()?, runtime_home.clone());
        assert!(git_paths.legacy_runtime_dirs.contains(&legacy_dir));
        assert!(
            git_paths
                .legacy_runtime_dirs
                .contains(&non_git_paths.repository_runtime_dir)
        );

        let migrated = MemoryService::open_paths(git_paths)?;
        assert_eq!(migrated.list_local_memory()?[0].id, local.id);
        assert_eq!(
            migrated.show_proposal(&proposal.id)?.status,
            ProposalStatus::Rejected
        );
        assert!(legacy_dir.is_dir());
        assert!(non_git_paths.repository_runtime_dir.is_dir());
        Ok(())
    }

    fn create_released_legacy_database(path: &Path, release: ReleasedSchema) -> Result<()> {
        let (runtime_columns, migration_versions) = match release {
            ReleasedSchema::V020 => ("", "(1)"),
            ReleasedSchema::V030 => (
                "lane TEXT NOT NULL DEFAULT 'semantic',\n\
                 destination TEXT NOT NULL DEFAULT 'repo',",
                "(1), (2), (3)",
            ),
            ReleasedSchema::V031 => (
                "lane TEXT NOT NULL DEFAULT 'semantic',\n\
                 destination TEXT NOT NULL DEFAULT 'repo',\n\
                 proposal_id TEXT,",
                "(1), (2), (3), (4)",
            ),
        };
        let schema = format!(
            r#"
            CREATE TABLE schema_migrations (
              version INTEGER PRIMARY KEY,
              applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );
            CREATE TABLE event_log (
              id TEXT PRIMARY KEY,
              event_type TEXT NOT NULL,
              actor TEXT NOT NULL,
              payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
              record_id TEXT,
              proposal_id TEXT,
              created_at TEXT NOT NULL
            );
            CREATE TABLE memory_record (
              rowid INTEGER PRIMARY KEY,
              id TEXT NOT NULL UNIQUE,
              type TEXT NOT NULL,
              {runtime_columns}
              scope_kind TEXT NOT NULL,
              scope_id TEXT,
              visibility TEXT NOT NULL DEFAULT 'repo',
              title TEXT NOT NULL,
              body TEXT NOT NULL,
              status TEXT NOT NULL,
              confidence REAL NOT NULL DEFAULT 1.0,
              source_kind TEXT,
              source_ref TEXT,
              content_hash TEXT NOT NULL,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL,
              supersedes_id TEXT REFERENCES memory_record(id),
              expires_at TEXT
            );
            CREATE TABLE memory_path (
              id TEXT PRIMARY KEY,
              record_id TEXT NOT NULL REFERENCES memory_record(id) ON DELETE CASCADE,
              repo_id TEXT,
              path TEXT NOT NULL,
              symbol TEXT,
              line_start INTEGER,
              line_end INTEGER
            );
            CREATE TABLE proposal (
              id TEXT PRIMARY KEY,
              operation TEXT NOT NULL,
              payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
              status TEXT NOT NULL,
              actor TEXT NOT NULL,
              validation_json TEXT CHECK (validation_json IS NULL OR json_valid(validation_json)),
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE TABLE memory_tag (
              record_id TEXT NOT NULL REFERENCES memory_record(id) ON DELETE CASCADE,
              tag TEXT NOT NULL,
              PRIMARY KEY (record_id, tag)
            );
            INSERT INTO schema_migrations(version) VALUES {migration_versions};
            INSERT INTO proposal (
              id, operation, payload_json, status, actor, validation_json, created_at, updated_at
            ) VALUES (
              'prop-released', 'create', '{{}}', 'pending', 'agent:released-fixture', NULL,
              '2026-07-01T00:00:00Z', '2026-07-01T00:00:00Z'
            );
            INSERT INTO event_log (
              id, event_type, actor, payload_json, record_id, proposal_id, created_at
            ) VALUES
              ('evt-local', 'memory.local_created', 'agent:released-fixture', '{{}}',
               'rec-local', NULL, '2026-07-01T00:00:00Z'),
              ('evt-session', 'memory.session_created', 'agent:released-fixture', '{{}}',
               'rec-session', NULL, '2026-07-01T00:00:00Z'),
              ('evt-proposal', 'memory.proposed', 'agent:released-fixture', '{{}}',
               NULL, 'prop-released', '2026-07-01T00:00:00Z'),
              ('evt-repo-record', 'memory.created', 'agent:released-fixture', '{{}}',
               'rec-repo', NULL, '2026-07-01T00:00:00Z'),
              ('evt-unrelated', 'memory.unrelated', 'agent:released-fixture', '{{}}',
               NULL, NULL, '2026-07-01T00:00:00Z');
            "#
        );
        let conn = Connection::open(path)?;
        conn.execute_batch(&schema)?;
        match release {
            ReleasedSchema::V020 => {
                conn.execute(
                    "INSERT INTO memory_record (
                       id, type, scope_kind, scope_id, visibility, title, body, status, confidence,
                       source_kind, source_ref, content_hash, created_at, updated_at,
                       supersedes_id, expires_at
                     ) VALUES (
                       'rec-repo', 'fact', 'repo', NULL, 'repo', 'Released repo record',
                       'A schema without destination has no provable runtime rows.', 'active', 1.0,
                       'test', 'released-v0.2.0', 'hash-repo', '2026-07-01T00:00:00Z',
                       '2026-07-01T00:00:00Z', NULL, NULL
                     )",
                    [],
                )?;
            }
            ReleasedSchema::V030 | ReleasedSchema::V031 => {
                let proposal_column = matches!(release, ReleasedSchema::V031)
                    .then_some(", proposal_id")
                    .unwrap_or_default();
                let proposal_value = matches!(release, ReleasedSchema::V031)
                    .then_some(", 'prop-released'")
                    .unwrap_or_default();
                conn.execute_batch(&format!(
                    "INSERT INTO memory_record (
                       id, type, lane, destination, scope_kind, scope_id, visibility, title, body,
                       status, confidence, source_kind, source_ref, content_hash, created_at,
                       updated_at, supersedes_id, expires_at{proposal_column}
                     ) VALUES
                       ('rec-local', 'preference', 'semantic', 'local', 'agent', NULL, 'private',
                        'Released local record', 'Local body', 'active', 1.0, 'test',
                        'released-runtime', 'hash-local', '2026-07-01T00:00:00Z',
                        '2026-07-01T00:00:00Z', NULL, NULL{proposal_value}),
                       ('rec-session', 'episode', 'session', 'session', 'agent', NULL, 'private',
                        'Released session record', 'Session body', 'active', 1.0, 'test',
                        'released-runtime', 'hash-session', '2026-07-01T00:00:00Z',
                        '2026-07-01T00:00:00Z', NULL, NULL{}),
                       ('rec-repo', 'fact', 'semantic', 'repo', 'repo', NULL, 'repo',
                        'Released repo record', 'Repo body', 'active', 1.0, 'test',
                        'released-runtime', 'hash-repo', '2026-07-01T00:00:00Z',
                        '2026-07-01T00:00:00Z', NULL, NULL{});",
                    if proposal_column.is_empty() {
                        ""
                    } else {
                        ", NULL"
                    },
                    if proposal_column.is_empty() {
                        ""
                    } else {
                        ", NULL"
                    },
                ))?;
                conn.execute_batch(
                    "INSERT INTO memory_tag(record_id, tag) VALUES
                       ('rec-local', 'legacy-local'), ('rec-session', 'legacy-session');
                     INSERT INTO memory_path(id, record_id, path, symbol, line_start, line_end)
                     VALUES ('path-local', 'rec-local', 'crates/legacy/**', NULL, NULL, NULL);",
                )?;
            }
        }
        drop(conn);
        Ok(())
    }

    fn released_proposal_row() -> ProposalRow {
        ProposalRow {
            id: "prop-released".to_owned(),
            operation: "create".to_owned(),
            payload_json: "{}".to_owned(),
            status: "pending".to_owned(),
            actor: "agent:released-fixture".to_owned(),
            validation_json: None,
            created_at: "2026-07-01T00:00:00Z".to_owned(),
            updated_at: "2026-07-01T00:00:00Z".to_owned(),
        }
    }

    fn released_event(
        id: &str,
        event_type: &str,
        record_id: Option<&str>,
        proposal_id: Option<&str>,
    ) -> EventRow {
        EventRow {
            id: id.to_owned(),
            event_type: event_type.to_owned(),
            actor: "agent:released-fixture".to_owned(),
            payload_json: "{}".to_owned(),
            record_id: record_id.map(str::to_owned),
            proposal_id: proposal_id.map(str::to_owned),
            created_at: "2026-07-01T00:00:00Z".to_owned(),
        }
    }

    fn database_schema_snapshot(path: &Path) -> Result<Vec<(String, String, String, String)>> {
        let conn = open_database_read_only(path, "failed to inspect test fixture schema")?;
        let mut statement = conn.prepare(
            "SELECT type, name, tbl_name, COALESCE(sql, '')
             FROM sqlite_schema
             ORDER BY type, name, tbl_name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn database_has_column(path: &Path, table: &str, column: &str) -> Result<bool> {
        let conn = open_database_read_only(path, "failed to inspect test fixture columns")?;
        let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let names = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(names.iter().any(|name| name == column))
    }

    fn database_has_table(path: &Path, table: &str) -> Result<bool> {
        let conn = open_database_read_only(path, "failed to inspect test fixture tables")?;
        Ok(conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1
             )",
            [table],
            |row| row.get(0),
        )?)
    }

    fn legacy_git_paths_fixture() -> Result<(TempDir, MemoryPaths, PathBuf)> {
        if Command::new("git").arg("--version").output().is_err() {
            bail!("Git is required for the legacy migration fixture");
        }
        let temp = TempDir::new()?;
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".memzoi/records"))?;
        run_git(&project, &["init", "-q"])?;
        run_git(&project, &["config", "user.email", "fixture@example.test"])?;
        run_git(&project, &["config", "user.name", "Fixture"])?;
        fs::write(project.join(".memzoi/records/.gitkeep"), "")?;
        run_git(&project, &["add", ".memzoi"])?;
        run_git(&project, &["commit", "-qm", "base"])?;

        let paths = MemoryPaths::with_runtime_home(
            project.canonicalize()?,
            temp.path().join("runtime-home"),
        );
        let legacy_dir = paths
            .legacy_runtime_dirs
            .first()
            .context("Git fixture should expose its path-keyed legacy runtime")?
            .clone();
        fs::create_dir_all(&legacy_dir)?;
        Ok((temp, paths, legacy_dir))
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

    fn legacy_git_fixture() -> Result<(TempDir, MemoryPaths, String, String)> {
        let (temp, paths, legacy_dir) = legacy_git_paths_fixture()?;
        fs::write(legacy_dir.join("config.toml"), default_config())?;
        let legacy = db::open_database(&legacy_dir.join("memory.db"))?;
        db::init_database(&legacy)?;
        let local = RuntimeRecords::new(&legacy).create_local(
            "agent:migration-test",
            &LocalMemoryInput {
                memory_type: MemoryType::Preference,
                lane: MemoryLane::Semantic,
                title: "Migrated local memory".to_owned(),
                body: "Linked worktrees must retain this local memory.".to_owned(),
            },
            "2026-07-14T12:00:00Z",
        )?;
        let proposal = proposals::propose_memory(
            &legacy,
            "agent:migration-test",
            sample_memory_draft("Migrated proposal", "Migration preserves proposal state"),
        )?;
        drop(legacy);
        Ok((temp, paths, local.id, proposal.id))
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
            source_ref: Some("legacy-runtime".to_owned()),
            sensitivity: OkfProposalSensitivity::RepoSafe,
            content_class: RepositoryContentClass::GeneralRepoKnowledge,
            confidence: 1.0,
        }
    }
}
