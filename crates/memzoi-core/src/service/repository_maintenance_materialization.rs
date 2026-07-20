use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{DropBehavior, Transaction, TransactionBehavior};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    AuthorizationProof, CANONICAL_REVISION_SCHEMA, CanonicalLifecycleProjection,
    CanonicalRecordSemanticContent, CanonicalRevision, CanonicalRevisionProjection,
    ExpectedPriorRevision, MAINTENANCE_MAX_INPUT_FILE_BYTES, MaintenanceActionClass,
    MaintenanceCounterpart, MaintenancePlan, MaintenanceRecordSnapshot, MaintenanceRecordVersion,
    MaintenanceRevalidation, MaterializationAction, MaterializationAuthorizationCapability,
    MaterializationCounterpartRelationship, MaterializationOutputOutcome,
    MaterializationOutputRole, MemoryStatus, OkfProposalSensitivity, OkfRecordFile,
    PriorCanonicalLifecycleProjection, REPOSITORY_MAINTENANCE_MATERIALIZATION_METADATA_SCHEMA,
    REPOSITORY_MAINTENANCE_MATERIALIZATION_RESULT_SCHEMA, REPOSITORY_WRITE_SAFETY_SCHEMA,
    RecordLineage, RecordLineageKind, RepositoryContentClass, RepositoryMaintenanceDecisionBinding,
    RepositoryMaintenanceMaterializationMetadata, RepositoryMaintenanceMaterializationOutputResult,
    RepositoryMaintenanceMaterializationRequest, RepositoryMaintenanceMaterializationResult,
    RepositoryMaintenanceOutputIntent, RepositoryMaterializationMetadata,
    RepositoryProjectionPurpose, RepositoryReviewCommand, RepositoryWriteRoute, SafetyFieldKind,
    ScopeKind, ValidatedRepositoryMaintenanceSelection, Visibility,
    canonical_revision_for_okf_record, canonical_revision_for_projection,
    repository_maintenance_decision_id, revalidate_maintenance_plan_at, role_rank,
    validate_repository_maintenance_selection,
};
use crate::{git_repository, okf, repository_io};

use super::{
    MemoryService,
    repository_mutation::{
        InstalledRepositoryProjection, OwnedRepositoryProjection, RepositoryMutationAuthorization,
        authorize_repository_projection_batch, borrowed_repository_projections,
        capture_authorized_existing_repository_projection_identity,
        capture_authorized_repository_projection_identity,
        copy_repository_file_to_transaction_with_identity, explicit_repository_provenance,
        install_authorized_repository_projection, memory_draft_safety_values,
        replace_authorized_repository_projection, rollback_authorized_repository_projection,
        safety_value, stage_authorized_file,
    },
    safe_files::{RepoLifecycleLock, sync_directory},
};

mod journal;

#[cfg(test)]
type MaintenanceTransitionHook = Box<dyn FnMut(&'static str, usize)>;

#[cfg(test)]
thread_local! {
    static MAINTENANCE_TRANSITION_HOOK: std::cell::RefCell<Option<MaintenanceTransitionHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn inject_transition_hook(hook: impl FnMut(&'static str, usize) + 'static) {
    MAINTENANCE_TRANSITION_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
pub(super) fn inject_journal_rewrite_partial_write_failure() {
    journal::inject_rewrite_partial_write_failure();
}

#[cfg(test)]
fn maintenance_transition(point: &'static str, index: usize) {
    MAINTENANCE_TRANSITION_HOOK.with(|slot| {
        let hook = slot.borrow_mut().take();
        if let Some(mut hook) = hook {
            hook(point, index);
            *slot.borrow_mut() = Some(hook);
        }
    });
}

#[cfg(not(test))]
fn maintenance_transition(_point: &'static str, _index: usize) {}

#[derive(Debug, Clone)]
struct OutputSpec {
    action_id: String,
    record_id: String,
    path: PathBuf,
    action: MaterializationAction,
    role: MaterializationOutputRole,
    counterpart_record_id: String,
    counterpart_revision: CanonicalRevision,
    expected_prior_revision: CanonicalRevision,
    reason: String,
    renewal_predecessor: Option<String>,
}

#[derive(Debug, Clone)]
struct LoadedRecord {
    path: PathBuf,
    bytes: Vec<u8>,
    record: OkfRecordFile,
    semantic_revision: CanonicalRevision,
}

#[derive(Debug, Clone)]
struct PreparedOutput {
    spec: OutputSpec,
    prior: LoadedRecord,
    record: OkfRecordFile,
    markdown: String,
    intent: RepositoryMaintenanceOutputIntent,
}

#[derive(Debug, Clone)]
struct PreparedComparison {
    path: PathBuf,
    bytes: Vec<u8>,
    expected_revision: CanonicalRevision,
}

#[derive(Debug, Clone)]
struct PreparedMaintenanceMaterialization {
    selection: ValidatedRepositoryMaintenanceSelection,
    decision: RepositoryMaintenanceDecisionBinding,
    decision_id: String,
    outputs: Vec<PreparedOutput>,
    comparisons: Vec<PreparedComparison>,
}

impl MemoryService {
    pub fn apply_repository_maintenance_materialization(
        &self,
        plan: &MaintenancePlan,
        request: &RepositoryMaintenanceMaterializationRequest,
    ) -> Result<RepositoryMaintenanceMaterializationResult> {
        let selection = validate_repository_maintenance_selection(plan, request)?;
        let specs = output_specs(plan, &selection)?;
        ensure_repository_maintenance_platform_supported()?;
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        recover_locked(&self.paths, &self.conn)?;
        let now = self.now();

        if let Some((prepared, result)) =
            recognize_exact_after_state(&self.paths, plan, request, selection.clone(), &specs)?
        {
            journal::cleanup_orphans_for_completed_decision(&self.paths, &prepared)?;
            reconcile_materialized_index(self, &prepared.outputs)?;
            return Ok(result);
        }

        validate_fresh_time(plan, request, now)?;
        let prepared = prepare_exact_pre_state(&self.paths, plan, request, selection, &specs)?;
        journal::cleanup_orphans_for_completed_decision(&self.paths, &prepared)?;
        let selected_paths = selected_paths(&prepared);
        git_repository::git_maintenance_targets_clean(&self.paths.project_root, &selected_paths)
            .context("repository maintenance Git target check failed")?;
        match revalidate_maintenance_plan_at(&self.paths, plan, now)? {
            MaintenanceRevalidation::Current { .. } => {}
            MaintenanceRevalidation::Stale { reasons, .. } => {
                bail!("repository maintenance plan is stale: {reasons:?}")
            }
        }

        apply_prepared_materialization(self, plan, request, prepared)
    }
}

pub(super) fn recover_on_open(
    paths: &crate::MemoryPaths,
    conn: &rusqlite::Connection,
) -> Result<()> {
    if journal::journal_exists(paths)? {
        let _lock = RepoLifecycleLock::acquire(paths)?;
        recover_locked(paths, conn)
            .context("failed to recover interrupted repository maintenance materialization")?;
    }
    Ok(())
}

fn recover_locked(paths: &crate::MemoryPaths, conn: &rusqlite::Connection) -> Result<()> {
    let Some(loaded) = journal::load_journal(paths)? else {
        return Ok(());
    };
    let persisted = &loaded.journal;
    journal::validate_recovery_context(paths, persisted)?;
    let prepared = prepare_from_journal(paths, persisted)?;
    let projections = maintenance_projections(paths, &prepared)?;
    let safety_values = maintenance_safety_values(&persisted.plan, &persisted.request, &prepared)?;
    ensure!(
        safety_fields_digest(&safety_values) == persisted.safety_fields_digest,
        "repository maintenance recovery safety fields changed"
    );
    let authorization = authorize_repository_projection_batch(
        paths,
        RepositoryWriteRoute::Maintenance,
        OkfProposalSensitivity::RepoSafe,
        ScopeKind::Repo,
        None,
        Visibility::Repo,
        AuthorizationProof::Maintenance {
            operation_id: &prepared.decision_id,
        },
        explicit_repository_provenance(
            RepositoryContentClass::GeneralRepoKnowledge,
            &prepared.decision_id,
        ),
        &safety_values,
        &projections,
    )?;
    let borrowed = borrowed_repository_projections(&projections);
    ensure!(
        encode_digest(&authorization.policy_context_digest) == persisted.policy_context_digest
            && encode_digest(&crate::repository_write_safety::projection_digest(
                &borrowed
            )) == persisted.projection_digest
            && authorization.digest() == persisted.authorization_digest,
        "repository maintenance recovery authorization does not match the journal"
    );
    let marker = journal::commit_marker_exists(conn, persisted)?;
    let states = classify_journal_outputs(paths, persisted, marker)?;
    ensure_comparisons_match(paths, persisted)?;
    let all_pre = states.iter().all(|state| *state == JournalPathState::Pre);
    let all_post = states.iter().all(|state| *state == JournalPathState::Post);
    let mutation = RepositoryMutationAuthorization {
        route: RepositoryWriteRoute::Maintenance,
        authorization: &authorization,
        projections: &projections,
    };

    if !all_pre && !all_post {
        restore_journal_state(
            paths,
            persisted,
            mutation,
            &states,
            if marker {
                JournalPathState::Post
            } else {
                JournalPathState::Pre
            },
        )?;
    }
    let final_state = if all_post || (!all_pre && marker) {
        JournalPathState::Post
    } else {
        JournalPathState::Pre
    };
    match final_state {
        JournalPathState::Pre => {
            verify_complete_pre_state(paths, &prepared)?;
            reconcile_recovery_index(conn, paths, &prepared, persisted, false)?;
        }
        JournalPathState::Post => {
            verify_complete_post_state(paths, &prepared)?;
            reconcile_recovery_index(conn, paths, &prepared, persisted, true)?;
        }
    }
    journal::cleanup(paths, &loaded)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JournalPathState {
    Pre,
    Post,
}

fn output_specs(
    plan: &MaintenancePlan,
    selection: &ValidatedRepositoryMaintenanceSelection,
) -> Result<Vec<OutputSpec>> {
    let records = plan
        .records
        .iter()
        .map(|record| (record.record_id.as_str(), record))
        .collect::<BTreeMap<_, _>>();
    let mut specs = Vec::new();
    for action in &selection.selected_actions {
        match action.class {
            MaintenanceActionClass::ConsolidateExactDuplicates => {
                let keeper = action
                    .keeper_record_id
                    .as_deref()
                    .context("duplicate consolidation has no keeper")?;
                let keeper_revision = plan_revision(&records, keeper)?.clone();
                for record_id in action
                    .record_ids
                    .iter()
                    .filter(|record_id| *record_id != keeper)
                {
                    specs.push(OutputSpec {
                        action_id: action.action_id.clone(),
                        record_id: record_id.clone(),
                        path: canonical_path(&records, record_id)?,
                        action: MaterializationAction::Supersede,
                        role: MaterializationOutputRole::LifecycleCounterpart,
                        counterpart_record_id: keeper.to_owned(),
                        counterpart_revision: keeper_revision.clone(),
                        expected_prior_revision: plan_revision(&records, record_id)?.clone(),
                        reason: bounded_record_reason("exact duplicate of keeper ", keeper),
                        renewal_predecessor: None,
                    });
                }
            }
            MaintenanceActionClass::CreateRenewalSuccessor => {
                let predecessor = action
                    .predecessor_record_id
                    .as_deref()
                    .context("renewal action has no predecessor")?;
                let evidence = action
                    .evidence_record_id
                    .as_deref()
                    .context("renewal action has no evidence record")?;
                let predecessor_revision = plan_revision(&records, predecessor)?.clone();
                let evidence_revision = plan_revision(&records, evidence)?.clone();
                specs.push(OutputSpec {
                    action_id: action.action_id.clone(),
                    record_id: evidence.to_owned(),
                    path: canonical_path(&records, evidence)?,
                    action: MaterializationAction::Supersede,
                    role: MaterializationOutputRole::CanonicalRecord,
                    counterpart_record_id: predecessor.to_owned(),
                    counterpart_revision: predecessor_revision.clone(),
                    expected_prior_revision: evidence_revision.clone(),
                    reason: bounded_record_reason(
                        "renewal successor for predecessor ",
                        predecessor,
                    ),
                    renewal_predecessor: Some(predecessor.to_owned()),
                });
                specs.push(OutputSpec {
                    action_id: action.action_id.clone(),
                    record_id: predecessor.to_owned(),
                    path: canonical_path(&records, predecessor)?,
                    action: MaterializationAction::Supersede,
                    role: MaterializationOutputRole::LifecycleCounterpart,
                    counterpart_record_id: evidence.to_owned(),
                    counterpart_revision: evidence_revision,
                    expected_prior_revision: predecessor_revision,
                    reason: bounded_record_reason("renewed by evidence record ", evidence),
                    renewal_predecessor: None,
                });
            }
            _ => bail!("unsupported repository maintenance action class"),
        }
    }
    specs.sort_by(|left, right| {
        (&left.path, role_rank(left.role), &left.action_id).cmp(&(
            &right.path,
            role_rank(right.role),
            &right.action_id,
        ))
    });
    ensure!(
        specs
            .windows(2)
            .all(|window| window[0].path != window[1].path),
        "maintenance outputs cannot share a path"
    );
    Ok(specs)
}

fn prepare_from_journal(
    paths: &crate::MemoryPaths,
    persisted: &journal::MaintenanceMaterializationJournal,
) -> Result<PreparedMaintenanceMaterialization> {
    let selection = validate_repository_maintenance_selection(&persisted.plan, &persisted.request)?;
    ensure!(
        selection.selection_id == persisted.selection_id
            && selection.selected_actions == persisted.selected_actions,
        "repository maintenance journal selection is invalid"
    );
    let specs = output_specs(&persisted.plan, &selection)?;
    ensure!(
        specs.len() == persisted.outputs.len(),
        "repository maintenance journal output topology changed"
    );
    let mut loaded = BTreeMap::new();
    for (spec, entry) in specs.iter().zip(&persisted.outputs) {
        ensure!(
            entry.action_id == spec.action_id
                && entry.path == path_text(&spec.path)?
                && entry.record_id == spec.record_id
                && entry.action == spec.action
                && entry.role == spec.role
                && entry.prior_semantic_revision == spec.expected_prior_revision,
            "repository maintenance journal output binding is invalid"
        );
        let prior_bytes = journal::read_backup(paths, persisted, entry)?;
        let prior = parse_loaded_record(paths, spec.path.clone(), prior_bytes)?;
        ensure!(
            prior.semantic_revision == entry.prior_semantic_revision,
            "repository maintenance backup semantic revision is invalid"
        );
        loaded.insert(spec.record_id.clone(), prior);
    }
    ensure!(
        persisted.comparisons.len() == selection.comparison_record_ids.len(),
        "repository maintenance journal comparison topology changed"
    );
    for (comparison, record_id) in persisted
        .comparisons
        .iter()
        .zip(&selection.comparison_record_ids)
    {
        let snapshot = plan_snapshot(&persisted.plan, record_id)?;
        ensure!(
            comparison.path
                == path_text(&canonical_path(
                    &persisted
                        .plan
                        .records
                        .iter()
                        .map(|record| (record.record_id.as_str(), record))
                        .collect(),
                    record_id,
                )?)?
                && comparison.semantic_revision == *plan_revision_for_snapshot(snapshot)?,
            "repository maintenance journal comparison binding is invalid"
        );
        let path = PathBuf::from(&comparison.path);
        let bytes = read_repository_exact(
            paths,
            &path,
            comparison.bytes,
            "repository maintenance comparison",
        )?
        .context("repository maintenance comparison is missing")?;
        ensure!(
            journal::hash(&bytes) == comparison.hash,
            "repository maintenance comparison changed"
        );
        let record = parse_loaded_record(paths, path, bytes)?;
        ensure!(
            record.semantic_revision == comparison.semantic_revision,
            "repository maintenance comparison revision changed"
        );
        loaded.insert(record.record.concept_id.clone(), record);
    }
    let prepared = prepare_from_loaded(
        &persisted.plan,
        &persisted.request,
        selection,
        &specs,
        loaded,
    )?;
    ensure!(
        prepared.decision_id == persisted.decision_id && prepared.decision == persisted.decision,
        "repository maintenance journal decision is invalid"
    );
    for (output, entry) in prepared.outputs.iter().zip(&persisted.outputs) {
        let staged = journal::read_stage(paths, persisted, entry)?;
        ensure!(
            output.markdown.as_bytes() == staged
                && output.intent.intended_semantic_revision == entry.post_semantic_revision,
            "repository maintenance staged final is not the deterministic projection"
        );
    }
    Ok(prepared)
}

fn classify_journal_outputs(
    paths: &crate::MemoryPaths,
    persisted: &journal::MaintenanceMaterializationJournal,
    committed: bool,
) -> Result<Vec<JournalPathState>> {
    let classified = persisted
        .outputs
        .iter()
        .map(|entry| {
            let path = PathBuf::from(&entry.path);
            let metadata = std::fs::symlink_metadata(paths.project_root.join(&path))
                .context("repository maintenance recovery output is missing or unsafe")?;
            let length = metadata.len();
            let expected = if length == entry.prior_bytes {
                entry.prior_bytes
            } else if length == entry.post_bytes {
                entry.post_bytes
            } else {
                bail!("repository maintenance recovery output has an unknown size")
            };
            let (bytes, identity) = repository_io::read_repository_file_with_identity_if_exists(
                &paths.project_root,
                &path,
                expected,
                "repository maintenance recovery output",
            )?
            .context("repository maintenance recovery output is missing")?;
            let hash = journal::hash(&bytes);
            if hash == entry.prior_hash {
                Ok((JournalPathState::Pre, identity))
            } else if hash == entry.post_hash {
                Ok((JournalPathState::Post, identity))
            } else {
                bail!("repository maintenance recovery output is ambiguous")
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let states = classified
        .iter()
        .map(|(state, _)| *state)
        .collect::<Vec<_>>();
    if states.iter().all(|state| *state == JournalPathState::Pre)
        || states.iter().all(|state| *state == JournalPathState::Post)
    {
        return Ok(states);
    }

    let post_identities_persisted = persisted.outputs[0].post_device != 0;
    for ((state, identity), entry) in classified.iter().zip(&persisted.outputs) {
        match (committed, state) {
            (true, JournalPathState::Pre) => ensure!(
                identity.device == entry.prior_device && identity.inode == entry.prior_inode,
                "repository maintenance prior bytes in a mixture are not transaction-owned"
            ),
            (false, JournalPathState::Post) if post_identities_persisted => ensure!(
                identity.device == entry.post_device && identity.inode == entry.post_inode,
                "repository maintenance post bytes in a mixture are not transaction-owned"
            ),
            _ => {}
        }
    }
    let mut saw_pre = false;
    for state in &states {
        match state {
            JournalPathState::Pre => saw_pre = true,
            JournalPathState::Post if saw_pre => {
                bail!("repository maintenance mixture is not an installed output prefix")
            }
            JournalPathState::Post => {}
        }
    }
    Ok(states)
}

fn ensure_comparisons_match(
    paths: &crate::MemoryPaths,
    persisted: &journal::MaintenanceMaterializationJournal,
) -> Result<()> {
    for comparison in &persisted.comparisons {
        let path = PathBuf::from(&comparison.path);
        let bytes = read_repository_exact(
            paths,
            &path,
            comparison.bytes,
            "repository maintenance recovery comparison",
        )?
        .context("repository maintenance recovery comparison is missing")?;
        ensure!(
            journal::hash(&bytes) == comparison.hash,
            "repository maintenance recovery comparison changed"
        );
    }
    Ok(())
}

fn restore_journal_state(
    paths: &crate::MemoryPaths,
    persisted: &journal::MaintenanceMaterializationJournal,
    mutation: RepositoryMutationAuthorization<'_>,
    states: &[JournalPathState],
    target: JournalPathState,
) -> Result<()> {
    let mut indices = (0..persisted.outputs.len()).collect::<Vec<_>>();
    if target == JournalPathState::Pre {
        indices.reverse();
    }
    for index in indices {
        let entry = &persisted.outputs[index];
        let state = states[index];
        if state == target {
            continue;
        }
        let destination = paths.project_root.join(&entry.path);
        let current_purpose = match state {
            JournalPathState::Pre => RepositoryProjectionPurpose::Existing,
            JournalPathState::Post => RepositoryProjectionPurpose::Write,
        };
        let target_purpose = match target {
            JournalPathState::Pre => RepositoryProjectionPurpose::Existing,
            JournalPathState::Post => RepositoryProjectionPurpose::Write,
        };
        let identity = capture_authorized_repository_projection_identity(
            paths,
            mutation,
            &destination,
            current_purpose,
        )?;
        replace_authorized_repository_projection(
            paths,
            mutation,
            &destination,
            target_purpose,
            current_purpose,
            identity,
        )?;
        maintenance_transition("after_recovery_restore", index);
    }
    sync_directory(&paths.records_dir())
}

fn reconcile_recovery_index(
    conn: &rusqlite::Connection,
    paths: &crate::MemoryPaths,
    prepared: &PreparedMaintenanceMaterialization,
    persisted: &journal::MaintenanceMaterializationJournal,
    post: bool,
) -> Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let records = prepared
        .outputs
        .iter()
        .map(|output| {
            if post {
                output.record.clone()
            } else {
                output.prior.record.clone()
            }
        })
        .collect::<Vec<_>>();
    okf::import_okf_records(&tx, &records)?;
    if post {
        if !journal::commit_marker_exists(&tx, persisted)? {
            journal::append_commit_marker(&tx, persisted)?;
        }
    } else {
        journal::delete_commit_marker(&tx, persisted)?;
    }
    let drift = super::derived_index::inspect(paths, &tx)?;
    ensure!(
        drift.is_current(),
        "recovered maintenance index is not current"
    );
    tx.commit()?;
    Ok(())
}

fn prepare_exact_pre_state(
    paths: &crate::MemoryPaths,
    plan: &MaintenancePlan,
    request: &RepositoryMaintenanceMaterializationRequest,
    selection: ValidatedRepositoryMaintenanceSelection,
    specs: &[OutputSpec],
) -> Result<PreparedMaintenanceMaterialization> {
    let mut loaded = BTreeMap::new();
    for record_id in selection
        .output_record_ids
        .iter()
        .chain(selection.comparison_record_ids.iter())
    {
        let snapshot = plan_snapshot(plan, record_id)?;
        let record = load_record(paths, snapshot)?;
        if record.semantic_revision != *plan_revision_for_snapshot(snapshot)? {
            bail!("repository maintenance selected record changed after planning");
        }
        loaded.insert(record_id.clone(), record);
    }
    prepare_from_loaded(plan, request, selection, specs, loaded)
}

fn prepare_from_loaded(
    plan: &MaintenancePlan,
    request: &RepositoryMaintenanceMaterializationRequest,
    selection: ValidatedRepositoryMaintenanceSelection,
    specs: &[OutputSpec],
    loaded: BTreeMap<String, LoadedRecord>,
) -> Result<PreparedMaintenanceMaterialization> {
    let mut projected = specs
        .iter()
        .map(|spec| {
            let prior = loaded
                .get(&spec.record_id)
                .cloned()
                .context("maintenance output record was not loaded")?;
            project_semantic_output(spec, prior)
        })
        .collect::<Result<Vec<_>>>()?;
    let intents = projected
        .iter()
        .map(|output| output.intent.clone())
        .collect::<Vec<_>>();
    let decision = RepositoryMaintenanceDecisionBinding {
        policy_version: plan.policy.policy_version.clone(),
        policy_digest: plan.policy.policy_digest.clone(),
        safety_contract: REPOSITORY_WRITE_SAFETY_SCHEMA.to_owned(),
        authorization_capability: MaterializationAuthorizationCapability::ExplicitCli,
        outputs: intents,
        decision_at: request.decision_at.clone(),
    };
    let decision_id = repository_maintenance_decision_id(&selection.selection_id, &decision)?;
    for output in &mut projected {
        attach_metadata_and_render(output, plan, &selection.selection_id, &decision_id, request)?;
    }
    let comparisons = selection
        .comparison_record_ids
        .iter()
        .map(|record_id| {
            let record = loaded
                .get(record_id)
                .context("maintenance comparison record was not loaded")?;
            Ok(PreparedComparison {
                path: record.path.clone(),
                bytes: record.bytes.clone(),
                expected_revision: record.semantic_revision.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PreparedMaintenanceMaterialization {
        selection,
        decision,
        decision_id,
        outputs: projected,
        comparisons,
    })
}

fn project_semantic_output(spec: &OutputSpec, prior: LoadedRecord) -> Result<PreparedOutput> {
    let mut record = prior.record.clone();
    match spec.role {
        MaterializationOutputRole::CanonicalRecord => {
            let predecessor = spec
                .renewal_predecessor
                .as_deref()
                .context("canonical maintenance output has no renewal predecessor")?;
            if record.status != MemoryStatus::Active
                || record.supersedes_id.is_some()
                || record
                    .lineage
                    .as_ref()
                    .is_some_and(|lineage| lineage.kind == RecordLineageKind::Renewal)
            {
                bail!("renewal evidence record already has incompatible lifecycle state");
            }
            record.supersedes_id = Some(predecessor.to_owned());
            record.lineage = Some(RecordLineage {
                kind: RecordLineageKind::Renewal,
                predecessor_id: predecessor.to_owned(),
            });
        }
        MaterializationOutputRole::LifecycleCounterpart => {
            record.status = MemoryStatus::Superseded;
        }
    }
    let relationship = relationship_for(spec.role);
    let lifecycle = CanonicalLifecycleProjection {
        action: Some(spec.action),
        target_expected_revision: Some(spec.counterpart_revision.clone()),
        counterpart_record_id: Some(spec.counterpart_record_id.clone()),
        counterpart_relationship: Some(relationship),
        reason: Some(spec.reason.clone()),
    };
    let intended_semantic_revision =
        canonical_revision_for_projection(&CanonicalRevisionProjection {
            schema: CANONICAL_REVISION_SCHEMA.to_owned(),
            record_id: record.concept_id.clone(),
            record: CanonicalRecordSemanticContent::from(&record),
            lifecycle,
        })?;
    let intent = RepositoryMaintenanceOutputIntent {
        action_id: spec.action_id.clone(),
        path: path_text(&spec.path)?,
        record_id: spec.record_id.clone(),
        action: spec.action,
        role: spec.role,
        expected_prior_revision: ExpectedPriorRevision::Revision(
            spec.expected_prior_revision.clone(),
        ),
        intended_semantic_revision,
        reason: spec.reason.clone(),
    };
    intent.validate()?;
    record.materialization = None;
    Ok(PreparedOutput {
        spec: spec.clone(),
        prior,
        record,
        markdown: String::new(),
        intent,
    })
}

fn attach_metadata_and_render(
    output: &mut PreparedOutput,
    plan: &MaintenancePlan,
    selection_id: &str,
    decision_id: &str,
    request: &RepositoryMaintenanceMaterializationRequest,
) -> Result<()> {
    let metadata = maintenance_metadata(
        output,
        plan,
        selection_id,
        decision_id,
        request,
        prior_lifecycle(&output.prior.record),
    )?;
    output.record.materialization = Some(RepositoryMaterializationMetadata::Maintenance(metadata));
    let markdown = okf::render_okf_record_markdown(&output.record)?;
    let parsed =
        okf::parse_okf_record_markdown(Path::new(".memzoi/records"), &output.spec.path, &markdown)?
            .context("maintenance projection did not parse as a canonical record")?;
    if parsed != output.record || okf::render_okf_record_markdown(&parsed)? != markdown {
        bail!("maintenance projection failed its canonical byte round trip");
    }
    if canonical_revision_for_okf_record(&parsed)? != output.intent.intended_semantic_revision {
        bail!("maintenance projection semantic revision changed after rendering");
    }
    output.markdown = markdown;
    Ok(())
}

fn maintenance_metadata(
    output: &PreparedOutput,
    plan: &MaintenancePlan,
    selection_id: &str,
    decision_id: &str,
    request: &RepositoryMaintenanceMaterializationRequest,
    prior_lifecycle: PriorCanonicalLifecycleProjection,
) -> Result<RepositoryMaintenanceMaterializationMetadata> {
    let metadata = RepositoryMaintenanceMaterializationMetadata {
        schema: REPOSITORY_MAINTENANCE_MATERIALIZATION_METADATA_SCHEMA.to_owned(),
        action_id: output.spec.action_id.clone(),
        action: output.spec.action,
        role: output.spec.role,
        plan_id: plan.plan_id.clone(),
        selection_id: selection_id.to_owned(),
        decision_id: decision_id.to_owned(),
        decision_at: request.decision_at.clone(),
        policy_version: plan.policy.policy_version.clone(),
        policy_digest: plan.policy.policy_digest.clone(),
        safety_contract: REPOSITORY_WRITE_SAFETY_SCHEMA.to_owned(),
        authorization_capability: MaterializationAuthorizationCapability::ExplicitCli,
        expected_prior_revision: output.intent.expected_prior_revision.clone(),
        intended_semantic_revision: output.intent.intended_semantic_revision.clone(),
        counterpart: Some(MaintenanceCounterpart {
            record_id: output.spec.counterpart_record_id.clone(),
            expected_prior_revision: output.spec.counterpart_revision.clone(),
            relationship: relationship_for(output.spec.role),
        }),
        reason: output.spec.reason.clone(),
        prior_lifecycle,
    };
    metadata.validate()?;
    Ok(metadata)
}

fn recognize_exact_after_state(
    paths: &crate::MemoryPaths,
    plan: &MaintenancePlan,
    request: &RepositoryMaintenanceMaterializationRequest,
    selection: ValidatedRepositoryMaintenanceSelection,
    specs: &[OutputSpec],
) -> Result<
    Option<(
        PreparedMaintenanceMaterialization,
        RepositoryMaintenanceMaterializationResult,
    )>,
> {
    let mut loaded_outputs = Vec::new();
    let mut matching_metadata = 0_usize;
    for spec in specs {
        let snapshot = plan_snapshot(plan, &spec.record_id)?;
        let loaded = load_record_without_revision_check(paths, snapshot)?;
        if loaded.record.materialization.as_ref().is_some_and(|metadata| {
            matches!(metadata, RepositoryMaterializationMetadata::Maintenance(metadata)
                if metadata.plan_id == plan.plan_id && metadata.selection_id == selection.selection_id)
        }) {
            matching_metadata += 1;
        }
        loaded_outputs.push((spec, loaded));
    }
    if matching_metadata == 0 {
        return Ok(None);
    }
    if matching_metadata != specs.len() {
        bail!("repository maintenance state is an ambiguous partial decision");
    }

    let mut outputs = Vec::new();
    let mut intents = Vec::new();
    let mut persisted_decision_id = None;
    for (spec, loaded) in loaded_outputs {
        let RepositoryMaterializationMetadata::Maintenance(metadata) = loaded
            .record
            .materialization
            .as_ref()
            .expect("matching metadata counted")
        else {
            unreachable!()
        };
        if metadata.action_id != spec.action_id
            || metadata.action != spec.action
            || metadata.role != spec.role
            || metadata.decision_at != request.decision_at
            || metadata.policy_version != plan.policy.policy_version
            || metadata.policy_digest != plan.policy.policy_digest
            || metadata.expected_prior_revision
                != ExpectedPriorRevision::Revision(spec.expected_prior_revision.clone())
            || metadata.intended_semantic_revision != loaded.semantic_revision
            || metadata.reason != spec.reason
            || metadata.counterpart
                != Some(MaintenanceCounterpart {
                    record_id: spec.counterpart_record_id.clone(),
                    expected_prior_revision: spec.counterpart_revision.clone(),
                    relationship: relationship_for(spec.role),
                })
        {
            bail!("repository maintenance after-state metadata does not match the decision");
        }
        if persisted_decision_id.get_or_insert_with(|| metadata.decision_id.clone())
            != &metadata.decision_id
        {
            bail!("repository maintenance outputs disagree on decision identity");
        }
        let prior_record = reconstruct_prior_record(&loaded.record, metadata, spec)?;
        let prior = LoadedRecord {
            path: loaded.path.clone(),
            bytes: Vec::new(),
            record: prior_record,
            semantic_revision: spec.expected_prior_revision.clone(),
        };
        let mut projected = project_semantic_output(spec, prior)?;
        let mut actual_without_metadata = loaded.record.clone();
        actual_without_metadata.materialization = None;
        ensure!(
            projected.record == actual_without_metadata
                && projected.intent.intended_semantic_revision == loaded.semantic_revision,
            "repository maintenance after-state is not the deterministic action projection"
        );
        let expected_metadata = maintenance_metadata(
            &projected,
            plan,
            &selection.selection_id,
            &metadata.decision_id,
            request,
            metadata.prior_lifecycle.clone(),
        )?;
        ensure!(
            expected_metadata == *metadata,
            "repository maintenance after-state metadata is not deterministic"
        );
        projected.record.materialization = Some(RepositoryMaterializationMetadata::Maintenance(
            expected_metadata,
        ));
        projected.markdown = okf::render_okf_record_markdown(&projected.record)?;
        ensure!(
            projected.markdown.as_bytes() == loaded.bytes,
            "repository maintenance after-state bytes are not the deterministic projection"
        );
        intents.push(projected.intent.clone());
        outputs.push(projected);
    }
    let decision = RepositoryMaintenanceDecisionBinding {
        policy_version: plan.policy.policy_version.clone(),
        policy_digest: plan.policy.policy_digest.clone(),
        safety_contract: REPOSITORY_WRITE_SAFETY_SCHEMA.to_owned(),
        authorization_capability: MaterializationAuthorizationCapability::ExplicitCli,
        outputs: intents,
        decision_at: request.decision_at.clone(),
    };
    let decision_id = repository_maintenance_decision_id(&selection.selection_id, &decision)?;
    if persisted_decision_id.as_deref() != Some(decision_id.as_str()) {
        bail!("repository maintenance after-state decision identity is invalid");
    }
    let comparisons = load_comparisons(paths, plan, &selection)?;
    let prepared = PreparedMaintenanceMaterialization {
        selection,
        decision,
        decision_id,
        outputs,
        comparisons,
    };
    let result = materialization_result(
        &prepared,
        paths,
        MaterializationOutputOutcome::AlreadyCurrent,
    )?;
    Ok(Some((prepared, result)))
}

fn reconstruct_prior_record(
    record: &OkfRecordFile,
    metadata: &RepositoryMaintenanceMaterializationMetadata,
    spec: &OutputSpec,
) -> Result<OkfRecordFile> {
    let mut prior = record.clone();
    prior.status = metadata.prior_lifecycle.status;
    prior.supersedes_id = metadata.prior_lifecycle.supersedes_id.clone();
    prior.lineage = metadata.prior_lifecycle.lineage.clone();
    prior.materialization = None;
    let revision = canonical_revision_for_projection(&CanonicalRevisionProjection {
        schema: CANONICAL_REVISION_SCHEMA.to_owned(),
        record_id: prior.concept_id.clone(),
        record: CanonicalRecordSemanticContent::from(&prior),
        lifecycle: metadata.prior_lifecycle.lifecycle_projection(),
    })?;
    if revision != spec.expected_prior_revision {
        bail!("repository maintenance after-state cannot reconstruct its prior revision");
    }
    Ok(prior)
}

fn load_comparisons(
    paths: &crate::MemoryPaths,
    plan: &MaintenancePlan,
    selection: &ValidatedRepositoryMaintenanceSelection,
) -> Result<Vec<PreparedComparison>> {
    selection
        .comparison_record_ids
        .iter()
        .map(|record_id| {
            let snapshot = plan_snapshot(plan, record_id)?;
            let loaded = load_record(paths, snapshot)?;
            if loaded.semantic_revision != *plan_revision_for_snapshot(snapshot)? {
                bail!("maintenance comparison record changed after planning");
            }
            Ok(PreparedComparison {
                path: loaded.path,
                bytes: loaded.bytes,
                expected_revision: loaded.semantic_revision,
            })
        })
        .collect()
}

fn apply_prepared_materialization(
    service: &MemoryService,
    plan: &MaintenancePlan,
    request: &RepositoryMaintenanceMaterializationRequest,
    prepared: PreparedMaintenanceMaterialization,
) -> Result<RepositoryMaintenanceMaterializationResult> {
    let projections = maintenance_projections(&service.paths, &prepared)?;
    let safety_values = maintenance_safety_values(plan, request, &prepared)?;
    let authorization = authorize_repository_projection_batch(
        &service.paths,
        RepositoryWriteRoute::Maintenance,
        OkfProposalSensitivity::RepoSafe,
        ScopeKind::Repo,
        None,
        Visibility::Repo,
        AuthorizationProof::Maintenance {
            operation_id: &prepared.decision_id,
        },
        explicit_repository_provenance(
            RepositoryContentClass::GeneralRepoKnowledge,
            &prepared.decision_id,
        ),
        &safety_values,
        &projections,
    )?;
    let mutation = RepositoryMutationAuthorization {
        route: RepositoryWriteRoute::Maintenance,
        authorization: &authorization,
        projections: &projections,
    };
    let identities = prepared
        .outputs
        .iter()
        .map(|output| {
            capture_authorized_existing_repository_projection_identity(
                &service.paths,
                mutation,
                &service.paths.project_root.join(&output.spec.path),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let mut journal = journal::build_journal(
        &service.paths,
        plan,
        &prepared,
        &identities,
        &authorization,
        &projections,
        safety_fields_digest(&safety_values),
    )?;
    if let Err(error) = stage_and_backup(&service.paths, mutation, &prepared, &identities, &journal)
    {
        return match journal::cleanup_artifacts_only(&service.paths, &journal) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(error).context(format!(
                "repository maintenance pre-journal cleanup also failed: {cleanup:#}"
            )),
        };
    }
    if let Err(error) = journal::write_journal(&service.paths, &journal) {
        return match journal::load_journal(&service.paths) {
            Ok(Some(loaded)) if loaded.journal == journal => Err(error).context(
                "repository maintenance journal was installed; recovery artifacts were retained",
            ),
            Ok(None) => match journal::cleanup_artifacts_only(&service.paths, &journal) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(error).context(format!(
                    "repository maintenance journal failure cleanup also failed: {cleanup:#}"
                )),
            },
            Ok(Some(_)) | Err(_) => Err(error).context(
                "repository maintenance journal publication is ambiguous; recovery artifacts were retained",
            ),
        };
    }
    maintenance_transition("after_journal", 0);
    let mut loaded_journal = journal::load_journal(&service.paths)?
        .context("durable repository maintenance journal disappeared before installation")?;
    ensure!(
        loaded_journal.journal == journal,
        "durable repository maintenance journal changed before installation"
    );
    let mut tx = match Transaction::new_unchecked(&service.conn, TransactionBehavior::Immediate) {
        Ok(tx) => tx,
        Err(error) => {
            return cleanup_verified_pre_state(
                service,
                &prepared,
                &loaded_journal,
                anyhow::Error::new(error).context("failed to begin maintenance index transaction"),
                None,
            );
        }
    };
    let final_records = prepared
        .outputs
        .iter()
        .map(|output| output.record.clone())
        .collect::<Vec<_>>();
    if let Err(error) = okf::import_okf_records(&tx, &final_records)
        .context("failed to prepare maintenance index projection")
    {
        let rollback = tx.rollback();
        return cleanup_verified_pre_state(
            service,
            &prepared,
            &loaded_journal,
            error,
            rollback.err().map(anyhow::Error::new),
        );
    }
    let mut installed: Vec<InstalledRepositoryProjection> = Vec::new();
    for (index, (output, identity)) in prepared.outputs.iter().zip(identities).enumerate() {
        let destination = service.paths.project_root.join(&output.spec.path);
        maintenance_transition("before_install", index);
        match install_authorized_repository_projection(
            &service.paths,
            mutation,
            &destination,
            Some(identity),
        ) {
            Ok(value) => {
                installed.push(value);
                maintenance_transition("after_install", index);
            }
            Err(error) => {
                let rollback_tx = tx.rollback().err().map(anyhow::Error::new);
                let error = rollback_failed_install_if_present(
                    &service.paths,
                    mutation,
                    &destination,
                    error,
                );
                let error = rollback_installed(&service.paths, mutation, &mut installed, error);
                return cleanup_verified_pre_state(
                    service,
                    &prepared,
                    &loaded_journal,
                    error,
                    rollback_tx,
                );
            }
        }
    }
    for (entry, installed_output) in journal.outputs.iter_mut().zip(&installed) {
        let identity = installed_output.new_identity();
        entry.post_device = identity.device;
        entry.post_inode = identity.inode;
    }
    loaded_journal = match journal::rewrite_journal(&service.paths, &loaded_journal, &journal) {
        Ok(loaded) => loaded,
        Err(error) => {
            let rollback_tx = tx.rollback().err().map(anyhow::Error::new);
            let error = rollback_installed(&service.paths, mutation, &mut installed, error);
            match journal::load_journal(&service.paths) {
                Ok(Some(current)) if current.journal.transaction_id == journal.transaction_id => {
                    return cleanup_verified_pre_state(
                        service,
                        &prepared,
                        &current,
                        error,
                        rollback_tx,
                    );
                }
                _ => {
                    let verification = sync_directory(&service.paths.records_dir())
                        .and_then(|_| verify_complete_pre_state(&service.paths, &prepared));
                    return match verification {
                        Ok(()) => Err(error).context(
                            "maintenance journal rewrite failed; exact pre-state was restored but transaction evidence was retained",
                        ),
                        Err(verification) => Err(error).context(format!(
                            "maintenance journal rewrite and rollback verification failed: {verification:#}"
                        )),
                    };
                }
            }
        }
    };
    let verified_post = sync_directory(&service.paths.records_dir())
        .context("failed to sync maintenance canonical records")
        .and_then(|_| verify_complete_post_state(&service.paths, &prepared));
    if let Err(error) = verified_post {
        let rollback_tx = tx.rollback().err().map(anyhow::Error::new);
        let error = rollback_installed(&service.paths, mutation, &mut installed, error);
        return cleanup_verified_pre_state(service, &prepared, &loaded_journal, error, rollback_tx);
    }
    maintenance_transition("after_canonical_commit", prepared.outputs.len());

    // Canonical commit point: every intended file is installed, synced, and
    // verified. Index or cleanup failures after this point must preserve post.
    if let Err(error) = journal::append_commit_marker(&tx, &journal)
        .and_then(|_| service.ensure_repository_index_current_with_conn(&tx))
    {
        let rollback = tx.rollback();
        return Err(error).context(format!(
            "canonical maintenance files are committed; index recovery is required{}",
            rollback
                .err()
                .map(|failure| format!("; index rollback also failed: {failure}"))
                .unwrap_or_default()
        ));
    }
    if let Err(error) = tx.execute_batch("COMMIT") {
        let rollback = tx.execute_batch("ROLLBACK");
        tx.set_drop_behavior(DropBehavior::Ignore);
        return Err(anyhow::Error::new(error)).context(format!(
            "canonical maintenance files are committed; index commit recovery is required{}",
            rollback
                .err()
                .map(|failure| format!("; explicit index rollback also failed: {failure}"))
                .unwrap_or_default()
        ));
    }
    tx.set_drop_behavior(DropBehavior::Ignore);
    drop(tx);
    maintenance_transition("after_index_commit", prepared.outputs.len());
    journal::cleanup(&service.paths, &loaded_journal)
        .context("canonical maintenance files are committed; transaction cleanup is required")?;
    maintenance_transition("after_cleanup", prepared.outputs.len());
    materialization_result(
        &prepared,
        &service.paths,
        MaterializationOutputOutcome::Written,
    )
}

fn stage_and_backup(
    paths: &crate::MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    prepared: &PreparedMaintenanceMaterialization,
    identities: &[super::repository_mutation::RepositoryFileIdentity],
    journal: &journal::MaintenanceMaterializationJournal,
) -> Result<()> {
    for (index, output) in prepared.outputs.iter().enumerate() {
        maintenance_transition("before_stage", index);
        stage_authorized_file(
            paths,
            RepositoryWriteRoute::Maintenance,
            mutation.authorization,
            mutation.projections,
            &paths.project_root.join(&output.spec.path),
            &output.markdown,
            &journal.transaction_id,
        )?;
        maintenance_transition("after_stage", index);
    }
    for (index, ((output, identity), entry)) in prepared
        .outputs
        .iter()
        .zip(identities)
        .zip(&journal.outputs)
        .enumerate()
    {
        maintenance_transition("before_backup", index);
        copy_repository_file_to_transaction_with_identity(
            paths,
            mutation,
            &paths.project_root.join(&output.spec.path),
            &journal::backup_path(paths, journal, entry),
            &journal::hash(&output.prior.bytes),
            *identity,
        )?;
        maintenance_transition("after_backup", index);
    }
    Ok(())
}

fn cleanup_verified_pre_state<T>(
    service: &MemoryService,
    prepared: &PreparedMaintenanceMaterialization,
    loaded_journal: &journal::LoadedMaintenanceJournal,
    error: anyhow::Error,
    rollback_error: Option<anyhow::Error>,
) -> Result<T> {
    let verification = sync_directory(&service.paths.records_dir())
        .context("failed to sync restored maintenance pre-state")
        .and_then(|_| verify_complete_pre_state(&service.paths, prepared));
    if let Err(verification_error) = verification {
        return Err(error).context(format!(
            "maintenance rollback did not restore exact pre-state: {verification_error:#}{}",
            rollback_error
                .map(|failure| format!("; index rollback also failed: {failure:#}"))
                .unwrap_or_default()
        ));
    }
    match journal::cleanup(&service.paths, loaded_journal) {
        Ok(()) => Err(error),
        Err(cleanup) => Err(error.context(format!(
            "maintenance rollback cleanup failed: {cleanup:#}{}",
            rollback_error
                .map(|failure| format!("; index rollback also failed: {failure:#}"))
                .unwrap_or_default()
        ))),
    }
}

fn rollback_failed_install_if_present(
    paths: &crate::MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    destination: &Path,
    error: anyhow::Error,
) -> anyhow::Error {
    match capture_authorized_repository_projection_identity(
        paths,
        mutation,
        destination,
        RepositoryProjectionPurpose::Write,
    ) {
        Ok(identity) => match replace_authorized_repository_projection(
            paths,
            mutation,
            destination,
            RepositoryProjectionPurpose::Existing,
            RepositoryProjectionPurpose::Write,
            identity,
        ) {
            Ok(_) => error,
            Err(rollback) => error.context(format!(
                "failed maintenance install also failed to restore its installed bytes: {rollback:#}"
            )),
        },
        Err(write_error) => match capture_authorized_existing_repository_projection_identity(
            paths,
            mutation,
            destination,
        ) {
            Ok(_) => error,
            Err(prior_error) => error.context(format!(
                "failed maintenance install left an ambiguous target: write check: {write_error:#}; prior check: {prior_error:#}"
            )),
        },
    }
}

fn rollback_installed(
    paths: &crate::MemoryPaths,
    mutation: RepositoryMutationAuthorization<'_>,
    installed: &mut Vec<InstalledRepositoryProjection>,
    error: anyhow::Error,
) -> anyhow::Error {
    let mut failures = Vec::new();
    for item in installed.drain(..).rev() {
        if let Err(rollback) = rollback_authorized_repository_projection(paths, mutation, &item) {
            failures.push(format!("{rollback:#}"));
        }
    }
    if failures.is_empty() {
        error
    } else {
        error.context(format!(
            "maintenance rollback also failed: {}",
            failures.join("; ")
        ))
    }
}

fn maintenance_projections(
    paths: &crate::MemoryPaths,
    prepared: &PreparedMaintenanceMaterialization,
) -> Result<Vec<OwnedRepositoryProjection>> {
    let mut projections = Vec::new();
    for output in &prepared.outputs {
        let absolute = paths.project_root.join(&output.spec.path);
        let prior_hash = blake3::hash(&output.prior.bytes).to_hex().to_string();
        projections.push(OwnedRepositoryProjection::from_absolute(
            paths,
            &absolute,
            output.markdown.as_bytes(),
            Some(&prior_hash),
        )?);
        projections.push(OwnedRepositoryProjection::existing_from_absolute(
            paths,
            &absolute,
            &output.prior.bytes,
            &prior_hash,
        )?);
    }
    for comparison in &prepared.comparisons {
        let hash = blake3::hash(&comparison.bytes).to_hex().to_string();
        projections.push(OwnedRepositoryProjection::existing_from_absolute(
            paths,
            &paths.project_root.join(&comparison.path),
            &comparison.bytes,
            &hash,
        )?);
    }
    Ok(projections)
}

fn maintenance_safety_values(
    plan: &MaintenancePlan,
    request: &RepositoryMaintenanceMaterializationRequest,
    prepared: &PreparedMaintenanceMaterialization,
) -> Result<Vec<super::repository_mutation::RepositorySafetyValue>> {
    let mut values = Vec::new();
    for (location, value) in [
        ("maintenance.plan", serde_json::to_vec(plan)?),
        ("maintenance.request", serde_json::to_vec(request)?),
        (
            "maintenance.selection",
            serde_json::to_vec(&prepared.selection.selected_actions)?,
        ),
        (
            "maintenance.decision",
            serde_json::to_vec(&prepared.decision)?,
        ),
    ] {
        values.push(safety_value(
            location.to_owned(),
            SafetyFieldKind::RenderedProjection,
            value,
        ));
    }
    for (index, output) in prepared.outputs.iter().enumerate() {
        values.extend(memory_draft_safety_values(
            &format!("maintenance.outputs[{index}].draft"),
            &output.record.draft,
        ));
        values.push(safety_value(
            format!("maintenance.outputs[{index}].record"),
            SafetyFieldKind::RenderedProjection,
            serde_json::to_vec(&output.record)?,
        ));
        values.push(safety_value(
            format!("maintenance.outputs[{index}].bytes"),
            SafetyFieldKind::RenderedProjection,
            output.markdown.as_bytes(),
        ));
        values.push(safety_value(
            format!("maintenance.outputs[{index}].path"),
            SafetyFieldKind::Path,
            output.spec.path.as_os_str().as_encoded_bytes(),
        ));
        values.push(safety_value(
            format!("maintenance.outputs[{index}].reason"),
            SafetyFieldKind::Reason,
            &output.spec.reason,
        ));
    }
    Ok(values)
}

fn safety_fields_digest(values: &[super::repository_mutation::RepositorySafetyValue]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"memzoi.repository-maintenance.safety-fields\0");
    for value in values {
        hasher.update(&(value.location.len() as u64).to_le_bytes());
        hasher.update(value.location.as_bytes());
        let kind = format!("{:?}", value.kind);
        hasher.update(&(kind.len() as u64).to_le_bytes());
        hasher.update(kind.as_bytes());
        hasher.update(&(value.value.len() as u64).to_le_bytes());
        hasher.update(&value.value);
    }
    hasher.finalize().to_hex().to_string()
}

fn encode_digest(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn verify_complete_pre_state(
    paths: &crate::MemoryPaths,
    prepared: &PreparedMaintenanceMaterialization,
) -> Result<()> {
    for output in &prepared.outputs {
        let bytes = read_repository_exact(
            paths,
            &output.spec.path,
            output.prior.bytes.len() as u64,
            "repository maintenance prior state",
        )?
        .context("repository maintenance prior state is missing")?;
        ensure!(
            bytes == output.prior.bytes,
            "repository maintenance prior state was not restored exactly"
        );
    }
    verify_comparison_bytes(paths, &prepared.comparisons)
}

fn verify_complete_post_state(
    paths: &crate::MemoryPaths,
    prepared: &PreparedMaintenanceMaterialization,
) -> Result<()> {
    for output in &prepared.outputs {
        let bytes = read_repository_exact(
            paths,
            &output.spec.path,
            output.markdown.len() as u64,
            "repository maintenance intended state",
        )?
        .context("repository maintenance intended state is missing")?;
        ensure!(
            bytes == output.markdown.as_bytes(),
            "repository maintenance intended state does not match exactly"
        );
    }
    verify_comparison_bytes(paths, &prepared.comparisons)
}

fn verify_comparison_bytes(
    paths: &crate::MemoryPaths,
    comparisons: &[PreparedComparison],
) -> Result<()> {
    for comparison in comparisons {
        let bytes = read_repository_exact(
            paths,
            &comparison.path,
            comparison.bytes.len() as u64,
            "repository maintenance comparison state",
        )?
        .context("repository maintenance comparison state is missing")?;
        ensure!(
            bytes == comparison.bytes,
            "repository maintenance comparison state changed"
        );
    }
    Ok(())
}

fn materialization_result(
    prepared: &PreparedMaintenanceMaterialization,
    paths: &crate::MemoryPaths,
    outcome: MaterializationOutputOutcome,
) -> Result<RepositoryMaintenanceMaterializationResult> {
    let result = RepositoryMaintenanceMaterializationResult {
        schema: REPOSITORY_MAINTENANCE_MATERIALIZATION_RESULT_SCHEMA.to_owned(),
        plan_id: prepared.selection.plan_id.clone(),
        selection_id: prepared.selection.selection_id.clone(),
        decision_id: prepared.decision_id.clone(),
        decision_at: prepared.decision.decision_at.clone(),
        selected_actions: prepared.selection.selected_actions.clone(),
        outputs: prepared
            .outputs
            .iter()
            .map(|output| RepositoryMaintenanceMaterializationOutputResult {
                action_id: output.spec.action_id.clone(),
                path: path_text(&output.spec.path).expect("validated maintenance path"),
                record_id: output.spec.record_id.clone(),
                action: output.spec.action,
                role: output.spec.role,
                semantic_revision: output.intent.intended_semantic_revision.clone(),
                outcome,
            })
            .collect(),
        review_commands: review_commands(paths, &prepared.outputs)?,
    };
    result.validate()?;
    Ok(result)
}

fn review_commands(
    paths: &crate::MemoryPaths,
    outputs: &[PreparedOutput],
) -> Result<Vec<RepositoryReviewCommand>> {
    let project_root = paths
        .project_root
        .to_str()
        .context("repository root is not UTF-8 for structured review guidance")?;
    let mut commands = Vec::with_capacity(outputs.len());
    for output in outputs {
        let path = path_text(&output.spec.path)?;
        match git_repository::git_review_visibility(&paths.project_root, &output.spec.path)
            .map_err(anyhow::Error::new)?
        {
            git_repository::GitReviewVisibility::Tracked => {
                commands.push(RepositoryReviewCommand {
                    program: "git".to_owned(),
                    args: vec![
                        "--no-optional-locks".to_owned(),
                        "-C".to_owned(),
                        project_root.to_owned(),
                        "diff".to_owned(),
                        "--".to_owned(),
                        path,
                    ],
                });
            }
            git_repository::GitReviewVisibility::UntrackedAndNotIgnored => {
                commands.push(RepositoryReviewCommand {
                    program: "git".to_owned(),
                    args: vec![
                        "--no-optional-locks".to_owned(),
                        "-C".to_owned(),
                        project_root.to_owned(),
                        "diff".to_owned(),
                        "--no-index".to_owned(),
                        "--".to_owned(),
                        "/dev/null".to_owned(),
                        path,
                    ],
                });
            }
            git_repository::GitReviewVisibility::IgnoredUntracked => {
                bail!("maintenance output is ignored by Git")
            }
        }
    }
    Ok(commands)
}

fn reconcile_materialized_index(service: &MemoryService, outputs: &[PreparedOutput]) -> Result<()> {
    let tx = service.conn.unchecked_transaction()?;
    let records = outputs
        .iter()
        .map(|output| output.record.clone())
        .collect::<Vec<_>>();
    okf::import_okf_records(&tx, &records)?;
    service.ensure_repository_index_current_with_conn(&tx)?;
    tx.commit()?;
    Ok(())
}

fn load_record(
    paths: &crate::MemoryPaths,
    snapshot: &MaintenanceRecordSnapshot,
) -> Result<LoadedRecord> {
    let loaded = load_record_without_revision_check(paths, snapshot)?;
    let expected = plan_revision_for_snapshot(snapshot)?;
    if &loaded.semantic_revision != expected {
        bail!("repository maintenance record revision no longer matches its plan");
    }
    Ok(loaded)
}

fn load_record_without_revision_check(
    paths: &crate::MemoryPaths,
    snapshot: &MaintenanceRecordSnapshot,
) -> Result<LoadedRecord> {
    let MaintenanceRecordVersion::CanonicalRepository { source_path, .. } = &snapshot.version
    else {
        bail!("maintenance record is not a repository record");
    };
    let path = PathBuf::from(source_path);
    let absolute = paths.project_root.join(&path);
    let metadata = std::fs::symlink_metadata(&absolute)
        .context("failed to inspect repository maintenance record")?;
    ensure!(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() <= MAINTENANCE_MAX_INPUT_FILE_BYTES as u64,
        "repository maintenance record is not a safe bounded regular file"
    );
    let bytes = read_repository_exact(
        paths,
        &path,
        metadata.len(),
        "repository maintenance record",
    )?
    .context("repository maintenance record is missing")?;
    let loaded = parse_loaded_record(paths, path, bytes)?;
    if loaded.record.concept_id != snapshot.record_id {
        bail!("repository maintenance record ID does not match its path");
    }
    Ok(loaded)
}

fn parse_loaded_record(
    paths: &crate::MemoryPaths,
    path: PathBuf,
    bytes: Vec<u8>,
) -> Result<LoadedRecord> {
    let markdown =
        std::str::from_utf8(&bytes).context("repository maintenance record is not UTF-8")?;
    let record = okf::parse_okf_record_markdown(
        paths.records_dir(),
        paths.project_root.join(&path),
        markdown,
    )?
    .context("repository maintenance record is not canonical OKF")?;
    let semantic_revision = canonical_revision_for_okf_record(&record)?;
    Ok(LoadedRecord {
        path,
        bytes,
        record,
        semantic_revision,
    })
}

fn read_repository_exact(
    paths: &crate::MemoryPaths,
    relative: &Path,
    expected_len: u64,
    label: &str,
) -> Result<Option<Vec<u8>>> {
    repository_io::read_repository_file_if_exists(
        &paths.project_root,
        relative,
        expected_len,
        label,
    )
}

fn prior_lifecycle(record: &OkfRecordFile) -> PriorCanonicalLifecycleProjection {
    let lifecycle = record
        .materialization
        .as_ref()
        .map(RepositoryMaterializationMetadata::lifecycle_projection)
        .unwrap_or_default();
    PriorCanonicalLifecycleProjection {
        status: record.status,
        supersedes_id: record.supersedes_id.clone(),
        lineage: record.lineage.clone(),
        action: lifecycle.action,
        counterpart_record_id: lifecycle.counterpart_record_id,
        counterpart_expected_revision: lifecycle.target_expected_revision,
        counterpart_relationship: lifecycle.counterpart_relationship,
        reason: lifecycle.reason,
    }
}

fn validate_fresh_time(
    plan: &MaintenancePlan,
    request: &RepositoryMaintenanceMaterializationRequest,
    now: OffsetDateTime,
) -> Result<()> {
    let evaluated = OffsetDateTime::parse(&plan.evaluated_at, &Rfc3339)?;
    let not_after = OffsetDateTime::parse(&plan.not_after, &Rfc3339)?;
    let decision = OffsetDateTime::parse(&request.decision_at, &Rfc3339)?;
    if decision < evaluated || decision >= not_after || decision > now || now >= not_after {
        bail!("repository maintenance decision is outside the current plan validity window");
    }
    Ok(())
}

fn selected_paths(prepared: &PreparedMaintenanceMaterialization) -> Vec<PathBuf> {
    let mut paths = prepared
        .outputs
        .iter()
        .map(|output| output.spec.path.clone())
        .chain(
            prepared
                .comparisons
                .iter()
                .map(|comparison| comparison.path.clone()),
        )
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn relationship_for(role: MaterializationOutputRole) -> MaterializationCounterpartRelationship {
    match role {
        MaterializationOutputRole::CanonicalRecord => {
            MaterializationCounterpartRelationship::Supersedes
        }
        MaterializationOutputRole::LifecycleCounterpart => {
            MaterializationCounterpartRelationship::SupersededBy
        }
    }
}

fn plan_snapshot<'a>(
    plan: &'a MaintenancePlan,
    record_id: &str,
) -> Result<&'a MaintenanceRecordSnapshot> {
    plan.records
        .binary_search_by_key(&record_id, |snapshot| snapshot.record_id.as_str())
        .ok()
        .map(|index| &plan.records[index])
        .context("maintenance plan record snapshot is missing")
}

fn plan_revision<'a>(
    records: &BTreeMap<&str, &'a MaintenanceRecordSnapshot>,
    record_id: &str,
) -> Result<&'a CanonicalRevision> {
    plan_revision_for_snapshot(
        records
            .get(record_id)
            .copied()
            .context("maintenance plan record snapshot is missing")?,
    )
}

fn plan_revision_for_snapshot(snapshot: &MaintenanceRecordSnapshot) -> Result<&CanonicalRevision> {
    snapshot
        .version
        .repository_revision()
        .context("maintenance plan record does not have a canonical revision")
}

fn canonical_path(
    records: &BTreeMap<&str, &MaintenanceRecordSnapshot>,
    record_id: &str,
) -> Result<PathBuf> {
    let snapshot = records
        .get(record_id)
        .context("maintenance plan record snapshot is missing")?;
    let MaintenanceRecordVersion::CanonicalRepository { source_path, .. } = &snapshot.version
    else {
        bail!("maintenance plan record is not repository-backed");
    };
    Ok(PathBuf::from(source_path))
}

fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .context("maintenance path is not UTF-8")
}

fn bounded_record_reason(prefix: &str, record_id: &str) -> String {
    let full = format!("{prefix}{record_id}");
    if full.len() <= crate::MAX_MATERIALIZATION_REASON_BYTES {
        full
    } else {
        format!(
            "{prefix}blake3:{}",
            blake3::hash(record_id.as_bytes()).to_hex()
        )
    }
}

#[cfg(unix)]
fn ensure_repository_maintenance_platform_supported() -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn ensure_repository_maintenance_platform_supported() -> Result<()> {
    bail!("secure repository maintenance materialization is unavailable on this platform")
}

#[cfg(test)]
mod tests {
    #[test]
    fn record_reasons_remain_bounded_for_maximal_identifiers() {
        let reason = super::bounded_record_reason("exact duplicate of keeper ", &"a".repeat(255));
        assert!(reason.len() <= crate::MAX_MATERIALIZATION_REASON_BYTES);
        assert!(reason.contains("blake3:"));
    }
}
