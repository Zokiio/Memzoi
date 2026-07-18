use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::{Date, OffsetDateTime, Time, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    AuthorizationProof, CaptureApplyResult, CapturePlan, CaptureRequest, CaptureReview,
    CaptureReviewInput, CaptureSourceInputs, ContextPack, ContextPackInput, HandoffInput,
    HandoffPack, ImportApplyResult, ImportDocument, ImportPlan, MemoryDestination, MemoryDraft,
    MemoryEvent, MemoryPaths, MemoryRecord, MemoryStatus, OkfProposalAction, OkfProposalFile,
    OkfProposalOutcome, OkfProposalResolution, OkfProposalSensitivity, PrecheckInput,
    PrecheckWarning, Proposal, ProposalStatus, ProposalStatusFilter, RepositoryContentClass,
    RepositoryWriteRoute, SafetyFieldKind, ScopeKind, SearchInput, SearchResult, SupersedeResult,
    ValidationResult, Visibility,
};
use crate::{
    config::{
        ProposalApprovalPolicy, discover_existing_paths, discover_paths, load_effective_config,
    },
    context, db,
    events::{AppendEvent, append_event, for_each_merged_event},
    expiry::{self, Clock, ExpiryDiagnostic, SystemClock},
    exporters, handoff, okf, precheck, proposals, schema, search,
    session_end::{SessionEndDocument, SessionEndResult},
};

mod canonical_write;
mod capture_route_apply;
mod derived_index;
mod import_lifecycle;
mod import_origin_journal;
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
pub(crate) use self::derived_index::{
    admit_repository_record_snapshot, ensure_repository_records_root_safe,
};
pub use self::proposal_packets::{
    FileProposalInventory, FileProposalInventoryEntry, FileProposalInventoryError,
    FileProposalResolutionResult, scan_file_proposal_inventory,
};
pub use self::runtime_records::{
    CheckpointCommandResult, CheckpointInput, CloseCheckpointCommand, ContinueCheckpointCommand,
    CreateCheckpointCommand, CreateCheckpointSuccessorCommand, LocalMemoryInput,
};

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

/// Idempotent promotion of one checkpoint's explicit session-end document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEndFromCheckpointCommand {
    pub operation_id: String,
    pub checkpoint_id: String,
    pub expected_version: String,
    pub document: SessionEndDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionEndFromCheckpointResult {
    pub promotion: SessionEndResult,
    pub closure: Option<CheckpointCommandResult>,
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
        if !paths.config_path.is_file() {
            bail!(
                "Memzoi bundle is not initialized at {}; run `memzoi init` first",
                paths.project_root.display()
            );
        }

        let shared_conn = open_current_shared_database(&paths)?;
        let index_conn = open_disposable_index_if_current(&paths)?;
        if index_conn.is_none() {
            if trusted_recall_evaluation {
                derived_index::rebuild_for_trusted_recall_eval(paths.clone())?;
            } else {
                derived_index::rebuild(paths.clone())?;
            }
        }
        let conn = match index_conn {
            Some(conn) => conn,
            None => open_current_index_database(&paths)?,
        };
        import_origin_journal::recover_on_open(&paths, &shared_conn)?;
        shared_runtime::refresh_index_mirrors(&paths, &shared_conn, &conn)?;
        capture_route_apply::recover_on_open(&paths, &conn)?;
        SessionEndRouteApply::new(&paths, &conn, &shared_conn, &clock).recover_on_open()?;
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
        init_current_bundle(&paths, request.force)?;
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
        validate_canonical_lifecycle_target(&target, self.now())?;
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
        validate_canonical_lifecycle_target(&target, self.now())?;
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
        validate_canonical_lifecycle_target(&target, self.now())?;
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
        validate_canonical_lifecycle_target(&target, self.now())?;
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

    /// Runs the query engine against a mechanically seeded derived index.
    ///
    /// Production callers must use [`Self::search_memory`], which verifies that
    /// repository rows still match their canonical OKF sources.
    #[doc(hidden)]
    pub fn search_memory_for_benchmark(&self, input: SearchInput) -> Result<Vec<SearchResult>> {
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
        let command = CreateCheckpointCommand {
            operation_id: Uuid::now_v7().to_string(),
            input,
        };
        let result = self.create_checkpoint_command(actor, command)?;
        RuntimeRecords::new(&self.shared_conn).checkpoint_for_lifecycle(&result.checkpoint_id)
    }

    pub fn create_checkpoint_command(
        &self,
        actor: &str,
        command: CreateCheckpointCommand,
    ) -> Result<CheckpointCommandResult> {
        ensure_non_empty_operation_id(&command.operation_id)?;
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        let reserved_ids = reserved_runtime_record_ids(&self.paths, &self.conn)?;
        let timestamp = self.now_timestamp()?;
        let route = crate::OriginRoute::CheckpointCreate;
        let descriptor = crate::OriginDescriptor::owner_command(&command.operation_id, route);
        let identity = crate::OriginIdentity::new(self.paths.repository_key(), descriptor.clone());
        let fingerprint = crate::origin_input_fingerprint(route, &command)?;
        let tx = self.shared_conn.unchecked_transaction()?;
        match crate::prepare_origin(&tx, &identity, &fingerprint, &timestamp)? {
            crate::OriginPreparation::Replay(outcome) => {
                return checkpoint_result_from_origin(&tx, &command.operation_id, &outcome, true);
            }
            crate::OriginPreparation::Pending(_) => {
                bail!("origin_operation_pending: checkpoint creation is already prepared")
            }
            crate::OriginPreparation::Acquired => {}
        }
        let record = RuntimeRecords::new(&tx).create_checkpoint_with_metadata_avoiding(
            actor,
            &command.input,
            &timestamp,
            descriptor,
            None,
            &reserved_ids,
        )?;
        let event_id = latest_checkpoint_event_id(&tx, &record.id, "memory.checkpoint_created")?;
        let outcome = crate::OriginOutcome::new(
            identity,
            fingerprint,
            crate::OriginOutcomeKind::Created,
            &timestamp,
        )
        .with_destination(MemoryDestination::Session)
        .with_record_id(&record.id)
        .with_lifecycle_event_id(event_id);
        let outcome = crate::finalize_origin(&tx, &outcome)?;
        let result = checkpoint_result_from_origin(&tx, &command.operation_id, &outcome, false)?;
        tx.commit()?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        Ok(result)
    }

    pub fn create_checkpoint_successor(
        &self,
        actor: &str,
        command: CreateCheckpointSuccessorCommand,
    ) -> Result<CheckpointCommandResult> {
        ensure_non_empty_operation_id(&command.operation_id)?;
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        let reserved_ids = reserved_runtime_record_ids(&self.paths, &self.conn)?;
        let now = self.now();
        let timestamp = expiry::format_timestamp(now)?;
        let route = crate::OriginRoute::CheckpointSuccessor;
        let descriptor = crate::OriginDescriptor::owner_command(&command.operation_id, route);
        let identity = crate::OriginIdentity::new(self.paths.repository_key(), descriptor.clone());
        let fingerprint = crate::origin_input_fingerprint(route, &command)?;
        let tx = self.shared_conn.unchecked_transaction()?;
        match crate::prepare_origin(&tx, &identity, &fingerprint, &timestamp)? {
            crate::OriginPreparation::Replay(outcome) => {
                return checkpoint_result_from_origin(&tx, &command.operation_id, &outcome, true);
            }
            crate::OriginPreparation::Pending(_) => {
                bail!("origin_operation_pending: checkpoint successor is already prepared")
            }
            crate::OriginPreparation::Acquired => {}
        }
        let predecessor =
            RuntimeRecords::new(&tx).checkpoint_for_lifecycle(&command.predecessor_id)?;
        RuntimeRecords::ensure_successor_predecessor(
            &predecessor,
            &command.expected_predecessor_version,
            now,
        )?;
        let lineage = crate::RecordLineage {
            kind: crate::RecordLineageKind::SessionSuccessor,
            predecessor_id: predecessor.id.clone(),
        };
        let record = RuntimeRecords::new(&tx).create_checkpoint_with_metadata_avoiding(
            actor,
            &command.input,
            &timestamp,
            descriptor,
            Some(lineage),
            &reserved_ids,
        )?;
        let record_version = RuntimeRecords::checkpoint_record_version(&record)?;
        let handoff_event = append_event(
            &tx,
            AppendEvent {
                event_type: "memory.checkpoint_succeeded".to_owned(),
                actor: actor.to_owned(),
                payload: json!({
                    "operation_id": &command.operation_id,
                    "predecessor_id": &predecessor.id,
                    "successor_id": &record.id,
                    "handoff_at": &timestamp,
                    "record_version": record_version,
                }),
                record_id: Some(record.id.clone()),
                proposal_id: None,
            },
        )?;
        let outcome = crate::OriginOutcome::new(
            identity,
            fingerprint,
            crate::OriginOutcomeKind::Created,
            &timestamp,
        )
        .with_destination(MemoryDestination::Session)
        .with_record_id(&record.id)
        .with_lifecycle_event_id(&handoff_event.id);
        let outcome = crate::finalize_origin(&tx, &outcome)?;
        let result = checkpoint_result_from_origin(&tx, &command.operation_id, &outcome, false)?;
        tx.commit()?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        Ok(result)
    }

    pub fn list_checkpoints(&self) -> Result<Vec<MemoryRecord>> {
        RuntimeRecords::new(&self.shared_conn).active_checkpoints(self.now())
    }

    pub fn show_checkpoint(&self, record_id: &str) -> Result<MemoryRecord> {
        RuntimeRecords::new(&self.shared_conn)
            .checkpoint(record_id, self.now())?
            .with_context(|| format!("checkpoint not found: {record_id}"))
    }

    /// Returns the optimistic-concurrency version for a checkpoint, including
    /// query-only history needed by owner lifecycle commands.
    pub fn checkpoint_record_version(&self, record_id: &str) -> Result<String> {
        let record = RuntimeRecords::new(&self.shared_conn).checkpoint_for_lifecycle(record_id)?;
        RuntimeRecords::checkpoint_record_version(&record)
    }

    /// Explicit lifecycle/history inspection; unlike ordinary checkpoint
    /// reads this includes closed and query-only records by identifier.
    pub fn inspect_checkpoint(&self, record_id: &str) -> Result<MemoryRecord> {
        RuntimeRecords::new(&self.shared_conn).checkpoint_for_lifecycle(record_id)
    }

    pub fn continue_checkpoint(
        &self,
        actor: &str,
        command: ContinueCheckpointCommand,
    ) -> Result<CheckpointCommandResult> {
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        let now = self.now();
        let timestamp = expiry::format_timestamp(now)?;
        let route = crate::OriginRoute::CheckpointContinue;
        let identity = crate::OriginIdentity::new(
            self.paths.repository_key(),
            crate::OriginDescriptor::owner_command(&command.operation_id, route),
        );
        let fingerprint = crate::origin_input_fingerprint(route, &command)?;
        let tx = self.shared_conn.unchecked_transaction()?;
        match crate::prepare_origin(&tx, &identity, &fingerprint, &timestamp)? {
            crate::OriginPreparation::Replay(outcome) => {
                return checkpoint_result_from_origin(&tx, &command.operation_id, &outcome, true);
            }
            crate::OriginPreparation::Pending(_) => {
                bail!("origin_operation_pending: checkpoint continuation is already prepared")
            }
            crate::OriginPreparation::Acquired => {}
        }
        let mutation =
            RuntimeRecords::new(&tx).continue_checkpoint(actor, &command, now, &timestamp)?;
        let event = mutation
            .event
            .as_ref()
            .context("checkpoint continuation must append a lifecycle event")?;
        let outcome = crate::OriginOutcome::new(
            identity,
            fingerprint,
            crate::OriginOutcomeKind::Created,
            &timestamp,
        )
        .with_destination(MemoryDestination::Session)
        .with_record_id(&mutation.record.id)
        .with_lifecycle_event_id(&event.id);
        let outcome = crate::finalize_origin(&tx, &outcome)?;
        let result = checkpoint_result_from_origin(&tx, &command.operation_id, &outcome, false)?;
        tx.commit()?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        Ok(result)
    }

    pub fn close_checkpoint(
        &self,
        actor: &str,
        command: CloseCheckpointCommand,
    ) -> Result<CheckpointCommandResult> {
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        let now = self.now();
        let timestamp = expiry::format_timestamp(now)?;
        let route = crate::OriginRoute::CheckpointClose;
        let identity = crate::OriginIdentity::new(
            self.paths.repository_key(),
            crate::OriginDescriptor::owner_command(&command.operation_id, route),
        );
        let fingerprint = crate::origin_input_fingerprint(route, &command)?;
        let tx = self.shared_conn.unchecked_transaction()?;
        match crate::prepare_origin(&tx, &identity, &fingerprint, &timestamp)? {
            crate::OriginPreparation::Replay(outcome) => {
                return checkpoint_result_from_origin(&tx, &command.operation_id, &outcome, true);
            }
            crate::OriginPreparation::Pending(_) => {
                bail!("origin_operation_pending: checkpoint closure is already prepared")
            }
            crate::OriginPreparation::Acquired => {}
        }
        let mutation =
            RuntimeRecords::new(&tx).close_checkpoint(actor, &command, now, &timestamp)?;
        let outcome_kind = if mutation.applied {
            crate::OriginOutcomeKind::Created
        } else {
            crate::OriginOutcomeKind::ExistingDuplicateNoWrite
        };
        let mut outcome =
            crate::OriginOutcome::new(identity, fingerprint, outcome_kind, &timestamp)
                .with_destination(MemoryDestination::Session)
                .with_record_id(&mutation.record.id);
        if let Some(event) = &mutation.event {
            outcome = outcome.with_lifecycle_event_id(&event.id);
        }
        let outcome = crate::finalize_origin(&tx, &outcome)?;
        let result = checkpoint_result_from_origin(&tx, &command.operation_id, &outcome, false)?;
        tx.commit()?;
        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)?;
        Ok(result)
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

    pub fn promote_session_end_from_checkpoint(
        &self,
        actor: &str,
        command: SessionEndFromCheckpointCommand,
    ) -> Result<SessionEndFromCheckpointResult> {
        ensure_non_empty_operation_id(&command.operation_id)?;
        self.ensure_repository_index_current()?;
        SessionEndRouteApply::new(
            &self.paths,
            &self.conn,
            &self.shared_conn,
            self.clock.as_ref(),
        )
        .promote_from_checkpoint(actor, command)
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

    pub fn plan_capture(&self, request: CaptureRequest) -> Result<CapturePlan> {
        self.plan_capture_with_inputs(request, &CaptureSourceInputs::default())
    }

    pub fn plan_capture_with_inputs(
        &self,
        request: CaptureRequest,
        source_inputs: &CaptureSourceInputs,
    ) -> Result<CapturePlan> {
        shared_runtime::refresh_index_mirrors(&self.paths, &self.shared_conn, &self.conn)?;
        self.ensure_repository_index_current()?;
        let evaluated_at = self.now_timestamp()?;
        crate::capture::plan_capture_with_connection_and_inputs(
            &self.paths,
            &self.conn,
            request,
            source_inputs,
            &evaluated_at,
        )
    }

    pub fn build_capture_review(
        &self,
        plan: &CapturePlan,
        input: CaptureReviewInput,
        reviewed_by: &str,
        reviewed_at: &str,
    ) -> Result<CaptureReview> {
        self.build_capture_review_with_inputs(
            plan,
            input,
            &CaptureSourceInputs::default(),
            reviewed_by,
            reviewed_at,
        )
    }

    pub fn build_capture_review_with_inputs(
        &self,
        plan: &CapturePlan,
        input: CaptureReviewInput,
        source_inputs: &CaptureSourceInputs,
        reviewed_by: &str,
        reviewed_at: &str,
    ) -> Result<CaptureReview> {
        self.build_capture_review_inner(plan, input, None, source_inputs, reviewed_by, reviewed_at)
    }

    pub fn build_capture_review_with_prior(
        &self,
        plan: &CapturePlan,
        input: CaptureReviewInput,
        prior_review: &CaptureReview,
        reviewed_by: &str,
        reviewed_at: &str,
    ) -> Result<CaptureReview> {
        self.build_capture_review_with_prior_and_inputs(
            plan,
            input,
            prior_review,
            &CaptureSourceInputs::default(),
            reviewed_by,
            reviewed_at,
        )
    }

    pub fn build_capture_review_with_prior_and_inputs(
        &self,
        plan: &CapturePlan,
        input: CaptureReviewInput,
        prior_review: &CaptureReview,
        source_inputs: &CaptureSourceInputs,
        reviewed_by: &str,
        reviewed_at: &str,
    ) -> Result<CaptureReview> {
        self.build_capture_review_inner(
            plan,
            input,
            Some(prior_review),
            source_inputs,
            reviewed_by,
            reviewed_at,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_capture_review_inner(
        &self,
        plan: &CapturePlan,
        input: CaptureReviewInput,
        prior_review: Option<&CaptureReview>,
        source_inputs: &CaptureSourceInputs,
        reviewed_by: &str,
        reviewed_at: &str,
    ) -> Result<CaptureReview> {
        shared_runtime::refresh_index_mirrors(&self.paths, &self.shared_conn, &self.conn)?;
        self.ensure_repository_index_current()?;
        let evaluated_at = self.now_timestamp()?;
        crate::capture::build_capture_review_with_connection_and_inputs(
            &self.paths,
            &self.conn,
            plan,
            input,
            prior_review,
            source_inputs,
            reviewed_by,
            reviewed_at,
            &evaluated_at,
        )
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

    /// Builds a context pack from a mechanically seeded benchmark index.
    ///
    /// Production callers must use [`Self::build_context_pack`].
    #[doc(hidden)]
    pub fn build_context_pack_for_benchmark(&self, input: ContextPackInput) -> Result<ContextPack> {
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

    /// Runs precheck against a mechanically seeded benchmark index.
    ///
    /// Production callers must use [`Self::precheck`].
    #[doc(hidden)]
    pub fn precheck_for_benchmark(&self, input: PrecheckInput) -> Result<Vec<PrecheckWarning>> {
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

fn checkpoint_result_from_origin(
    conn: &Connection,
    operation_id: &str,
    outcome: &crate::OriginOutcome,
    replayed: bool,
) -> Result<CheckpointCommandResult> {
    let checkpoint_id = outcome
        .record_id
        .as_deref()
        .context("checkpoint origin outcome has no record_id")?;
    let record = RuntimeRecords::new(conn).checkpoint_for_lifecycle(checkpoint_id)?;
    let current_version = RuntimeRecords::checkpoint_record_version(&record)?;
    let record_version = match outcome.lifecycle_event_id.as_deref() {
        Some(event_id) => conn
            .query_row(
                "SELECT payload_json FROM event_log WHERE id = ?1",
                [event_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
            .and_then(|payload| {
                payload
                    .get("record_version")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .unwrap_or(current_version),
        None => current_version,
    };
    Ok(CheckpointCommandResult {
        operation_id: operation_id.to_owned(),
        checkpoint_id: checkpoint_id.to_owned(),
        record_version,
        lifecycle_event_id: outcome.lifecycle_event_id.clone(),
        applied: outcome.outcome == crate::OriginOutcomeKind::Created,
        replayed,
    })
}

fn latest_checkpoint_event_id(
    conn: &Connection,
    checkpoint_id: &str,
    event_type: &str,
) -> Result<String> {
    conn.query_row(
        "SELECT id
         FROM event_log
         WHERE record_id = ?1 AND event_type = ?2
         ORDER BY rowid DESC
         LIMIT 1",
        rusqlite::params![checkpoint_id, event_type],
        |row| row.get(0),
    )
    .with_context(|| {
        format!("checkpoint {checkpoint_id} did not append expected event {event_type}")
    })
}

fn ensure_non_empty_operation_id(operation_id: &str) -> Result<()> {
    if operation_id.trim().is_empty() {
        bail!("operation_id is required");
    }
    Ok(())
}

pub fn lifecycle_transaction_artifact_count(paths: &MemoryPaths) -> Result<usize> {
    Ok(lifecycle_transaction_artifacts(paths)?.len())
}

fn validate_canonical_lifecycle_target(
    target: &MemoryRecord,
    evaluated_at: OffsetDateTime,
) -> Result<()> {
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
    if !crate::evaluate_current_assertion(
        &target.id,
        target.status,
        target.lane,
        &target.retention,
        evaluated_at,
        Vec::new(),
    )?
    .is_current
    {
        bail!(
            "record {} cannot be changed canonically because it is not a current assertion",
            target.id
        );
    }
    Ok(())
}

fn open_current_shared_database(paths: &MemoryPaths) -> Result<Connection> {
    let conn = db::open_database(&paths.shared_db_path).with_context(|| {
        format!(
            "failed to open current shared database {}",
            paths.shared_db_path.display()
        )
    })?;
    db::init_database(&conn).with_context(|| {
        format!(
            "failed to initialize current shared database {}",
            paths.shared_db_path.display()
        )
    })?;
    Ok(conn)
}

fn open_disposable_index_if_current(paths: &MemoryPaths) -> Result<Option<Connection>> {
    if !paths.index_db_path.is_file() {
        return Ok(None);
    }
    match open_current_index_database(paths) {
        Ok(conn) => Ok(Some(conn)),
        Err(error) if schema::is_unsupported_schema_error(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn open_current_index_database(paths: &MemoryPaths) -> Result<Connection> {
    let conn = db::open_database(&paths.index_db_path).with_context(|| {
        format!(
            "failed to open current disposable index {}",
            paths.index_db_path.display()
        )
    })?;
    db::init_database(&conn).with_context(|| {
        format!(
            "failed to initialize current disposable index {}",
            paths.index_db_path.display()
        )
    })?;
    Ok(conn)
}

pub fn init_bundle(paths: &MemoryPaths, force: bool) -> Result<InitBundleResult> {
    paths.validate_runtime_identity()?;
    init_current_bundle(paths, force)
}

fn init_current_bundle(paths: &MemoryPaths, force: bool) -> Result<InitBundleResult> {
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

    if paths.config_path.exists() && !force {
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
    r#"scope_kind = "repo"

[exports]
okf = "exports/okf"
agents_md = "exports/AGENTS.memory.md"
claude_md = "exports/CLAUDE.memory.md"
"#
}
