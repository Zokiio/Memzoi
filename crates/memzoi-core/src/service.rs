use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::{Date, OffsetDateTime, Time, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::import::{self, ExistingDuplicate};
use crate::{
    CaptureApplyResult, CapturePlan, CaptureReview, CaptureSourceInputs, ContextPack,
    ContextPackInput, HandoffInput, HandoffPack, ImportApplyResult, ImportDocument, ImportPlan,
    MemoryDestination, MemoryDraft, MemoryEvent, MemoryLane, MemoryPath, MemoryPaths, MemoryRecord,
    MemoryStatus, MemoryType, OkfProposalAction, OkfProposalFile, OkfProposalOutcome,
    OkfProposalResolution, OkfProposalSensitivity, PrecheckInput, PrecheckWarning, Proposal,
    ProposalStatus, ProposalStatusFilter, ScopeKind, SearchInput, SearchResult, SupersedeResult,
    ValidationResult, Visibility,
};
use crate::{
    config::{
        ProposalApprovalPolicy, discover_existing_paths, discover_paths, load_effective_config,
    },
    context, db,
    events::{AppendEvent, append_event, for_each_event as stream_events},
    expiry::{self, Clock, ExpiryDiagnostic, SystemClock},
    exporters, handoff, okf, precheck, proposals, repository_io,
    repository_write_safety::{
        AuthorizationProof, ProvenanceAssessment, RepositoryContentClass, RepositoryProjection,
        RepositoryScope, RepositoryWriteRequest, RepositoryWriteRoute, SafetyField,
        SafetyFieldKind, authorize_repository_write, repository_write_policy_context_digest,
    },
    search,
    session_end::{
        SessionEndCandidateResult, SessionEndCandidateStatus, SessionEndDocument, SessionEndResult,
        SessionEndWrite, repo_sensitivity_block_reason, session_end_proposal_draft,
        validate_session_end_document,
    },
};

mod canonical_write;
mod capture_route_apply;
mod proposal_packets;
mod repository_mutation;
mod safe_files;

pub use self::proposal_packets::{
    FileProposalInventory, FileProposalInventoryEntry, FileProposalInventoryError,
    FileProposalResolutionResult, scan_file_proposal_inventory,
};

use self::canonical_write::{CanonicalFileWrite, CanonicalWriteSession, FileWriteMode};
use self::proposal_packets::{ProposalPacketLifecycle, prepare_pending_proposal_root};
use self::repository_mutation::{
    AuthorizedRepositoryProjectionBatch, OwnedRepositoryProjection, RepositorySafetyValue,
    repository_transaction_root,
};
use self::safe_files::{RepoLifecycleLock, ensure_safe_directory, lifecycle_transaction_artifacts};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildResult {
    pub records_root: PathBuf,
    pub db_path: PathBuf,
    pub record_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoIndexDrift {
    pub missing_from_index: Vec<String>,
    pub stale_in_index: Vec<String>,
    pub changed_in_index: Vec<String>,
    pub fts_out_of_sync: bool,
}

impl RepoIndexDrift {
    pub fn is_current(&self) -> bool {
        self.missing_from_index.is_empty()
            && self.stale_in_index.is_empty()
            && self.changed_in_index.is_empty()
            && !self.fts_out_of_sync
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalMemoryInput {
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointInput {
    pub task: String,
    pub note: String,
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
    clock: Arc<dyn Clock>,
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
        if !paths.config_path.is_file() {
            bail!(
                "Memzoi bundle is not initialized at {}; run `memzoi init` first",
                paths.project_root.display()
            );
        }

        let conn = db::open_database(&paths.db_path)?;
        db::init_database(&conn)?;
        capture_route_apply::recover_on_open(&paths, &conn)?;
        Ok(Self {
            paths,
            conn,
            clock: Arc::new(clock),
        })
    }

    pub fn initialize(start: impl AsRef<Path>, request: InitRequest) -> Result<InitResult> {
        let paths = discover_paths(start)?;
        Self::initialize_paths(paths, request)
    }

    pub fn initialize_paths(paths: MemoryPaths, request: InitRequest) -> Result<InitResult> {
        init_bundle(&paths, request.force)?;
        Ok(InitResult { paths })
    }

    pub fn paths(&self) -> &MemoryPaths {
        &self.paths
    }

    pub fn for_each_event(&self, visit: impl FnMut(MemoryEvent) -> Result<()>) -> Result<()> {
        stream_events(&self.conn, visit)
    }

    pub fn propose_memory(&self, actor: &str, draft: MemoryDraft) -> Result<Proposal> {
        proposals::propose_memory(&self.conn, actor, draft)
    }

    pub fn propose_memory_with_options(
        &self,
        actor: &str,
        draft: MemoryDraft,
        options: ProposeOptions,
    ) -> Result<ProposeResult> {
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

        let proposal = proposals::propose_memory(&self.conn, actor, draft)?;
        if policy == ProposalApprovalPolicy::Manual {
            return Ok(ProposeResult {
                proposal,
                record: None,
                validation: None,
                applied: false,
            });
        }

        let validation = proposals::validate_proposal(&self.conn, &proposal.id)?;
        if !validation.is_valid {
            return Ok(ProposeResult {
                proposal: proposals::load_proposal_public(&self.conn, &proposal.id)?,
                record: None,
                validation: Some(validation),
                applied: false,
            });
        }

        let approved = proposals::approve_proposal(&self.conn, &proposal.id, actor)?;
        if !options.apply {
            return Ok(ProposeResult {
                proposal: approved,
                record: None,
                validation: Some(validation),
                applied: false,
            });
        }

        let record = self.apply_proposal(&proposal.id, actor)?;
        Ok(ProposeResult {
            proposal: proposals::load_proposal_public(&self.conn, &proposal.id)?,
            record: Some(record),
            validation: Some(validation),
            applied: true,
        })
    }

    pub fn list_proposals(&self, filter: ProposalStatusFilter) -> Result<Vec<Proposal>> {
        proposals::list_proposals(&self.conn, filter)
    }

    pub fn show_proposal(&self, proposal_id: &str) -> Result<Proposal> {
        proposals::load_proposal_public(&self.conn, proposal_id)
    }

    pub fn open_proposal_counts(&self) -> Result<BTreeMap<ProposalStatus, usize>> {
        proposals::open_proposal_counts(&self.conn)
    }

    pub fn approve_proposal(&self, proposal_id: &str, actor: &str) -> Result<Proposal> {
        proposals::approve_proposal(&self.conn, proposal_id, actor)
    }

    pub fn reject_proposal(
        &self,
        proposal_id: &str,
        actor: &str,
        reason: &str,
    ) -> Result<Proposal> {
        proposals::reject_proposal(&self.conn, proposal_id, actor, reason)
    }

    pub fn validate_proposal(&self, proposal_id: &str) -> Result<ValidationResult> {
        proposals::validate_proposal(&self.conn, proposal_id)
    }

    pub fn apply_proposal(&self, proposal_id: &str, actor: &str) -> Result<MemoryRecord> {
        let session = CanonicalWriteSession::begin(&self.paths)?;
        let proposal = proposals::load_proposal_public(&self.conn, proposal_id)?;
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
        session.commit(
            RepositoryWriteRoute::DatabaseProposalApply,
            &authorization,
            tx,
            &[write],
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
        let target = record_by_id(&self.conn, record_id)?
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
        let target = record_by_id(&tx, record_id)?
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
        let target = record_by_id(&self.conn, record_id)?
            .with_context(|| format!("memory record not found: {record_id}"))?;
        validate_legacy_canonical_target(&target)?;
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
        let target = record_by_id(&tx, record_id)?
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
        let tags = record_tags(conn, &record.id)?;
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
        search::search_memory_at(&self.conn, input, self.now())
    }

    pub fn inspect_expiry(&self, record_id: &str) -> Result<ExpiryDiagnostic> {
        let record = record_by_id(&self.conn, record_id)?
            .with_context(|| format!("memory record not found: {record_id}"))?;
        expiry::diagnose(record, self.now())
    }

    pub fn repo_index_drift(&self) -> Result<RepoIndexDrift> {
        ensure_safe_directory(
            &self.paths.project_root,
            &self.paths.records_dir(),
            false,
            "canonical record root",
        )?;
        let canonical = okf::read_okf_record_files(self.paths.records_dir())?
            .into_iter()
            .filter(|record| record.status == MemoryStatus::Active)
            .map(|record| (record.concept_id.clone(), record))
            .collect::<BTreeMap<_, _>>();
        let indexed = indexed_active_records_for_destination(&self.conn, MemoryDestination::Repo)?
            .into_iter()
            .map(|record| (record.id.clone(), record))
            .collect::<BTreeMap<_, _>>();

        let missing_from_index = canonical
            .keys()
            .filter(|id| !indexed.contains_key(*id))
            .cloned()
            .collect();
        let stale_in_index = indexed
            .keys()
            .filter(|id| !canonical.contains_key(*id))
            .cloned()
            .collect();
        let changed_in_index = canonical
            .iter()
            .filter_map(|(id, canonical)| {
                indexed
                    .get(id)
                    .filter(|indexed| !repo_record_matches(canonical, indexed))
                    .map(|_| id.clone())
            })
            .collect();
        let fts_out_of_sync = !fts_content_index_is_current(&self.conn)?;

        Ok(RepoIndexDrift {
            missing_from_index,
            stale_in_index,
            changed_in_index,
            fts_out_of_sync,
        })
    }

    pub fn create_local_memory(
        &self,
        actor: &str,
        input: LocalMemoryInput,
    ) -> Result<MemoryRecord> {
        let now = self.now_timestamp()?;
        create_local_memory_with_conn(&self.conn, actor, &input, &now)
    }

    pub fn list_local_memory(&self) -> Result<Vec<MemoryRecord>> {
        active_records_for_destination(&self.conn, MemoryDestination::Local, self.now())
    }

    pub fn search_local_memory(&self, query: String, limit: usize) -> Result<Vec<SearchResult>> {
        search::search_memory_at(
            &self.conn,
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
        let now = self.now_timestamp()?;
        create_checkpoint_with_conn(&self.conn, actor, &input, &now)
    }

    pub fn list_checkpoints(&self) -> Result<Vec<MemoryRecord>> {
        active_checkpoint_records(&self.conn, self.now())
    }

    pub fn show_checkpoint(&self, record_id: &str) -> Result<MemoryRecord> {
        checkpoint_record(&self.conn, record_id, self.now())?
            .with_context(|| format!("checkpoint not found: {record_id}"))
    }

    pub fn promote_session_end(
        &self,
        actor: &str,
        document: SessionEndDocument,
    ) -> Result<SessionEndResult> {
        validate_session_end_document(&document)?;
        let mut unsafe_repo_candidates = BTreeSet::new();
        for (index, candidate) in document.candidates.iter().enumerate() {
            if candidate.destination != MemoryDestination::Repo {
                continue;
            }
            let scope = candidate.scope.as_ref();
            let mut values = vec![
                safety_value(
                    format!("candidate[{index}].title"),
                    SafetyFieldKind::Text,
                    &candidate.title,
                ),
                safety_value(
                    format!("candidate[{index}].body"),
                    SafetyFieldKind::Text,
                    &candidate.body,
                ),
            ];
            if let Some(reason) = candidate.reason.as_deref() {
                values.push(safety_value(
                    format!("candidate[{index}].reason"),
                    SafetyFieldKind::Reason,
                    reason,
                ));
            }
            for (tag_index, tag) in candidate.tags.iter().enumerate() {
                values.push(safety_value(
                    format!("candidate[{index}].tags[{tag_index}]"),
                    SafetyFieldKind::Text,
                    tag,
                ));
            }
            if authorize_repository_projection_batch(
                &self.paths,
                RepositoryWriteRoute::SessionEndPromotion,
                candidate.sensitivity,
                scope.map_or(ScopeKind::Repo, |scope| scope.kind),
                scope.and_then(|scope| scope.id.as_deref()),
                Visibility::Repo,
                AuthorizationProof::ExplicitCommand {
                    operation: "session_end_assessment",
                },
                explicit_repository_provenance(candidate.content_class, &document.task),
                &values,
                &[],
            )
            .is_err()
            {
                unsafe_repo_candidates.insert(index);
            }
        }
        if !unsafe_repo_candidates.is_empty() {
            return Ok(blocked_session_end_result(
                document,
                &unsafe_repo_candidates,
            ));
        }
        let has_repo_writes = document
            .candidates
            .iter()
            .any(|candidate| candidate.destination == MemoryDestination::Repo);
        let _lifecycle_lock = has_repo_writes
            .then(|| RepoLifecycleLock::acquire(&self.paths))
            .transpose()?;
        let proposal_packets = ProposalPacketLifecycle::new(&self.paths, &self.conn);
        let timestamp = self.now_timestamp()?;
        let pending_root = self.paths.proposals_dir().join("pending");
        let mut reserved_proposal_ids = if has_repo_writes {
            proposal_packets.prepare_identity_space()?
        } else {
            BTreeSet::new()
        };
        let mut repo_plans = Vec::with_capacity(document.candidates.len());

        for candidate in &document.candidates {
            if candidate.destination == MemoryDestination::Repo {
                let base_slug = proposals::title_to_concept_slug(&candidate.title)
                    .unwrap_or_else(|| "memory".to_owned());
                let base_id = format!("mem_session_{base_slug}");
                let proposal_id = okf::reserve_okf_proposal_id(
                    &pending_root,
                    &base_id,
                    &mut reserved_proposal_ids,
                )?;
                let draft = session_end_proposal_draft(candidate, actor, &timestamp, proposal_id)?;
                repo_plans.push(Some(okf::plan_okf_create_proposal(&pending_root, &draft)?));
            } else {
                repo_plans.push(None);
            }
        }

        let repo_projections = repo_plans
            .iter()
            .filter_map(Option::as_ref)
            .map(|plan| {
                OwnedRepositoryProjection::from_absolute(
                    &self.paths,
                    &plan.path,
                    plan.markdown.as_bytes(),
                    None,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let mut safety_values = vec![safety_value(
            "session_end.task".to_owned(),
            SafetyFieldKind::SourceReference,
            document.task.as_bytes(),
        )];
        for (index, plan) in repo_plans.iter().filter_map(Option::as_ref).enumerate() {
            safety_values.extend(okf_proposal_safety_values(
                &format!("candidate[{index}]"),
                &plan.parsed,
            ));
        }
        let repo_authorization = has_repo_writes
            .then(|| {
                authorize_repository_projection_batch(
                    &self.paths,
                    RepositoryWriteRoute::SessionEndPromotion,
                    OkfProposalSensitivity::RepoSafe,
                    ScopeKind::Repo,
                    None,
                    Visibility::Repo,
                    AuthorizationProof::ExplicitCommand {
                        operation: "session_end_promotion",
                    },
                    explicit_repository_provenance(
                        RepositoryContentClass::GeneralRepoKnowledge,
                        &document.task,
                    ),
                    &safety_values,
                    &repo_projections,
                )
            })
            .transpose()?;
        if has_repo_writes {
            prepare_pending_proposal_root(&self.paths)?;
        }

        let mut repo_writes = vec![None::<(String, PathBuf)>; document.candidates.len()];
        let mut runtime_writes =
            vec![None::<(String, MemoryDestination)>; document.candidates.len()];
        let mut created_proposal_files = Vec::new();
        let write_result = (|| -> Result<()> {
            if has_repo_writes {
                proposal_packets
                    .ensure_planned_available(repo_plans.iter().filter_map(Option::as_ref))?;
            }
            if let Some(authorization) = repo_authorization.as_ref() {
                let created = create_authorized_repository_batch(
                    &self.paths,
                    RepositoryWriteRoute::SessionEndPromotion,
                    authorization,
                    &repo_projections,
                )?;
                created_proposal_files.extend(created.iter().cloned());
                let mut created = created.into_iter();
                for (index, plan) in repo_plans.iter().enumerate() {
                    let Some(plan) = plan else {
                        continue;
                    };
                    let path = created
                        .next()
                        .context("authorized session-end projection count changed")?;
                    repo_writes[index] = Some((plan.proposal_id.clone(), path));
                }
            }
            let tx = self.conn.unchecked_transaction()?;
            for (index, candidate) in document.candidates.iter().enumerate() {
                match candidate.destination {
                    MemoryDestination::Local => {
                        let record = create_local_memory_with_conn(
                            &tx,
                            actor,
                            &LocalMemoryInput {
                                memory_type: candidate.memory_type,
                                lane: candidate.lane,
                                title: candidate.title.clone(),
                                body: candidate.body.clone(),
                            },
                            &timestamp,
                        )?;
                        runtime_writes[index] = Some((record.id, MemoryDestination::Local));
                    }
                    MemoryDestination::Session => {
                        let record = create_checkpoint_with_conn(
                            &tx,
                            actor,
                            &CheckpointInput {
                                task: candidate.title.clone(),
                                note: candidate.body.clone(),
                            },
                            &timestamp,
                        )?;
                        runtime_writes[index] = Some((record.id, MemoryDestination::Session));
                    }
                    MemoryDestination::Repo
                    | MemoryDestination::Discard
                    | MemoryDestination::NeedsReview => {}
                }
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            if let Err(cleanup_error) = okf::cleanup_okf_proposal_files(&created_proposal_files) {
                return Err(error).context(format!("session-end promotion failed; additionally failed to clean up created proposal files: {cleanup_error}"));
            }
            return Err(error);
        }

        let mut results = Vec::with_capacity(document.candidates.len());
        for (index, candidate) in document.candidates.into_iter().enumerate() {
            let title = candidate.title.trim().to_owned();
            let write = match candidate.destination {
                MemoryDestination::Repo => {
                    let (proposal_id, path) = repo_writes[index]
                        .clone()
                        .context("repo session-end candidate should have a proposal write")?;
                    Some(SessionEndWrite::ProposalFile { proposal_id, path })
                }
                MemoryDestination::Local => {
                    let (record_id, destination) = runtime_writes[index]
                        .clone()
                        .context("local session-end candidate should have a runtime write")?;
                    Some(SessionEndWrite::RuntimeRecord {
                        record_id,
                        destination,
                    })
                }
                MemoryDestination::Session => {
                    let (record_id, destination) = runtime_writes[index]
                        .clone()
                        .context("session-end candidate should have a runtime write")?;
                    Some(SessionEndWrite::RuntimeRecord {
                        record_id,
                        destination,
                    })
                }
                MemoryDestination::Discard | MemoryDestination::NeedsReview => None,
            };
            let status = match candidate.destination {
                MemoryDestination::Discard => SessionEndCandidateStatus::Skipped,
                MemoryDestination::NeedsReview => SessionEndCandidateStatus::Blocked,
                MemoryDestination::Repo | MemoryDestination::Local | MemoryDestination::Session => {
                    SessionEndCandidateStatus::Written
                }
            };
            results.push(SessionEndCandidateResult {
                index,
                destination: candidate.destination,
                memory_type: candidate.memory_type,
                lane: candidate.lane,
                title,
                sensitivity: candidate.sensitivity,
                status,
                reason: match candidate.destination {
                    MemoryDestination::Discard => {
                        Some("discard destination performs no write".to_owned())
                    }
                    MemoryDestination::NeedsReview => {
                        Some("candidate requires human review before writing".to_owned())
                    }
                    MemoryDestination::Repo
                    | MemoryDestination::Local
                    | MemoryDestination::Session => None,
                },
                write,
            });
        }

        Ok(SessionEndResult {
            task: document.task.trim().to_owned(),
            candidates: results,
        })
    }
    pub fn plan_import(&self, actor: &str, document: ImportDocument) -> Result<ImportPlan> {
        if actor.trim().is_empty() {
            bail!("import actor cannot be empty");
        }
        import::validate_document(&document)?;
        let proposal_packets = ProposalPacketLifecycle::new(&self.paths, &self.conn);
        let (inventory, reserved_proposal_ids) = proposal_packets.planning_inventory()?;
        let existing = self.load_import_duplicates(&inventory.pending)?;
        import::build_plan(
            actor,
            &document,
            &existing,
            &self.paths.proposals_dir().join("pending"),
            &reserved_proposal_ids,
        )
    }

    pub fn apply_import(
        &self,
        actor: &str,
        document: ImportDocument,
        expected_plan_id: &str,
    ) -> Result<ImportApplyResult> {
        let has_repo_candidates = document
            .candidates
            .iter()
            .any(|candidate| candidate.destination == MemoryDestination::Repo);
        let _lifecycle_lock = has_repo_candidates
            .then(|| RepoLifecycleLock::acquire(&self.paths))
            .transpose()?;
        let proposal_packets = ProposalPacketLifecycle::new(&self.paths, &self.conn);
        if has_repo_candidates {
            proposal_packets.preflight_pending_root()?;
        }
        let plan = self.plan_import(actor, document.clone())?;
        if plan.plan_id != expected_plan_id {
            bail!(
                "stale import plan: expected {expected_plan_id}, recomputed {}",
                plan.plan_id
            );
        }
        if plan.candidates.iter().any(|candidate| {
            matches!(
                &candidate.action,
                import::ImportCandidateAction::CreateProposal { .. }
            )
        }) {
            proposal_packets.prepare_pending_root()?;
        }
        let timestamp = self.now_timestamp()?;
        let pending_root = self.paths.proposals_dir().join("pending");
        let mut planned = Vec::new();
        for candidate in &plan.candidates {
            let import::ImportCandidateAction::CreateProposal { proposal_id, .. } =
                &candidate.action
            else {
                continue;
            };
            let draft =
                import::proposal_draft(candidate, actor, &timestamp, proposal_id, &plan.sources);
            planned.push((
                candidate.index,
                okf::plan_okf_create_proposal(&pending_root, &draft)?,
            ));
        }
        let repo_projections = planned
            .iter()
            .map(|(_, proposal)| {
                OwnedRepositoryProjection::from_absolute(
                    &self.paths,
                    &proposal.path,
                    proposal.markdown.as_bytes(),
                    None,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let mut safety_values = Vec::new();
        for (index, source) in plan.sources.iter().enumerate() {
            for (name, value) in [
                ("path", source.path.as_deref()),
                ("url", source.url.as_deref()),
                ("ref", source.reference.as_deref()),
            ] {
                if let Some(value) = value {
                    safety_values.push(safety_value(
                        format!("sources[{index}].{name}"),
                        SafetyFieldKind::SourceReference,
                        value,
                    ));
                }
            }
        }
        for (candidate_index, proposal) in &planned {
            safety_values.extend(okf_proposal_safety_values(
                &format!("candidate[{candidate_index}]"),
                &proposal.parsed,
            ));
        }
        let repo_authorization = (!planned.is_empty())
            .then(|| {
                authorize_repository_projection_batch(
                    &self.paths,
                    RepositoryWriteRoute::ImportApply,
                    OkfProposalSensitivity::RepoSafe,
                    ScopeKind::Repo,
                    None,
                    Visibility::Repo,
                    AuthorizationProof::ImportPlan {
                        plan_id: &plan.plan_id,
                    },
                    explicit_repository_provenance(
                        RepositoryContentClass::GeneralRepoKnowledge,
                        &plan.plan_id,
                    ),
                    &safety_values,
                    &repo_projections,
                )
            })
            .transpose()?;
        if !planned.is_empty() {
            prepare_pending_proposal_root(&self.paths)?;
        }
        let mut writes = Vec::new();
        let mut created = Vec::new();
        let result = (|| -> Result<()> {
            if !planned.is_empty() {
                proposal_packets
                    .ensure_planned_available(planned.iter().map(|(_, proposal)| proposal))?;
            }
            let created_paths = match repo_authorization.as_ref() {
                Some(authorization) => create_authorized_repository_batch(
                    &self.paths,
                    RepositoryWriteRoute::ImportApply,
                    authorization,
                    &repo_projections,
                )?,
                None => Vec::new(),
            };
            created.extend(created_paths.iter().cloned());
            for ((candidate_index, proposal), path) in planned.iter().zip(created_paths) {
                let candidate_plan = plan
                    .candidates
                    .iter()
                    .find(|candidate| candidate.index == *candidate_index)
                    .context("import proposal candidate should remain in the plan")?;
                let display_path = match &candidate_plan.action {
                    import::ImportCandidateAction::CreateProposal { path, .. } => path.clone(),
                    _ => bail!("import proposal candidate action changed before apply"),
                };
                writes.push(crate::ImportWrite::ProposalFile {
                    index: *candidate_index,
                    proposal_id: proposal.proposal_id.clone(),
                    path: display_path,
                });
                debug_assert_eq!(path, proposal.path);
            }

            let tx = self.conn.unchecked_transaction()?;
            for candidate in &plan.candidates {
                let import::ImportCandidateAction::CreateRuntime { route } = candidate.action
                else {
                    continue;
                };
                let record = match route {
                    crate::MemoryWriteRoute::RuntimeLocal => create_local_memory_with_conn(
                        &tx,
                        actor,
                        &LocalMemoryInput {
                            memory_type: candidate.memory_type,
                            lane: candidate.lane,
                            title: candidate.title.clone(),
                            body: candidate.body.clone(),
                        },
                        &timestamp,
                    )?,
                    crate::MemoryWriteRoute::RuntimeSession => create_checkpoint_with_conn(
                        &tx,
                        actor,
                        &CheckpointInput {
                            task: candidate.title.clone(),
                            note: candidate.body.clone(),
                        },
                        &timestamp,
                    )?,
                    _ => bail!("import runtime candidate has invalid route {route}"),
                };
                writes.push(crate::ImportWrite::RuntimeRecord {
                    index: candidate.index,
                    record_id: record.id,
                    destination: record.destination,
                });
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = result {
            if let Err(cleanup) = okf::cleanup_okf_proposal_files(&created) {
                return Err(error)
                    .context(format!("import apply failed; cleanup failed: {cleanup}"));
            }
            return Err(error);
        }
        writes.sort_by_key(|write| match write {
            crate::ImportWrite::ProposalFile { index, .. }
            | crate::ImportWrite::RuntimeRecord { index, .. } => *index,
        });
        Ok(ImportApplyResult { plan, writes })
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
        capture_route_apply::CaptureRouteApply::new(&self.paths, &self.conn, self.clock.as_ref())
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

    fn load_import_duplicates(
        &self,
        pending_proposals: &[FileProposalInventoryEntry],
    ) -> Result<Vec<ExistingDuplicate>> {
        let mut entries = Vec::new();
        ensure_safe_directory(
            &self.paths.project_root,
            &self.paths.records_dir(),
            false,
            "canonical record root",
        )?;
        for record in okf::read_okf_record_files(self.paths.records_dir())? {
            entries.push(ExistingDuplicate {
                kind: import::ImportDuplicateKind::CanonicalRecord,
                id: record.concept_id,
                destination: Some(MemoryDestination::Repo),
                hash: import::content_hash(&record.draft.body),
            });
        }
        for entry in pending_proposals {
            entries.push(ExistingDuplicate {
                kind: import::ImportDuplicateKind::PendingProposal,
                id: entry.proposal.id.clone(),
                destination: Some(MemoryDestination::Repo),
                hash: import::content_hash(&entry.proposal.body),
            });
        }
        let runtime_records = records_for_runtime_preservation(&self.conn)?;
        let now = self.now();
        let mut runtime = Vec::new();
        for record in runtime_records {
            if record.status == MemoryStatus::Active
                && !expiry::is_expired(record.expires_at.as_deref(), now)?
            {
                runtime.push(record);
            }
        }
        runtime.sort_by(|a, b| a.id.cmp(&b.id));
        for record in runtime {
            entries.push(ExistingDuplicate {
                kind: import::ImportDuplicateKind::RuntimeRecord,
                id: record.id,
                destination: Some(record.destination),
                hash: import::content_hash(&record.body),
            });
        }
        entries.sort_by(|a, b| (a.kind, a.id.as_str()).cmp(&(b.kind, b.id.as_str())));
        Ok(entries)
    }

    pub fn build_context_pack(&self, input: ContextPackInput) -> Result<ContextPack> {
        context::build_context_pack_at(&self.conn, input, self.now())
    }

    pub fn build_handoff_pack(&self, input: HandoffInput) -> Result<HandoffPack> {
        handoff::build_handoff_pack_at(&self.conn, input, self.now())
    }

    pub fn precheck(&self, input: PrecheckInput) -> Result<Vec<PrecheckWarning>> {
        precheck::precheck_at(&self.conn, input, self.now())
    }

    pub fn export(&self, input: ExportInput) -> Result<ExportResult> {
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
        Self::rebuild_paths_impl(paths, || Ok(()), true)
    }

    #[cfg(test)]
    fn rebuild_paths_with_snapshot_hook(
        paths: MemoryPaths,
        after_snapshot: impl FnOnce() -> Result<()>,
    ) -> Result<RebuildResult> {
        Self::rebuild_paths_impl(paths, after_snapshot, true)
    }

    pub(crate) fn rebuild_paths_for_trusted_recall_eval(
        paths: MemoryPaths,
    ) -> Result<RebuildResult> {
        Self::rebuild_paths_impl(paths, || Ok(()), false)
    }

    fn rebuild_paths_impl(
        paths: MemoryPaths,
        after_snapshot: impl FnOnce() -> Result<()>,
        validate_repository_safety: bool,
    ) -> Result<RebuildResult> {
        let _lifecycle_lock = RepoLifecycleLock::acquire(&paths)?;
        let records_root = paths.records_dir();
        ensure_safe_directory(
            &paths.project_root,
            &records_root,
            false,
            "canonical record root",
        )?;
        let snapshots = okf::read_okf_record_snapshots(&records_root)?;
        after_snapshot()?;
        if validate_repository_safety {
            validate_canonical_record_snapshots_for_rebuild(&paths, &snapshots)?;
        }
        let records = snapshots
            .into_iter()
            .map(|snapshot| snapshot.record)
            .collect::<Vec<_>>();
        guard_no_open_proposals(&paths.db_path)?;
        let runtime_records = load_runtime_records_for_rebuild(&paths.db_path)?;
        guard_no_runtime_record_id_collisions(&records, &runtime_records)?;
        remove_database_files(&paths.db_path)?;
        let conn = db::open_database(&paths.db_path)?;
        db::init_database(&conn)?;
        okf::import_okf_records(&conn, &records)?;
        restore_runtime_records_after_rebuild(&conn, &runtime_records)?;
        Ok(RebuildResult {
            records_root,
            db_path: paths.db_path,
            record_ids: records
                .into_iter()
                .map(|record| record.concept_id)
                .collect(),
        })
    }
}

fn validate_canonical_record_snapshots_for_rebuild(
    paths: &MemoryPaths,
    snapshots: &[okf::OkfRecordSnapshot],
) -> Result<()> {
    for snapshot in snapshots {
        let relative = snapshot
            .path
            .strip_prefix(&paths.project_root)
            .context("canonical record escaped the project root during rebuild")?;
        let report = crate::scan_managed_repository_blob(
            paths.project_root.as_os_str().as_encoded_bytes(),
            relative,
            &snapshot.bytes,
        );
        if !report.allowed {
            let findings = report
                .findings
                .iter()
                .map(|finding| format!("{}:{}", finding.code.as_str(), finding.fingerprint))
                .collect::<Vec<_>>()
                .join(",");
            bail!(
                "rebuild refused because a canonical record failed repository safety validation ({findings})"
            );
        }
    }
    Ok(())
}

fn memory_draft_safety_values(prefix: &str, draft: &MemoryDraft) -> Vec<RepositorySafetyValue> {
    let mut values = vec![
        safety_value(
            format!("{prefix}.title"),
            SafetyFieldKind::Text,
            &draft.title,
        ),
        safety_value(format!("{prefix}.body"), SafetyFieldKind::Text, &draft.body),
        safety_value(
            format!("{prefix}.content_class"),
            SafetyFieldKind::Identifier,
            draft.content_class.as_str(),
        ),
    ];
    for (index, tag) in draft.tags.iter().enumerate() {
        values.push(safety_value(
            format!("{prefix}.tags[{index}]"),
            SafetyFieldKind::Text,
            tag,
        ));
    }
    if let Some(scope_id) = draft.scope_id.as_deref() {
        values.push(safety_value(
            format!("{prefix}.scope_id"),
            SafetyFieldKind::Identifier,
            scope_id,
        ));
    }
    if let Some(source_kind) = draft.source_kind.as_deref() {
        values.push(safety_value(
            format!("{prefix}.source_kind"),
            SafetyFieldKind::SourceReference,
            source_kind,
        ));
    }
    if let Some(source_ref) = draft.source_ref.as_deref() {
        values.push(safety_value(
            format!("{prefix}.source_ref"),
            SafetyFieldKind::SourceReference,
            source_ref,
        ));
    }
    values
}

fn okf_proposal_safety_values(
    prefix: &str,
    proposal: &OkfProposalFile,
) -> Vec<RepositorySafetyValue> {
    let mut values = vec![
        safety_value(
            format!("{prefix}.id"),
            SafetyFieldKind::Identifier,
            &proposal.id,
        ),
        safety_value(
            format!("{prefix}.file_id"),
            SafetyFieldKind::Identifier,
            &proposal.file_id,
        ),
        safety_value(
            format!("{prefix}.title"),
            SafetyFieldKind::Text,
            &proposal.title,
        ),
        safety_value(
            format!("{prefix}.description"),
            SafetyFieldKind::Text,
            &proposal.description,
        ),
        safety_value(
            format!("{prefix}.body"),
            SafetyFieldKind::Text,
            &proposal.body,
        ),
        safety_value(
            format!("{prefix}.proposed_by"),
            SafetyFieldKind::Identifier,
            &proposal.proposal.proposed_by,
        ),
        safety_value(
            format!("{prefix}.content_class"),
            SafetyFieldKind::Identifier,
            proposal.content_class.as_str(),
        ),
    ];
    if let Some(reason) = proposal.proposal.reason.as_deref() {
        values.push(safety_value(
            format!("{prefix}.reason"),
            SafetyFieldKind::Reason,
            reason,
        ));
    }
    if let Some(target) = proposal.proposal.target.as_deref() {
        values.push(safety_value(
            format!("{prefix}.target"),
            SafetyFieldKind::Identifier,
            target,
        ));
    }
    if let Some(scope_id) = proposal.scope_id.as_deref() {
        values.push(safety_value(
            format!("{prefix}.scope_id"),
            SafetyFieldKind::Identifier,
            scope_id,
        ));
    }
    for (index, path) in proposal.applies_to.iter().enumerate() {
        values.push(safety_value(
            format!("{prefix}.applies_to[{index}]"),
            SafetyFieldKind::Path,
            path,
        ));
    }
    for (index, tag) in proposal.tags.iter().enumerate() {
        values.push(safety_value(
            format!("{prefix}.tags[{index}]"),
            SafetyFieldKind::Text,
            tag,
        ));
    }
    for (index, source) in proposal.sources.iter().enumerate() {
        for (name, value) in [
            ("path", source.path.as_deref()),
            ("url", source.url.as_deref()),
            ("ref", source.reference.as_deref()),
        ] {
            if let Some(value) = value {
                values.push(safety_value(
                    format!("{prefix}.sources[{index}].{name}"),
                    SafetyFieldKind::SourceReference,
                    value,
                ));
            }
        }
    }
    values
}

fn safety_value(
    location: String,
    kind: SafetyFieldKind,
    value: impl AsRef<[u8]>,
) -> RepositorySafetyValue {
    RepositorySafetyValue {
        location,
        kind,
        value: value.as_ref().to_vec(),
    }
}

fn borrowed_repository_projections(
    projections: &[OwnedRepositoryProjection],
) -> Vec<RepositoryProjection<'_>> {
    projections
        .iter()
        .map(|projection| RepositoryProjection {
            path: &projection.relative_path,
            bytes: &projection.bytes,
            target_revision: projection.target_revision.as_deref(),
            purpose: projection.purpose,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn authorize_repository_projection_batch(
    paths: &MemoryPaths,
    route: RepositoryWriteRoute,
    sensitivity: OkfProposalSensitivity,
    scope_kind: ScopeKind,
    scope_id: Option<&str>,
    visibility: Visibility,
    authorization: AuthorizationProof<'_>,
    provenance: ProvenanceAssessment<'_>,
    values: &[RepositorySafetyValue],
    projections: &[OwnedRepositoryProjection],
) -> Result<AuthorizedRepositoryProjectionBatch> {
    let fields = values
        .iter()
        .map(|value| SafetyField {
            location: &value.location,
            kind: value.kind,
            value: &value.value,
        })
        .collect();
    let projections = borrowed_repository_projections(projections);
    let request = RepositoryWriteRequest {
        route,
        destination: MemoryDestination::Repo,
        sensitivity,
        scope: RepositoryScope {
            kind: scope_kind,
            id: scope_id,
            current_project_identity: paths.project_root.as_os_str().as_encoded_bytes(),
            configured_project_id: None,
        },
        visibility,
        authorization,
        freshness: Vec::new(),
        provenance,
        fields,
        projections,
    };
    let policy_context_digest = repository_write_policy_context_digest(&request);
    let capability = authorize_repository_write(&request).map_err(anyhow::Error::new)?;
    Ok(AuthorizedRepositoryProjectionBatch {
        capability,
        policy_context_digest,
    })
}

fn create_authorized_repository_batch(
    paths: &MemoryPaths,
    expected_route: RepositoryWriteRoute,
    authorization: &AuthorizedRepositoryProjectionBatch,
    projections: &[OwnedRepositoryProjection],
) -> Result<Vec<PathBuf>> {
    let borrowed = borrowed_repository_projections(projections);
    repository_io::create_repository_batch(
        &paths.project_root,
        expected_route,
        &authorization.policy_context_digest,
        &authorization.capability,
        &borrowed,
    )
}

fn explicit_repository_provenance<'a>(
    content_class: RepositoryContentClass,
    source_identity: &'a str,
) -> ProvenanceAssessment<'a> {
    let valid = !source_identity.trim().is_empty();
    ProvenanceAssessment {
        present: valid,
        evidence_valid: valid,
        content_class,
        source_identity: valid.then_some(source_identity),
    }
}

fn canonical_write_projections(
    paths: &MemoryPaths,
    writes: &[CanonicalFileWrite],
) -> Result<Vec<OwnedRepositoryProjection>> {
    let mut projections = Vec::with_capacity(writes.len() * 2);
    for write in writes {
        projections.push(OwnedRepositoryProjection::from_absolute(
            paths,
            &write.path,
            write.markdown.as_bytes(),
            write.expected_existing_hash.as_deref(),
        )?);
        if let Some(expected_revision) = write.expected_existing_hash.as_deref() {
            let existing_bytes = fs::read(&write.path)
                .context("failed to snapshot existing repository projection bytes")?;
            projections.push(OwnedRepositoryProjection::existing_from_absolute(
                paths,
                &write.path,
                &existing_bytes,
                expected_revision,
            )?);
        }
    }
    Ok(projections)
}

fn blocked_session_end_result(
    document: SessionEndDocument,
    unsafe_repo_candidates: &BTreeSet<usize>,
) -> SessionEndResult {
    let candidates = document
        .candidates
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| {
            let unsafe_repo_candidate = unsafe_repo_candidates.contains(&index);
            let (title, status, reason) = if unsafe_repo_candidate {
                (
                    if candidate.sensitivity != OkfProposalSensitivity::RepoSafe {
                        "Redacted non-repo-safe candidate".to_owned()
                    } else {
                        "Redacted unsafe repository candidate".to_owned()
                    },
                    SessionEndCandidateStatus::Blocked,
                    Some(if candidate.sensitivity != OkfProposalSensitivity::RepoSafe {
                        repo_sensitivity_block_reason(candidate.sensitivity)
                    } else {
                        "repository safety policy blocked this candidate; inspect the hash-only safety report and sanitize the source before retrying".to_owned()
                    }),
                )
            } else {
                let status = match candidate.destination {
                    MemoryDestination::Discard => SessionEndCandidateStatus::Skipped,
                    MemoryDestination::Repo
                    | MemoryDestination::Local
                    | MemoryDestination::Session
                    | MemoryDestination::NeedsReview => SessionEndCandidateStatus::Blocked,
                };
                let reason = match candidate.destination {
                    MemoryDestination::Discard => {
                        "discard destination performs no write".to_owned()
                    }
                    MemoryDestination::NeedsReview => {
                        "candidate requires human review before writing".to_owned()
                    }
                    MemoryDestination::Repo
                    | MemoryDestination::Local
                    | MemoryDestination::Session => "session-end batch contains a non-repo-safe repo candidate; no writes were performed".to_owned(),
                };
                (candidate.title.trim().to_owned(), status, Some(reason))
            };
            SessionEndCandidateResult {
                index,
                destination: candidate.destination,
                memory_type: candidate.memory_type,
                lane: candidate.lane,
                title,
                sensitivity: candidate.sensitivity,
                status,
                reason,
                write: None,
            }
        })
        .collect();
    SessionEndResult {
        task: document.task.trim().to_owned(),
        candidates,
    }
}

pub fn lifecycle_transaction_artifact_count(paths: &MemoryPaths) -> Result<usize> {
    Ok(lifecycle_transaction_artifacts(paths)?.len())
}

fn repo_record_matches(canonical: &okf::OkfRecordFile, indexed: &MemoryRecord) -> bool {
    let draft = &canonical.draft;
    indexed.memory_type == draft.memory_type
        && indexed.lane == draft.lane
        && indexed.destination == MemoryDestination::Repo
        && indexed.scope_kind == draft.scope_kind
        && indexed.scope_id == draft.scope_id
        && indexed.visibility == draft.visibility
        && indexed.title == draft.title
        && indexed.body == draft.body
        && indexed.status == canonical.status
        && indexed.confidence == draft.confidence
        && indexed.source_kind == draft.source_kind
        && indexed.source_ref == draft.source_ref
        && indexed.proposal_id == canonical.proposal_id
        && indexed.content_hash == blake3::hash(draft.body.as_bytes()).to_hex().to_string()
        && indexed.created_at == canonical.created
        && indexed.updated_at == canonical.updated.as_deref().unwrap_or(&canonical.created)
        && indexed.supersedes_id == canonical.supersedes_id
        && indexed.expires_at == canonical.expires_at
}

fn fts_content_index_is_current(conn: &Connection) -> Result<bool> {
    match conn.execute(
        "INSERT INTO memory_fts(memory_fts, rank) VALUES ('integrity-check', 1)",
        [],
    ) {
        Ok(_) => Ok(true),
        Err(error) if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseCorrupt) => {
            Ok(false)
        }
        Err(error) => Err(error).context("failed to verify full-text index integrity"),
    }
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
    fs::create_dir_all(&paths.runtime_dir).with_context(|| {
        format!(
            "failed to create runtime directory {}",
            paths.runtime_dir.display()
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

    let conn = db::open_database(&paths.db_path)?;
    db::init_database(&conn)?;

    Ok(InitBundleResult {
        project_root: paths.project_root.clone(),
        memory_dir: paths.memory_dir.clone(),
        runtime_dir: paths.runtime_dir.clone(),
        config_path: paths.config_path.clone(),
        db_path: paths.db_path.clone(),
        exports_dir: paths.exports_dir.clone(),
    })
}

fn record_tags(conn: &Connection, record_id: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT tag FROM memory_tag WHERE record_id = ?1 ORDER BY tag ASC")?;
    let rows = stmt.query_map([record_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InsertMode {
    Create,
    RestoreIfAbsent,
}

fn create_local_memory_with_conn(
    conn: &Connection,
    actor: &str,
    input: &LocalMemoryInput,
    now: &str,
) -> Result<MemoryRecord> {
    validate_local_memory_input(input)?;
    let id = next_prefixed_record_id(conn, "local", &input.title)?;
    let body = input.body.trim().to_owned();
    let record = MemoryRecord {
        id,
        memory_type: input.memory_type,
        lane: input.lane,
        destination: MemoryDestination::Local,
        scope_kind: ScopeKind::Personal,
        scope_id: None,
        visibility: Visibility::Private,
        title: input.title.trim().to_owned(),
        body,
        status: MemoryStatus::Active,
        confidence: 1.0,
        source_kind: Some("memzoi-local".to_owned()),
        source_ref: None,
        proposal_id: None,
        capture: None,
        content_hash: blake3::hash(input.body.trim().as_bytes())
            .to_hex()
            .to_string(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
        supersedes_id: None,
        expires_at: None,
    };
    insert_memory_record_row(conn, &record, InsertMode::Create)?;
    append_event(
        conn,
        AppendEvent {
            event_type: "memory.local_created".to_owned(),
            actor: actor.to_owned(),
            payload: json!({
                "record_id": &record.id,
                "destination": record.destination.as_str(),
                "title": &record.title,
            }),
            record_id: Some(record.id.clone()),
            proposal_id: None,
        },
    )?;
    Ok(record)
}

fn create_checkpoint_with_conn(
    conn: &Connection,
    actor: &str,
    input: &CheckpointInput,
    now: &str,
) -> Result<MemoryRecord> {
    validate_checkpoint_input(input)?;
    let id = next_prefixed_record_id(conn, "session", &input.task)?;
    let body = input.note.trim().to_owned();
    let record = MemoryRecord {
        id,
        memory_type: MemoryType::Episode,
        lane: MemoryLane::Session,
        destination: MemoryDestination::Session,
        scope_kind: ScopeKind::Personal,
        scope_id: None,
        visibility: Visibility::Private,
        title: input.task.trim().to_owned(),
        body,
        status: MemoryStatus::Active,
        confidence: 1.0,
        source_kind: Some("memzoi-checkpoint".to_owned()),
        source_ref: None,
        proposal_id: None,
        capture: None,
        content_hash: blake3::hash(input.note.trim().as_bytes())
            .to_hex()
            .to_string(),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
        supersedes_id: None,
        expires_at: None,
    };
    insert_memory_record_row(conn, &record, InsertMode::Create)?;
    append_event(
        conn,
        AppendEvent {
            event_type: "memory.checkpoint_created".to_owned(),
            actor: actor.to_owned(),
            payload: json!({
                "record_id": &record.id,
                "destination": record.destination.as_str(),
                "title": &record.title,
            }),
            record_id: Some(record.id.clone()),
            proposal_id: None,
        },
    )?;
    Ok(record)
}

fn validate_local_memory_input(input: &LocalMemoryInput) -> Result<()> {
    if input.title.trim().is_empty() {
        bail!("title is required");
    }
    if input.body.trim().is_empty() {
        bail!("body is required");
    }
    Ok(())
}

fn validate_checkpoint_input(input: &CheckpointInput) -> Result<()> {
    if input.task.trim().is_empty() {
        bail!("task is required");
    }
    if input.note.trim().is_empty() {
        bail!("note is required");
    }
    Ok(())
}

fn next_prefixed_record_id(conn: &Connection, prefix: &str, title: &str) -> Result<String> {
    let slug = proposals::title_to_concept_slug(title)
        .unwrap_or_else(|| format!("memory-{}", Uuid::now_v7()));
    let base = format!("{prefix}-{slug}");
    if !record_id_exists(conn, &base)? {
        return Ok(base);
    }
    for suffix in 2.. {
        let candidate = format!("{base}-{suffix}");
        if !record_id_exists(conn, &candidate)? {
            return Ok(candidate);
        }
    }
    unreachable!("unbounded suffix search returns")
}

fn record_id_exists(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_record WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?)
}

fn insert_memory_record_row(
    conn: &Connection,
    record: &MemoryRecord,
    mode: InsertMode,
) -> Result<()> {
    let verb = match mode {
        InsertMode::Create => "INSERT INTO",
        InsertMode::RestoreIfAbsent => "INSERT OR IGNORE INTO",
    };
    let sql = format!(
        "{verb} memory_record (
          id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
          confidence, source_kind, source_ref, proposal_id, content_hash, created_at, updated_at,
          supersedes_id, expires_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)"
    );
    conn.execute(
        &sql,
        rusqlite::params![
            &record.id,
            record.memory_type.as_str(),
            record.lane.as_str(),
            record.destination.as_str(),
            record.scope_kind.as_str(),
            &record.scope_id,
            record.visibility.as_str(),
            &record.title,
            &record.body,
            record.status.as_str(),
            record.confidence,
            &record.source_kind,
            &record.source_ref,
            &record.proposal_id,
            &record.content_hash,
            &record.created_at,
            &record.updated_at,
            &record.supersedes_id,
            &record.expires_at,
        ],
    )?;
    crate::capture::store_capture_provenance(conn, &record.id, record.capture.as_ref())?;
    Ok(())
}

fn indexed_active_records_for_destination(
    conn: &Connection,
    destination: MemoryDestination,
) -> Result<Vec<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
                confidence, source_kind, source_ref, content_hash, created_at, updated_at,
                supersedes_id, expires_at, proposal_id
         FROM memory_record
         WHERE status = 'active'
           AND destination = ?1
         ORDER BY updated_at DESC, id ASC",
    )?;
    let rows = stmt.query_map([destination.as_str()], search::record_from_row)?;
    let mut records = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    load_capture_provenance_for_records(conn, &mut records)?;
    Ok(records)
}

fn active_records_for_destination(
    conn: &Connection,
    destination: MemoryDestination,
    now: OffsetDateTime,
) -> Result<Vec<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
                confidence, source_kind, source_ref, content_hash, created_at, updated_at,
                supersedes_id, expires_at, proposal_id
         FROM memory_record
         WHERE status = 'active'
           AND destination = ?1
           AND memzoi_is_expired(expires_at, ?2) = 0
         ORDER BY updated_at DESC, id ASC",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![destination.as_str(), expiry::format_timestamp(now)?],
        search::record_from_row,
    )?;
    let mut records = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    load_capture_provenance_for_records(conn, &mut records)?;
    Ok(records)
}

fn active_checkpoint_records(conn: &Connection, now: OffsetDateTime) -> Result<Vec<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
                confidence, source_kind, source_ref, content_hash, created_at, updated_at,
                supersedes_id, expires_at, proposal_id
         FROM memory_record
         WHERE status = 'active'
           AND destination = 'session'
           AND source_kind = 'memzoi-checkpoint'
           AND memzoi_is_expired(expires_at, ?1) = 0
         ORDER BY created_at DESC, id ASC",
    )?;
    let rows = stmt.query_map([expiry::format_timestamp(now)?], search::record_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn checkpoint_record(
    conn: &Connection,
    record_id: &str,
    now: OffsetDateTime,
) -> Result<Option<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
                confidence, source_kind, source_ref, content_hash, created_at, updated_at,
                supersedes_id, expires_at, proposal_id
         FROM memory_record
         WHERE id = ?1
           AND status = 'active'
           AND destination = 'session'
           AND source_kind = 'memzoi-checkpoint'
           AND memzoi_is_expired(expires_at, ?2) = 0",
    )?;
    stmt.query_row(
        rusqlite::params![record_id, expiry::format_timestamp(now)?],
        search::record_from_row,
    )
    .optional()
    .map_err(Into::into)
}

fn record_by_id(conn: &Connection, record_id: &str) -> Result<Option<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
                confidence, source_kind, source_ref, content_hash, created_at, updated_at,
                supersedes_id, expires_at, proposal_id
         FROM memory_record
         WHERE id = ?1",
    )?;
    let mut record = stmt
        .query_row([record_id], search::record_from_row)
        .optional()
        .map_err(anyhow::Error::from)?;
    drop(stmt);
    if let Some(record) = &mut record {
        record.capture = crate::capture::load_capture_provenance(conn, &record.id)?;
    }
    Ok(record)
}

fn load_capture_provenance_for_records(
    conn: &Connection,
    records: &mut [MemoryRecord],
) -> Result<()> {
    for record in records {
        record.capture = crate::capture::load_capture_provenance(conn, &record.id)?;
    }
    Ok(())
}

fn records_for_runtime_preservation(conn: &Connection) -> Result<Vec<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
                confidence, source_kind, source_ref, content_hash, created_at, updated_at,
                supersedes_id, expires_at, proposal_id
         FROM memory_record
         WHERE destination IN ('local', 'session')
         ORDER BY updated_at DESC, id ASC",
    )?;
    let rows = stmt.query_map([], search::record_from_row)?;
    let mut records = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    load_capture_provenance_for_records(conn, &mut records)?;
    Ok(records)
}

#[derive(Debug, Clone)]
struct RuntimeRecordPreservation {
    record: MemoryRecord,
    tags: Vec<String>,
    paths: Vec<MemoryPath>,
}

fn runtime_records_for_rebuild_preservation(
    conn: &Connection,
) -> Result<Vec<RuntimeRecordPreservation>> {
    records_for_runtime_preservation(conn)?
        .into_iter()
        .map(|record| {
            let tags = record_tags(conn, &record.id)?;
            let paths = runtime_record_paths(conn, &record.id)?;
            Ok(RuntimeRecordPreservation {
                record,
                tags,
                paths,
            })
        })
        .collect()
}

fn runtime_record_paths(conn: &Connection, record_id: &str) -> Result<Vec<MemoryPath>> {
    let mut stmt = conn.prepare(
        "SELECT path, symbol, line_start, line_end
         FROM memory_path
         WHERE record_id = ?1
         ORDER BY path ASC, COALESCE(symbol, '') ASC, COALESCE(line_start, 0) ASC",
    )?;
    let rows = stmt.query_map([record_id], |row| {
        Ok(MemoryPath {
            path: row.get(0)?,
            symbol: row.get(1)?,
            line_start: row.get(2)?,
            line_end: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn load_runtime_records_for_rebuild(db_path: &Path) -> Result<Vec<RuntimeRecordPreservation>> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let conn = db::open_database(db_path).with_context(|| {
        format!(
            "rebuild refused because local/session runtime memory could not be preserved from {}",
            db_path.display()
        )
    })?;
    db::init_database(&conn).with_context(|| {
        format!(
            "rebuild refused because local/session runtime memory could not be migrated before preservation from {}",
            db_path.display()
        )
    })?;
    runtime_records_for_rebuild_preservation(&conn).context(
        "rebuild refused because local/session runtime memory could not be loaded for preservation",
    )
}

fn guard_no_runtime_record_id_collisions(
    records: &[okf::OkfRecordFile],
    runtime_records: &[RuntimeRecordPreservation],
) -> Result<()> {
    if runtime_records.is_empty() {
        return Ok(());
    }

    let repo_ids = records
        .iter()
        .map(|record| record.concept_id.as_str())
        .collect::<BTreeSet<_>>();
    let collisions = runtime_records
        .iter()
        .filter_map(|snapshot| {
            let record = &snapshot.record;
            repo_ids
                .contains(record.id.as_str())
                .then_some(record.id.as_str())
        })
        .collect::<Vec<_>>();
    if collisions.is_empty() {
        return Ok(());
    }

    bail!(
        "rebuild refused because local/session runtime memory record id{} would collide with canonical repo record{}: {}",
        if collisions.len() == 1 { "" } else { "s" },
        if collisions.len() == 1 { "" } else { "s" },
        collisions.join(", ")
    );
}

fn restore_runtime_records_after_rebuild(
    conn: &Connection,
    records: &[RuntimeRecordPreservation],
) -> Result<()> {
    for snapshot in records {
        let record = &snapshot.record;
        insert_memory_record_row(conn, record, InsertMode::RestoreIfAbsent)?;
        for tag in &snapshot.tags {
            conn.execute(
                "INSERT OR IGNORE INTO memory_tag(record_id, tag) VALUES (?1, ?2)",
                rusqlite::params![record.id, tag],
            )?;
        }
        for (index, path) in snapshot.paths.iter().enumerate() {
            conn.execute(
                "INSERT INTO memory_path(id, record_id, path, symbol, line_start, line_end)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    format!("{}_restored_path_{index}", record.id),
                    record.id,
                    path.path,
                    path.symbol,
                    path.line_start,
                    path.line_end,
                ],
            )?;
        }
    }
    Ok(())
}

fn guard_no_open_proposals(db_path: &Path) -> Result<()> {
    if !db_path.exists() {
        return Ok(());
    }

    let Ok(open_proposals) = open_proposal_summaries(db_path) else {
        return Ok(());
    };
    if !open_proposals.is_empty() {
        let count = open_proposals.len();
        let summaries = open_proposals
            .into_iter()
            .map(|(id, status)| format!("{id} ({status})"))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "rebuild refused because {count} open proposal{} would be discarded: {summaries}. Run `memzoi proposals list --status open`, `memzoi proposals apply --all-approved`, or `memzoi reject <proposal-id> --reason \"...\"` before rebuilding.",
            if count == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

fn open_proposal_summaries(db_path: &Path) -> rusqlite::Result<Vec<(String, String)>> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let has_proposal_table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'proposal')",
        [],
        |row| row.get(0),
    )?;
    if !has_proposal_table {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, status
         FROM proposal
         WHERE status IN ('pending', 'validated', 'approved')
         ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

fn remove_database_files(db_path: &Path) -> Result<()> {
    for path in [
        db_path.to_path_buf(),
        db_path.with_extension("db-wal"),
        db_path.with_extension("db-shm"),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to remove derived database file {}", path.display())
                });
            }
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{MemoryLane, MemoryStatus, MemoryType, ProposalStatus, ScopeKind, Visibility};
    use tempfile::TempDir;

    #[test]
    fn propose_with_options_auto_approves_unique_proposals_by_default() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;

        let result = service.propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft("Default auto proposal", "Unique default auto proposal body"),
            ProposeOptions {
                approval_override: None,
                apply: false,
            },
        )?;

        assert_eq!(result.proposal.status, ProposalStatus::Approved);
        assert_eq!(
            result
                .validation
                .as_ref()
                .map(|validation| validation.is_valid),
            Some(true)
        );
        assert_eq!(result.record, None);
        assert!(!result.applied);
        let approved =
            service.list_proposals(ProposalStatusFilter::Status(ProposalStatus::Approved))?;
        assert_eq!(
            approved
                .iter()
                .map(|proposal| proposal.id.as_str())
                .collect::<Vec<_>>(),
            vec![result.proposal.id.as_str()]
        );

        Ok(())
    }

    #[test]
    fn auto_approval_and_apply_cannot_bypass_unknown_sensitivity() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let mut draft = sample_memory_draft(
            "Unknown sensitivity proposal",
            "Content must remain outside canonical memory until classified.",
        );
        draft.sensitivity = crate::OkfProposalSensitivity::Unknown;

        let result = service.propose_memory_with_options(
            "agent:red-tests",
            draft,
            ProposeOptions {
                approval_override: Some(ProposalApprovalOverride::Auto),
                apply: true,
            },
        )?;

        assert_eq!(result.proposal.status, ProposalStatus::Pending);
        assert!(!result.applied);
        assert_eq!(result.record, None);
        assert!(result.validation.as_ref().is_some_and(|validation| {
            validation
                .issues
                .iter()
                .any(|issue| issue.code == "repo_sensitivity_required")
        }));
        let record_count: i64 =
            service
                .conn
                .query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))?;
        assert_eq!(record_count, 0);

        Ok(())
    }

    #[test]
    fn database_apply_route_blocks_every_prohibited_content_class() -> anyhow::Result<()> {
        use crate::{RepositoryWriteBlocked, RepositoryWriteSafetyReasonCode};

        let cases = [
            (
                RepositoryContentClass::RawTranscript,
                RepositoryWriteSafetyReasonCode::RawTranscript,
            ),
            (
                RepositoryContentClass::PrivatePersonalData,
                RepositoryWriteSafetyReasonCode::PrivatePersonalData,
            ),
            (
                RepositoryContentClass::ScreenOrActivityHistory,
                RepositoryWriteSafetyReasonCode::ActivityHistory,
            ),
            (
                RepositoryContentClass::PrivateEndpoint,
                RepositoryWriteSafetyReasonCode::PrivateEndpoint,
            ),
            (
                RepositoryContentClass::UndisclosedVulnerability,
                RepositoryWriteSafetyReasonCode::UndisclosedVulnerability,
            ),
            (
                RepositoryContentClass::UnminimizedPrivateEvidence,
                RepositoryWriteSafetyReasonCode::PrivateEvidenceUnminimized,
            ),
            (
                RepositoryContentClass::TemporaryTaskState,
                RepositoryWriteSafetyReasonCode::TemporaryTaskState,
            ),
            (
                RepositoryContentClass::LocalOnlyState,
                RepositoryWriteSafetyReasonCode::LocalOnlyState,
            ),
            (
                RepositoryContentClass::Unknown,
                RepositoryWriteSafetyReasonCode::UnknownContentClass,
            ),
        ];

        for (index, (content_class, expected_code)) in cases.into_iter().enumerate() {
            let (_temp, service) = initialized_service()?;
            let mut draft = sample_memory_draft(
                &format!("Contextual policy case {index}"),
                "Lexically harmless content must still honor its contextual classification.",
            );
            draft.content_class = content_class;
            let proposal = service.propose_memory("agent:red-tests", draft)?;
            service.validate_proposal(&proposal.id)?;
            service.approve_proposal(&proposal.id, "reviewer:human")?;

            let error = service
                .apply_proposal(&proposal.id, "agent:applier")
                .expect_err("prohibited contextual class must fail at the production route");
            let blocked = error
                .downcast_ref::<RepositoryWriteBlocked>()
                .expect("apply error should retain the structured safety report");
            assert!(
                blocked
                    .report()
                    .findings
                    .iter()
                    .any(|finding| finding.code == expected_code),
                "missing {expected_code:?} for {content_class:?}: {:?}",
                blocked.report().findings
            );
            let record_count: i64 =
                service
                    .conn
                    .query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))?;
            assert_eq!(record_count, 0);
        }

        Ok(())
    }

    #[test]
    fn database_apply_route_fails_closed_for_unconfigured_project_scope() -> anyhow::Result<()> {
        use crate::{RepositoryWriteBlocked, RepositoryWriteSafetyReasonCode};

        let (_temp, service) = initialized_service()?;
        let mut draft = sample_memory_draft(
            "Untrusted project identity",
            "A candidate cannot attest that its own arbitrary project ID is current.",
        );
        draft.scope_kind = ScopeKind::Project;
        draft.scope_id = Some("candidate-controlled-project".to_owned());
        let proposal = service.propose_memory("agent:red-tests", draft)?;
        service.validate_proposal(&proposal.id)?;
        service.approve_proposal(&proposal.id, "reviewer:human")?;

        let error = service
            .apply_proposal(&proposal.id, "agent:applier")
            .expect_err("project scope must fail without an independent configured identity");
        let blocked = error
            .downcast_ref::<RepositoryWriteBlocked>()
            .expect("apply error should retain the structured safety report");
        assert!(blocked.report().findings.iter().any(|finding| {
            finding.code == RepositoryWriteSafetyReasonCode::ScopeProjectMismatch
        }));
        assert!(
            !service
                .paths
                .records_dir()
                .join(format!("{}.md", proposal.id))
                .exists()
        );

        Ok(())
    }

    #[test]
    fn database_apply_route_fails_closed_for_unconfigured_team_scope() -> anyhow::Result<()> {
        use crate::{RepositoryWriteBlocked, RepositoryWriteSafetyReasonCode};

        let (_temp, service) = initialized_service()?;
        let mut draft = sample_memory_draft(
            "Untrusted team identity",
            "A candidate cannot attest that an arbitrary team ID owns this repository.",
        );
        draft.scope_kind = ScopeKind::Team;
        draft.scope_id = Some("candidate-controlled-team".to_owned());
        let proposal = service.propose_memory("agent:red-tests", draft)?;
        service.validate_proposal(&proposal.id)?;
        service.approve_proposal(&proposal.id, "reviewer:human")?;

        let error = service
            .apply_proposal(&proposal.id, "agent:applier")
            .expect_err("team scope must fail without an independent configured identity");
        let blocked = error
            .downcast_ref::<RepositoryWriteBlocked>()
            .expect("apply error should retain the structured safety report");
        assert!(blocked.report().findings.iter().any(|finding| {
            finding.code == RepositoryWriteSafetyReasonCode::ScopeNotRepository
        }));
        assert!(
            !service
                .paths
                .records_dir()
                .join(format!("{}.md", proposal.id))
                .exists()
        );

        Ok(())
    }

    #[test]
    fn propose_with_options_manual_override_leaves_proposal_pending() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;

        let result = service.propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft("Manual proposal", "Manual proposal body"),
            ProposeOptions {
                approval_override: Some(ProposalApprovalOverride::Manual),
                apply: false,
            },
        )?;

        assert_eq!(result.proposal.status, ProposalStatus::Pending);
        assert_eq!(result.validation, None);
        assert_eq!(result.record, None);
        assert!(!result.applied);
        let counts = service.open_proposal_counts()?;
        assert_eq!(counts.get(&ProposalStatus::Pending), Some(&1));

        Ok(())
    }

    #[test]
    fn propose_with_options_apply_writes_canonical_record_file() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;

        let result = service.propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft("Applied proposal", "Applied proposal body"),
            ProposeOptions {
                approval_override: None,
                apply: true,
            },
        )?;

        let record = result
            .record
            .as_ref()
            .expect("apply mode should return the canonical record");
        assert!(result.applied);
        assert_eq!(result.proposal.status, ProposalStatus::Applied);
        assert_eq!(record.status, MemoryStatus::Active);
        assert_eq!(record.title, "Applied proposal");
        assert_eq!(record.source_ref.as_deref(), Some("service-proposal-tests"));
        assert_eq!(record.source_kind.as_deref(), Some("test"));
        assert_eq!(
            record.proposal_id.as_deref(),
            Some(result.proposal.id.as_str())
        );

        let mut applied_event_proposal_ids = Vec::new();
        service.for_each_event(|event| {
            if event.event_type == "memory.applied" {
                applied_event_proposal_ids.push(event.proposal_id);
            }
            Ok(())
        })?;
        assert_eq!(
            applied_event_proposal_ids,
            vec![Some(result.proposal.id.clone())]
        );

        let record_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", record.id));
        let canonical = fs::read_to_string(&record_path)?;
        assert!(
            canonical.contains("status: active\n"),
            "canonical record should be written as an active OKF record: {canonical}"
        );
        assert!(
            canonical.contains("# Applied proposal"),
            "canonical record should include the approved title: {canonical}"
        );
        assert!(
            canonical.contains("Applied proposal body"),
            "canonical record should include the approved body: {canonical}"
        );
        assert!(
            canonical.contains("source_ref: service-proposal-tests\n"),
            "canonical record should preserve the evidence reference: {canonical}"
        );
        assert!(
            canonical.contains(&format!("proposal_id: {}\n", result.proposal.id)),
            "canonical record should store proposal lineage separately: {canonical}"
        );

        Ok(())
    }

    #[test]
    fn evidence_provenance_and_proposal_lineage_survive_rebuild_and_export() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let paths = service.paths.clone();
        let mut draft = sample_memory_draft(
            "Lineage survives rebuild",
            "\n  Evidence-backed zircon lineage.  \n",
        );
        draft.source_kind = Some("  test  ".to_owned());
        draft.source_ref = Some("  service-proposal-tests  ".to_owned());
        let applied = service.propose_memory_with_options(
            "agent:red-tests",
            draft,
            ProposeOptions {
                approval_override: None,
                apply: true,
            },
        )?;
        let proposal_id = applied.proposal.id.clone();
        let applied_record = applied.record.expect("record should be applied");
        let record_id = applied_record.id.clone();
        let expected_hash = blake3::hash("Evidence-backed zircon lineage.".as_bytes())
            .to_hex()
            .to_string();
        assert_eq!(applied_record.content_hash, expected_hash);
        assert!(
            service.repo_index_drift()?.is_current(),
            "normalized proposal apply must agree with its canonical file immediately"
        );

        service.rebuild()?;
        let rebuilt = MemoryService::open_paths(paths)?;
        let results = rebuilt.search_memory(SearchInput {
            query: "zircon lineage".to_owned(),
            scope_kind: Some(ScopeKind::Repo),
            scope_id: None,
            memory_type: None,
            lane: None,
            destination: Some(MemoryDestination::Repo),
            path_prefix: None,
            limit: 10,
            include_inactive: false,
        })?;
        let result = results
            .iter()
            .find(|result| result.record.id == record_id)
            .expect("rebuilt recall should return the applied record");
        assert_eq!(result.record.source_kind.as_deref(), Some("test"));
        assert_eq!(
            result.record.source_ref.as_deref(),
            Some("service-proposal-tests")
        );
        assert_eq!(
            result.record.proposal_id.as_deref(),
            Some(proposal_id.as_str())
        );
        assert_eq!(result.record.content_hash, expected_hash);
        assert_eq!(
            result.citations[0].source_ref.as_deref(),
            Some("service-proposal-tests"),
            "recall citations must point at original evidence, not the review packet"
        );

        let exported = rebuilt.export(ExportInput {
            format: ExportFormat::Okf,
            scope_kind: ScopeKind::Repo,
        })?;
        let markdown = fs::read_to_string(&exported.written_paths[0])?;
        assert!(markdown.contains("source_kind: \"test\""));
        assert!(markdown.contains("source_ref: \"service-proposal-tests\""));
        assert!(markdown.contains(&format!("proposal_id: \"{proposal_id}\"")));

        Ok(())
    }

    #[test]
    fn propose_with_options_rejects_manual_apply_without_creating_proposal() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;

        let error = service
            .propose_memory_with_options(
                "agent:red-tests",
                sample_memory_draft("Manual apply proposal", "Manual apply proposal body"),
                ProposeOptions {
                    approval_override: Some(ProposalApprovalOverride::Manual),
                    apply: true,
                },
            )
            .expect_err("manual apply should fail before creating an unappliable proposal");

        let message = error.to_string();
        assert!(
            message.contains("proposal apply mode requires auto approval"),
            "manual apply error should explain the auto-approval requirement: {message}"
        );
        assert!(
            message.contains("manual proposals must be approved before apply"),
            "manual apply error should tell callers how manual proposals progress: {message}"
        );
        assert!(
            service
                .list_proposals(ProposalStatusFilter::All)?
                .is_empty(),
            "manual apply refusal should not leave an unreviewed proposal behind"
        );

        Ok(())
    }

    #[test]
    fn duplicate_propose_with_apply_remains_unapproved_and_unapplied() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let draft = sample_memory_draft("Duplicate proposal", "Duplicate proposal body");
        let original = service.propose_memory_with_options(
            "agent:red-tests",
            draft.clone(),
            ProposeOptions {
                approval_override: None,
                apply: true,
            },
        )?;
        let original_record = original
            .record
            .as_ref()
            .expect("initial unique proposal should apply");

        let duplicate = service.propose_memory_with_options(
            "agent:red-tests",
            draft,
            ProposeOptions {
                approval_override: None,
                apply: true,
            },
        )?;

        let validation = duplicate
            .validation
            .as_ref()
            .expect("duplicate proposal should be validated before approval");
        assert!(!validation.is_valid);
        assert!(
            validation.issues.iter().any(|issue| {
                issue.code == "duplicate_content_hash"
                    && issue.record_id.as_deref() == Some(original_record.id.as_str())
            }),
            "duplicate validation should name the conflicting canonical record: {validation:?}"
        );
        assert_eq!(duplicate.proposal.status, ProposalStatus::Pending);
        assert_eq!(duplicate.record, None);
        assert!(!duplicate.applied);
        assert_eq!(
            service
                .show_proposal(duplicate.proposal.id.as_str())?
                .status,
            ProposalStatus::Pending
        );

        Ok(())
    }

    #[test]
    fn legacy_lifecycle_rejects_runtime_targets_without_canonical_leaks() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let local = service.create_local_memory(
            "agent:red-tests",
            LocalMemoryInput {
                memory_type: MemoryType::Preference,
                lane: MemoryLane::Semantic,
                title: "Private runtime preference".to_owned(),
                body: "This local-only body must never become canonical.".to_owned(),
            },
        )?;
        let checkpoint = service.create_checkpoint(
            "agent:red-tests",
            CheckpointInput {
                task: "Private runtime checkpoint".to_owned(),
                note: "This session-only body must never become canonical.".to_owned(),
            },
        )?;

        let local_error = service
            .tombstone_record(&local.id, "agent:red-tests", "must stay local")
            .expect_err("local targets must not be written as canonical tombstones");
        assert!(local_error.to_string().contains("destination local"));

        let session_error = service
            .supersede_record(
                &checkpoint.id,
                "agent:red-tests",
                sample_memory_draft(
                    "Replacement for session checkpoint",
                    "A repo replacement must not promote the private target.",
                ),
            )
            .expect_err("session targets must not be written as canonical supersedes");
        assert!(
            session_error.to_string().contains("destination session"),
            "unexpected session-target error: {session_error:#}"
        );

        for record in [&local, &checkpoint] {
            assert!(
                !service
                    .paths
                    .records_dir()
                    .join(format!("{}.md", record.id))
                    .exists(),
                "runtime target {} leaked into canonical records",
                record.id
            );
            let stored = service.inspect_expiry(&record.id)?.record;
            assert_eq!(stored.status, MemoryStatus::Active);
            assert_eq!(stored.destination, record.destination);
        }
        Ok(())
    }

    #[test]
    fn legacy_lifecycle_rejects_private_and_inactive_repo_targets() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let private_draft = sample_memory_draft(
            "Private repo target",
            "Private visibility must not be rewritten through a legacy lifecycle route.",
        );
        let private = apply_test_record(&service, private_draft)?;
        service.conn.execute(
            "UPDATE memory_record SET visibility = 'private' WHERE id = ?1",
            [&private.id],
        )?;
        let private_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", private.id));
        let private_before = fs::read(&private_path)?;

        let private_error = service
            .tombstone_record(&private.id, "agent:red-tests", "must stay private")
            .expect_err("private targets must not be rewritten canonically");
        assert!(
            private_error.to_string().contains("visibility private"),
            "unexpected private-target error: {private_error:#}"
        );
        assert_eq!(fs::read(&private_path)?, private_before);
        assert_eq!(
            service.inspect_expiry(&private.id)?.record.status,
            MemoryStatus::Active
        );

        let active = apply_test_record(
            &service,
            sample_memory_draft(
                "Inactive lifecycle target",
                "Inactive targets must be rejected before file mutation.",
            ),
        )?;
        let active_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", active.id));
        let active_before = fs::read(&active_path)?;
        service.conn.execute(
            "UPDATE memory_record SET status = 'superseded' WHERE id = ?1",
            [active.id.as_str()],
        )?;

        let inactive_error = service
            .tombstone_record(&active.id, "agent:red-tests", "already inactive")
            .expect_err("inactive targets must be rejected");
        assert!(inactive_error.to_string().contains("status superseded"));
        assert_eq!(fs::read(&active_path)?, active_before);
        assert_eq!(
            service.inspect_expiry(&active.id)?.record.status,
            MemoryStatus::Superseded
        );
        Ok(())
    }

    #[test]
    fn legacy_supersede_rejects_cross_scope_replacements_before_mutation() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let target = apply_test_record(
            &service,
            sample_memory_draft(
                "Same-scope lifecycle target",
                "The target must remain active when replacement scope differs.",
            ),
        )?;
        let target_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", target.id));
        let target_before = fs::read(&target_path)?;
        let mut replacement = sample_memory_draft(
            "Cross-scope lifecycle replacement",
            "A team-scoped replacement cannot supersede a repo-scoped target.",
        );
        replacement.scope_kind = ScopeKind::Team;
        replacement.scope_id = Some("platform".to_owned());

        let error = service
            .supersede_record(&target.id, "agent:red-tests", replacement)
            .expect_err("cross-scope replacements must be rejected");
        assert!(
            error.to_string().contains("cross-scope"),
            "unexpected cross-scope error: {error:#}"
        );
        assert_eq!(fs::read(&target_path)?, target_before);
        assert_eq!(
            service.inspect_expiry(&target.id)?.record.status,
            MemoryStatus::Active
        );
        assert!(
            !service
                .paths
                .records_dir()
                .join("cross-scope-lifecycle-replacement.md")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn legacy_supersede_rolls_back_db_and_files_when_second_install_fails() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let target = apply_test_record(
            &service,
            sample_memory_draft(
                "Second-write rollback target",
                "The original canonical body must survive a second-write failure.",
            ),
        )?;
        let target_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", target.id));
        let target_before = fs::read(&target_path)?;
        let replacement = sample_memory_draft(
            "Second-write rollback replacement",
            "This replacement must disappear when its install is interrupted.",
        );

        let error = service
            .supersede_record_with_hooks(
                &target.id,
                "agent:red-tests",
                replacement,
                |index| {
                    if index == 1 {
                        return Err(anyhow::anyhow!("injected second-write install failure"));
                    }
                    Ok(())
                },
                |_| Ok(()),
            )
            .expect_err("second-write failure must abort the lifecycle transaction");
        assert!(error.to_string().contains("second-write install failure"));

        assert_legacy_supersede_unchanged(
            &service,
            &target,
            &target_path,
            &target_before,
            "second-write-rollback-replacement",
        )?;
        Ok(())
    }

    #[test]
    fn legacy_supersede_revalidates_staged_bytes_at_install() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let target = apply_test_record(
            &service,
            sample_memory_draft(
                "Staged-byte target",
                "The original canonical body must survive staged-byte tampering.",
            ),
        )?;
        let target_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", target.id));
        let target_before = fs::read(&target_path)?;
        let transaction_root = repository_transaction_root(&service.paths);

        let error = service
            .supersede_record_with_hooks(
                &target.id,
                "agent:red-tests",
                sample_memory_draft(
                    "Staged-byte replacement",
                    "These authorized bytes must never be replaced by tampered staging bytes.",
                ),
                move |index| {
                    if index == 0 {
                        for entry in fs::read_dir(&transaction_root)? {
                            let path = entry?.path();
                            if path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .is_some_and(|name| name.ends_with(".write.tmp"))
                            {
                                fs::write(path, "tampered staged bytes")?;
                            }
                        }
                    }
                    Ok(())
                },
                |_| Ok(()),
            )
            .expect_err("tampered staged bytes must abort the lifecycle transaction");
        assert!(
            error
                .to_string()
                .contains("staged repository bytes changed after authorization"),
            "unexpected staged-byte error: {error:#}"
        );
        assert_legacy_supersede_unchanged(
            &service,
            &target,
            &target_path,
            &target_before,
            "staged-byte-replacement",
        )?;
        Ok(())
    }

    #[test]
    fn legacy_supersede_rolls_back_installed_files_when_db_commit_fails() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let target = apply_test_record(
            &service,
            sample_memory_draft(
                "Commit rollback target",
                "The original canonical body must survive a database commit failure.",
            ),
        )?;
        let target_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", target.id));
        let target_before = fs::read(&target_path)?;
        let replacement = sample_memory_draft(
            "Commit rollback replacement",
            "This replacement must disappear when SQLite refuses commit.",
        );

        let error = service
            .supersede_record_with_hooks(
                &target.id,
                "agent:red-tests",
                replacement,
                |_| Ok(()),
                |tx| {
                    tx.pragma_update(None, "defer_foreign_keys", "ON")?;
                    tx.execute(
                        "INSERT INTO memory_tag(record_id, tag) VALUES ('missing-record', 'deferred-commit-failure')",
                        [],
                    )?;
                    Ok(())
                },
            )
            .expect_err("deferred foreign-key violation must fail commit");
        assert!(
            error
                .to_string()
                .contains("failed to commit memory lifecycle transaction"),
            "expected actual commit failure, got: {error:#}"
        );

        assert_legacy_supersede_unchanged(
            &service,
            &target,
            &target_path,
            &target_before,
            "commit-rollback-replacement",
        )?;
        Ok(())
    }

    #[test]
    fn rebuild_refuses_to_discard_open_proposals_with_actionable_details() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let pending = service.propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft("Pending rebuild proposal", "Pending rebuild proposal body"),
            ProposeOptions {
                approval_override: Some(ProposalApprovalOverride::Manual),
                apply: false,
            },
        )?;
        let validated = service.propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft(
                "Validated rebuild proposal",
                "Validated rebuild proposal body",
            ),
            ProposeOptions {
                approval_override: Some(ProposalApprovalOverride::Manual),
                apply: false,
            },
        )?;
        service.conn.execute(
            "UPDATE proposal SET status = 'validated' WHERE id = ?1",
            [validated.proposal.id.as_str()],
        )?;
        let approved = service.propose_memory_with_options(
            "agent:red-tests",
            sample_memory_draft(
                "Approved rebuild proposal",
                "Approved rebuild proposal body",
            ),
            ProposeOptions {
                approval_override: None,
                apply: false,
            },
        )?;

        let error = service
            .rebuild()
            .expect_err("rebuild should not discard open proposals");
        let message = error.to_string();

        assert!(
            message.contains("rebuild refused because 3 open proposals would be discarded"),
            "rebuild refusal should include the open proposal count: {message}"
        );
        for (proposal_id, status) in [
            (pending.proposal.id.as_str(), "pending"),
            (validated.proposal.id.as_str(), "validated"),
            (approved.proposal.id.as_str(), "approved"),
        ] {
            let summary = format!("{proposal_id} ({status})");
            assert!(
                message.contains(&summary),
                "rebuild refusal should include open proposal summary {summary}: {message}"
            );
        }
        assert!(
            message.contains("memzoi proposals list --status open"),
            "rebuild refusal should suggest listing open proposals: {message}"
        );
        assert!(
            message.contains("memzoi proposals apply --all-approved"),
            "rebuild refusal should suggest applying approved proposals: {message}"
        );
        assert!(
            message.contains("memzoi reject <proposal-id> --reason"),
            "rebuild refusal should suggest rejecting proposals before rebuild: {message}"
        );

        Ok(())
    }

    #[test]
    fn rebuild_rejects_contextually_prohibited_canonical_edits_before_index_mutation()
    -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let record = apply_test_record(
            &service,
            sample_memory_draft("Rebuild safety baseline", "Safe indexed baseline body."),
        )?;
        let record_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", record.id));
        let edited = fs::read_to_string(&record_path)?
            .replace(
                "content_class: general_repo_knowledge",
                "content_class: raw_transcript",
            )
            .replace(
                "Safe indexed baseline body.",
                "Lexically harmless forbiddenrebuildsentinel payload.",
            );
        fs::write(&record_path, edited)?;
        let before_count: i64 =
            service
                .conn
                .query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))?;

        let error = MemoryService::rebuild_paths(service.paths.clone())
            .expect_err("contextually prohibited canonical edits must block rebuild");
        assert!(format!("{error:#}").contains("raw_transcript"));
        let after_count: i64 =
            service
                .conn
                .query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))?;
        assert_eq!(
            after_count, before_count,
            "rebuild mutated the runtime index"
        );
        assert!(
            service
                .search_memory(SearchInput {
                    query: "forbiddenrebuildsentinel".to_owned(),
                    limit: 10,
                    ..SearchInput::default()
                })?
                .is_empty(),
            "prohibited canonical bytes reached runtime search"
        );
        Ok(())
    }

    #[test]
    fn rebuild_scans_the_same_immutable_snapshot_that_it_would_import() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let record = apply_test_record(
            &service,
            sample_memory_draft("Immutable rebuild snapshot", "Safe indexed baseline body."),
        )?;
        let record_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", record.id));
        let safe = fs::read_to_string(&record_path)?;
        let prohibited = safe
            .replace(
                "content_class: general_repo_knowledge",
                "content_class: raw_transcript",
            )
            .replace(
                "Safe indexed baseline body.",
                "Lexically harmless snapshotracesentinel payload.",
            );
        fs::write(&record_path, prohibited)?;

        let error = MemoryService::rebuild_paths_with_snapshot_hook(service.paths.clone(), || {
            fs::write(&record_path, &safe)?;
            Ok(())
        })
        .expect_err("the prohibited first snapshot must block even after a safe replacement");
        assert!(format!("{error:#}").contains("raw_transcript"));
        assert!(
            service
                .search_memory(SearchInput {
                    query: "snapshotracesentinel".to_owned(),
                    limit: 10,
                    ..SearchInput::default()
                })?
                .is_empty(),
            "the unscanned first snapshot reached runtime search"
        );
        Ok(())
    }

    #[test]
    fn rebuild_rejects_candidate_controlled_project_scope_before_index_mutation()
    -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;
        let record = apply_test_record(
            &service,
            sample_memory_draft("Rebuild scope baseline", "Safe indexed scope baseline."),
        )?;
        let record_path = service
            .paths
            .records_dir()
            .join(format!("{}.md", record.id));
        let edited = fs::read_to_string(&record_path)?.replace(
            "scope: repo",
            "scope: project\nscope_id: candidate-controlled-project",
        );
        fs::write(&record_path, edited)?;
        let before_count: i64 =
            service
                .conn
                .query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))?;

        let error = MemoryService::rebuild_paths(service.paths.clone())
            .expect_err("candidate-controlled project scope must block rebuild");
        assert!(format!("{error:#}").contains("scope_project_mismatch"));
        let after_count: i64 =
            service
                .conn
                .query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))?;
        assert_eq!(
            after_count, before_count,
            "rebuild mutated the runtime index"
        );
        Ok(())
    }

    #[test]
    fn show_proposal_reports_missing_ids() -> anyhow::Result<()> {
        let (_temp, service) = initialized_service()?;

        let error = service
            .show_proposal("prop_missing")
            .expect_err("missing proposal show should fail");

        assert!(
            error
                .to_string()
                .contains("proposal not found: prop_missing"),
            "missing proposal show error should include the requested id: {error:#}"
        );

        Ok(())
    }

    fn initialized_service() -> anyhow::Result<(TempDir, MemoryService)> {
        let temp = TempDir::new()?;
        let project_root = temp.path().join("project");
        fs::create_dir(&project_root)?;
        let paths = MemoryPaths::with_runtime_home(
            project_root.canonicalize()?,
            temp.path().join("runtime-home"),
        );
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let service = MemoryService::open_paths(paths)?;
        Ok((temp, service))
    }

    fn apply_test_record(
        service: &MemoryService,
        draft: MemoryDraft,
    ) -> anyhow::Result<MemoryRecord> {
        let proposal = service.propose_memory("agent:red-tests", draft)?;
        service.validate_proposal(&proposal.id)?;
        service.approve_proposal(&proposal.id, "reviewer:human")?;
        service.apply_proposal(&proposal.id, "agent:applier")
    }

    fn assert_legacy_supersede_unchanged(
        service: &MemoryService,
        target: &MemoryRecord,
        target_path: &Path,
        target_before: &[u8],
        replacement_id: &str,
    ) -> anyhow::Result<()> {
        assert_eq!(fs::read(target_path)?, target_before);
        assert_eq!(
            service.inspect_expiry(&target.id)?.record.status,
            MemoryStatus::Active
        );
        assert!(
            service.inspect_expiry(replacement_id).is_err(),
            "replacement row {replacement_id} survived rollback"
        );
        assert!(
            !service
                .paths
                .records_dir()
                .join(format!("{replacement_id}.md"))
                .exists(),
            "replacement file {replacement_id} survived rollback"
        );
        let transaction_files = fs::read_dir(service.paths.records_dir())?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.starts_with('.') && name.ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert!(
            transaction_files.is_empty(),
            "staged transaction files survived rollback: {transaction_files:?}"
        );
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
            tags: vec!["rust".to_owned(), "tests".to_owned()],
            source_kind: Some("test".to_owned()),
            source_ref: Some("service-proposal-tests".to_owned()),
            sensitivity: crate::OkfProposalSensitivity::RepoSafe,
            content_class: RepositoryContentClass::GeneralRepoKnowledge,
            confidence: 0.82,
        }
    }
}
