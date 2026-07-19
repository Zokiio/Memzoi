use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    MaintenanceAction, MaintenanceActionClass, MaintenanceActionGroupKind, MaintenancePlan,
    MaintenancePlanRequest, MaintenanceScope, MemoryDestination, MemoryLane, MemoryRecord,
    MemoryStatus, MemoryType, OriginDescriptor, OriginRoute, PRIVATE_LIFECYCLE_GRANT_SCHEMA,
    PRIVATE_LIFECYCLE_POLICY_VERSION, PRIVATE_LIFECYCLE_RECORD_INSPECTION_SCHEMA,
    PRIVATE_LIFECYCLE_RESULT_SCHEMA, PrivateLifecycleAction, PrivateLifecycleActionResult,
    PrivateLifecycleApplyResult, PrivateLifecycleGrant, PrivateLifecycleRecordInspection,
    PrivateLifecycleRelationSnapshot, PrivateLifecycleRequest, PrivateLifecycleRevokeOutcome,
    PrivateLifecycleRevokeResult, PrivateLifecycleSource, PrivateLifecycleStateSnapshot,
    PrivateMaintenanceRecordInput, ScopeKind, Visibility, plan_private_maintenance_at,
    private_maintenance_runtime_fingerprint,
};

use super::{
    MemoryService,
    runtime_records::{
        OwnerActionGrantRow, OwnerActionGrantState as StoredGrantState,
        PrivateLifecycleApplicationRow, PrivateLifecycleRelation, PrivateLifecycleRelationKind,
        PrivateLifecycleState, PrivateLifecycleStorage, RevokeGrantOutcome, RuntimeRecords,
        lifecycle_generation,
    },
    safe_files::RepoLifecycleLock,
    shared_runtime,
};

const GRANT_BINDING_SCHEMA: &str = "memzoi/private-lifecycle-grant-binding";
const MAX_GRANT_LIFETIME: Duration = Duration::hours(24);

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleApplyFault {
    AfterValidation,
    DuringMutations,
    AfterRenewalPredecessorStatus,
    AfterRenewalSuccessorLink,
    AfterRenewalRelation,
    AfterCorrectionReplacementInsert,
    AfterCorrectionReplacementState,
    AfterCorrectionTagCopy,
    AfterCorrectionPathCopy,
    AfterCorrectionPredecessorStatus,
    AfterCorrectionRelation,
    AfterSupersessionPredecessorStatus,
    AfterSupersessionSuccessorLink,
    AfterSupersessionRelation,
    AfterConsolidationMemberStatus,
    AfterConsolidationMemberRelation,
    AfterContradictionMemberStatus,
    AfterContradictionMemberRelation,
    AfterAuditInsert,
    AfterReceiptInsert,
    AfterGrantConsume,
    AfterSharedCommitBeforeMirror,
}

#[cfg(test)]
std::thread_local! {
    static LIFECYCLE_APPLY_FAULT: std::cell::Cell<Option<LifecycleApplyFault>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn fail_lifecycle_apply_at(stage: LifecycleApplyFault) -> Result<()> {
    if LIFECYCLE_APPLY_FAULT.get() == Some(stage) {
        bail!("test_injected_private_lifecycle_apply_failure:{stage:?}");
    }
    Ok(())
}

#[cfg(test)]
type AfterPrivateLifecycleRecordSnapshotHook = Box<dyn FnOnce() -> Result<()>>;

#[cfg(test)]
std::thread_local! {
    static AFTER_PRIVATE_LIFECYCLE_RECORD_SNAPSHOT_HOOK:
        std::cell::RefCell<Option<AfterPrivateLifecycleRecordSnapshotHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn inject_after_private_lifecycle_record_snapshot_hook(
    hook: impl FnOnce() -> Result<()> + 'static,
) {
    AFTER_PRIVATE_LIFECYCLE_RECORD_SNAPSHOT_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_private_lifecycle_record_snapshot_hook() -> Result<()> {
    AFTER_PRIVATE_LIFECYCLE_RECORD_SNAPSHOT_HOOK
        .with(|slot| slot.borrow_mut().take())
        .map_or(Ok(()), |hook| hook())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantBinding {
    schema: String,
    request: PrivateLifecycleRequest,
    policy_version: String,
    records: Vec<GrantRecordBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<GrantPlanBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantRecordBinding {
    record_id: String,
    expected_version: String,
    memory_type: MemoryType,
    lane: MemoryLane,
    destination: MemoryDestination,
    scope_kind: ScopeKind,
    scope_id: Option<String>,
    visibility: Visibility,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantPlanBinding {
    plan_id: String,
    selected_action_ids: Vec<String>,
    runtime_fingerprint: String,
    policy_digest: String,
    detector_digest: String,
    not_after: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LifecycleAuditPayload {
    grant_id: String,
    application_id: String,
    operation_id: String,
    action_kinds: Vec<String>,
    target_record_ids: Vec<String>,
    applied_at: String,
}

struct PreparedApply {
    replacement_ids: BTreeMap<usize, String>,
}

impl MemoryService {
    /// Build private maintenance evidence exclusively from the authoritative
    /// shared runtime. This method performs no database or filesystem writes.
    pub fn plan_private_lifecycle(
        &self,
        record_ids: Vec<String>,
        evaluated_at: Option<String>,
    ) -> Result<MaintenancePlan> {
        let transaction =
            Transaction::new_unchecked(&self.shared_conn, TransactionBehavior::Deferred)?;
        let plan = self.plan_private_lifecycle_with_conn(
            &transaction,
            MaintenancePlanRequest {
                schema: crate::MAINTENANCE_REQUEST_SCHEMA.to_owned(),
                evaluated_at,
                record_ids,
            },
            self.now(),
        )?;
        transaction.rollback()?;
        Ok(plan)
    }

    /// Authorize the exact immutable request. Apart from returning an existing
    /// identical active grant, the only allowed write is one grant-row insert.
    pub fn authorize_private_lifecycle(
        &self,
        request: &PrivateLifecycleRequest,
        plan: Option<&MaintenancePlan>,
        requested_expires_at: Option<&str>,
    ) -> Result<PrivateLifecycleGrant> {
        request.validate()?;
        ensure!(
            self._lifecycle_read_lock.is_none(),
            "lifecycle authorization requires an authority service"
        );
        let _lock = RepoLifecycleLock::acquire(&self.paths)?;
        let now = self.now();
        let authorized_at = crate::expiry::format_timestamp(now)?;
        let transaction =
            Transaction::new_unchecked(&self.shared_conn, TransactionBehavior::Immediate)?;
        let storage = PrivateLifecycleStorage::new(&transaction);

        // An operation ID is permanently one-request-only once application is
        // recorded. Re-authorization of the exact request returns its
        // historical consumed authority; it must never mint a fresh grant
        // whose expected versions can no longer be current after application.
        if let Some(application) = storage.application_by_operation_id(&request.operation_id)? {
            if application.request_id != request.request_id {
                transaction.rollback()?;
                bail!(
                    "operation_id_conflict: {} is already bound to a different request",
                    request.operation_id
                );
            }
            let historical = storage
                .grant(&application.grant_id)?
                .context("recorded lifecycle application has no historical grant")?;
            let historical_binding: GrantBinding =
                serde_json::from_str(&historical.request_json)
                    .context("stored owner grant binding is invalid")?;
            let result = grant_from_row(&historical, &historical_binding)?;
            ensure!(
                result.state == crate::OwnerActionGrantState::Consumed,
                "recorded lifecycle application grant is not consumed"
            );
            transaction.rollback()?;
            return Ok(result);
        }

        let binding = self.build_grant_binding(&transaction, request, plan, now, true)?;
        // Authorization validates the complete action group but performs none
        // of its mutations. Apply repeats this validation under its lock and
        // transaction before consuming authority.
        validate_action_group(&transaction, request, now)?;
        let binding_json = canonical_json(&binding)?;
        let effective_expiry = effective_grant_expiry(now, requested_expires_at, plan)?;
        let expires_at = crate::expiry::format_timestamp(effective_expiry)?;
        let mut identical_live_grant = None;
        for candidate in storage.identical_active_grants(&request.request_id, &binding_json)? {
            let existing_expiry = crate::private_lifecycle::parse_timestamp(
                &candidate.expires_at,
                "stored owner grant expires_at",
            )?;
            if existing_expiry > now {
                identical_live_grant = Some((candidate, existing_expiry));
                break;
            }
        }
        if let Some((existing, existing_expiry)) = identical_live_grant {
            if existing_expiry > effective_expiry {
                transaction.rollback()?;
                bail!(
                    "owner_action_grant_expiry_conflict: identical active grant {} expires at {}, later than the requested authority {}; revoke it before explicitly authorizing shorter authority",
                    existing.grant_id,
                    existing.expires_at,
                    expires_at
                );
            }
            let result = grant_from_row(&existing, &binding)?;
            transaction.rollback()?;
            return Ok(result);
        }
        let row = OwnerActionGrantRow {
            grant_id: format!("grant_{}", Uuid::now_v7()),
            request_id: request.request_id.clone(),
            request_json: binding_json,
            state: StoredGrantState::Active,
            authorized_at,
            expires_at,
            revoked_at: None,
            consumed_at: None,
            consumed_application_id: None,
        };
        storage.insert_grant(&row)?;
        let result = grant_from_row(&row, &binding)?;
        transaction.commit()?;
        Ok(result)
    }

    /// Revoke authority only; private records, events, relations, receipts,
    /// versions, and mirror generations are deliberately untouched.
    pub fn revoke_private_lifecycle(&self, grant_id: &str) -> Result<PrivateLifecycleRevokeResult> {
        ensure_identifier(grant_id, "grant_id")?;
        ensure!(
            self._lifecycle_read_lock.is_none(),
            "lifecycle revocation requires an authority service"
        );
        let _lock = RepoLifecycleLock::acquire(&self.paths)?;
        let revoked_at = self.now_timestamp()?;
        let transaction =
            Transaction::new_unchecked(&self.shared_conn, TransactionBehavior::Immediate)?;
        let outcome =
            PrivateLifecycleStorage::new(&transaction).revoke_grant(grant_id, &revoked_at)?;
        let outcome = match outcome {
            RevokeGrantOutcome::Revoked => {
                transaction.commit()?;
                PrivateLifecycleRevokeOutcome::Revoked
            }
            RevokeGrantOutcome::AlreadyRevoked => {
                transaction.rollback()?;
                PrivateLifecycleRevokeOutcome::AlreadyRevoked
            }
            RevokeGrantOutcome::AlreadyConsumed => {
                transaction.rollback()?;
                PrivateLifecycleRevokeOutcome::AlreadyConsumed
            }
            RevokeGrantOutcome::Missing => {
                transaction.rollback()?;
                bail!("owner_action_grant_not_found: {grant_id}")
            }
        };
        Ok(PrivateLifecycleRevokeResult {
            grant_id: grant_id.to_owned(),
            outcome,
        })
    }

    pub fn inspect_private_lifecycle_record(
        &self,
        record_id: &str,
    ) -> Result<PrivateLifecycleRecordInspection> {
        ensure_identifier(record_id, "record_id")?;
        let transaction =
            Transaction::new_unchecked(&self.shared_conn, TransactionBehavior::Deferred)?;
        let record = require_private_record(&transaction, record_id)?;
        #[cfg(test)]
        run_after_private_lifecycle_record_snapshot_hook()?;
        let storage = PrivateLifecycleStorage::new(&transaction);
        let state = storage.require_state(record_id)?;
        let relations = storage
            .relations_for_record(record_id)?
            .into_iter()
            .map(relation_snapshot)
            .collect();
        let inspection = PrivateLifecycleRecordInspection {
            schema: PRIVATE_LIFECYCLE_RECORD_INSPECTION_SCHEMA.to_owned(),
            record,
            version: state.record_version.clone(),
            state: state_snapshot(&state),
            relations,
        };
        transaction.rollback()?;
        Ok(inspection)
    }

    pub fn inspect_private_lifecycle_grant(&self, grant_id: &str) -> Result<PrivateLifecycleGrant> {
        ensure_identifier(grant_id, "grant_id")?;
        let row = PrivateLifecycleStorage::new(&self.shared_conn)
            .grant(grant_id)?
            .with_context(|| format!("owner_action_grant_not_found: {grant_id}"))?;
        let binding: GrantBinding = serde_json::from_str(&row.request_json)
            .context("stored owner grant binding is invalid")?;
        validate_stored_binding(&binding, &row)?;
        grant_from_row(&row, &binding)
    }

    /// Atomically apply one complete owner action group and consume exactly one
    /// active grant. Mirror convergence occurs only after the shared commit.
    pub fn apply_private_lifecycle(
        &self,
        request: &PrivateLifecycleRequest,
        grant_id: &str,
        plan: Option<&MaintenancePlan>,
    ) -> Result<PrivateLifecycleApplyResult> {
        request.validate()?;
        ensure_identifier(grant_id, "grant_id")?;
        ensure!(
            self._lifecycle_read_lock.is_none(),
            "lifecycle apply requires an authority service"
        );
        let _lock = RepoLifecycleLock::acquire(&self.paths)?;
        let now = self.now();
        let applied_at = crate::expiry::format_timestamp(now)?;
        let transaction =
            Transaction::new_unchecked(&self.shared_conn, TransactionBehavior::Immediate)?;
        let storage = PrivateLifecycleStorage::new(&transaction);

        if let Some(application) = storage.application_by_operation_id(&request.operation_id)? {
            if application.request_id != request.request_id {
                transaction.rollback()?;
                bail!(
                    "operation_id_conflict: {} is already bound to a different request",
                    request.operation_id
                );
            }
            let mut result: PrivateLifecycleApplyResult =
                serde_json::from_str(&application.result_json)
                    .context("stored private lifecycle result is invalid")?;
            transaction.rollback()?;
            shared_runtime::refresh_index_mirrors_locked(
                &self.paths,
                &self.shared_conn,
                &self.conn,
            )
            .context("mirror refresh required after lifecycle application replay")?;
            result.replayed = true;
            return Ok(result);
        }

        let grant = storage
            .grant(grant_id)?
            .with_context(|| format!("owner_action_grant_not_found: {grant_id}"))?;
        ensure!(
            grant.state == StoredGrantState::Active,
            "owner_action_grant_not_active: {grant_id} is {}",
            grant.state.as_str()
        );
        let grant_expiry =
            crate::private_lifecycle::parse_timestamp(&grant.expires_at, "grant expires_at")?;
        ensure!(now < grant_expiry, "owner_action_grant_expired: {grant_id}");
        ensure!(
            grant.request_id == request.request_id,
            "owner_action_grant_request_mismatch"
        );
        let stored_binding: GrantBinding = serde_json::from_str(&grant.request_json)
            .context("stored owner grant binding is invalid")?;
        validate_stored_binding(&stored_binding, &grant)?;
        let current_binding = self.build_grant_binding(&transaction, request, plan, now, true)?;
        ensure!(
            stored_binding == current_binding,
            "owner_action_grant_binding_stale_or_unauthorized"
        );
        let prepared = validate_action_group(&transaction, request, now)?;
        #[cfg(test)]
        fail_lifecycle_apply_at(LifecycleApplyFault::AfterValidation)?;
        let generation_before = lifecycle_generation(&transaction)?;

        let application_id = format!("application_{}", Uuid::now_v7());
        let event_id = format!("evt_{}", Uuid::now_v7());
        let action_results = apply_action_group(
            &transaction,
            request,
            &prepared,
            &application_id,
            &event_id,
            &applied_at,
        )?;
        append_content_free_lifecycle_event(
            &transaction,
            &event_id,
            &application_id,
            grant_id,
            request,
            &action_results,
            &applied_at,
        )?;
        #[cfg(test)]
        fail_lifecycle_apply_at(LifecycleApplyFault::AfterAuditInsert)?;
        let generation = lifecycle_generation(&transaction)?;
        ensure!(
            generation > generation_before,
            "private lifecycle generation did not advance"
        );
        let result = PrivateLifecycleApplyResult {
            schema: PRIVATE_LIFECYCLE_RESULT_SCHEMA.to_owned(),
            application_id: application_id.clone(),
            operation_id: request.operation_id.clone(),
            request_id: request.request_id.clone(),
            grant_id: grant_id.to_owned(),
            applied_at: applied_at.clone(),
            lifecycle_generation: u64::try_from(generation)
                .context("private lifecycle generation is negative")?,
            replayed: false,
            actions: action_results,
        };
        storage.insert_application(&PrivateLifecycleApplicationRow {
            application_id: application_id.clone(),
            operation_id: request.operation_id.clone(),
            request_id: request.request_id.clone(),
            grant_id: grant_id.to_owned(),
            result_json: canonical_json(&result)?,
            lifecycle_generation: generation,
            applied_at: applied_at.clone(),
        })?;
        #[cfg(test)]
        fail_lifecycle_apply_at(LifecycleApplyFault::AfterReceiptInsert)?;
        ensure!(
            storage.consume_active_grant(grant_id, &application_id, &applied_at)?,
            "owner action grant changed before atomic consumption"
        );
        #[cfg(test)]
        fail_lifecycle_apply_at(LifecycleApplyFault::AfterGrantConsume)?;
        transaction.commit()?;

        #[cfg(test)]
        fail_lifecycle_apply_at(LifecycleApplyFault::AfterSharedCommitBeforeMirror)?;

        shared_runtime::refresh_index_mirrors_locked(&self.paths, &self.shared_conn, &self.conn)
            .context("mirror refresh required after committed private lifecycle application")?;
        Ok(result)
    }

    fn plan_private_lifecycle_with_conn(
        &self,
        conn: &Connection,
        request: MaintenancePlanRequest,
        fallback_now: OffsetDateTime,
    ) -> Result<MaintenancePlan> {
        let runtime_fingerprint =
            private_maintenance_runtime_fingerprint(self.paths.repository_key())?;
        let records = RuntimeRecords::new(conn).records_for_preservation()?;
        #[cfg(test)]
        run_after_private_lifecycle_record_snapshot_hook()?;
        ensure!(
            records.len() <= crate::MAINTENANCE_MAX_RECORDS,
            "private maintenance snapshot exceeds the admitted-record limit"
        );
        let evaluated_at = request
            .evaluated_at
            .as_deref()
            .map(|value| crate::private_lifecycle::parse_timestamp(value, "evaluated_at"))
            .transpose()?
            .unwrap_or(fallback_now);
        let storage = PrivateLifecycleStorage::new(conn);
        let mut inputs = Vec::with_capacity(records.len());
        for record in records {
            ensure_private_shape(&record)?;
            let state = storage.require_state(&record.id)?;
            let decision = effective_current_assertion(conn, &record, &state, evaluated_at)?;
            inputs.push(PrivateMaintenanceRecordInput {
                record,
                version_token: state.record_version,
                current_assertion: decision.is_current,
                retention_state: decision.retention.state,
                retention_reason: decision.retention.reason,
                retention_boundary: decision.retention.effective_boundary,
            });
        }
        plan_private_maintenance_at(runtime_fingerprint, request, inputs, fallback_now)
    }

    fn build_grant_binding(
        &self,
        conn: &Connection,
        request: &PrivateLifecycleRequest,
        plan: Option<&MaintenancePlan>,
        now: OffsetDateTime,
        revalidate_plan: bool,
    ) -> Result<GrantBinding> {
        validate_request_plan_binding(self, conn, request, plan, now, revalidate_plan)?;
        let mut expected = BTreeMap::<String, String>::new();
        for action in &request.actions {
            for (record_id, version) in action.expected_versions() {
                if let Some(prior) = expected.insert(record_id.to_owned(), version.to_owned()) {
                    ensure!(
                        prior == version,
                        "private record {record_id} has conflicting expected versions"
                    );
                }
            }
        }
        let mut records = Vec::with_capacity(expected.len());
        for (record_id, expected_version) in expected {
            let record = require_private_record(conn, &record_id)?;
            let state = PrivateLifecycleStorage::new(conn).require_state(&record_id)?;
            verify_lifecycle_authority(conn, &record_id, &state)?;
            RuntimeRecords::new(conn)
                .ensure_private_record_version(&record_id, &expected_version)?;
            records.push(GrantRecordBinding {
                record_id,
                expected_version,
                memory_type: record.memory_type,
                lane: record.lane,
                destination: record.destination,
                scope_kind: record.scope_kind,
                scope_id: record.scope_id,
                visibility: record.visibility,
            });
        }
        let plan = match (&request.source, plan) {
            (PrivateLifecycleSource::Direct, None) => None,
            (
                PrivateLifecycleSource::MaintenancePlan {
                    plan_id,
                    selected_action_ids,
                },
                Some(plan),
            ) => {
                let runtime_fingerprint = match &plan.scope {
                    MaintenanceScope::PrivateRuntime {
                        runtime_fingerprint,
                        ..
                    } => runtime_fingerprint.clone(),
                    MaintenanceScope::Repository { .. } => unreachable!("validated above"),
                };
                Some(GrantPlanBinding {
                    plan_id: plan_id.clone(),
                    selected_action_ids: selected_action_ids.clone(),
                    runtime_fingerprint,
                    policy_digest: plan.policy.policy_digest.clone(),
                    detector_digest: plan.preconditions.detector_digest.clone(),
                    not_after: plan.not_after.clone(),
                })
            }
            _ => unreachable!("request/plan pairing validated above"),
        };
        Ok(GrantBinding {
            schema: GRANT_BINDING_SCHEMA.to_owned(),
            request: request.clone(),
            policy_version: PRIVATE_LIFECYCLE_POLICY_VERSION.to_owned(),
            records,
            plan,
        })
    }
}

fn canonical_json(value: &impl Serialize) -> Result<String> {
    let bytes = serde_json_canonicalizer::to_vec(value)
        .context("failed to canonicalize private lifecycle authority")?;
    String::from_utf8(bytes).context("canonical private lifecycle JSON was not UTF-8")
}

fn ensure_identifier(value: &str, label: &str) -> Result<()> {
    ensure!(!value.trim().is_empty(), "{label} is required");
    ensure!(value == value.trim(), "{label} must be canonical");
    ensure!(
        !value.chars().any(char::is_control),
        "{label} must not contain control characters"
    );
    Ok(())
}

fn require_private_record(conn: &Connection, record_id: &str) -> Result<MemoryRecord> {
    let record = RuntimeRecords::new(conn)
        .get(record_id)?
        .with_context(|| format!("private record not found: {record_id}"))?;
    ensure_private_shape(&record)?;
    Ok(record)
}

fn ensure_private_shape(record: &MemoryRecord) -> Result<()> {
    ensure!(
        matches!(
            record.destination,
            MemoryDestination::Local | MemoryDestination::Session
        ) && record.visibility == Visibility::Private,
        "record {} is not private runtime memory",
        record.id
    );
    Ok(())
}

fn state_snapshot(state: &PrivateLifecycleState) -> PrivateLifecycleStateSnapshot {
    PrivateLifecycleStateSnapshot {
        automatic_recall_until: state.automatic_recall_until.clone(),
        validity_until: state.validity_until.clone(),
        retain_until: state.retain_until.clone(),
        pinned: state.pinned,
        quarantined: state.quarantined,
        quarantine_reason_code: state.quarantine_reason_code.clone(),
        updated_at: state.updated_at.clone(),
    }
}

fn relation_snapshot(relation: PrivateLifecycleRelation) -> PrivateLifecycleRelationSnapshot {
    PrivateLifecycleRelationSnapshot {
        relation_id: relation.id,
        kind: relation.relation_kind.as_str().to_owned(),
        subject_record_id: relation.subject_record_id,
        related_record_id: relation.related_record_id,
        application_id: relation.application_id,
        created_at: relation.created_at,
    }
}

fn validate_stored_binding(binding: &GrantBinding, row: &OwnerActionGrantRow) -> Result<()> {
    ensure!(
        binding.schema == GRANT_BINDING_SCHEMA,
        "stored owner grant binding uses an unsupported schema"
    );
    binding.request.validate()?;
    ensure!(
        binding.request.request_id == row.request_id,
        "stored owner grant request identity is inconsistent"
    );
    ensure!(
        binding.policy_version == PRIVATE_LIFECYCLE_POLICY_VERSION,
        "stored owner grant policy is not current"
    );
    ensure!(
        binding
            .records
            .windows(2)
            .all(|window| window[0].record_id < window[1].record_id),
        "stored owner grant record bindings are not sorted and unique"
    );
    Ok(())
}

fn grant_from_row(
    row: &OwnerActionGrantRow,
    binding: &GrantBinding,
) -> Result<PrivateLifecycleGrant> {
    validate_stored_binding(binding, row)?;
    let state = match row.state {
        StoredGrantState::Active => crate::OwnerActionGrantState::Active,
        StoredGrantState::Consumed => crate::OwnerActionGrantState::Consumed,
        StoredGrantState::Revoked => crate::OwnerActionGrantState::Revoked,
    };
    let mut target_record_ids = binding
        .request
        .actions
        .iter()
        .flat_map(PrivateLifecycleAction::mutation_targets)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    target_record_ids.sort();
    target_record_ids.dedup();
    Ok(PrivateLifecycleGrant {
        schema: PRIVATE_LIFECYCLE_GRANT_SCHEMA.to_owned(),
        grant_id: row.grant_id.clone(),
        request_id: row.request_id.clone(),
        operation_id: binding.request.operation_id.clone(),
        state,
        authorized_at: row.authorized_at.clone(),
        expires_at: row.expires_at.clone(),
        policy_version: binding.policy_version.clone(),
        source: binding.request.source.clone(),
        action_kinds: binding
            .request
            .actions
            .iter()
            .map(|action| action.kind().to_owned())
            .collect(),
        target_record_ids,
        consumed_application_id: row.consumed_application_id.clone(),
    })
}

fn effective_grant_expiry(
    authorization_time: OffsetDateTime,
    requested_expires_at: Option<&str>,
    plan: Option<&MaintenancePlan>,
) -> Result<OffsetDateTime> {
    let mut expiry = authorization_time
        .checked_add(MAX_GRANT_LIFETIME)
        .context("authorization time cannot represent the maximum grant lifetime")?;
    if let Some(requested) = requested_expires_at {
        let requested = crate::private_lifecycle::parse_timestamp(requested, "expires_at")?;
        expiry = expiry.min(requested);
    }
    if let Some(plan) = plan {
        expiry = expiry.min(crate::private_lifecycle::parse_timestamp(
            &plan.not_after,
            "maintenance plan not_after",
        )?);
    }
    ensure!(
        expiry > authorization_time,
        "owner grant expiry must be later than authorization time"
    );
    Ok(expiry)
}

fn effective_current_assertion(
    conn: &Connection,
    record: &MemoryRecord,
    state: &PrivateLifecycleState,
    evaluated_at: OffsetDateTime,
) -> Result<crate::CurrentAssertionDecision> {
    verify_lifecycle_authority(conn, &record.id, state)?;
    crate::retention::evaluate_private_current_assertion_decision(
        &record.id,
        record.status,
        record.lane,
        &record.retention,
        evaluated_at,
        crate::retention::PrivateLifecycleReadFacts {
            automatic_recall_until: state.automatic_recall_until.as_deref(),
            validity_until: state.validity_until.as_deref(),
            automatic_recall_event_id: state.automatic_recall_event_id.as_deref(),
            validity_event_id: state.validity_event_id.as_deref(),
            quarantined: state.quarantined,
            quarantine_reason: state.quarantine_reason_code.as_deref(),
        },
    )
}

fn verify_lifecycle_authority(
    conn: &Connection,
    record_id: &str,
    state: &PrivateLifecycleState,
) -> Result<()> {
    ensure!(
        state.automatic_recall_until.is_some() == state.automatic_recall_event_id.is_some(),
        "private automatic recall fact and authority event are inconsistent"
    );
    ensure!(
        state.validity_until.is_some() == state.validity_event_id.is_some(),
        "private validity fact and authority event are inconsistent"
    );
    if state.automatic_recall_until.is_some() {
        require_authorizing_event(
            conn,
            state.automatic_recall_event_id.as_deref(),
            record_id,
            &["extend_automatic_recall", "correct"],
            "automatic recall",
        )?;
    }
    if state.validity_until.is_some() {
        require_authorizing_event(
            conn,
            state.validity_event_id.as_deref(),
            record_id,
            &["extend_validity", "correct"],
            "validity",
        )?;
    }
    if state.retention_event_id.is_some() {
        require_authorizing_event(
            conn,
            state.retention_event_id.as_deref(),
            record_id,
            &["retain_until", "pin", "unpin", "correct"],
            "physical retention",
        )?;
    } else {
        ensure!(
            state.retain_until.is_none() && !state.pinned,
            "private physical retention fact has no authorizing event"
        );
    }
    if state.quarantine_event_id.is_some() {
        let actions: &[&str] = if state.quarantined {
            &["quarantine", "correct"]
        } else {
            &["release_quarantine", "correct"]
        };
        require_authorizing_event(
            conn,
            state.quarantine_event_id.as_deref(),
            record_id,
            actions,
            "quarantine",
        )?;
    } else {
        ensure!(
            !state.quarantined,
            "private quarantine fact has no authorizing event"
        );
    }
    Ok(())
}

fn require_authorizing_event(
    conn: &Connection,
    event_id: Option<&str>,
    record_id: &str,
    allowed_action_kinds: &[&str],
    fact: &str,
) -> Result<()> {
    let event_id =
        event_id.with_context(|| format!("private {fact} fact has no authorizing event"))?;
    let payload_json: Option<String> = conn
        .query_row(
            "SELECT payload_json
         FROM event_log
         WHERE id = ?1 AND event_type = 'memory.private_lifecycle_applied'",
            [event_id],
            |row| row.get(0),
        )
        .optional()?;
    let payload_json = payload_json
        .with_context(|| format!("private {fact} fact has an unverifiable authorizing event"))?;
    let payload: LifecycleAuditPayload = serde_json::from_str(&payload_json)
        .with_context(|| format!("private {fact} authority event is malformed"))?;
    ensure!(
        payload
            .target_record_ids
            .iter()
            .any(|target| target == record_id)
            && payload
                .action_kinds
                .iter()
                .any(|kind| allowed_action_kinds.contains(&kind.as_str())),
        "private {fact} event does not authorize record {record_id} and its action kind"
    );
    Ok(())
}

fn validate_request_plan_binding(
    service: &MemoryService,
    conn: &Connection,
    request: &PrivateLifecycleRequest,
    plan: Option<&MaintenancePlan>,
    now: OffsetDateTime,
    revalidate: bool,
) -> Result<()> {
    match (&request.source, plan) {
        (PrivateLifecycleSource::Direct, None) => return Ok(()),
        (PrivateLifecycleSource::Direct, Some(_)) => {
            bail!("direct private lifecycle requests must not supply a maintenance plan")
        }
        (PrivateLifecycleSource::MaintenancePlan { .. }, None) => {
            bail!("planned private lifecycle requests require --plan-file")
        }
        (PrivateLifecycleSource::MaintenancePlan { .. }, Some(_)) => {}
    }
    let plan = plan.context("validated planned request lost its plan")?;
    plan.validate()?;
    let (plan_id, selected_action_ids) = match &request.source {
        PrivateLifecycleSource::MaintenancePlan {
            plan_id,
            selected_action_ids,
        } => (plan_id, selected_action_ids),
        PrivateLifecycleSource::Direct => unreachable!(),
    };
    ensure!(plan_id == &plan.plan_id, "maintenance plan_id mismatch");
    let expected_runtime = private_maintenance_runtime_fingerprint(service.paths.repository_key())?;
    match &plan.scope {
        MaintenanceScope::PrivateRuntime {
            runtime_fingerprint,
            ..
        } => ensure!(
            runtime_fingerprint == &expected_runtime,
            "maintenance private-runtime scope mismatch"
        ),
        MaintenanceScope::Repository { .. } => {
            bail!("private lifecycle authority requires a private-runtime maintenance plan")
        }
    }
    let evaluated_at =
        crate::private_lifecycle::parse_timestamp(&plan.evaluated_at, "maintenance evaluated_at")?;
    let not_after =
        crate::private_lifecycle::parse_timestamp(&plan.not_after, "maintenance not_after")?;
    ensure!(now >= evaluated_at, "maintenance plan is not yet valid");
    ensure!(now < not_after, "maintenance plan has expired");

    let owner_actions = plan
        .action_groups
        .iter()
        .find(|group| group.kind == MaintenanceActionGroupKind::OwnerAuthorizedPrivateMutation)
        .context("maintenance plan has no owner-authorized action group")?;
    let selected = selected_action_ids.iter().collect::<BTreeSet<_>>();
    ensure!(
        selected.len() == request.actions.len(),
        "planned request must bind exactly one lifecycle action to each selected plan action"
    );
    let mut matched_request_actions = BTreeSet::new();
    for selected_id in selected_action_ids {
        let candidate = owner_actions
            .actions
            .iter()
            .find(|action| action.action_id == *selected_id)
            .with_context(|| format!("selected maintenance action not found: {selected_id}"))?;
        let matches = request
            .actions
            .iter()
            .enumerate()
            .filter(|(_, action)| request_action_matches_candidate(action, candidate))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        ensure!(
            matches.len() == 1 && matched_request_actions.insert(matches[0]),
            "selected maintenance action {selected_id} does not bind one exact request action"
        );
    }
    ensure!(
        matched_request_actions.len() == request.actions.len(),
        "planned request contains an action not selected from the maintenance plan"
    );

    if revalidate {
        let current =
            service.plan_private_lifecycle_with_conn(conn, plan.request.clone(), evaluated_at)?;
        ensure!(
            current.plan_id == plan.plan_id,
            "maintenance plan is stale: private snapshot, neighbourhood, detector, or policy changed"
        );
    }
    Ok(())
}

fn request_action_matches_candidate(
    request: &PrivateLifecycleAction,
    candidate: &MaintenanceAction,
) -> bool {
    let candidate_versions = candidate
        .preconditions
        .record_versions
        .iter()
        .filter_map(|(id, version)| {
            version
                .private_version_token()
                .map(|token| (id.as_str(), token))
        })
        .collect::<BTreeMap<_, _>>();
    match (request, candidate.class) {
        (
            PrivateLifecycleAction::Consolidate {
                record_ids,
                expected_versions,
                keeper_record_id,
            },
            MaintenanceActionClass::OwnerConsolidateExactDuplicates,
        ) => {
            record_ids.iter().collect::<BTreeSet<_>>()
                == candidate.record_ids.iter().collect::<BTreeSet<_>>()
                && record_ids.contains(keeper_record_id)
                && expected_version_map_matches(expected_versions, &candidate_versions)
        }
        (
            PrivateLifecycleAction::ResolveContradiction {
                record_ids,
                expected_versions,
                winner_record_id,
            },
            MaintenanceActionClass::OwnerResolveContradiction,
        ) => {
            record_ids.iter().collect::<BTreeSet<_>>()
                == candidate.record_ids.iter().collect::<BTreeSet<_>>()
                && record_ids.contains(winner_record_id)
                && expected_version_map_matches(expected_versions, &candidate_versions)
        }
        (
            PrivateLifecycleAction::RenewFromEvidence {
                predecessor_id,
                expected_predecessor_version,
                evidence_record_id,
                expected_evidence_version,
            },
            MaintenanceActionClass::OwnerCreateRenewalSuccessor,
        ) => {
            candidate.predecessor_record_id.as_deref() == Some(predecessor_id)
                && candidate.evidence_record_id.as_deref() == Some(evidence_record_id)
                && candidate_versions.get(predecessor_id.as_str())
                    == Some(&expected_predecessor_version.as_str())
                && candidate_versions.get(evidence_record_id.as_str())
                    == Some(&expected_evidence_version.as_str())
        }
        _ => false,
    }
}

fn expected_version_map_matches(
    request: &BTreeMap<String, String>,
    candidate: &BTreeMap<&str, &str>,
) -> bool {
    request.len() == candidate.len()
        && request
            .iter()
            .all(|(id, version)| candidate.get(id.as_str()).copied() == Some(version.as_str()))
}

fn validate_action_group(
    conn: &Connection,
    request: &PrivateLifecycleRequest,
    now: OffsetDateTime,
) -> Result<PreparedApply> {
    let runtime = RuntimeRecords::new(conn);
    for action in &request.actions {
        for (record_id, expected_version) in action.expected_versions() {
            runtime.ensure_private_record_version(record_id, expected_version)?;
        }
    }
    let mut replacement_ids = BTreeMap::new();
    for (index, action) in request.actions.iter().enumerate() {
        validate_action_semantics(conn, action, now)?;
        if let PrivateLifecycleAction::Correct { record_id, .. } = action {
            replacement_ids.insert(index, next_replacement_id(conn, record_id)?);
        }
    }
    Ok(PreparedApply { replacement_ids })
}

fn validate_action_semantics(
    conn: &Connection,
    action: &PrivateLifecycleAction,
    now: OffsetDateTime,
) -> Result<()> {
    let storage = PrivateLifecycleStorage::new(conn);
    match action {
        PrivateLifecycleAction::ExtendAutomaticRecall {
            record_id,
            recall_until,
            ..
        } => {
            let record = require_private_record(conn, record_id)?;
            ensure!(
                record.lane == MemoryLane::Episodic,
                "automatic recall extension is limited to episodic records"
            );
            ensure!(
                record.status == MemoryStatus::Active,
                "automatic recall extension requires active lifecycle status"
            );
            let state = storage.require_state(record_id)?;
            let occurred_at = crate::private_lifecycle::parse_timestamp(
                record
                    .retention
                    .occurred_at
                    .as_deref()
                    .context("episodic record has no occurrence timestamp")?,
                "occurred_at",
            )?;
            let ordinary_boundary = occurred_at
                .checked_add(Duration::days(30))
                .context("episodic occurrence cannot represent ordinary recall boundary")?;
            let current_boundary = state
                .automatic_recall_until
                .as_deref()
                .map(|value| {
                    crate::private_lifecycle::parse_timestamp(value, "automatic_recall_until")
                })
                .transpose()?
                .unwrap_or(ordinary_boundary);
            let requested =
                crate::private_lifecycle::parse_timestamp(recall_until, "recall_until")?;
            let maximum = occurred_at
                .checked_add(Duration::days(90))
                .context("episodic occurrence cannot represent maximum recall boundary")?;
            ensure!(
                requested > current_boundary && requested > now,
                "automatic recall must extend the current boundary into the future"
            );
            ensure!(
                requested <= maximum,
                "automatic episodic recall cannot extend beyond 90 days from occurrence"
            );
        }
        PrivateLifecycleAction::ExtendValidity {
            record_id,
            validity_until,
            ..
        } => {
            let record = require_private_record(conn, record_id)?;
            ensure!(
                record.lane != MemoryLane::Session,
                "session validity extension is not allowed"
            );
            ensure!(
                record.status == MemoryStatus::Active,
                "validity extension requires active lifecycle status"
            );
            let state = storage.require_state(record_id)?;
            let prior = state
                .validity_until
                .as_deref()
                .or(record.retention.explicit_expires_at.as_deref())
                .context("validity extension requires an existing finite boundary")?;
            let prior = crate::private_lifecycle::parse_timestamp(prior, "validity boundary")?;
            let requested =
                crate::private_lifecycle::parse_timestamp(validity_until, "validity_until")?;
            ensure!(
                requested > prior && requested > now,
                "validity_until must be a later finite future boundary"
            );
        }
        PrivateLifecycleAction::RetainUntil {
            record_id,
            retain_until,
            ..
        } => {
            require_private_record(conn, record_id)?;
            let state = storage.require_state(record_id)?;
            let requested =
                crate::private_lifecycle::parse_timestamp(retain_until, "retain_until")?;
            ensure!(requested > now, "retain_until must be in the future");
            if let Some(prior) = state.retain_until.as_deref() {
                let prior = crate::private_lifecycle::parse_timestamp(prior, "retain_until")?;
                ensure!(
                    requested > prior,
                    "retain_until is monotonic and may only be extended"
                );
            }
        }
        PrivateLifecycleAction::Pin { record_id, .. } => {
            let record = require_private_record(conn, record_id)?;
            ensure!(
                matches!(
                    record.lane,
                    MemoryLane::Episodic | MemoryLane::Semantic | MemoryLane::Procedural
                ),
                "indefinite session pinning is not allowed"
            );
            ensure!(
                !storage.require_state(record_id)?.pinned,
                "private record is already pinned"
            );
        }
        PrivateLifecycleAction::Unpin { record_id, .. } => {
            require_private_record(conn, record_id)?;
            ensure!(
                storage.require_state(record_id)?.pinned,
                "private record is not pinned"
            );
        }
        PrivateLifecycleAction::RenewFromEvidence {
            predecessor_id,
            evidence_record_id,
            ..
        } => validate_renewal(conn, predecessor_id, evidence_record_id, now)?,
        PrivateLifecycleAction::Correct {
            record_id,
            evidence_versions,
            ..
        } => {
            let target = require_private_record(conn, record_id)?;
            ensure!(
                matches!(target.status, MemoryStatus::Active | MemoryStatus::Expired),
                "correction requires an active or expired target"
            );
            for evidence_id in evidence_versions.keys() {
                let evidence = require_private_record(conn, evidence_id)?;
                let state = storage.require_state(evidence_id)?;
                let decision = effective_current_assertion(conn, &evidence, &state, now)?;
                ensure!(
                    evidence.status == MemoryStatus::Active && decision.is_current,
                    "correction evidence {evidence_id} is not a current accepted private record"
                );
            }
        }
        PrivateLifecycleAction::Supersede {
            record_id,
            successor_record_id,
            ..
        } => {
            let target = require_private_record(conn, record_id)?;
            let successor = require_private_record(conn, successor_record_id)?;
            ensure!(
                matches!(target.status, MemoryStatus::Active | MemoryStatus::Expired),
                "supersession target must be active or expired"
            );
            ensure!(
                successor.status == MemoryStatus::Active,
                "supersession successor must be active"
            );
            ensure_compatible_classification(&target, &successor)?;
            ensure!(
                successor.supersedes_id.is_none() && successor.lineage.is_none(),
                "successor is already bound to predecessor or lineage history"
            );
        }
        PrivateLifecycleAction::Consolidate {
            record_ids,
            keeper_record_id,
            ..
        } => validate_consolidation(conn, record_ids, keeper_record_id, now)?,
        PrivateLifecycleAction::ResolveContradiction {
            record_ids,
            winner_record_id,
            ..
        } => validate_contradiction_resolution(conn, record_ids, winner_record_id, now)?,
        PrivateLifecycleAction::Quarantine { record_id, .. } => {
            require_private_record(conn, record_id)?;
            ensure!(
                !storage.require_state(record_id)?.quarantined,
                "private record is already quarantined"
            );
        }
        PrivateLifecycleAction::ReleaseQuarantine { record_id, .. } => {
            require_private_record(conn, record_id)?;
            ensure!(
                storage.require_state(record_id)?.quarantined,
                "private record is not quarantined"
            );
        }
    }
    Ok(())
}

fn validate_renewal(
    conn: &Connection,
    predecessor_id: &str,
    evidence_record_id: &str,
    now: OffsetDateTime,
) -> Result<()> {
    let predecessor = require_private_record(conn, predecessor_id)?;
    let evidence = require_private_record(conn, evidence_record_id)?;
    ensure!(
        matches!(
            predecessor.lane,
            MemoryLane::Semantic | MemoryLane::Procedural
        ) && predecessor.lane == evidence.lane
            && claim_bearing_type(predecessor.memory_type)
            && claim_bearing_type(evidence.memory_type),
        "renewal is limited to semantic or procedural claims"
    );
    ensure!(
        matches!(
            predecessor.status,
            MemoryStatus::Active | MemoryStatus::Expired
        ),
        "renewal predecessor must be active or expired"
    );
    ensure!(
        evidence.status == MemoryStatus::Active,
        "renewal evidence record must be active"
    );
    ensure!(
        evidence.supersedes_id.is_none() && evidence.lineage.is_none(),
        "renewal evidence is already bound to predecessor or lineage history"
    );
    ensure_compatible_classification(&predecessor, &evidence)?;
    ensure!(
        normalize_text(&predecessor.title) == normalize_text(&evidence.title)
            && predecessor.body.trim() == evidence.body.trim(),
        "renewal evidence must support the same exact claim"
    );
    ensure!(
        predecessor.origin != evidence.origin,
        "renewal evidence must have a distinct origin"
    );
    let storage = PrivateLifecycleStorage::new(conn);
    let predecessor_state = storage.require_state(predecessor_id)?;
    let predecessor_decision =
        effective_current_assertion(conn, &predecessor, &predecessor_state, now)?;
    ensure!(
        predecessor_decision.retention.state == crate::RetentionState::QueryOnly,
        "renewal predecessor is not expired"
    );
    let boundary = predecessor_decision
        .retention
        .effective_boundary
        .as_deref()
        .context("renewal predecessor has no finite expiry boundary")?;
    let boundary = crate::private_lifecycle::parse_timestamp(boundary, "renewal boundary")?;
    let evidence_state = storage.require_state(evidence_record_id)?;
    ensure!(
        effective_current_assertion(conn, &evidence, &evidence_state, now)?.is_current,
        "renewal evidence is not current"
    );
    let capture = evidence
        .capture
        .as_ref()
        .context("renewal requires fresh accepted capture evidence")?;
    ensure!(
        matches!(
            capture.review_outcome,
            crate::CaptureReviewOutcome::Accept | crate::CaptureReviewOutcome::Edit
        ),
        "renewal evidence was not accepted"
    );
    let reviewed_at =
        crate::private_lifecycle::parse_timestamp(&capture.reviewed_at, "reviewed_at")?;
    ensure!(
        reviewed_at > boundary && reviewed_at <= now,
        "renewal evidence is not fresh relative to the predecessor boundary"
    );
    if let Some(prior) = predecessor.capture.as_ref() {
        ensure!(
            canonical_json(&prior.evidence)? != canonical_json(&capture.evidence)?,
            "renewal evidence is not distinct from predecessor evidence"
        );
    }
    Ok(())
}

fn ensure_compatible_classification(left: &MemoryRecord, right: &MemoryRecord) -> Result<()> {
    ensure!(
        left.memory_type == right.memory_type
            && left.lane == right.lane
            && left.destination == right.destination
            && left.scope_kind == right.scope_kind
            && left.scope_id == right.scope_id
            && left.visibility == right.visibility,
        "private records have incompatible type, lane, destination, scope, or visibility"
    );
    Ok(())
}

fn validate_consolidation(
    conn: &Connection,
    record_ids: &[String],
    keeper_record_id: &str,
    now: OffsetDateTime,
) -> Result<()> {
    ensure!(
        record_ids.iter().any(|id| id == keeper_record_id),
        "owner-selected duplicate keeper is not in the exact duplicate set"
    );
    let storage = PrivateLifecycleStorage::new(conn);
    let first = require_private_record(
        conn,
        record_ids
            .first()
            .context("consolidation requires at least two records")?,
    )?;
    let projection = exact_duplicate_projection(&first)?;
    let mut complete = BTreeSet::new();
    for record in RuntimeRecords::new(conn).records_for_preservation()? {
        if record.status != MemoryStatus::Active
            || exact_duplicate_projection(&record)? != projection
        {
            continue;
        }
        let state = storage.require_state(&record.id)?;
        if effective_current_assertion(conn, &record, &state, now)?.is_current {
            complete.insert(record.id);
        }
    }
    let selected = record_ids.iter().cloned().collect::<BTreeSet<_>>();
    ensure!(
        selected == complete,
        "consolidation must name the complete exact duplicate set"
    );
    Ok(())
}

fn exact_duplicate_projection(record: &MemoryRecord) -> Result<Vec<u8>> {
    serde_json_canonicalizer::to_vec(&json!({
        "memory_type": record.memory_type,
        "lane": record.lane,
        "destination": record.destination,
        "scope_kind": record.scope_kind,
        "scope_id": record.scope_id,
        "visibility": record.visibility,
        "title": normalize_text(&record.title),
        "body": record.body.trim(),
        "confidence": record.confidence,
        "retention": record.retention,
    }))
    .context("failed to compare exact duplicate records")
}

fn validate_contradiction_resolution(
    conn: &Connection,
    record_ids: &[String],
    winner_record_id: &str,
    now: OffsetDateTime,
) -> Result<()> {
    ensure!(
        record_ids.iter().any(|id| id == winner_record_id),
        "owner-selected contradiction winner is not in the incompatible set"
    );
    let all_records = RuntimeRecords::new(conn).records_for_preservation()?;
    let storage = PrivateLifecycleStorage::new(conn);
    let selected_records = record_ids
        .iter()
        .map(|id| require_private_record(conn, id))
        .collect::<Result<Vec<_>>>()?;
    let first = selected_records
        .first()
        .context("contradiction resolution has no records")?;
    let (signature, _) = polarity_signature(&first.body)
        .context("contradiction record does not have allowlisted symmetric polarity")?;
    let mut complete = BTreeSet::new();
    let mut polarities = BTreeSet::new();
    for record in all_records {
        if record.status != MemoryStatus::Active
            || !matches!(record.lane, MemoryLane::Semantic | MemoryLane::Procedural)
            || !claim_bearing_type(record.memory_type)
            || contains_conditional_or_temporal_language(&record.body)
            || !same_claim_context(first, &record)
            || records_are_temporally_related(first, &record)
        {
            continue;
        }
        let state = storage.require_state(&record.id)?;
        if !effective_current_assertion(conn, &record, &state, now)?.is_current {
            continue;
        }
        let Some((candidate_signature, negative)) = polarity_signature(&record.body) else {
            continue;
        };
        if candidate_signature == signature {
            complete.insert(record.id);
            polarities.insert(negative);
        }
    }
    ensure!(
        polarities.len() == 2,
        "record set is not an incompatible polarity set"
    );
    let selected = record_ids.iter().cloned().collect::<BTreeSet<_>>();
    ensure!(
        selected == complete,
        "contradiction resolution must name the complete incompatible set"
    );
    Ok(())
}

fn normalize_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn same_claim_context(left: &MemoryRecord, right: &MemoryRecord) -> bool {
    left.memory_type == right.memory_type
        && left.lane == right.lane
        && left.destination == right.destination
        && normalize_text(&left.title) == normalize_text(&right.title)
        && left.scope_kind == right.scope_kind
        && left.scope_id == right.scope_id
}

fn records_are_temporally_related(left: &MemoryRecord, right: &MemoryRecord) -> bool {
    left.supersedes_id.as_deref() == Some(right.id.as_str())
        || right.supersedes_id.as_deref() == Some(left.id.as_str())
        || left
            .lineage
            .as_ref()
            .is_some_and(|lineage| lineage.predecessor_id == right.id)
        || right
            .lineage
            .as_ref()
            .is_some_and(|lineage| lineage.predecessor_id == left.id)
}

fn claim_bearing_type(memory_type: MemoryType) -> bool {
    matches!(
        memory_type,
        MemoryType::Fact
            | MemoryType::Preference
            | MemoryType::Decision
            | MemoryType::Procedure
            | MemoryType::InstructionProjection
    )
}

fn apply_action_group(
    conn: &Connection,
    request: &PrivateLifecycleRequest,
    prepared: &PreparedApply,
    application_id: &str,
    event_id: &str,
    applied_at: &str,
) -> Result<Vec<PrivateLifecycleActionResult>> {
    let mut results = Vec::with_capacity(request.actions.len());
    for (index, action) in request.actions.iter().enumerate() {
        let mut result_targets = action
            .mutation_targets()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let replacement_record_id = match action {
            PrivateLifecycleAction::ExtendAutomaticRecall {
                record_id,
                recall_until,
                ..
            } => {
                update_state(conn, record_id, applied_at, |state| {
                    state.automatic_recall_until = Some(recall_until.clone());
                    state.automatic_recall_event_id = Some(event_id.to_owned());
                })?;
                None
            }
            PrivateLifecycleAction::ExtendValidity {
                record_id,
                validity_until,
                ..
            } => {
                update_state(conn, record_id, applied_at, |state| {
                    state.validity_until = Some(validity_until.clone());
                    state.validity_event_id = Some(event_id.to_owned());
                })?;
                None
            }
            PrivateLifecycleAction::RetainUntil {
                record_id,
                retain_until,
                ..
            } => {
                update_state(conn, record_id, applied_at, |state| {
                    state.retain_until = Some(retain_until.clone());
                    state.retention_event_id = Some(event_id.to_owned());
                })?;
                None
            }
            PrivateLifecycleAction::Pin { record_id, .. } => {
                update_state(conn, record_id, applied_at, |state| {
                    state.pinned = true;
                    state.retention_event_id = Some(event_id.to_owned());
                })?;
                None
            }
            PrivateLifecycleAction::Unpin { record_id, .. } => {
                update_state(conn, record_id, applied_at, |state| {
                    state.pinned = false;
                    state.retention_event_id = Some(event_id.to_owned());
                })?;
                None
            }
            PrivateLifecycleAction::RenewFromEvidence {
                predecessor_id,
                evidence_record_id,
                ..
            } => {
                update_record_status(conn, predecessor_id, MemoryStatus::Superseded, applied_at)?;
                #[cfg(test)]
                fail_lifecycle_apply_at(LifecycleApplyFault::AfterRenewalPredecessorStatus)?;
                set_successor_predecessor(
                    conn,
                    evidence_record_id,
                    predecessor_id,
                    Some(crate::RecordLineageKind::Renewal),
                    applied_at,
                )?;
                #[cfg(test)]
                fail_lifecycle_apply_at(LifecycleApplyFault::AfterRenewalSuccessorLink)?;
                insert_relation(
                    conn,
                    PrivateLifecycleRelationKind::RenewedBy,
                    predecessor_id,
                    evidence_record_id,
                    application_id,
                    applied_at,
                )?;
                #[cfg(test)]
                fail_lifecycle_apply_at(LifecycleApplyFault::AfterRenewalRelation)?;
                None
            }
            PrivateLifecycleAction::Correct {
                record_id,
                replacement,
                ..
            } => {
                let replacement_id = prepared
                    .replacement_ids
                    .get(&index)
                    .context("validated correction has no reserved replacement ID")?;
                create_correction_record(
                    conn,
                    record_id,
                    replacement_id,
                    replacement,
                    &request.operation_id,
                    event_id,
                    applied_at,
                )?;
                update_record_status(conn, record_id, MemoryStatus::Superseded, applied_at)?;
                #[cfg(test)]
                fail_lifecycle_apply_at(LifecycleApplyFault::AfterCorrectionPredecessorStatus)?;
                insert_relation(
                    conn,
                    PrivateLifecycleRelationKind::CorrectedBy,
                    record_id,
                    replacement_id,
                    application_id,
                    applied_at,
                )?;
                #[cfg(test)]
                fail_lifecycle_apply_at(LifecycleApplyFault::AfterCorrectionRelation)?;
                result_targets.push(replacement_id.clone());
                Some(replacement_id.clone())
            }
            PrivateLifecycleAction::Supersede {
                record_id,
                successor_record_id,
                ..
            } => {
                update_record_status(conn, record_id, MemoryStatus::Superseded, applied_at)?;
                #[cfg(test)]
                fail_lifecycle_apply_at(LifecycleApplyFault::AfterSupersessionPredecessorStatus)?;
                set_successor_predecessor(conn, successor_record_id, record_id, None, applied_at)?;
                #[cfg(test)]
                fail_lifecycle_apply_at(LifecycleApplyFault::AfterSupersessionSuccessorLink)?;
                insert_relation(
                    conn,
                    PrivateLifecycleRelationKind::SupersededBy,
                    record_id,
                    successor_record_id,
                    application_id,
                    applied_at,
                )?;
                #[cfg(test)]
                fail_lifecycle_apply_at(LifecycleApplyFault::AfterSupersessionRelation)?;
                None
            }
            PrivateLifecycleAction::Consolidate {
                record_ids,
                keeper_record_id,
                ..
            } => {
                for record_id in record_ids {
                    if record_id == keeper_record_id {
                        continue;
                    }
                    update_record_status(conn, record_id, MemoryStatus::Superseded, applied_at)?;
                    #[cfg(test)]
                    fail_lifecycle_apply_at(LifecycleApplyFault::AfterConsolidationMemberStatus)?;
                    insert_relation(
                        conn,
                        PrivateLifecycleRelationKind::ConsolidatedInto,
                        record_id,
                        keeper_record_id,
                        application_id,
                        applied_at,
                    )?;
                    #[cfg(test)]
                    fail_lifecycle_apply_at(LifecycleApplyFault::AfterConsolidationMemberRelation)?;
                }
                None
            }
            PrivateLifecycleAction::ResolveContradiction {
                record_ids,
                winner_record_id,
                ..
            } => {
                for record_id in record_ids {
                    if record_id == winner_record_id {
                        continue;
                    }
                    update_record_status(conn, record_id, MemoryStatus::Superseded, applied_at)?;
                    #[cfg(test)]
                    fail_lifecycle_apply_at(LifecycleApplyFault::AfterContradictionMemberStatus)?;
                    insert_relation(
                        conn,
                        PrivateLifecycleRelationKind::ContradictionResolvedBy,
                        record_id,
                        winner_record_id,
                        application_id,
                        applied_at,
                    )?;
                    #[cfg(test)]
                    fail_lifecycle_apply_at(LifecycleApplyFault::AfterContradictionMemberRelation)?;
                }
                None
            }
            PrivateLifecycleAction::Quarantine {
                record_id,
                reason_code,
                ..
            } => {
                update_state(conn, record_id, applied_at, |state| {
                    state.quarantined = true;
                    state.quarantine_reason_code = Some(reason_code.clone());
                    state.quarantine_event_id = Some(event_id.to_owned());
                })?;
                None
            }
            PrivateLifecycleAction::ReleaseQuarantine { record_id, .. } => {
                update_state(conn, record_id, applied_at, |state| {
                    state.quarantined = false;
                    state.quarantine_reason_code = None;
                    state.quarantine_event_id = Some(event_id.to_owned());
                })?;
                None
            }
        };
        #[cfg(test)]
        if index == 0 {
            fail_lifecycle_apply_at(LifecycleApplyFault::DuringMutations)?;
        }
        result_targets.sort();
        result_targets.dedup();
        let resulting_versions = result_targets
            .iter()
            .map(|record_id| {
                RuntimeRecords::new(conn)
                    .private_record_version(record_id)
                    .map(|version| (record_id.clone(), version))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        results.push(PrivateLifecycleActionResult {
            kind: action.kind().to_owned(),
            target_record_ids: result_targets,
            resulting_versions,
            replacement_record_id,
        });
    }
    Ok(results)
}

fn update_state(
    conn: &Connection,
    record_id: &str,
    updated_at: &str,
    update: impl FnOnce(&mut PrivateLifecycleState),
) -> Result<PrivateLifecycleState> {
    let storage = PrivateLifecycleStorage::new(conn);
    let mut state = storage.require_state(record_id)?;
    update(&mut state);
    state.updated_at = updated_at.to_owned();
    storage.update_state_facts(&state)
}

fn update_record_status(
    conn: &Connection,
    record_id: &str,
    status: MemoryStatus,
    updated_at: &str,
) -> Result<()> {
    let changed = conn.execute(
        "UPDATE memory_record
         SET status = ?1, updated_at = ?2
         WHERE id = ?3 AND destination IN ('local', 'session')",
        rusqlite::params![status.as_str(), updated_at, record_id],
    )?;
    ensure!(
        changed == 1,
        "private record changed during lifecycle apply: {record_id}"
    );
    Ok(())
}

fn set_successor_predecessor(
    conn: &Connection,
    successor_id: &str,
    predecessor_id: &str,
    lineage_kind: Option<crate::RecordLineageKind>,
    updated_at: &str,
) -> Result<()> {
    let changed = if let Some(kind) = lineage_kind {
        let lineage = crate::RecordLineage {
            kind,
            predecessor_id: predecessor_id.to_owned(),
        };
        conn.execute(
            "UPDATE memory_record
             SET supersedes_id = ?1, lineage_json = ?2, updated_at = ?3
             WHERE id = ?4
               AND destination IN ('local', 'session')
               AND status = 'active'",
            rusqlite::params![
                predecessor_id,
                serde_json::to_string(&lineage)?,
                updated_at,
                successor_id,
            ],
        )?
    } else {
        conn.execute(
            "UPDATE memory_record
             SET supersedes_id = ?1, updated_at = ?2
             WHERE id = ?3
               AND destination IN ('local', 'session')
               AND status = 'active'",
            rusqlite::params![predecessor_id, updated_at, successor_id],
        )?
    };
    ensure!(
        changed == 1,
        "private successor changed during lifecycle apply: {successor_id}"
    );
    Ok(())
}

fn insert_relation(
    conn: &Connection,
    relation_kind: PrivateLifecycleRelationKind,
    subject_record_id: &str,
    related_record_id: &str,
    application_id: &str,
    created_at: &str,
) -> Result<()> {
    PrivateLifecycleStorage::new(conn).insert_relation(&PrivateLifecycleRelation {
        id: format!("relation_{}", Uuid::now_v7()),
        relation_kind,
        subject_record_id: subject_record_id.to_owned(),
        related_record_id: related_record_id.to_owned(),
        application_id: application_id.to_owned(),
        created_at: created_at.to_owned(),
    })
}

fn next_replacement_id(conn: &Connection, predecessor_id: &str) -> Result<String> {
    let destination: String = conn.query_row(
        "SELECT destination FROM memory_record WHERE id = ?1",
        [predecessor_id],
        |row| row.get(0),
    )?;
    ensure!(
        matches!(destination.as_str(), "local" | "session"),
        "correction replacement requires a private runtime predecessor"
    );
    for _ in 0..8 {
        let candidate = format!("{destination}-{}", Uuid::new_v4());
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM memory_record WHERE id = ?1)",
            [&candidate],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(candidate);
        }
    }
    bail!("could not reserve a private correction record ID")
}

fn create_correction_record(
    conn: &Connection,
    predecessor_id: &str,
    replacement_id: &str,
    replacement: &crate::PrivateCorrectionReplacement,
    operation_id: &str,
    event_id: &str,
    timestamp: &str,
) -> Result<()> {
    let predecessor = require_private_record(conn, predecessor_id)?;
    let predecessor_state = PrivateLifecycleStorage::new(conn).require_state(predecessor_id)?;
    let origin = OriginDescriptor::owner_command(operation_id, OriginRoute::OwnerCommand);
    let inserted = conn.execute(
        "INSERT INTO memory_record (
           id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
           confidence, source_kind, source_ref, proposal_id, content_hash, created_at, updated_at,
           supersedes_id, retention_json, origin_json, lineage_json
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'active',
           ?10, 'memzoi-lifecycle-correction', NULL, NULL, ?11, ?12, ?12,
           ?13, ?14, ?15, NULL
         )",
        rusqlite::params![
            replacement_id,
            predecessor.memory_type.as_str(),
            predecessor.lane.as_str(),
            predecessor.destination.as_str(),
            predecessor.scope_kind.as_str(),
            predecessor.scope_id,
            predecessor.visibility.as_str(),
            replacement.title.trim(),
            replacement.body.trim(),
            predecessor.confidence,
            blake3::hash(replacement.body.trim().as_bytes())
                .to_hex()
                .to_string(),
            timestamp,
            predecessor_id,
            serde_json::to_string(&predecessor.retention)?,
            serde_json::to_string(&origin)?,
        ],
    )?;
    ensure!(inserted == 1, "private correction record was not inserted");
    #[cfg(test)]
    fail_lifecycle_apply_at(LifecycleApplyFault::AfterCorrectionReplacementInsert)?;

    // A correction changes content, not its independent temporal or physical
    // retention clocks. Preserve each fact, but bind inherited authority to
    // this correction event and its newly generated opaque replacement ID.
    let storage = PrivateLifecycleStorage::new(conn);
    let mut replacement_state = storage.require_state(replacement_id)?;
    replacement_state.automatic_recall_until = predecessor_state.automatic_recall_until;
    replacement_state.validity_until = predecessor_state.validity_until;
    replacement_state.retain_until = predecessor_state.retain_until;
    replacement_state.pinned = predecessor_state.pinned;
    replacement_state.quarantined = predecessor_state.quarantined;
    replacement_state.quarantine_reason_code = predecessor_state.quarantine_reason_code;
    replacement_state.automatic_recall_event_id = predecessor_state
        .automatic_recall_event_id
        .map(|_| event_id.to_owned());
    replacement_state.validity_event_id = predecessor_state
        .validity_event_id
        .map(|_| event_id.to_owned());
    replacement_state.retention_event_id = predecessor_state
        .retention_event_id
        .map(|_| event_id.to_owned());
    replacement_state.quarantine_event_id = predecessor_state
        .quarantine_event_id
        .map(|_| event_id.to_owned());
    replacement_state.updated_at = timestamp.to_owned();
    storage.update_state_facts(&replacement_state)?;
    #[cfg(test)]
    fail_lifecycle_apply_at(LifecycleApplyFault::AfterCorrectionReplacementState)?;

    conn.execute(
        "INSERT INTO memory_tag(record_id, tag)
         SELECT ?1, tag FROM memory_tag WHERE record_id = ?2",
        rusqlite::params![replacement_id, predecessor_id],
    )?;
    #[cfg(test)]
    fail_lifecycle_apply_at(LifecycleApplyFault::AfterCorrectionTagCopy)?;
    let paths = crate::search::load_paths(conn, predecessor_id)?;
    for path in paths {
        conn.execute(
            "INSERT INTO memory_path(id, record_id, path, symbol, line_start, line_end)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                format!("path_{}", Uuid::now_v7()),
                replacement_id,
                path.path,
                path.symbol,
                path.line_start,
                path.line_end,
            ],
        )?;
        #[cfg(test)]
        fail_lifecycle_apply_at(LifecycleApplyFault::AfterCorrectionPathCopy)?;
    }
    Ok(())
}

fn append_content_free_lifecycle_event(
    conn: &Connection,
    event_id: &str,
    application_id: &str,
    grant_id: &str,
    request: &PrivateLifecycleRequest,
    action_results: &[PrivateLifecycleActionResult],
    timestamp: &str,
) -> Result<()> {
    let action_kinds = request
        .actions
        .iter()
        .map(PrivateLifecycleAction::kind)
        .collect::<Vec<_>>();
    let mut target_record_ids = request
        .actions
        .iter()
        .flat_map(audit_target_ids)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    target_record_ids.extend(
        action_results
            .iter()
            .filter(|result| result.kind == "correct")
            .filter_map(|result| result.replacement_record_id.clone()),
    );
    target_record_ids.sort();
    target_record_ids.dedup();
    let payload = LifecycleAuditPayload {
        grant_id: grant_id.to_owned(),
        application_id: application_id.to_owned(),
        operation_id: request.operation_id.clone(),
        action_kinds: action_kinds.into_iter().map(str::to_owned).collect(),
        target_record_ids,
        applied_at: timestamp.to_owned(),
    };
    let inserted = conn.execute(
        "INSERT INTO event_log(
           id, event_type, actor, payload_json, record_id, proposal_id, created_at
         ) VALUES (?1, 'memory.private_lifecycle_applied', 'owner:local-cli', ?2, NULL, NULL, ?3)",
        rusqlite::params![event_id, serde_json::to_string(&payload)?, timestamp],
    )?;
    ensure!(
        inserted == 1,
        "private lifecycle audit event was not appended"
    );
    Ok(())
}

fn audit_target_ids(action: &PrivateLifecycleAction) -> Vec<&str> {
    match action {
        // The fresh evidence record is deliberately omitted from audit. It is
        // private evidence even though renewal promotes it transactionally.
        PrivateLifecycleAction::RenewFromEvidence { predecessor_id, .. } => {
            vec![predecessor_id]
        }
        PrivateLifecycleAction::Correct { record_id, .. } => vec![record_id],
        _ => action.mutation_targets(),
    }
}

fn polarity_signature(body: &str) -> Option<(String, bool)> {
    let mut tokens = normalized_tokens(body);
    if tokens.is_empty()
        || tokens
            .iter()
            .any(|token| token == "not" && tokens.len() == 1)
    {
        return None;
    }
    let boolean_positions = tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.as_str() == "true" || token.as_str() == "false")
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if boolean_positions.len() == 1 {
        let index = boolean_positions[0];
        let terminal_predicate = index + 1 == tokens.len()
            && index > 0
            && (matches!(tokens[index - 1].as_str(), "is" | "are")
                || (tokens[index - 1] == "be"
                    && index > 1
                    && matches!(tokens[index - 2].as_str(), "must" | "should")));
        if !terminal_predicate {
            return None;
        }
        let negative = tokens[index] == "false";
        tokens[index] = "<boolean>".to_owned();
        return Some((tokens.join(" "), negative));
    }
    if !boolean_positions.is_empty() {
        return None;
    }
    let not_positions = tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.as_str() == "not")
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if not_positions.len() > 1 {
        return None;
    }
    if let Some(index) = not_positions.first().copied() {
        if index == 0 || !matches!(tokens[index - 1].as_str(), "is" | "are" | "must" | "should") {
            return None;
        }
        tokens.remove(index);
        return Some((tokens.join(" "), true));
    }
    tokens
        .iter()
        .any(|token| matches!(token.as_str(), "is" | "are" | "must" | "should"))
        .then(|| (tokens.join(" "), false))
}

fn contains_conditional_or_temporal_language(body: &str) -> bool {
    let tokens = normalized_tokens(body);
    const BLOCKED: &[&str] = &[
        "if",
        "when",
        "whenever",
        "unless",
        "provided",
        "providing",
        "assuming",
        "depending",
        "where",
        "wherever",
        "while",
        "except",
        "otherwise",
        "until",
        "before",
        "after",
        "since",
        "once",
        "now",
        "today",
        "tomorrow",
        "yesterday",
        "formerly",
        "current",
        "currently",
        "previously",
        "temporarily",
        "during",
    ];
    tokens.iter().any(|token| {
        BLOCKED.contains(&token.as_str())
            || (token.len() == 4 && token.bytes().all(|byte| byte.is_ascii_digit()))
    }) || tokens
        .windows(2)
        .any(|window| window[0] == "as" && window[1] == "of")
}

fn normalized_tokens(value: &str) -> Vec<String> {
    value
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::OptionalExtension;
    use tempfile::TempDir;

    use super::*;
    use crate::{FixedClock, InitRequest, LocalMemoryInput, MemoryPaths};

    fn initialized_service() -> Result<(TempDir, MemoryService)> {
        let temp = TempDir::new()?;
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        let paths = MemoryPaths::with_runtime_home(
            project.canonicalize()?,
            temp.path().join("runtime-home"),
        );
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let service = MemoryService::open_paths_with_clock(
            paths,
            FixedClock::from_rfc3339("2026-07-19T12:00:00Z")?,
        )?;
        Ok((temp, service))
    }

    fn local(service: &MemoryService, title: &str, body: &str) -> Result<MemoryRecord> {
        service.create_local_memory(
            "test",
            LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: title.to_owned(),
                body: body.to_owned(),
            },
        )
    }

    fn pin_request(
        service: &MemoryService,
        operation_id: &str,
        record_id: &str,
    ) -> Result<PrivateLifecycleRequest> {
        PrivateLifecycleRequest::with_computed_id(
            operation_id,
            PrivateLifecycleSource::Direct,
            vec![PrivateLifecycleAction::Pin {
                record_id: record_id.to_owned(),
                expected_version: RuntimeRecords::new(&service.shared_conn)
                    .private_record_version(record_id)?,
            }],
        )
    }

    fn record_version(service: &MemoryService, record_id: &str) -> Result<String> {
        RuntimeRecords::new(&service.shared_conn).private_record_version(record_id)
    }

    fn record_versions(
        service: &MemoryService,
        record_ids: &[String],
    ) -> Result<BTreeMap<String, String>> {
        record_ids
            .iter()
            .map(|record_id| {
                record_version(service, record_id).map(|version| (record_id.clone(), version))
            })
            .collect()
    }

    fn planned_duplicate_request(
        service: &MemoryService,
        operation_id: &str,
    ) -> Result<(MaintenancePlan, PrivateLifecycleRequest, Vec<String>)> {
        let plan =
            service.plan_private_lifecycle(Vec::new(), Some("2026-07-19T12:00:00Z".to_owned()))?;
        let candidate = plan
            .action_groups
            .iter()
            .find(|group| group.kind == MaintenanceActionGroupKind::OwnerAuthorizedPrivateMutation)
            .and_then(|group| {
                group.actions.iter().find(|action| {
                    action.class == MaintenanceActionClass::OwnerConsolidateExactDuplicates
                })
            })
            .context("private duplicate maintenance action")?;
        let mut record_ids = candidate.record_ids.clone();
        record_ids.reverse();
        let expected_versions = candidate
            .preconditions
            .record_versions
            .iter()
            .map(|(record_id, version)| {
                version
                    .private_version_token()
                    .map(|token| (record_id.clone(), token.to_owned()))
                    .context("private plan action used a canonical record version")
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let request = PrivateLifecycleRequest::with_computed_id(
            operation_id,
            PrivateLifecycleSource::MaintenancePlan {
                plan_id: plan.plan_id.clone(),
                selected_action_ids: vec![candidate.action_id.clone()],
            },
            vec![PrivateLifecycleAction::Consolidate {
                record_ids: record_ids.clone(),
                expected_versions,
                keeper_record_id: record_ids[0].clone(),
            }],
        )?;
        Ok((plan, request, record_ids))
    }

    fn authorize_and_apply(
        service: &MemoryService,
        operation_id: &str,
        actions: Vec<PrivateLifecycleAction>,
    ) -> Result<PrivateLifecycleApplyResult> {
        let request = PrivateLifecycleRequest::with_computed_id(
            operation_id,
            PrivateLifecycleSource::Direct,
            actions,
        )?;
        let grant = service.authorize_private_lifecycle(&request, None, None)?;
        service.apply_private_lifecycle(&request, &grant.grant_id, None)
    }

    fn timestamp_after(days: i64) -> Result<String> {
        let now = crate::private_lifecycle::parse_timestamp("2026-07-19T12:00:00Z", "test clock")?;
        crate::expiry::format_timestamp(now + Duration::days(days))
    }

    fn set_explicit_expiry(
        service: &MemoryService,
        record: &MemoryRecord,
        expires_at: &str,
    ) -> Result<()> {
        let mut retention = record.retention.clone();
        retention.explicit_expires_at = Some(expires_at.to_owned());
        let changed = service.shared_conn.execute(
            "UPDATE memory_record SET retention_json = ?1 WHERE id = ?2",
            rusqlite::params![serde_json::to_string(&retention)?, record.id],
        )?;
        ensure!(changed == 1, "test record retention was not updated");
        Ok(())
    }

    fn accepted_capture(reviewed_at: &str, evidence_fill: char) -> crate::CaptureProvenance {
        let identity = |fill: char| format!("blake3:{}", fill.to_string().repeat(64));
        crate::CaptureProvenance {
            schema: crate::CAPTURE_PROVENANCE_SCHEMA.to_owned(),
            plan_id: identity('a'),
            review_id: identity('b'),
            claim_id: identity('c'),
            reviewed_claim_id: identity('d'),
            candidate_id: identity('e'),
            reviewed_candidate_id: identity('f'),
            extraction: crate::CaptureExtractorIdentity {
                kind: "markdown".to_owned(),
                id: "markdown".to_owned(),
                implementation_digest: identity('0'),
            },
            evidence: vec![crate::CaptureEvidence {
                source_id: format!("source-{evidence_fill}"),
                locator: crate::CaptureSourceLocator::ProjectPath {
                    path: format!("evidence-{evidence_fill}.md"),
                },
                source_content_hash: identity(evidence_fill),
                span: crate::CaptureEvidenceSpan {
                    byte_start: 0,
                    byte_end: 8,
                    line_start: 1,
                    line_end: 1,
                },
                evidence_content_hash: identity(evidence_fill),
                text: None,
                heading_path: vec!["Evidence".to_owned()],
                section_kind: "fact".to_owned(),
                semantic_location: None,
            }],
            confidence: "1.0".to_owned(),
            classification: crate::CaptureClassification {
                destination: MemoryDestination::Local,
                destination_reason: "owner-reviewed private evidence".to_owned(),
                sensitivity: crate::OkfProposalSensitivity::LocalOnly,
                sensitivity_reason: "private runtime test evidence".to_owned(),
                content_class: crate::RepositoryContentClass::LocalOnlyState,
                policy: MemoryDestination::Local.policy(),
            },
            destination: MemoryDestination::Local,
            sensitivity: crate::OkfProposalSensitivity::LocalOnly,
            review_outcome: crate::CaptureReviewOutcome::Accept,
            review_reason_code: None,
            reviewed_by: "owner".to_owned(),
            reviewed_at: reviewed_at.to_owned(),
            routed_by: "test".to_owned(),
        }
    }

    struct LifecycleApplyFaultGuard;

    impl LifecycleApplyFaultGuard {
        fn install(stage: LifecycleApplyFault) -> Self {
            LIFECYCLE_APPLY_FAULT.with(|slot| {
                assert_eq!(slot.replace(Some(stage)), None, "nested lifecycle fault");
            });
            Self
        }
    }

    impl Drop for LifecycleApplyFaultGuard {
        fn drop(&mut self) {
            LIFECYCLE_APPLY_FAULT.set(None);
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct NonGrantSnapshot {
        records: i64,
        states: i64,
        relations: i64,
        events: i64,
        applications: i64,
        generation: i64,
        mirror_revision: Option<String>,
    }

    fn non_grant_snapshot(service: &MemoryService) -> Result<NonGrantSnapshot> {
        let count = |table: &str| -> Result<i64> {
            let sql = format!("SELECT COUNT(*) FROM {table}");
            Ok(service.shared_conn.query_row(&sql, [], |row| row.get(0))?)
        };
        Ok(NonGrantSnapshot {
            records: count("memory_record")?,
            states: count("private_lifecycle_state")?,
            relations: count("private_lifecycle_relation")?,
            events: count("event_log")?,
            applications: count("private_lifecycle_application")?,
            generation: lifecycle_generation(&service.shared_conn)?,
            mirror_revision: service
                .shared_conn
                .query_row(
                    "SELECT revision FROM runtime_mirror_state WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?,
        })
    }

    #[derive(Debug, PartialEq, Eq)]
    struct LifecycleDatabaseSnapshot {
        records: Vec<String>,
        states: Vec<String>,
        relations: Vec<String>,
        events: Vec<String>,
        applications: Vec<String>,
        grants: Vec<String>,
        tags: Vec<String>,
        paths: Vec<String>,
        captures: Vec<String>,
        fts_records: Vec<String>,
        generation: i64,
        revision: Option<String>,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct AtomicLifecycleSnapshot {
        shared: LifecycleDatabaseSnapshot,
        mirror: LifecycleDatabaseSnapshot,
    }

    fn query_json_rows(conn: &Connection, sql: &str) -> Result<Vec<String>> {
        let mut statement = conn.prepare(sql)?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to snapshot lifecycle database rows")
    }

    fn lifecycle_database_snapshot(conn: &Connection) -> Result<LifecycleDatabaseSnapshot> {
        Ok(LifecycleDatabaseSnapshot {
            records: query_json_rows(
                conn,
                "SELECT json_object(
                   'rowid', rowid, 'id', id, 'type', type, 'lane', lane,
                   'destination', destination, 'scope_kind', scope_kind, 'scope_id', scope_id,
                   'visibility', visibility, 'title', title, 'body', body, 'status', status,
                   'confidence', confidence, 'source_kind', source_kind, 'source_ref', source_ref,
                   'proposal_id', proposal_id, 'retention_json', retention_json,
                   'origin_json', origin_json, 'lineage_json', lineage_json,
                   'content_hash', content_hash, 'created_at', created_at, 'updated_at', updated_at,
                   'supersedes_id', supersedes_id
                 ) FROM memory_record ORDER BY id",
            )?,
            states: query_json_rows(
                conn,
                "SELECT json_object(
                   'record_id', record_id, 'automatic_recall_until', automatic_recall_until,
                   'validity_until', validity_until, 'retain_until', retain_until,
                   'pinned', pinned, 'quarantined', quarantined,
                   'quarantine_reason_code', quarantine_reason_code,
                   'record_version', record_version,
                   'automatic_recall_event_id', automatic_recall_event_id,
                   'validity_event_id', validity_event_id,
                   'retention_event_id', retention_event_id,
                   'quarantine_event_id', quarantine_event_id, 'updated_at', updated_at
                 ) FROM private_lifecycle_state ORDER BY record_id",
            )?,
            relations: query_json_rows(
                conn,
                "SELECT json_object(
                   'id', id, 'relation_kind', relation_kind,
                   'subject_record_id', subject_record_id, 'related_record_id', related_record_id,
                   'application_id', application_id, 'created_at', created_at
                 ) FROM private_lifecycle_relation ORDER BY id",
            )?,
            events: query_json_rows(
                conn,
                "SELECT json_object(
                   'id', id, 'event_type', event_type, 'actor', actor,
                   'payload_json', payload_json, 'record_id', record_id,
                   'proposal_id', proposal_id, 'created_at', created_at
                 ) FROM event_log ORDER BY id",
            )?,
            applications: query_json_rows(
                conn,
                "SELECT json_object(
                   'application_id', application_id, 'operation_id', operation_id,
                   'request_id', request_id, 'grant_id', grant_id,
                   'result_json', result_json, 'lifecycle_generation', lifecycle_generation,
                   'applied_at', applied_at
                 ) FROM private_lifecycle_application ORDER BY application_id",
            )?,
            grants: query_json_rows(
                conn,
                "SELECT json_object(
                   'grant_id', grant_id, 'request_id', request_id, 'request_json', request_json,
                   'state', state, 'authorized_at', authorized_at, 'expires_at', expires_at,
                   'revoked_at', revoked_at, 'consumed_at', consumed_at,
                   'consumed_application_id', consumed_application_id
                 ) FROM owner_action_grant ORDER BY grant_id",
            )?,
            tags: query_json_rows(
                conn,
                "SELECT json_object('record_id', record_id, 'tag', tag)
                 FROM memory_tag ORDER BY record_id, tag",
            )?,
            paths: query_json_rows(
                conn,
                "SELECT json_object(
                   'id', id, 'record_id', record_id, 'repo_id', repo_id, 'path', path,
                   'symbol', symbol, 'line_start', line_start, 'line_end', line_end
                 ) FROM memory_path ORDER BY id",
            )?,
            captures: query_json_rows(
                conn,
                "SELECT json_object('record_id', record_id, 'provenance_json', provenance_json)
                 FROM memory_capture ORDER BY record_id",
            )?,
            fts_records: query_json_rows(
                conn,
                "SELECT json_object('rowid', rowid, 'title', title, 'body', body)
                 FROM memory_fts ORDER BY rowid",
            )?,
            generation: lifecycle_generation(conn)?,
            revision: conn
                .query_row(
                    "SELECT revision FROM runtime_mirror_state WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?,
        })
    }

    fn atomic_lifecycle_snapshot(service: &MemoryService) -> Result<AtomicLifecycleSnapshot> {
        Ok(AtomicLifecycleSnapshot {
            shared: lifecycle_database_snapshot(&service.shared_conn)?,
            mirror: lifecycle_database_snapshot(&service.conn)?,
        })
    }

    #[derive(Debug, Clone, Copy)]
    enum GranularFaultAction {
        Renewal,
        Correction,
        Supersession,
        Consolidation,
        Contradiction,
    }

    fn granular_fault_request(
        service: &MemoryService,
        action: GranularFaultAction,
        operation_id: &str,
    ) -> Result<PrivateLifecycleRequest> {
        let action = match action {
            GranularFaultAction::Renewal => {
                let predecessor = local(
                    service,
                    "Atomic renewable claim",
                    "The exact renewable claim remains supported.",
                )?;
                let evidence = local(
                    service,
                    "Atomic renewable claim",
                    "The exact renewable claim remains supported.",
                )?;
                set_explicit_expiry(service, &predecessor, "2026-07-18T12:00:00Z")?;
                crate::capture::store_capture_provenance(
                    &service.shared_conn,
                    &evidence.id,
                    Some(&accepted_capture("2026-07-19T11:00:00Z", '9')),
                )?;
                PrivateLifecycleAction::RenewFromEvidence {
                    predecessor_id: predecessor.id.clone(),
                    expected_predecessor_version: record_version(service, &predecessor.id)?,
                    evidence_record_id: evidence.id.clone(),
                    expected_evidence_version: record_version(service, &evidence.id)?,
                }
            }
            GranularFaultAction::Correction => {
                let target = local(
                    service,
                    "Atomic incorrect claim",
                    "The incorrect claim must survive a rolled-back correction.",
                )?;
                let evidence = local(
                    service,
                    "Atomic correction evidence",
                    "A current private record supports the replacement.",
                )?;
                service.shared_conn.execute(
                    "INSERT INTO memory_tag(record_id, tag) VALUES (?1, 'atomic-correction')",
                    [&target.id],
                )?;
                service.shared_conn.execute(
                    "INSERT INTO memory_path(id, record_id, path, symbol, line_start, line_end)
                     VALUES ('atomic-correction-path', ?1, 'src/atomic.rs', 'claim', 3, 3)",
                    [&target.id],
                )?;
                PrivateLifecycleAction::Correct {
                    record_id: target.id.clone(),
                    expected_version: record_version(service, &target.id)?,
                    replacement: crate::PrivateCorrectionReplacement {
                        title: "Atomic corrected claim".to_owned(),
                        body: "The corrected claim must be rolled back at every injected stage."
                            .to_owned(),
                    },
                    reason_code: "accepted_evidence".to_owned(),
                    evidence_versions: BTreeMap::from([(
                        evidence.id.clone(),
                        record_version(service, &evidence.id)?,
                    )]),
                }
            }
            GranularFaultAction::Supersession => {
                let predecessor = local(
                    service,
                    "Atomic supersession predecessor",
                    "The predecessor must remain active after rollback.",
                )?;
                let successor = local(
                    service,
                    "Atomic supersession successor",
                    "The successor must retain empty lineage after rollback.",
                )?;
                PrivateLifecycleAction::Supersede {
                    record_id: predecessor.id.clone(),
                    expected_version: record_version(service, &predecessor.id)?,
                    successor_record_id: successor.id.clone(),
                    expected_successor_version: record_version(service, &successor.id)?,
                    reason_code: "owner_selected_successor".to_owned(),
                }
            }
            GranularFaultAction::Consolidation => {
                let records = [
                    local(service, "Atomic duplicate", "Identical atomic duplicate.")?,
                    local(service, "Atomic duplicate", "Identical atomic duplicate.")?,
                    local(service, "Atomic duplicate", "Identical atomic duplicate.")?,
                ];
                let record_ids = records
                    .iter()
                    .map(|record| record.id.clone())
                    .collect::<Vec<_>>();
                PrivateLifecycleAction::Consolidate {
                    expected_versions: record_versions(service, &record_ids)?,
                    keeper_record_id: record_ids[2].clone(),
                    record_ids,
                }
            }
            GranularFaultAction::Contradiction => {
                let records = [
                    local(service, "Atomic feature flag", "The feature flag is true")?,
                    local(service, "Atomic feature flag", "The feature flag is false")?,
                    local(service, "Atomic feature flag", "The feature flag is true")?,
                ];
                let record_ids = records
                    .iter()
                    .map(|record| record.id.clone())
                    .collect::<Vec<_>>();
                PrivateLifecycleAction::ResolveContradiction {
                    expected_versions: record_versions(service, &record_ids)?,
                    winner_record_id: record_ids[1].clone(),
                    record_ids,
                }
            }
        };
        PrivateLifecycleRequest::with_computed_id(
            operation_id,
            PrivateLifecycleSource::Direct,
            vec![action],
        )
    }

    #[test]
    fn authorize_only_inserts_one_grant_and_apply_replays_exactly() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let record = local(&service, "Pinned fact", "The pinned body remains private.")?;
        let request = pin_request(&service, "operation-pin-1", &record.id)?;
        let before = non_grant_snapshot(&service)?;

        let grant = service.authorize_private_lifecycle(&request, None, None)?;
        let repeated = service.authorize_private_lifecycle(&request, None, None)?;
        assert_eq!(grant.grant_id, repeated.grant_id);
        assert_eq!(non_grant_snapshot(&service)?, before);
        let grants: i64 = service.shared_conn.query_row(
            "SELECT COUNT(*) FROM owner_action_grant",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(grants, 1);

        let result = service.apply_private_lifecycle(&request, &grant.grant_id, None)?;
        assert!(!result.replayed);
        assert!(
            service
                .inspect_private_lifecycle_record(&record.id)?
                .state
                .pinned
        );
        assert_eq!(
            service
                .inspect_private_lifecycle_grant(&grant.grant_id)?
                .state,
            crate::OwnerActionGrantState::Consumed
        );
        let replay = service.apply_private_lifecycle(&request, &grant.grant_id, None)?;
        assert!(replay.replayed);
        assert_eq!(replay.application_id, result.application_id);
        assert_eq!(replay.actions, result.actions);

        let applications: i64 = service.shared_conn.query_row(
            "SELECT COUNT(*) FROM private_lifecycle_application",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(applications, 1);
        let payload: String = service.shared_conn.query_row(
            "SELECT payload_json FROM event_log
             WHERE event_type = 'memory.private_lifecycle_applied'",
            [],
            |row| row.get(0),
        )?;
        assert!(shared_runtime::lifecycle_generations_match(
            &service.shared_conn,
            &service.conn,
        )?);
        let payload: serde_json::Value = serde_json::from_str(&payload)?;
        assert_eq!(payload["grant_id"], grant.grant_id);
        assert_eq!(payload["operation_id"], request.operation_id);
        for forbidden in [
            "title",
            "body",
            "evidence",
            "provenance",
            "request_id",
            "content_hash",
            "version",
        ] {
            assert!(payload.get(forbidden).is_none(), "audit leaked {forbidden}");
        }
        Ok(())
    }

    #[test]
    fn quarantine_is_reversible_and_excluded_from_ordinary_reads() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let record = local(
            &service,
            "Quarantine fact",
            "This distinctive quarantine phrase must leave ordinary recall.",
        )?;
        let version = service
            .inspect_private_lifecycle_record(&record.id)?
            .version;
        let request = PrivateLifecycleRequest::with_computed_id(
            "operation-quarantine-1",
            PrivateLifecycleSource::Direct,
            vec![PrivateLifecycleAction::Quarantine {
                record_id: record.id.clone(),
                expected_version: version,
                reason_code: "owner_requested".to_owned(),
            }],
        )?;
        let grant = service.authorize_private_lifecycle(&request, None, None)?;
        service.apply_private_lifecycle(&request, &grant.grant_id, None)?;
        let mirrored_authority_events: i64 = service.conn.query_row(
            "SELECT COUNT(*) FROM event_log
             WHERE event_type = 'memory.private_lifecycle_applied'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(mirrored_authority_events, 1);
        assert!(service.list_local_memory()?.is_empty());
        assert!(
            service
                .search_local_memory("distinctive quarantine".to_owned(), 10)?
                .is_empty()
        );
        let inspection = service.inspect_private_lifecycle_record(&record.id)?;
        assert!(inspection.state.quarantined);
        assert_eq!(inspection.record.body, record.body);

        let release = PrivateLifecycleRequest::with_computed_id(
            "operation-release-1",
            PrivateLifecycleSource::Direct,
            vec![PrivateLifecycleAction::ReleaseQuarantine {
                record_id: record.id.clone(),
                expected_version: inspection.version,
            }],
        )?;
        let grant = service.authorize_private_lifecycle(&release, None, None)?;
        service.apply_private_lifecycle(&release, &grant.grant_id, None)?;
        let released = service.inspect_private_lifecycle_record(&record.id)?;
        assert!(!released.state.quarantined);
        assert_eq!(service.list_local_memory()?.len(), 1);
        Ok(())
    }

    #[test]
    fn revoke_and_operation_conflict_are_typed_zero_write_outcomes() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let first = local(&service, "First fact", "First exact operation record.")?;
        let second = local(&service, "Second fact", "Second exact operation record.")?;

        let revoked_request = pin_request(&service, "operation-revoked", &first.id)?;
        let revoked_grant = service.authorize_private_lifecycle(&revoked_request, None, None)?;
        assert_eq!(
            service
                .revoke_private_lifecycle(&revoked_grant.grant_id)?
                .outcome,
            PrivateLifecycleRevokeOutcome::Revoked
        );
        assert_eq!(
            service
                .revoke_private_lifecycle(&revoked_grant.grant_id)?
                .outcome,
            PrivateLifecycleRevokeOutcome::AlreadyRevoked
        );
        assert!(
            service
                .apply_private_lifecycle(&revoked_request, &revoked_grant.grant_id, None)
                .is_err()
        );
        assert!(
            !service
                .inspect_private_lifecycle_record(&first.id)?
                .state
                .pinned
        );

        let first_request = pin_request(&service, "shared-operation-id", &first.id)?;
        let first_grant = service.authorize_private_lifecycle(&first_request, None, None)?;
        service.apply_private_lifecycle(&first_request, &first_grant.grant_id, None)?;
        let second_request = pin_request(&service, "shared-operation-id", &second.id)?;
        let error = service
            .authorize_private_lifecycle(&second_request, None, None)
            .expect_err(
                "same operation ID with a different request must conflict at authorization",
            );
        assert!(format!("{error:#}").contains("operation_id_conflict"));
        assert!(
            !service
                .inspect_private_lifecycle_record(&second.id)?
                .state
                .pinned
        );
        Ok(())
    }

    #[test]
    fn authorization_reuses_consumed_operation_authority_and_conflicts_permanently() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let first = local(&service, "Historical first", "Applied historical request.")?;
        let second = local(
            &service,
            "Historical second",
            "Conflicting historical request.",
        )?;
        let request = pin_request(&service, "operation-historical", &first.id)?;
        let grant = service.authorize_private_lifecycle(&request, None, None)?;
        let applied = service.apply_private_lifecycle(&request, &grant.grant_id, None)?;

        let historical = service.authorize_private_lifecycle(&request, None, None)?;
        assert_eq!(historical.grant_id, grant.grant_id);
        assert_eq!(
            historical.consumed_application_id,
            Some(applied.application_id)
        );
        assert_eq!(historical.state, crate::OwnerActionGrantState::Consumed);
        let grants: i64 = service.shared_conn.query_row(
            "SELECT COUNT(*) FROM owner_action_grant",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(grants, 1);

        let conflict = pin_request(&service, "operation-historical", &second.id)?;
        let error = service
            .authorize_private_lifecycle(&conflict, None, None)
            .expect_err("a recorded operation cannot be rebound during authorization");
        assert!(format!("{error:#}").contains("operation_id_conflict"));
        let grants_after: i64 = service.shared_conn.query_row(
            "SELECT COUNT(*) FROM owner_action_grant",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(grants_after, 1);
        assert!(
            !service
                .inspect_private_lifecycle_record(&second.id)?
                .state
                .pinned
        );
        Ok(())
    }

    #[test]
    fn every_precommit_apply_fault_rolls_back_the_group_and_leaves_grant_active() -> Result<()> {
        for stage in [
            LifecycleApplyFault::AfterValidation,
            LifecycleApplyFault::DuringMutations,
            LifecycleApplyFault::AfterAuditInsert,
            LifecycleApplyFault::AfterReceiptInsert,
            LifecycleApplyFault::AfterGrantConsume,
        ] {
            let (_temp, service) = initialized_service()?;
            let first = local(&service, "Atomic first", "First atomic lifecycle target.")?;
            let second = local(&service, "Atomic second", "Second atomic lifecycle target.")?;
            let request = PrivateLifecycleRequest::with_computed_id(
                format!("operation-atomic-{stage:?}"),
                PrivateLifecycleSource::Direct,
                vec![
                    PrivateLifecycleAction::Pin {
                        record_id: first.id.clone(),
                        expected_version: record_version(&service, &first.id)?,
                    },
                    PrivateLifecycleAction::Pin {
                        record_id: second.id.clone(),
                        expected_version: record_version(&service, &second.id)?,
                    },
                ],
            )?;
            let grant = service.authorize_private_lifecycle(&request, None, None)?;
            let before_non_grant = non_grant_snapshot(&service)?;
            let before_first = service.inspect_private_lifecycle_record(&first.id)?;
            let before_second = service.inspect_private_lifecycle_record(&second.id)?;

            let error = {
                let _fault = LifecycleApplyFaultGuard::install(stage);
                service
                    .apply_private_lifecycle(&request, &grant.grant_id, None)
                    .expect_err("injected pre-commit apply failure must abort")
            };
            assert!(format!("{error:#}").contains("test_injected_private_lifecycle_apply_failure"));
            assert_eq!(non_grant_snapshot(&service)?, before_non_grant, "{stage:?}");
            assert_eq!(
                service.inspect_private_lifecycle_record(&first.id)?,
                before_first,
                "{stage:?} changed first target"
            );
            assert_eq!(
                service.inspect_private_lifecycle_record(&second.id)?,
                before_second,
                "{stage:?} changed second target"
            );
            assert_eq!(
                service
                    .inspect_private_lifecycle_grant(&grant.grant_id)?
                    .state,
                crate::OwnerActionGrantState::Active,
                "{stage:?} consumed the grant"
            );
        }
        Ok(())
    }

    #[test]
    fn every_granular_multistatement_fault_rolls_back_all_lifecycle_data_and_authority()
    -> Result<()> {
        for (action, stage) in [
            (
                GranularFaultAction::Renewal,
                LifecycleApplyFault::AfterRenewalPredecessorStatus,
            ),
            (
                GranularFaultAction::Renewal,
                LifecycleApplyFault::AfterRenewalSuccessorLink,
            ),
            (
                GranularFaultAction::Renewal,
                LifecycleApplyFault::AfterRenewalRelation,
            ),
            (
                GranularFaultAction::Correction,
                LifecycleApplyFault::AfterCorrectionReplacementInsert,
            ),
            (
                GranularFaultAction::Correction,
                LifecycleApplyFault::AfterCorrectionReplacementState,
            ),
            (
                GranularFaultAction::Correction,
                LifecycleApplyFault::AfterCorrectionTagCopy,
            ),
            (
                GranularFaultAction::Correction,
                LifecycleApplyFault::AfterCorrectionPathCopy,
            ),
            (
                GranularFaultAction::Correction,
                LifecycleApplyFault::AfterCorrectionPredecessorStatus,
            ),
            (
                GranularFaultAction::Correction,
                LifecycleApplyFault::AfterCorrectionRelation,
            ),
            (
                GranularFaultAction::Supersession,
                LifecycleApplyFault::AfterSupersessionPredecessorStatus,
            ),
            (
                GranularFaultAction::Supersession,
                LifecycleApplyFault::AfterSupersessionSuccessorLink,
            ),
            (
                GranularFaultAction::Supersession,
                LifecycleApplyFault::AfterSupersessionRelation,
            ),
            (
                GranularFaultAction::Consolidation,
                LifecycleApplyFault::AfterConsolidationMemberStatus,
            ),
            (
                GranularFaultAction::Consolidation,
                LifecycleApplyFault::AfterConsolidationMemberRelation,
            ),
            (
                GranularFaultAction::Contradiction,
                LifecycleApplyFault::AfterContradictionMemberStatus,
            ),
            (
                GranularFaultAction::Contradiction,
                LifecycleApplyFault::AfterContradictionMemberRelation,
            ),
        ] {
            let (_temp, service) = initialized_service()?;
            let operation_id = format!("operation-granular-{action:?}-{stage:?}");
            let request = granular_fault_request(&service, action, &operation_id)?;
            let grant = service.authorize_private_lifecycle(&request, None, None)?;
            let grant_before = service.inspect_private_lifecycle_grant(&grant.grant_id)?;
            let before = atomic_lifecycle_snapshot(&service)?;

            let error = {
                let _fault = LifecycleApplyFaultGuard::install(stage);
                service
                    .apply_private_lifecycle(&request, &grant.grant_id, None)
                    .expect_err("an injected mid-action failure must abort the shared transaction")
            };
            assert!(
                format!("{error:#}").contains("test_injected_private_lifecycle_apply_failure"),
                "unexpected error at {stage:?}: {error:#}"
            );
            assert_eq!(
                atomic_lifecycle_snapshot(&service)?,
                before,
                "{stage:?} left a record, state, relation, event, receipt, generation, or mirror change"
            );
            assert_eq!(
                service.inspect_private_lifecycle_grant(&grant.grant_id)?,
                grant_before,
                "{stage:?} changed the one-shot grant"
            );
            assert_eq!(
                grant_before.state,
                crate::OwnerActionGrantState::Active,
                "test fixture grant was not active"
            );
        }
        Ok(())
    }

    #[test]
    fn postcommit_mirror_failure_is_repaired_by_exact_replay() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let record = local(
            &service,
            "Mirror recovery",
            "Committed before mirror convergence.",
        )?;
        let request = pin_request(&service, "operation-mirror-recovery", &record.id)?;
        let grant = service.authorize_private_lifecycle(&request, None, None)?;

        let error = {
            let _fault = LifecycleApplyFaultGuard::install(
                LifecycleApplyFault::AfterSharedCommitBeforeMirror,
            );
            service
                .apply_private_lifecycle(&request, &grant.grant_id, None)
                .expect_err("post-commit mirror fault must surface")
        };
        assert!(format!("{error:#}").contains("test_injected_private_lifecycle_apply_failure"));
        assert!(
            service
                .inspect_private_lifecycle_record(&record.id)?
                .state
                .pinned
        );
        assert_eq!(
            service
                .inspect_private_lifecycle_grant(&grant.grant_id)?
                .state,
            crate::OwnerActionGrantState::Consumed
        );
        assert!(!shared_runtime::lifecycle_generations_match(
            &service.shared_conn,
            &service.conn,
        )?);

        let replay = service.apply_private_lifecycle(&request, &grant.grant_id, None)?;
        assert!(replay.replayed);
        assert!(shared_runtime::lifecycle_generations_match(
            &service.shared_conn,
            &service.conn,
        )?);
        Ok(())
    }

    #[test]
    fn reauthorization_cannot_ignore_a_new_shorter_expiry() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let record = local(
            &service,
            "Bounded authority",
            "Repeated authorization must not widen the requested time window.",
        )?;
        let request = pin_request(&service, "operation-bounded-authority", &record.id)?;
        let grant = service.authorize_private_lifecycle(&request, None, None)?;
        assert_eq!(grant.expires_at, "2026-07-20T12:00:00Z");
        let before = atomic_lifecycle_snapshot(&service)?;

        let error = service
            .authorize_private_lifecycle(&request, None, Some("2026-07-19T13:00:00Z"))
            .expect_err(
                "an active grant with a longer lifetime must not satisfy shorter authority",
            );
        assert!(format!("{error:#}").contains("owner_action_grant_expiry_conflict"));
        assert_eq!(atomic_lifecycle_snapshot(&service)?, before);

        let repeated =
            service.authorize_private_lifecycle(&request, None, Some("2026-07-20T13:00:00Z"))?;
        assert_eq!(repeated.grant_id, grant.grant_id);
        assert_eq!(repeated.expires_at, grant.expires_at);
        assert_eq!(atomic_lifecycle_snapshot(&service)?, before);
        Ok(())
    }

    #[test]
    fn reauthorization_compares_fractional_expiry_instants_chronologically() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let paths = service.paths.clone();
        let record = local(
            &service,
            "Fractional expiry",
            "Grant liveness is an instant comparison, not an RFC 3339 text comparison.",
        )?;
        let request = pin_request(&service, "operation-fractional-expiry", &record.id)?;
        let expired_grant = service.authorize_private_lifecycle(&request, None, None)?;
        assert_eq!(expired_grant.expires_at, "2026-07-20T12:00:00Z");
        drop(service);

        let service = MemoryService::open_paths_with_clock(
            paths,
            FixedClock::from_rfc3339("2026-07-20T12:00:00.1Z")?,
        )?;
        let replacement = service.authorize_private_lifecycle(&request, None, None)?;
        assert_ne!(replacement.grant_id, expired_grant.grant_id);
        assert_eq!(replacement.authorized_at, "2026-07-20T12:00:00.1Z");
        assert_eq!(replacement.expires_at, "2026-07-21T12:00:00.1Z");
        let grants: i64 = service.shared_conn.query_row(
            "SELECT COUNT(*) FROM owner_action_grant WHERE request_id = ?1",
            [&request.request_id],
            |row| row.get(0),
        )?;
        assert_eq!(grants, 2);
        Ok(())
    }

    #[test]
    fn invalid_group_is_rejected_before_grant_creation() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let checkpoint = service.create_checkpoint(
            "test",
            crate::CheckpointInput {
                task: "Session cannot pin".to_owned(),
                note: "This session record cannot be pinned indefinitely.".to_owned(),
            },
        )?;
        let request = pin_request(&service, "operation-session-pin", &checkpoint.id)?;
        let error = service
            .authorize_private_lifecycle(&request, None, None)
            .expect_err("session pin must fail during authorization validation");
        assert!(format!("{error:#}").contains("session pinning"));
        let grants: i64 = service.shared_conn.query_row(
            "SELECT COUNT(*) FROM owner_action_grant",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(grants, 0);
        Ok(())
    }

    #[test]
    fn grant_expiry_is_clock_driven_and_expired_apply_is_zero_write() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let paths = service.paths.clone();
        let record = local(
            &service,
            "Expiring authority",
            "Authority expires independently.",
        )?;
        let request = pin_request(&service, "operation-expiring-authority", &record.id)?;
        let grant =
            service.authorize_private_lifecycle(&request, None, Some("2026-07-19T13:00:00Z"))?;
        assert_eq!(grant.authorized_at, "2026-07-19T12:00:00Z");
        assert_eq!(grant.expires_at, "2026-07-19T13:00:00Z");
        drop(service);

        let expired = MemoryService::open_paths_with_clock(
            paths,
            FixedClock::from_rfc3339("2026-07-19T13:00:00Z")?,
        )?;
        let before = non_grant_snapshot(&expired)?;
        let error = expired
            .apply_private_lifecycle(&request, &grant.grant_id, None)
            .expect_err("the inclusive grant expiry instant must reject apply");
        assert!(format!("{error:#}").contains("owner_action_grant_expired"));
        assert_eq!(non_grant_snapshot(&expired)?, before);
        assert_eq!(
            expired
                .inspect_private_lifecycle_grant(&grant.grant_id)?
                .state,
            crate::OwnerActionGrantState::Active
        );
        assert!(
            !expired
                .inspect_private_lifecycle_record(&record.id)?
                .state
                .pinned
        );
        Ok(())
    }

    #[test]
    fn planned_duplicate_members_are_sets_and_apply_revalidates_the_plan() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        local(
            &service,
            "Planned duplicate",
            "Exact planned duplicate claim.",
        )?;
        local(
            &service,
            "Planned duplicate",
            "Exact planned duplicate claim.",
        )?;
        let (plan, request, reversed_ids) =
            planned_duplicate_request(&service, "operation-planned-duplicate")?;
        let grant = service.authorize_private_lifecycle(&request, Some(&plan), None)?;
        service.apply_private_lifecycle(&request, &grant.grant_id, Some(&plan))?;
        assert_eq!(
            service
                .inspect_private_lifecycle_record(&reversed_ids[0])?
                .record
                .status,
            MemoryStatus::Active
        );
        assert_eq!(
            service
                .inspect_private_lifecycle_record(&reversed_ids[1])?
                .record
                .status,
            MemoryStatus::Superseded
        );

        let (_temp, stale_service) = initialized_service()?;
        local(
            &stale_service,
            "Stale planned duplicate",
            "Exact stale planned duplicate claim.",
        )?;
        local(
            &stale_service,
            "Stale planned duplicate",
            "Exact stale planned duplicate claim.",
        )?;
        let (stale_plan, stale_request, stale_ids) =
            planned_duplicate_request(&stale_service, "operation-stale-planned-duplicate")?;
        let stale_grant =
            stale_service.authorize_private_lifecycle(&stale_request, Some(&stale_plan), None)?;
        local(
            &stale_service,
            "Stale planned duplicate",
            "Exact stale planned duplicate claim.",
        )?;
        let before = non_grant_snapshot(&stale_service)?;
        let error = stale_service
            .apply_private_lifecycle(&stale_request, &stale_grant.grant_id, Some(&stale_plan))
            .expect_err("comparison-neighbourhood drift must stale the planned request");
        assert!(format!("{error:#}").contains("maintenance plan is stale"));
        assert_eq!(non_grant_snapshot(&stale_service)?, before);
        assert_eq!(
            stale_service
                .inspect_private_lifecycle_grant(&stale_grant.grant_id)?
                .state,
            crate::OwnerActionGrantState::Active
        );
        for record_id in stale_ids {
            assert_eq!(
                stale_service
                    .inspect_private_lifecycle_record(&record_id)?
                    .record
                    .status,
                MemoryStatus::Active
            );
        }
        Ok(())
    }

    #[test]
    fn direct_renewal_rejects_non_claim_bearing_types_before_grant_creation() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let predecessor = service.create_local_memory(
            "test",
            LocalMemoryInput {
                memory_type: MemoryType::Risk,
                lane: MemoryLane::Semantic,
                title: "Non-claim renewal".to_owned(),
                body: "A risk record is not an eligible renewable claim.".to_owned(),
            },
        )?;
        let evidence = service.create_local_memory(
            "test",
            LocalMemoryInput {
                memory_type: MemoryType::Risk,
                lane: MemoryLane::Semantic,
                title: predecessor.title.clone(),
                body: predecessor.body.clone(),
            },
        )?;
        set_explicit_expiry(&service, &predecessor, "2026-07-18T12:00:00Z")?;
        let request = PrivateLifecycleRequest::with_computed_id(
            "operation-non-claim-renewal",
            PrivateLifecycleSource::Direct,
            vec![PrivateLifecycleAction::RenewFromEvidence {
                predecessor_id: predecessor.id.clone(),
                expected_predecessor_version: record_version(&service, &predecessor.id)?,
                evidence_record_id: evidence.id.clone(),
                expected_evidence_version: record_version(&service, &evidence.id)?,
            }],
        )?;
        let error = service
            .authorize_private_lifecycle(&request, None, None)
            .expect_err("non-claim-bearing renewal must fail before grant creation");
        assert!(format!("{error:#}").contains("semantic or procedural claims"));
        let grants: i64 = service.shared_conn.query_row(
            "SELECT COUNT(*) FROM owner_action_grant",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(grants, 0);
        Ok(())
    }

    #[test]
    fn episodic_automatic_recall_is_capped_at_ninety_days_without_changing_other_clocks()
    -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let admissible = service.create_local_memory(
            "test",
            LocalMemoryInput {
                memory_type: MemoryType::Episode,
                lane: MemoryLane::Episodic,
                title: "Episodic recall cap".to_owned(),
                body: "An episodic record may be recalled for at most ninety days.".to_owned(),
            },
        )?;
        let maximum = timestamp_after(90)?;
        authorize_and_apply(
            &service,
            "operation-recall-maximum",
            vec![PrivateLifecycleAction::ExtendAutomaticRecall {
                record_id: admissible.id.clone(),
                expected_version: record_version(&service, &admissible.id)?,
                recall_until: maximum.clone(),
            }],
        )?;
        let state = service
            .inspect_private_lifecycle_record(&admissible.id)?
            .state;
        assert_eq!(state.automatic_recall_until, Some(maximum));
        assert_eq!(state.validity_until, None);
        assert_eq!(state.retain_until, None);
        assert!(!state.pinned);

        let rejected = service.create_local_memory(
            "test",
            LocalMemoryInput {
                memory_type: MemoryType::Episode,
                lane: MemoryLane::Episodic,
                title: "Episodic over-cap".to_owned(),
                body: "This extension exceeds the hard occurrence-relative cap.".to_owned(),
            },
        )?;
        let over_maximum = crate::expiry::format_timestamp(
            crate::private_lifecycle::parse_timestamp("2026-07-19T12:00:00Z", "test clock")?
                + Duration::days(90)
                + Duration::seconds(1),
        )?;
        let request = PrivateLifecycleRequest::with_computed_id(
            "operation-recall-over-maximum",
            PrivateLifecycleSource::Direct,
            vec![PrivateLifecycleAction::ExtendAutomaticRecall {
                record_id: rejected.id.clone(),
                expected_version: record_version(&service, &rejected.id)?,
                recall_until: over_maximum,
            }],
        )?;
        let error = service
            .authorize_private_lifecycle(&request, None, None)
            .expect_err("recall beyond occurrence plus ninety days must be rejected");
        assert!(format!("{error:#}").contains("beyond 90 days"));
        assert_eq!(
            service
                .inspect_private_lifecycle_record(&rejected.id)?
                .state
                .automatic_recall_until,
            None
        );
        Ok(())
    }

    #[test]
    fn validity_retention_and_pin_are_independent_and_unpin_preserves_finite_retention()
    -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let record = local(
            &service,
            "Independent lifecycle clocks",
            "Validity, finite retention, and indefinite pinning are separate facts.",
        )?;
        let original_validity = timestamp_after(1)?;
        set_explicit_expiry(&service, &record, &original_validity)?;
        let retained_until = timestamp_after(10)?;
        let validity_until = timestamp_after(20)?;

        authorize_and_apply(
            &service,
            "operation-retain-independent",
            vec![PrivateLifecycleAction::RetainUntil {
                record_id: record.id.clone(),
                expected_version: record_version(&service, &record.id)?,
                retain_until: retained_until.clone(),
            }],
        )?;
        authorize_and_apply(
            &service,
            "operation-pin-independent",
            vec![PrivateLifecycleAction::Pin {
                record_id: record.id.clone(),
                expected_version: record_version(&service, &record.id)?,
            }],
        )?;
        authorize_and_apply(
            &service,
            "operation-validity-independent",
            vec![PrivateLifecycleAction::ExtendValidity {
                record_id: record.id.clone(),
                expected_version: record_version(&service, &record.id)?,
                validity_until: validity_until.clone(),
            }],
        )?;
        let before_unpin = service.inspect_private_lifecycle_record(&record.id)?;
        assert_eq!(
            before_unpin.state.retain_until,
            Some(retained_until.clone())
        );
        assert_eq!(
            before_unpin.state.validity_until,
            Some(validity_until.clone())
        );
        assert!(before_unpin.state.pinned);
        assert_eq!(
            before_unpin.record.retention.explicit_expires_at,
            Some(original_validity)
        );

        authorize_and_apply(
            &service,
            "operation-unpin-independent",
            vec![PrivateLifecycleAction::Unpin {
                record_id: record.id.clone(),
                expected_version: before_unpin.version,
            }],
        )?;
        let after_unpin = service.inspect_private_lifecycle_record(&record.id)?;
        assert!(!after_unpin.state.pinned);
        assert_eq!(after_unpin.state.retain_until, Some(retained_until.clone()));
        assert_eq!(after_unpin.state.validity_until, Some(validity_until));

        let shortening = PrivateLifecycleRequest::with_computed_id(
            "operation-retain-shortening",
            PrivateLifecycleSource::Direct,
            vec![PrivateLifecycleAction::RetainUntil {
                record_id: record.id.clone(),
                expected_version: after_unpin.version,
                retain_until: timestamp_after(5)?,
            }],
        )?;
        let error = service
            .authorize_private_lifecycle(&shortening, None, None)
            .expect_err("finite retention shortening must be rejected");
        assert!(format!("{error:#}").contains("monotonic"));
        assert_eq!(
            service
                .inspect_private_lifecycle_record(&record.id)?
                .state
                .retain_until,
            Some(retained_until)
        );
        Ok(())
    }

    #[test]
    fn renewal_promotes_fresh_accepted_evidence_without_creating_a_third_record() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let predecessor = local(
            &service,
            "Renewable claim",
            "The release channel is stable.",
        )?;
        let evidence = local(
            &service,
            "Renewable claim",
            "The release channel is stable.",
        )?;
        set_explicit_expiry(&service, &predecessor, "2026-07-18T12:00:00Z")?;
        crate::capture::store_capture_provenance(
            &service.shared_conn,
            &evidence.id,
            Some(&accepted_capture("2026-07-19T11:00:00Z", '7')),
        )?;
        let record_count_before: i64 =
            service
                .shared_conn
                .query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))?;

        authorize_and_apply(
            &service,
            "operation-renew-evidence",
            vec![PrivateLifecycleAction::RenewFromEvidence {
                predecessor_id: predecessor.id.clone(),
                expected_predecessor_version: record_version(&service, &predecessor.id)?,
                evidence_record_id: evidence.id.clone(),
                expected_evidence_version: record_version(&service, &evidence.id)?,
            }],
        )?;
        let record_count_after: i64 =
            service
                .shared_conn
                .query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))?;
        assert_eq!(record_count_after, record_count_before);
        let predecessor_after = service.inspect_private_lifecycle_record(&predecessor.id)?;
        let evidence_after = service.inspect_private_lifecycle_record(&evidence.id)?;
        assert_eq!(predecessor_after.record.status, MemoryStatus::Superseded);
        assert_eq!(evidence_after.record.status, MemoryStatus::Active);
        assert_eq!(
            evidence_after.record.supersedes_id.as_deref(),
            Some(predecessor.id.as_str())
        );
        assert_eq!(
            evidence_after.record.lineage,
            Some(crate::RecordLineage {
                kind: crate::RecordLineageKind::Renewal,
                predecessor_id: predecessor.id.clone(),
            })
        );
        assert!(predecessor_after.relations.iter().any(|relation| {
            relation.kind == "renewed_by" && relation.related_record_id == evidence.id
        }));
        Ok(())
    }

    #[test]
    fn supersession_preserves_owner_successor_and_rejects_preexisting_lineage() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let target = local(
            &service,
            "Old compatible claim",
            "The old compatible content.",
        )?;
        let successor = local(
            &service,
            "New compatible claim",
            "The owner-selected compatible successor content.",
        )?;
        authorize_and_apply(
            &service,
            "operation-compatible-supersession",
            vec![PrivateLifecycleAction::Supersede {
                record_id: target.id.clone(),
                expected_version: record_version(&service, &target.id)?,
                successor_record_id: successor.id.clone(),
                expected_successor_version: record_version(&service, &successor.id)?,
                reason_code: "owner_selected_successor".to_owned(),
            }],
        )?;
        assert_eq!(
            service
                .inspect_private_lifecycle_record(&target.id)?
                .record
                .status,
            MemoryStatus::Superseded
        );
        let successor_after = service.inspect_private_lifecycle_record(&successor.id)?;
        assert_eq!(successor_after.record.status, MemoryStatus::Active);
        assert_eq!(
            successor_after.record.supersedes_id.as_deref(),
            Some(target.id.as_str())
        );

        let rejected_target = local(
            &service,
            "Lineage target",
            "A target for an already-lineaged successor.",
        )?;
        let rejected_successor = local(
            &service,
            "Lineage successor",
            "This active record already belongs to another lineage.",
        )?;
        let lineage = crate::RecordLineage {
            kind: crate::RecordLineageKind::SessionSuccessor,
            predecessor_id: successor.id.clone(),
        };
        service.shared_conn.execute(
            "UPDATE memory_record SET lineage_json = ?1 WHERE id = ?2",
            rusqlite::params![serde_json::to_string(&lineage)?, rejected_successor.id],
        )?;
        let request = PrivateLifecycleRequest::with_computed_id(
            "operation-reject-existing-lineage",
            PrivateLifecycleSource::Direct,
            vec![PrivateLifecycleAction::Supersede {
                record_id: rejected_target.id.clone(),
                expected_version: record_version(&service, &rejected_target.id)?,
                successor_record_id: rejected_successor.id.clone(),
                expected_successor_version: record_version(&service, &rejected_successor.id)?,
                reason_code: "must_not_rebind".to_owned(),
            }],
        )?;
        let error = service
            .authorize_private_lifecycle(&request, None, None)
            .expect_err("successor with lineage must not be rebound");
        assert!(format!("{error:#}").contains("lineage history"));
        Ok(())
    }

    #[test]
    fn renewal_rejects_evidence_already_bound_to_lineage() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let predecessor = local(
            &service,
            "Lineaged renewal",
            "The renewal claim remains exactly supported.",
        )?;
        let evidence = local(
            &service,
            "Lineaged renewal",
            "The renewal claim remains exactly supported.",
        )?;
        let prior = local(
            &service,
            "Prior lineage",
            "An existing lineage predecessor.",
        )?;
        set_explicit_expiry(&service, &predecessor, "2026-07-18T12:00:00Z")?;
        crate::capture::store_capture_provenance(
            &service.shared_conn,
            &evidence.id,
            Some(&accepted_capture("2026-07-19T11:00:00Z", '8')),
        )?;
        let lineage = crate::RecordLineage {
            kind: crate::RecordLineageKind::SessionSuccessor,
            predecessor_id: prior.id,
        };
        service.shared_conn.execute(
            "UPDATE memory_record SET lineage_json = ?1 WHERE id = ?2",
            rusqlite::params![serde_json::to_string(&lineage)?, evidence.id],
        )?;
        let request = PrivateLifecycleRequest::with_computed_id(
            "operation-renew-existing-lineage",
            PrivateLifecycleSource::Direct,
            vec![PrivateLifecycleAction::RenewFromEvidence {
                predecessor_id: predecessor.id.clone(),
                expected_predecessor_version: record_version(&service, &predecessor.id)?,
                evidence_record_id: evidence.id.clone(),
                expected_evidence_version: record_version(&service, &evidence.id)?,
            }],
        )?;
        let error = service
            .authorize_private_lifecycle(&request, None, None)
            .expect_err("lineaged renewal evidence must not be rebound");
        assert!(format!("{error:#}").contains("lineage history"));
        Ok(())
    }

    #[test]
    fn correction_preserves_classification_temporal_facts_tags_and_paths() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let target = local(
            &service,
            "Incorrect private fact",
            "The original private content is incorrect.",
        )?;
        let evidence = local(
            &service,
            "Correction evidence",
            "A current accepted private record supports the correction.",
        )?;
        let original_validity = timestamp_after(1)?;
        set_explicit_expiry(&service, &target, &original_validity)?;
        let retained_until = timestamp_after(15)?;
        authorize_and_apply(
            &service,
            "operation-correction-retain",
            vec![PrivateLifecycleAction::RetainUntil {
                record_id: target.id.clone(),
                expected_version: record_version(&service, &target.id)?,
                retain_until: retained_until.clone(),
            }],
        )?;
        authorize_and_apply(
            &service,
            "operation-correction-pin",
            vec![PrivateLifecycleAction::Pin {
                record_id: target.id.clone(),
                expected_version: record_version(&service, &target.id)?,
            }],
        )?;
        service.shared_conn.execute(
            "INSERT INTO memory_tag(record_id, tag) VALUES (?1, 'correction-test')",
            [&target.id],
        )?;
        service.shared_conn.execute(
            "INSERT INTO memory_path(id, record_id, path, symbol, line_start, line_end)
             VALUES ('correction-test-path', ?1, 'src/private.rs', 'claim', 7, 9)",
            [&target.id],
        )?;
        let target_before = service.inspect_private_lifecycle_record(&target.id)?;
        let target_state_before =
            PrivateLifecycleStorage::new(&service.shared_conn).require_state(&target.id)?;
        let request = PrivateLifecycleRequest::with_computed_id(
            "operation-correct-preserving",
            PrivateLifecycleSource::Direct,
            vec![PrivateLifecycleAction::Correct {
                record_id: target.id.clone(),
                expected_version: target_before.version,
                replacement: crate::PrivateCorrectionReplacement {
                    title: "Corrected private fact".to_owned(),
                    body: "The corrected private content is now accurate.".to_owned(),
                },
                reason_code: "accepted_evidence".to_owned(),
                evidence_versions: BTreeMap::from([(
                    evidence.id.clone(),
                    record_version(&service, &evidence.id)?,
                )]),
            }],
        )?;
        let grant = service.authorize_private_lifecycle(&request, None, None)?;
        let result = service.apply_private_lifecycle(&request, &grant.grant_id, None)?;
        let replacement_id = result.actions[0]
            .replacement_record_id
            .as_ref()
            .context("correction result has no replacement")?;
        let original_after = service.inspect_private_lifecycle_record(&target.id)?;
        let replacement = service.inspect_private_lifecycle_record(replacement_id)?;
        assert_eq!(original_after.record.status, MemoryStatus::Superseded);
        assert_eq!(replacement.record.status, MemoryStatus::Active);
        assert_eq!(replacement.record.memory_type, target.memory_type);
        assert_eq!(replacement.record.lane, target.lane);
        assert_eq!(replacement.record.destination, target.destination);
        assert_eq!(replacement.record.scope_kind, target.scope_kind);
        assert_eq!(replacement.record.scope_id, target.scope_id);
        assert_eq!(replacement.record.visibility, target.visibility);
        assert_eq!(replacement.record.retention, target_before.record.retention);
        assert_eq!(replacement.state.retain_until, Some(retained_until));
        assert!(replacement.state.pinned);
        let replacement_state =
            PrivateLifecycleStorage::new(&service.shared_conn).require_state(replacement_id)?;
        let correction_event_id = replacement_state
            .retention_event_id
            .as_deref()
            .context("corrected retention facts have no correction authority")?;
        assert_ne!(
            Some(correction_event_id),
            target_state_before.retention_event_id.as_deref()
        );
        let correction_payload_json: String = service.shared_conn.query_row(
            "SELECT payload_json FROM event_log WHERE id = ?1",
            [correction_event_id],
            |row| row.get(0),
        )?;
        let correction_payload: LifecycleAuditPayload =
            serde_json::from_str(&correction_payload_json)?;
        assert!(
            correction_payload
                .action_kinds
                .iter()
                .any(|kind| kind == "correct")
        );
        assert!(
            correction_payload
                .target_record_ids
                .contains(replacement_id)
        );
        assert!(!correction_payload.target_record_ids.contains(&evidence.id));
        assert_eq!(replacement.record.title, "Corrected private fact");
        assert_eq!(
            replacement.record.body,
            "The corrected private content is now accurate."
        );
        let copied_tags: i64 = service.shared_conn.query_row(
            "SELECT COUNT(*) FROM memory_tag WHERE record_id = ?1 AND tag = 'correction-test'",
            [replacement_id],
            |row| row.get(0),
        )?;
        let copied_paths: i64 = service.shared_conn.query_row(
            "SELECT COUNT(*) FROM memory_path
             WHERE record_id = ?1 AND path = 'src/private.rs' AND symbol = 'claim'",
            [replacement_id],
            |row| row.get(0),
        )?;
        assert_eq!(copied_tags, 1);
        assert_eq!(copied_paths, 1);
        Ok(())
    }

    #[test]
    fn consolidation_requires_full_duplicate_set_and_keeps_exact_owner_selection() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let records = [
            local(
                &service,
                "Exact duplicate",
                "Identical private duplicate body.",
            )?,
            local(
                &service,
                "Exact duplicate",
                "Identical private duplicate body.",
            )?,
            local(
                &service,
                "Exact duplicate",
                "Identical private duplicate body.",
            )?,
        ];
        let ids = records
            .iter()
            .map(|record| record.id.clone())
            .collect::<Vec<_>>();
        let partial_ids = ids[..2].to_vec();
        let partial = PrivateLifecycleRequest::with_computed_id(
            "operation-partial-consolidation",
            PrivateLifecycleSource::Direct,
            vec![PrivateLifecycleAction::Consolidate {
                record_ids: partial_ids.clone(),
                expected_versions: record_versions(&service, &partial_ids)?,
                keeper_record_id: partial_ids[0].clone(),
            }],
        )?;
        let error = service
            .authorize_private_lifecycle(&partial, None, None)
            .expect_err("partial duplicate selection must be rejected");
        assert!(format!("{error:#}").contains("complete exact duplicate set"));

        let keeper = ids[1].clone();
        authorize_and_apply(
            &service,
            "operation-full-consolidation",
            vec![PrivateLifecycleAction::Consolidate {
                record_ids: ids.clone(),
                expected_versions: record_versions(&service, &ids)?,
                keeper_record_id: keeper.clone(),
            }],
        )?;
        for record_id in &ids {
            let inspection = service.inspect_private_lifecycle_record(record_id)?;
            if record_id == &keeper {
                assert_eq!(inspection.record.status, MemoryStatus::Active);
            } else {
                assert_eq!(inspection.record.status, MemoryStatus::Superseded);
                assert!(inspection.relations.iter().any(|relation| {
                    relation.kind == "consolidated_into" && relation.related_record_id == keeper
                }));
            }
        }
        Ok(())
    }

    #[test]
    fn contradiction_resolution_requires_full_set_and_keeps_exact_owner_winner() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let records = [
            local(&service, "Feature flag", "The feature flag is true")?,
            local(&service, "Feature flag", "The feature flag is false")?,
            local(&service, "Feature flag", "The feature flag is true")?,
        ];
        let ids = records
            .iter()
            .map(|record| record.id.clone())
            .collect::<Vec<_>>();
        let partial_ids = ids[..2].to_vec();
        let partial = PrivateLifecycleRequest::with_computed_id(
            "operation-partial-contradiction",
            PrivateLifecycleSource::Direct,
            vec![PrivateLifecycleAction::ResolveContradiction {
                record_ids: partial_ids.clone(),
                expected_versions: record_versions(&service, &partial_ids)?,
                winner_record_id: partial_ids[1].clone(),
            }],
        )?;
        let error = service
            .authorize_private_lifecycle(&partial, None, None)
            .expect_err("partial contradiction selection must be rejected");
        assert!(format!("{error:#}").contains("complete incompatible set"));

        let winner = ids[1].clone();
        authorize_and_apply(
            &service,
            "operation-full-contradiction",
            vec![PrivateLifecycleAction::ResolveContradiction {
                record_ids: ids.clone(),
                expected_versions: record_versions(&service, &ids)?,
                winner_record_id: winner.clone(),
            }],
        )?;
        for record_id in &ids {
            let inspection = service.inspect_private_lifecycle_record(record_id)?;
            if record_id == &winner {
                assert_eq!(inspection.record.status, MemoryStatus::Active);
            } else {
                assert_eq!(inspection.record.status, MemoryStatus::Superseded);
                assert!(inspection.relations.iter().any(|relation| {
                    relation.kind == "contradiction_resolved_by"
                        && relation.related_record_id == winner
                }));
            }
        }
        Ok(())
    }

    #[test]
    fn correction_replacement_ids_are_opaque_and_preserve_private_destination() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let local = local(
            &service,
            "Secret correction title",
            "A private local correction predecessor.",
        )?;
        let checkpoint = service.create_checkpoint(
            "test",
            crate::CheckpointInput {
                task: "Secret checkpoint correction title".to_owned(),
                note: "A private session correction predecessor.".to_owned(),
            },
        )?;

        for (predecessor, prefix) in [(&local.id, "local"), (&checkpoint.id, "session")] {
            let replacement = next_replacement_id(&service.shared_conn, predecessor)?;
            let uuid = replacement
                .strip_prefix(&format!("{prefix}-"))
                .and_then(|value| Uuid::parse_str(value).ok())
                .context("correction replacement ID must be an opaque destination UUID")?;
            assert_eq!(uuid.get_version_num(), 4);
            assert!(!replacement.contains("secret"));
            assert!(!replacement.contains("correction"));
        }
        Ok(())
    }

    #[test]
    fn private_plan_and_inspection_use_consistent_authority_snapshots() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        let planned = local(
            &service,
            "Plan snapshot before concurrent change",
            "The private plan must bind one consistent snapshot.",
        )?;
        let planned_version = record_version(&service, &planned.id)?;
        let plan_writer = MemoryService::open_paths_with_clock(
            service.paths.clone(),
            FixedClock::from_rfc3339("2026-07-19T12:00:00Z")?,
        )?;
        let planned_id = planned.id.clone();
        inject_after_private_lifecycle_record_snapshot_hook(move || {
            let changed = plan_writer.shared_conn.execute(
                "UPDATE memory_record SET title = ?1 WHERE id = ?2",
                rusqlite::params!["Plan snapshot after concurrent change", planned_id],
            )?;
            ensure!(changed == 1, "concurrent plan fixture was not updated");
            Ok(())
        });
        let plan = service.plan_private_lifecycle(
            vec![planned.id.clone()],
            Some("2026-07-19T12:00:00Z".to_owned()),
        )?;
        let snapshot = plan
            .records
            .iter()
            .find(|snapshot| snapshot.record_id == planned.id)
            .context("planned private snapshot")?;
        assert_eq!(
            snapshot.version.private_version_token(),
            Some(planned_version.as_str())
        );
        assert_ne!(record_version(&service, &planned.id)?, planned_version);

        let inspected = local(
            &service,
            "Inspection snapshot before concurrent change",
            "Inspection must not pair old content with a new version.",
        )?;
        let inspected_version = record_version(&service, &inspected.id)?;
        let inspection_writer = MemoryService::open_paths_with_clock(
            service.paths.clone(),
            FixedClock::from_rfc3339("2026-07-19T12:00:00Z")?,
        )?;
        let inspected_id = inspected.id.clone();
        inject_after_private_lifecycle_record_snapshot_hook(move || {
            let changed = inspection_writer.shared_conn.execute(
                "UPDATE memory_record SET title = ?1 WHERE id = ?2",
                rusqlite::params!["Inspection snapshot after concurrent change", inspected_id],
            )?;
            ensure!(
                changed == 1,
                "concurrent inspection fixture was not updated"
            );
            Ok(())
        });
        let historical = service.inspect_private_lifecycle_record(&inspected.id)?;
        assert_eq!(historical.record.title, inspected.title);
        assert_eq!(historical.version, inspected_version);

        let current = service.inspect_private_lifecycle_record(&inspected.id)?;
        assert_eq!(
            current.record.title,
            "Inspection snapshot after concurrent change"
        );
        assert_ne!(current.version, historical.version);
        Ok(())
    }

    #[test]
    fn private_plan_is_deterministic_content_free_and_owner_selection_free() -> Result<()> {
        let (_temp, service) = initialized_service()?;
        local(&service, "Duplicate", "Exact duplicate private claim.")?;
        local(&service, "Duplicate", "Exact duplicate private claim.")?;
        let first =
            service.plan_private_lifecycle(Vec::new(), Some("2026-07-19T12:00:00Z".to_owned()))?;
        let second =
            service.plan_private_lifecycle(Vec::new(), Some("2026-07-19T12:00:00Z".to_owned()))?;
        assert_eq!(first, second);
        let json = serde_json::to_string(&first)?;
        assert!(!json.contains("Exact duplicate private claim"));
        assert!(!json.contains("\"title\""));
        assert!(!json.contains("\"body\""));
        let owner_actions = first
            .action_groups
            .iter()
            .find(|group| group.kind == MaintenanceActionGroupKind::OwnerAuthorizedPrivateMutation)
            .context("owner action group")?;
        assert_eq!(owner_actions.actions.len(), 1);
        assert!(owner_actions.actions[0].keeper_record_id.is_none());
        Ok(())
    }
}
