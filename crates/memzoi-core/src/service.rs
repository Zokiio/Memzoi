use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::{Date, OffsetDateTime, Time, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    AuthorizationProof, CaptureApplyResult, CapturePlan, CaptureReview, CaptureSourceInputs,
    ContextPack, ContextPackInput, HandoffInput, HandoffPack, ImportApplyResult, ImportDocument,
    ImportPlan, MemoryDestination, MemoryDraft, MemoryEvent, MemoryPaths, MemoryRecord,
    MemoryStatus, OkfProposalAction, OkfProposalFile, OkfProposalOutcome, OkfProposalResolution,
    OkfProposalSensitivity, PrecheckInput, PrecheckWarning, Proposal, ProposalStatus,
    ProposalStatusFilter, RepositoryContentClass, RepositoryWriteRoute, SafetyFieldKind, ScopeKind,
    SearchInput, SearchResult, SupersedeResult, ValidationResult, Visibility,
};
use crate::{
    config::{
        ProposalApprovalPolicy, discover_existing_paths, discover_paths, load_effective_config,
    },
    context, db,
    events::{AppendEvent, append_event, for_each_merged_event},
    expiry::{self, Clock, ExpiryDiagnostic, SystemClock},
    exporters, handoff, okf, precheck, proposals, search,
    session_end::{SessionEndDocument, SessionEndResult},
};

mod canonical_write;
mod capture_route_apply;
mod derived_index;
mod import_lifecycle;
mod materialization;
mod proposal_packets;
mod repository_mutation;
mod runtime_records;
mod safe_files;
mod session_end_route_apply;
mod shared_runtime;
#[cfg(test)]
mod tests;

pub use self::derived_index::{RebuildResult, RepoIndexDrift};
pub use self::proposal_packets::{
    FileProposalInventory, FileProposalInventoryEntry, FileProposalInventoryError,
    FileProposalResolutionResult, scan_file_proposal_inventory,
};
pub use self::runtime_records::{CheckpointInput, LocalMemoryInput};

use self::canonical_write::{CanonicalFileWrite, CanonicalWriteSession, FileWriteMode};
use self::import_lifecycle::ImportLifecycle;
use self::repository_mutation::{
    authorize_repository_projection_batch, canonical_write_projections,
    explicit_repository_provenance, memory_draft_safety_values, repository_transaction_root,
    safety_value,
};
use self::runtime_records::{RuntimeRecords, reserved_runtime_record_ids};
use self::safe_files::{RepoLifecycleLock, lifecycle_transaction_artifacts};
use self::session_end_route_apply::SessionEndRouteApply;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitRequest {
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitResult {
    pub paths: MemoryPaths,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitBundleResult {
    pub project_root: PathBuf,
    pub memory_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub config_path: PathBuf,
    pub db_path: PathBuf,
    pub exports_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Okf,
    AgentsMd,
    ClaudeMd,
}

impl ExportFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Okf => "okf",
            Self::AgentsMd => "agents-md",
            Self::ClaudeMd => "claude-md",
        }
    }
}

impl FromStr for ExportFormat {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "okf" => Ok(Self::Okf),
            "agents-md" => Ok(Self::AgentsMd),
            "claude-md" => Ok(Self::ClaudeMd),
            _ => bail!("invalid export format: {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportInput {
    pub format: ExportFormat,
    pub scope_kind: ScopeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportResult {
    pub format: ExportFormat,
    pub written_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalApprovalOverride {
    Auto,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposeOptions {
    pub approval_override: Option<ProposalApprovalOverride>,
    pub apply: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposeResult {
    pub proposal: Proposal,
    pub record: Option<MemoryRecord>,
    pub validation: Option<ValidationResult>,
    pub applied: bool,
}

pub struct MemoryService {
    paths: MemoryPaths,
    conn: Connection,
    shared_conn: Connection,
    clock: Arc<dyn Clock>,
    trusted_recall_evaluation: bool,
}

impl MemoryService {
    pub fn open(start: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_clock(start, SystemClock)
    }

    pub fn open_with_clock(start: impl AsRef<Path>, clock: impl Clock + 'static) -> Result<Self> {
        let paths = discover_existing_paths(start)?;
        Self::open_paths_with_clock(paths, clock)
    }

    pub fn open_paths(paths: MemoryPaths) -> Result<Self> {
        Self::open_paths_with_clock(paths, SystemClock)
    }

    pub fn open_paths_with_clock(paths: MemoryPaths, clock: impl Clock + 'static) -> Result<Self> {
        Self::open_paths_with_clock_and_admission(paths, clock, false)
    }

    /// Opens an isolated recall-evaluation fixture whose canonical inputs were
    /// deliberately staged through the explicit trusted-evaluation boundary.
    pub(crate) fn open_paths_with_clock_for_trusted_recall_eval(
        paths: MemoryPaths,
        clock: impl Clock + 'static,
    ) -> Result<Self> {
        Self::open_paths_with_clock_and_admission(paths, clock, true)
    }

    fn open_paths_with_clock_and_admission(
        paths: MemoryPaths,
        clock: impl Clock + 'static,
        trusted_recall_evaluation: bool,
    ) -> Result<Self> {
        paths.validate_runtime_identity()?;
        shared_runtime::migrate_legacy_runtime_if_needed(&paths)?;
        if !paths.config_path.is_file() {
            bail!(
                "Memzoi bundle is not initialized at {}; run `memzoi init` first",
                paths.project_root.display()
            );
        }

        let shared_conn = db::open_database(&paths.shared_db_path)?;
        db::init_database(&shared_conn)?;
        if !paths.index_db_path.is_file() {
            if trusted_recall_evaluation {
                derived_index::rebuild_for_trusted_recall_eval(paths.clone())?;
            } else {
                derived_index::rebuild(paths.clone())?;
            }
        }
        let conn = db::open_database(&paths.index_db_path)?;
        db::init_database(&conn)?;
        shared_runtime::refresh_index_mirrors(&paths, &shared_conn, &conn)?;
        capture_route_apply::recover_on_open(&paths, &conn)?;
        Ok(Self {
            paths,
            conn,
            shared_conn,
            clock: Arc::new(clock),
            trusted_recall_evaluation,
        })
    }

    pub fn initialize(start: impl AsRef<Path>, request: InitRequest) -> Result<InitResult> {
        let paths = discover_paths(start)?;
        Self::initialize_paths(paths, request)
    }

    pub fn initialize_paths(paths: MemoryPaths, request: InitRequest) -> Result<InitResult> {
        paths.validate_runtime_identity()?;
        let migrated = shared_runtime::migrate_legacy_runtime_if_needed(&paths)?;
        init_bundle_after_migration(&paths, request.force, migrated)?;
        Ok(InitResult { paths })
    }

    pub fn paths(&self) -> &MemoryPaths {
        &self.paths
    }

    pub fn for_each_event(&self, visit: impl FnMut(MemoryEvent) -> Result<()>) -> Result<()> {
        for_each_merged_event(&self.shared_conn, &self.conn, visit)
    }

    pub fn propose_memory(&self, actor: &str, draft: MemoryDraft) -> Result<Proposal> {
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        let proposal = proposals::propose_memory(&self.shared_conn, actor, draft)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        Ok(proposal)
    }

    pub fn propose_memory_with_options(
        &self,
        actor: &str,
        draft: MemoryDraft,
        options: ProposeOptions,
    ) -> Result<ProposeResult> {
        let lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        let mut policy = load_effective_config(&self.paths)?
            .workflow
            .proposal_approval;
        if let Some(approval_override) = options.approval_override {
            policy = match approval_override {
                ProposalApprovalOverride::Auto => ProposalApprovalPolicy::Auto,
                ProposalApprovalOverride::Manual => ProposalApprovalPolicy::Manual,
            };
        }
        if options.apply && policy == ProposalApprovalPolicy::Manual {
            bail!(
                "proposal apply mode requires auto approval; manual proposals must be approved before apply"
            );
        }

        let proposal = proposals::propose_memory(&self.shared_conn, actor, draft)?;
        if policy == ProposalApprovalPolicy::Manual {
            shared_runtime::refresh_index_mirrors_locked(
                &self.paths,
                &self.shared_conn,
                &self.conn,
            )?;
            return Ok(ProposeResult {
                proposal,
                record: None,
                validation: None,
                applied: false,
            });
        }

        let validation = proposals::validate_proposal_against_records(
            &self.shared_conn,
            &self.conn,
            &proposal.id,
        )?;
        if !validation.is_valid {
            shared_runtime::refresh_index_mirrors_locked(
                &self.paths,
                &self.shared_conn,
                &self.conn,
            )?;
            return Ok(ProposeResult {
                proposal: proposals::load_proposal_public(&self.shared_conn, &proposal.id)?,
                record: None,
                validation: Some(validation),
                applied: false,
            });
        }

        let approved = proposals::approve_proposal(&self.shared_conn, &proposal.id, actor)?;
        if !options.apply {
            shared_runtime::refresh_index_mirrors_locked(
                &self.paths,
                &self.shared_conn,
                &self.conn,
            )?;
            return Ok(ProposeResult {
                proposal: approved,
                record: None,
                validation: Some(validation),
                applied: false,
            });
        }

        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        drop(lifecycle_lock);
        let record = self.apply_proposal(&proposal.id, actor)?;
        Ok(ProposeResult {
            proposal: proposals::load_proposal_public(&self.shared_conn, &proposal.id)?,
            record: Some(record),
            validation: Some(validation),
            applied: true,
        })
    }

    pub fn list_proposals(&self, filter: ProposalStatusFilter) -> Result<Vec<Proposal>> {
        proposals::list_proposals(&self.shared_conn, filter)
    }

    pub fn show_proposal(&self, proposal_id: &str) -> Result<Proposal> {
        proposals::load_proposal_public(&self.shared_conn, proposal_id)
    }

    pub fn open_proposal_counts(&self) -> Result<BTreeMap<ProposalStatus, usize>> {
        proposals::open_proposal_counts(&self.shared_conn)
    }

    pub fn approve_proposal(&self, proposal_id: &str, actor: &str) -> Result<Proposal> {
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        let proposal = proposals::approve_proposal(&self.shared_conn, proposal_id, actor)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        Ok(proposal)
    }

    pub fn reject_proposal(
        &self,
        proposal_id: &str,
        actor: &str,
        reason: &str,
    ) -> Result<Proposal> {
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        let proposal = proposals::reject_proposal(&self.shared_conn, proposal_id, actor, reason)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        Ok(proposal)
    }

    pub fn validate_proposal(&self, proposal_id: &str) -> Result<ValidationResult> {
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        self.ensure_repository_index_current()?;
        let validation = proposals::validate_proposal_against_records(
            &self.shared_conn,
            &self.conn,
            proposal_id,
        )?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        Ok(validation)
    }

    pub fn apply_proposal(&self, proposal_id: &str, actor: &str) -> Result<MemoryRecord> {
        let session = CanonicalWriteSession::begin(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        self.ensure_repository_index_current()?;
        let proposal = proposals::load_proposal_public(&self.shared_conn, proposal_id)?;
        let mut safety_values = memory_draft_safety_values("proposal", &proposal.payload);
        safety_values.push(safety_value(
            "proposal.id".to_owned(),
            SafetyFieldKind::Identifier,
            proposal_id,
        ));
        authorize_repository_projection_batch(
            &self.paths,
            RepositoryWriteRoute::DatabaseProposalApply,
            proposal.payload.sensitivity,
            proposal.payload.scope_kind,
            proposal.payload.scope_id.as_deref(),
            proposal.payload.visibility,
            AuthorizationProof::ApprovedDatabaseProposal { proposal_id },
            explicit_repository_provenance(proposal.payload.content_class, proposal_id),
            &safety_values,
            &[],
        )?;
        let tx = self.conn.unchecked_transaction()?;
        let record = proposals::apply_proposal(&tx, proposal_id, actor)?;
        let write = self.prepare_record_file_write_with_conn(
            &session,
            &tx,
            &record,
            FileWriteMode::CreateNew,
        )?;
        let projections = canonical_write_projections(&self.paths, std::slice::from_ref(&write))?;
        let authorization = authorize_repository_projection_batch(
            &self.paths,
            RepositoryWriteRoute::DatabaseProposalApply,
            proposal.payload.sensitivity,
            proposal.payload.scope_kind,
            proposal.payload.scope_id.as_deref(),
            proposal.payload.visibility,
            AuthorizationProof::ApprovedDatabaseProposal { proposal_id },
            explicit_repository_provenance(proposal.payload.content_class, proposal_id),
            &safety_values,
            &projections,
        )?;
        shared_runtime::prepare_proposal_apply_sync_journal(
            &self.paths,
            &tx,
            &self.shared_conn,
            proposal_id,
            &write,
        )?;
        session.commit_and_then(
            RepositoryWriteRoute::DatabaseProposalApply,
            &authorization,
            tx,
            &[write],
            || shared_runtime::complete_pending_shared_sync_locked(&self.paths, &self.shared_conn),
        )?;
        Ok(record)
    }

    pub fn supersede_record(
        &self,
        record_id: &str,
        actor: &str,
        draft: MemoryDraft,
    ) -> Result<SupersedeResult> {
        self.supersede_record_with_hooks(record_id, actor, draft, |_| Ok(()), |_| Ok(()))
    }

    fn supersede_record_with_hooks<BeforeInstall, BeforeCommit>(
        &self,
        record_id: &str,
        actor: &str,
        draft: MemoryDraft,
        before_install: BeforeInstall,
        before_commit: BeforeCommit,
    ) -> Result<SupersedeResult>
    where
        BeforeInstall: FnMut(usize) -> Result<()>,
        BeforeCommit: FnOnce(&Transaction<'_>) -> Result<()>,
    {
        let session = CanonicalWriteSession::begin(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        let target = RuntimeRecords::new(&self.conn)
            .get(record_id)?
            .with_context(|| format!("memory record not found: {record_id}"))?;
        validate_legacy_canonical_target(&target)?;
        if target.scope_kind != draft.scope_kind || target.scope_id != draft.scope_id {
            bail!(
                "cannot supersede record {record_id} cross-scope: target={}:{}, replacement={}:{}",
                target.scope_kind.as_str(),
                target.scope_id.as_deref().unwrap_or("-"),
                draft.scope_kind.as_str(),
                draft.scope_id.as_deref().unwrap_or("-")
            );
        }
        self.ensure_repository_index_current()?;
        let mut safety_values = memory_draft_safety_values("replacement", &draft);
        safety_values.push(safety_value(
            "target_record_id".to_owned(),
            SafetyFieldKind::Identifier,
            record_id,
        ));
        authorize_repository_projection_batch(
            &self.paths,
            RepositoryWriteRoute::Supersede,
            draft.sensitivity,
            draft.scope_kind,
            draft.scope_id.as_deref(),
            draft.visibility,
            AuthorizationProof::LifecycleOperation {
                target_id: record_id,
            },
            explicit_repository_provenance(draft.content_class, record_id),
            &safety_values,
            &[],
        )?;
        let tx = self.conn.unchecked_transaction()?;
        let target = RuntimeRecords::new(&tx)
            .get(record_id)?
            .with_context(|| format!("memory record not found: {record_id}"))?;
        validate_legacy_canonical_target(&target)?;
        if target.scope_kind != draft.scope_kind || target.scope_id != draft.scope_id {
            bail!(
                "cannot supersede record {record_id} cross-scope: target={}:{}, replacement={}:{}",
                target.scope_kind.as_str(),
                target.scope_id.as_deref().unwrap_or("-"),
                draft.scope_kind.as_str(),
                draft.scope_id.as_deref().unwrap_or("-")
            );
        }
        let result = proposals::supersede_record(&tx, record_id, actor, draft.clone())?;
        let previous_write = self.prepare_record_file_write_with_conn(
            &session,
            &tx,
            &result.previous,
            FileWriteMode::Overwrite,
        )?;
        let replacement_write = self.prepare_record_file_write_with_conn(
            &session,
            &tx,
            &result.replacement,
            FileWriteMode::CreateNew,
        )?;
        let writes = [previous_write, replacement_write];
        let projections = canonical_write_projections(&self.paths, &writes)?;
        let authorization = authorize_repository_projection_batch(
            &self.paths,
            RepositoryWriteRoute::Supersede,
            draft.sensitivity,
            draft.scope_kind,
            draft.scope_id.as_deref(),
            draft.visibility,
            AuthorizationProof::LifecycleOperation {
                target_id: record_id,
            },
            explicit_repository_provenance(draft.content_class, record_id),
            &safety_values,
            &projections,
        )?;
        session.commit_with_hooks(
            RepositoryWriteRoute::Supersede,
            &authorization,
            tx,
            &writes,
            before_install,
            before_commit,
        )?;
        Ok(result)
    }

    pub fn tombstone_record(
        &self,
        record_id: &str,
        actor: &str,
        reason: &str,
    ) -> Result<MemoryRecord> {
        let session = CanonicalWriteSession::begin(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        let target = RuntimeRecords::new(&self.conn)
            .get(record_id)?
            .with_context(|| format!("memory record not found: {record_id}"))?;
        validate_legacy_canonical_target(&target)?;
        self.ensure_repository_index_current()?;
        let safety_values = vec![
            safety_value(
                "target_record_id".to_owned(),
                SafetyFieldKind::Identifier,
                record_id,
            ),
            safety_value(
                "tombstone.reason".to_owned(),
                SafetyFieldKind::Reason,
                reason,
            ),
        ];
        authorize_repository_projection_batch(
            &self.paths,
            RepositoryWriteRoute::Tombstone,
            OkfProposalSensitivity::RepoSafe,
            target.scope_kind,
            target.scope_id.as_deref(),
            target.visibility,
            AuthorizationProof::LifecycleOperation {
                target_id: record_id,
            },
            explicit_repository_provenance(RepositoryContentClass::GeneralRepoKnowledge, record_id),
            &safety_values,
            &[],
        )?;
        let tx = self.conn.unchecked_transaction()?;
        let target = RuntimeRecords::new(&tx)
            .get(record_id)?
            .with_context(|| format!("memory record not found: {record_id}"))?;
        validate_legacy_canonical_target(&target)?;
        let record = proposals::tombstone_record(&tx, record_id, actor, reason)?;
        let write = self.prepare_record_file_write_with_conn(
            &session,
            &tx,
            &record,
            FileWriteMode::Overwrite,
        )?;
        let projections = canonical_write_projections(&self.paths, std::slice::from_ref(&write))?;
        let authorization = authorize_repository_projection_batch(
            &self.paths,
            RepositoryWriteRoute::Tombstone,
            OkfProposalSensitivity::RepoSafe,
            target.scope_kind,
            target.scope_id.as_deref(),
            target.visibility,
            AuthorizationProof::LifecycleOperation {
                target_id: record_id,
            },
            explicit_repository_provenance(RepositoryContentClass::GeneralRepoKnowledge, record_id),
            &safety_values,
            &projections,
        )?;
        session.commit(
            RepositoryWriteRoute::Tombstone,
            &authorization,
            tx,
            &[write],
        )?;
        Ok(record)
    }

    fn prepare_record_file_write_with_conn(
        &self,
        session: &CanonicalWriteSession<'_>,
        conn: &Connection,
        record: &MemoryRecord,
        mode: FileWriteMode,
    ) -> Result<CanonicalFileWrite> {
        let tags = RuntimeRecords::new(conn).tags(&record.id)?;
        let applies_to = search::load_paths(conn, &record.id)?
            .into_iter()
            .map(|path| path.path)
            .collect::<Vec<_>>();
        match mode {
            FileWriteMode::CreateNew => session.prepare_create(record.clone(), tags, applies_to),
            FileWriteMode::Overwrite => session.prepare_replace(record.clone(), tags, applies_to),
        }
    }

    pub fn search_memory(&self, input: SearchInput) -> Result<Vec<SearchResult>> {
        shared_runtime::refresh_index_mirrors(&self.paths, &self.shared_conn, &self.conn)?;
        self.ensure_repository_index_current()?;
        search::search_memory_at(&self.conn, input, self.now())
    }

    pub fn inspect_expiry(&self, record_id: &str) -> Result<ExpiryDiagnostic> {
        shared_runtime::refresh_index_mirrors(&self.paths, &self.shared_conn, &self.conn)?;
        self.ensure_repository_index_current()?;
        let record = RuntimeRecords::new(&self.conn)
            .get(record_id)?
            .with_context(|| format!("memory record not found: {record_id}"))?;
        expiry::diagnose(record, self.now())
    }

    pub fn repo_index_drift(&self) -> Result<RepoIndexDrift> {
        derived_index::inspect(&self.paths, &self.conn)
    }

    fn ensure_repository_index_current(&self) -> Result<()> {
        self.ensure_repository_index_current_with_conn(&self.conn)
    }

    fn ensure_repository_index_current_with_conn(&self, conn: &Connection) -> Result<()> {
        let drift = if self.trusted_recall_evaluation {
            derived_index::inspect_for_trusted_recall_eval(&self.paths, conn)?
        } else {
            derived_index::inspect(&self.paths, conn)?
        };
        if drift.is_current() {
            return Ok(());
        }
        bail!(
            "repository derived index is stale (missing={}, stale={}, changed={}, fts_out_of_sync={}); run `memzoi rebuild` before accessing repository memory",
            drift.missing_from_index.len(),
            drift.stale_in_index.len(),
            drift.changed_in_index.len(),
            drift.fts_out_of_sync,
        );
    }

    pub fn create_local_memory(
        &self,
        actor: &str,
        input: LocalMemoryInput,
    ) -> Result<MemoryRecord> {
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        let reserved_ids = reserved_runtime_record_ids(&self.paths, &self.conn)?;
        let now = self.now_timestamp()?;
        let record = RuntimeRecords::new(&self.shared_conn).create_local_avoiding(
            actor,
            &input,
            &now,
            &reserved_ids,
        )?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        Ok(record)
    }

    pub fn list_local_memory(&self) -> Result<Vec<MemoryRecord>> {
        RuntimeRecords::new(&self.shared_conn)
            .active_for_destination(MemoryDestination::Local, self.now())
    }

    pub fn search_local_memory(&self, query: String, limit: usize) -> Result<Vec<SearchResult>> {
        search::search_memory_at(
            &self.shared_conn,
            SearchInput {
                query,
                destination: Some(MemoryDestination::Local),
                limit,
                include_inactive: false,
                ..SearchInput::default()
            },
            self.now(),
        )
    }

    pub fn create_checkpoint(&self, actor: &str, input: CheckpointInput) -> Result<MemoryRecord> {
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        let reserved_ids = reserved_runtime_record_ids(&self.paths, &self.conn)?;
        let now = self.now_timestamp()?;
        let record = RuntimeRecords::new(&self.shared_conn).create_checkpoint_avoiding(
            actor,
            &input,
            &now,
            &reserved_ids,
        )?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        Ok(record)
    }

    pub fn list_checkpoints(&self) -> Result<Vec<MemoryRecord>> {
        RuntimeRecords::new(&self.shared_conn).active_checkpoints(self.now())
    }

    pub fn show_checkpoint(&self, record_id: &str) -> Result<MemoryRecord> {
        RuntimeRecords::new(&self.shared_conn)
            .checkpoint(record_id, self.now())?
            .with_context(|| format!("checkpoint not found: {record_id}"))
    }

    pub fn promote_session_end(
        &self,
        actor: &str,
        document: SessionEndDocument,
    ) -> Result<SessionEndResult> {
        self.ensure_repository_index_current()?;
        SessionEndRouteApply::new(
            &self.paths,
            &self.conn,
            &self.shared_conn,
            self.clock.as_ref(),
        )
        .promote(actor, document)
    }
    pub fn plan_import(&self, actor: &str, document: ImportDocument) -> Result<ImportPlan> {
        shared_runtime::refresh_index_mirrors(&self.paths, &self.shared_conn, &self.conn)?;
        self.ensure_repository_index_current()?;
        ImportLifecycle::new(
            &self.paths,
            &self.conn,
            &self.shared_conn,
            self.clock.as_ref(),
        )
        .plan(actor, document)
    }

    pub fn apply_import(
        &self,
        actor: &str,
        document: ImportDocument,
        expected_plan_id: &str,
    ) -> Result<ImportApplyResult> {
        self.ensure_repository_index_current()?;
        ImportLifecycle::new(
            &self.paths,
            &self.conn,
            &self.shared_conn,
            self.clock.as_ref(),
        )
        .apply(actor, document, expected_plan_id)
    }

    pub fn apply_capture(
        &self,
        actor: &str,
        plan: CapturePlan,
        review: CaptureReview,
        expected_plan_id: &str,
        expected_review_id: &str,
    ) -> Result<CaptureApplyResult> {
        self.apply_capture_with_inputs(
            actor,
            plan,
            review,
            &CaptureSourceInputs::default(),
            expected_plan_id,
            expected_review_id,
        )
    }

    pub fn apply_capture_with_inputs(
        &self,
        actor: &str,
        plan: CapturePlan,
        review: CaptureReview,
        source_inputs: &CaptureSourceInputs,
        expected_plan_id: &str,
        expected_review_id: &str,
    ) -> Result<CaptureApplyResult> {
        self.apply_capture_inner(
            actor,
            plan,
            review,
            None,
            source_inputs,
            expected_plan_id,
            expected_review_id,
        )
    }

    pub fn apply_capture_with_prior(
        &self,
        actor: &str,
        plan: CapturePlan,
        review: CaptureReview,
        prior_review: &CaptureReview,
        expected_plan_id: &str,
        expected_review_id: &str,
    ) -> Result<CaptureApplyResult> {
        self.apply_capture_with_prior_and_inputs(
            actor,
            plan,
            review,
            prior_review,
            &CaptureSourceInputs::default(),
            expected_plan_id,
            expected_review_id,
        )
    }

    // Prior-review lineage, replay material, and pinned IDs are separate trust inputs.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_capture_with_prior_and_inputs(
        &self,
        actor: &str,
        plan: CapturePlan,
        review: CaptureReview,
        prior_review: &CaptureReview,
        source_inputs: &CaptureSourceInputs,
        expected_plan_id: &str,
        expected_review_id: &str,
    ) -> Result<CaptureApplyResult> {
        self.apply_capture_inner(
            actor,
            plan,
            review,
            Some(prior_review),
            source_inputs,
            expected_plan_id,
            expected_review_id,
        )
    }

    // Keep every apply-boundary input explicit through lock and transaction revalidation.
    #[allow(clippy::too_many_arguments)]
    fn apply_capture_inner(
        &self,
        actor: &str,
        plan: CapturePlan,
        review: CaptureReview,
        prior_review: Option<&CaptureReview>,
        source_inputs: &CaptureSourceInputs,
        expected_plan_id: &str,
        expected_review_id: &str,
    ) -> Result<CaptureApplyResult> {
        capture_route_apply::CaptureRouteApply::new(
            &self.paths,
            &self.conn,
            &self.shared_conn,
            self.clock.as_ref(),
        )
        .apply(capture_route_apply::CaptureRouteApplyCommand {
            actor,
            plan,
            review,
            prior_review,
            source_inputs,
            expected_plan_id,
            expected_review_id,
        })
    }

    pub fn build_context_pack(&self, input: ContextPackInput) -> Result<ContextPack> {
        shared_runtime::refresh_index_mirrors(&self.paths, &self.shared_conn, &self.conn)?;
        self.ensure_repository_index_current()?;
        context::build_context_pack_at(&self.conn, input, self.now())
    }

    pub fn build_handoff_pack(&self, input: HandoffInput) -> Result<HandoffPack> {
        shared_runtime::refresh_index_mirrors(&self.paths, &self.shared_conn, &self.conn)?;
        self.ensure_repository_index_current()?;
        handoff::build_handoff_pack_at(&self.conn, input, self.now())
    }

    pub fn precheck(&self, input: PrecheckInput) -> Result<Vec<PrecheckWarning>> {
        shared_runtime::refresh_index_mirrors(&self.paths, &self.shared_conn, &self.conn)?;
        self.ensure_repository_index_current()?;
        precheck::precheck_at(&self.conn, input, self.now())
    }

    pub fn export(&self, input: ExportInput) -> Result<ExportResult> {
        shared_runtime::refresh_index_mirrors(&self.paths, &self.shared_conn, &self.conn)?;
        self.ensure_repository_index_current()?;
        let written_paths = match input.format {
            ExportFormat::Okf => exporters::export_okf_at(
                &self.conn,
                &self.paths.exports_dir.join("okf"),
                input.scope_kind,
                self.now(),
            )?,
            ExportFormat::AgentsMd => vec![exporters::export_agents_md_at(
                &self.conn,
                &self.paths.exports_dir.join("AGENTS.memory.md"),
                input.scope_kind,
                self.now(),
            )?],
            ExportFormat::ClaudeMd => vec![exporters::export_claude_md_at(
                &self.conn,
                &self.paths.exports_dir.join("CLAUDE.memory.md"),
                input.scope_kind,
                self.now(),
            )?],
        };

        Ok(ExportResult {
            format: input.format,
            written_paths,
        })
    }

    fn now(&self) -> OffsetDateTime {
        self.clock.now_utc()
    }

    fn now_timestamp(&self) -> Result<String> {
        expiry::format_timestamp(self.now())
    }

    pub fn rebuild(self) -> Result<RebuildResult> {
        let paths = self.paths.clone();
        drop(self);
        Self::rebuild_paths(paths)
    }

    pub fn rebuild_at(start: impl AsRef<Path>) -> Result<RebuildResult> {
        let paths = discover_existing_paths(start)?;
        Self::rebuild_paths(paths)
    }

    pub fn rebuild_paths(paths: MemoryPaths) -> Result<RebuildResult> {
        paths.validate_runtime_identity()?;
        derived_index::rebuild(paths)
    }

    #[cfg(test)]
    fn rebuild_paths_with_snapshot_hook(
        paths: MemoryPaths,
        after_snapshot: impl FnOnce() -> Result<()>,
    ) -> Result<RebuildResult> {
        derived_index::rebuild_with_snapshot_hook(paths, after_snapshot)
    }

    pub(crate) fn rebuild_paths_for_trusted_recall_eval(
        paths: MemoryPaths,
    ) -> Result<RebuildResult> {
        derived_index::rebuild_for_trusted_recall_eval(paths)
    }
}

pub fn lifecycle_transaction_artifact_count(paths: &MemoryPaths) -> Result<usize> {
    Ok(lifecycle_transaction_artifacts(paths)?.len())
}

fn validate_legacy_canonical_target(target: &MemoryRecord) -> Result<()> {
    if target.destination != MemoryDestination::Repo {
        bail!(
            "record {} cannot be changed canonically because destination {} is not repo",
            target.id,
            target.destination.as_str()
        );
    }
    if target.visibility == Visibility::Private {
        bail!(
            "record {} cannot be changed canonically because visibility private is not repo-shareable",
            target.id
        );
    }
    if target.status != MemoryStatus::Active {
        bail!(
            "record {} cannot be changed canonically because status {} is not active",
            target.id,
            target.status.as_str()
        );
    }
    Ok(())
}

pub fn init_bundle(paths: &MemoryPaths, force: bool) -> Result<InitBundleResult> {
    paths.validate_runtime_identity()?;
    init_bundle_after_migration(paths, force, false)
}

fn init_bundle_after_migration(
    paths: &MemoryPaths,
    force: bool,
    migrated: bool,
) -> Result<InitBundleResult> {
    fs::create_dir_all(&paths.memory_dir).with_context(|| {
        format!(
            "failed to create memory directory {}",
            paths.memory_dir.display()
        )
    })?;
    fs::create_dir_all(paths.records_dir()).with_context(|| {
        format!(
            "failed to create records directory {}",
            paths.records_dir().display()
        )
    })?;
    fs::create_dir_all(&paths.repository_runtime_dir).with_context(|| {
        format!(
            "failed to create repository runtime directory {}",
            paths.repository_runtime_dir.display()
        )
    })?;
    fs::create_dir_all(&paths.worktree_runtime_dir).with_context(|| {
        format!(
            "failed to create worktree runtime directory {}",
            paths.worktree_runtime_dir.display()
        )
    })?;
    fs::create_dir_all(repository_transaction_root(paths)).with_context(|| {
        format!(
            "failed to create repository transaction directory {}",
            repository_transaction_root(paths).display()
        )
    })?;
    fs::create_dir_all(&paths.exports_dir).with_context(|| {
        format!(
            "failed to create exports directory {}",
            paths.exports_dir.display()
        )
    })?;

    if paths.config_path.exists() && !force && !migrated {
        bail!(
            "{} already exists; pass --force to overwrite it",
            paths.config_path.display()
        );
    }

    if force || !paths.config_path.exists() {
        fs::write(&paths.config_path, default_config())
            .with_context(|| format!("failed to write config {}", paths.config_path.display()))?;
    }

    let shared = db::open_database(&paths.shared_db_path)?;
    db::init_database(&shared)?;
    drop(shared);
    derived_index::rebuild(paths.clone())?;

    Ok(InitBundleResult {
        project_root: paths.project_root.clone(),
        memory_dir: paths.memory_dir.clone(),
        runtime_dir: paths.runtime_dir.clone(),
        config_path: paths.config_path.clone(),
        db_path: paths.db_path.clone(),
        exports_dir: paths.exports_dir.clone(),
    })
}

fn default_config() -> &'static str {
    r#"version = 1
scope_kind = "repo"

[exports]
okf = "exports/okf"
agents_md = "exports/AGENTS.memory.md"
claude_md = "exports/CLAUDE.memory.md"
"#
}
