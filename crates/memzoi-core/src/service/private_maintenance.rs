use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    MAINTENANCE_POLICY_VERSION, MaintenanceActionClass, MaintenanceActionGroupKind,
    MaintenanceFindingKind, MaintenancePlanRequest, PRIVATE_MAINTENANCE_GRANT_SCHEMA,
    PRIVATE_MAINTENANCE_INSPECTION_SCHEMA, PRIVATE_MAINTENANCE_RESULT_SCHEMA,
    PrivateConflictParticipation, PrivateMaintenanceGrant, PrivateMaintenanceGrantState,
    PrivateMaintenanceInspection, PrivateMaintenanceOutcome,
    PrivateMaintenanceProjectionInspection, PrivateMaintenanceProjectionState,
    PrivateMaintenanceResult,
};

use super::{
    PrivateLifecycleService, private_lifecycle::plan_private_lifecycle_with_conn,
    runtime_records::lifecycle_generation, safe_files::RepoLifecycleLock, shared_runtime,
};

const AUDIT_SCHEMA: &str = "memzoi/private-maintenance-audit/1";
const AUDIT_ACTOR: &str = "system:private-maintenance";

#[cfg(test)]
std::thread_local! {
    static FAIL_NEXT_RECONCILIATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn inject_reconciliation_failure() {
    FAIL_NEXT_RECONCILIATION.set(true);
}

#[cfg(test)]
fn fail_reconciliation_if_injected() -> Result<()> {
    ensure!(
        !FAIL_NEXT_RECONCILIATION.replace(false),
        "detector injected failure"
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GrantRow {
    grant_id: String,
    grant_fingerprint: String,
    state: PrivateMaintenanceGrantState,
    policy_version: String,
    authorized_at: String,
    revoked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectionRow {
    state: PrivateMaintenanceProjectionState,
    grant_fingerprint: Option<String>,
    projection_id: Option<String>,
    plan_id: Option<String>,
    authoritative_generation: i64,
    policy_version: String,
    detector_digest: Option<String>,
    not_after: Option<String>,
    reason_code: Option<String>,
    member_count: i64,
    edge_count: i64,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConflictSetRow {
    conflict_id: String,
    projection_id: String,
    finding_id: String,
    comparison_set_digest: String,
    grant_fingerprint: String,
    detector_version: String,
    policy_version: String,
    reason_code: String,
    resolution_state: String,
    recall_effect: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConflictMemberRow {
    conflict_id: String,
    record_id: String,
    record_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConflictEdgeRow {
    conflict_id: String,
    left_record_id: String,
    right_record_id: String,
    evidence_digest: String,
    reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PrivateMaintenanceMirrorSnapshot {
    grants: Vec<GrantRow>,
    projection: ProjectionRow,
    sets: Vec<ConflictSetRow>,
    members: Vec<ConflictMemberRow>,
    edges: Vec<ConflictEdgeRow>,
}

impl PrivateLifecycleService {
    pub fn enable_private_maintenance(&self) -> Result<PrivateMaintenanceResult> {
        ensure!(
            self.authority_enabled(),
            "private maintenance enable requires an authority service"
        );
        let _lock = RepoLifecycleLock::acquire(&self.paths)?;
        shared_runtime::ensure_read_only_lifecycle_snapshot_ready(&self.paths)?;
        let now = self.clock.now_utc();
        let now_text = crate::expiry::format_timestamp(now)?;
        let tx = Transaction::new_unchecked(&self.shared_conn, TransactionBehavior::Immediate)?;
        let existing = active_grant(&tx)?;
        let (grant, created) = match existing {
            Some(grant) => (grant, false),
            None => {
                let grant_id = format!("maintenance_grant_{}", Uuid::now_v7());
                let grant_fingerprint = identity(
                    "memzoi/private-maintenance/grant",
                    &json!({
                        "grant_id": grant_id,
                        "policy_version": MAINTENANCE_POLICY_VERSION,
                    }),
                )?;
                let row = GrantRow {
                    grant_id,
                    grant_fingerprint,
                    state: PrivateMaintenanceGrantState::Active,
                    policy_version: MAINTENANCE_POLICY_VERSION.to_owned(),
                    authorized_at: now_text.clone(),
                    revoked_at: None,
                };
                insert_grant(&tx, &row)?;
                (row, true)
            }
        };
        let projection = reconcile_transaction(&self.paths, &tx, now, &grant)?;
        if created {
            append_audit(
                &tx,
                "memory.private_maintenance_authority_changed",
                "enable",
                "enabled",
                Some(&grant.grant_fingerprint),
                projection.projection_id.as_deref(),
                i64::try_from(projection.authoritative_generation)
                    .context("private maintenance generation exceeds SQLite range")?,
                projection.detector_digest.as_deref(),
                i64::try_from(projection.member_count)
                    .context("private maintenance member count exceeds SQLite range")?,
                i64::try_from(projection.edge_count)
                    .context("private maintenance edge count exceeds SQLite range")?,
                None,
                &now_text,
            )?;
        }
        tx.commit()?;
        refresh_existing_index_locked(&self.paths, &self.shared_conn)?;
        Ok(PrivateMaintenanceResult {
            schema: PRIVATE_MAINTENANCE_RESULT_SCHEMA.to_owned(),
            outcome: if created {
                PrivateMaintenanceOutcome::Enabled
            } else {
                PrivateMaintenanceOutcome::Unchanged
            },
            grant: Some(grant_public(&grant)),
            projection,
        })
    }

    pub fn disable_private_maintenance(&self) -> Result<PrivateMaintenanceResult> {
        ensure!(
            self.authority_enabled(),
            "private maintenance disable requires an authority service"
        );
        let _lock = RepoLifecycleLock::acquire(&self.paths)?;
        let now_text = crate::expiry::format_timestamp(self.clock.now_utc())?;
        let tx = Transaction::new_unchecked(&self.shared_conn, TransactionBehavior::Immediate)?;
        let prior = active_grant(&tx)?;
        if let Some(grant) = &prior {
            tx.execute(
                "UPDATE private_maintenance_grant
                 SET state = 'revoked', revoked_at = ?1
                 WHERE grant_id = ?2 AND state = 'active'",
                rusqlite::params![now_text, grant.grant_id],
            )?;
        }
        tx.execute("DELETE FROM private_conflict_set", [])?;
        let generation = lifecycle_generation(&tx)?;
        tx.execute(
            "UPDATE private_maintenance_projection
             SET state = 'disabled', grant_fingerprint = NULL, projection_id = NULL,
                 plan_id = NULL, authoritative_generation = ?1,
                 policy_version = ?2, detector_digest = NULL, not_after = NULL,
                 reason_code = NULL,
                 member_count = 0, edge_count = 0, updated_at = ?3
             WHERE singleton = 1",
            rusqlite::params![generation, MAINTENANCE_POLICY_VERSION, now_text],
        )?;
        append_audit(
            &tx,
            "memory.private_maintenance_authority_changed",
            "disable",
            "disabled",
            prior.as_ref().map(|grant| grant.grant_fingerprint.as_str()),
            None,
            generation,
            None,
            0,
            0,
            None,
            &now_text,
        )?;
        tx.commit()?;
        refresh_existing_index_locked(&self.paths, &self.shared_conn)?;
        Ok(PrivateMaintenanceResult {
            schema: PRIVATE_MAINTENANCE_RESULT_SCHEMA.to_owned(),
            outcome: PrivateMaintenanceOutcome::Disabled,
            grant: prior.map(|mut grant| {
                grant.state = PrivateMaintenanceGrantState::Revoked;
                grant.revoked_at = Some(now_text.clone());
                grant_public(&grant)
            }),
            projection: projection_inspection(
                &projection_row(&self.shared_conn)?,
                self.clock.now_utc(),
            )?,
        })
    }

    pub fn reconcile_private_maintenance(&self) -> Result<PrivateMaintenanceResult> {
        ensure!(
            self.authority_enabled(),
            "private maintenance reconcile requires an authority service"
        );
        let _lock = RepoLifecycleLock::acquire(&self.paths)?;
        shared_runtime::ensure_read_only_lifecycle_snapshot_ready(&self.paths)?;
        let outcome = reconcile_locked(&self.paths, &self.shared_conn, self.clock.now_utc())?;
        refresh_existing_index_locked(&self.paths, &self.shared_conn)?;
        Ok(outcome)
    }

    pub fn inspect_private_maintenance(&self) -> Result<PrivateMaintenanceInspection> {
        let _read_lock = self.acquire_transient_read_lock()?;
        shared_runtime::ensure_read_only_lifecycle_snapshot_ready(&self.paths)?;
        Ok(PrivateMaintenanceInspection {
            schema: PRIVATE_MAINTENANCE_INSPECTION_SCHEMA.to_owned(),
            active_grant: active_grant(&self.shared_conn)?.as_ref().map(grant_public),
            projection: projection_inspection(
                &projection_row(&self.shared_conn)?,
                self.clock.now_utc(),
            )?,
        })
    }
}

pub(super) fn reconcile_if_dirty_locked(
    paths: &crate::MemoryPaths,
    conn: &Connection,
    now: OffsetDateTime,
) -> Result<()> {
    let grant = active_grant(conn)?;
    if grant.is_some() && reconciliation_required(conn, now)? {
        let _ = reconcile_locked(paths, conn, now)?;
    }
    Ok(())
}

pub(super) fn reconciliation_required(conn: &Connection, now: OffsetDateTime) -> Result<bool> {
    let grant = active_grant(conn)?;
    if grant.is_none() {
        return Ok(false);
    }
    let projection = projection_row(conn)?;
    let detector_digest = crate::maintenance::current_maintenance_detector_digest()?;
    let needs_reconciliation = projection.state == PrivateMaintenanceProjectionState::Dirty
        || (projection.state == PrivateMaintenanceProjectionState::Current
            && grant.as_ref().is_some_and(|grant| {
                projection.grant_fingerprint.as_deref() != Some(&grant.grant_fingerprint)
                    || projection.policy_version != MAINTENANCE_POLICY_VERSION
                    || projection.detector_digest.as_deref() != Some(&detector_digest)
            }))
        || (projection.state == PrivateMaintenanceProjectionState::Current
            && projection_is_expired(&projection, now)?);
    Ok(needs_reconciliation)
}

fn reconcile_locked(
    paths: &crate::MemoryPaths,
    conn: &Connection,
    now: OffsetDateTime,
) -> Result<PrivateMaintenanceResult> {
    reconcile_locked_attempt(paths, conn, now, true)
}

fn reconcile_locked_attempt(
    paths: &crate::MemoryPaths,
    conn: &Connection,
    now: OffsetDateTime,
    retry_stale_snapshot: bool,
) -> Result<PrivateMaintenanceResult> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let Some(grant) = active_grant(&tx)? else {
        tx.rollback()?;
        return Ok(PrivateMaintenanceResult {
            schema: PRIVATE_MAINTENANCE_RESULT_SCHEMA.to_owned(),
            outcome: PrivateMaintenanceOutcome::Disabled,
            grant: None,
            projection: projection_inspection(&projection_row(conn)?, now)?,
        });
    };
    match reconcile_transaction(paths, &tx, now, &grant) {
        Ok(projection) => {
            tx.commit()?;
            Ok(PrivateMaintenanceResult {
                schema: PRIVATE_MAINTENANCE_RESULT_SCHEMA.to_owned(),
                outcome: PrivateMaintenanceOutcome::Reconciled,
                grant: Some(grant_public(&grant)),
                projection,
            })
        }
        Err(error) => {
            tx.rollback()?;
            let reason = blocked_reason(&error);
            if reason == "stale_snapshot" && retry_stale_snapshot {
                return reconcile_locked_attempt(paths, conn, now, false);
            }
            let blocked = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
            let generation = lifecycle_generation(&blocked)?;
            let now_text = crate::expiry::format_timestamp(now)?;
            blocked.execute(
                "UPDATE private_maintenance_projection
                 SET state = 'blocked', grant_fingerprint = ?1,
                     authoritative_generation = ?2, policy_version = ?3,
                     reason_code = ?4, updated_at = ?5
                 WHERE singleton = 1",
                rusqlite::params![
                    grant.grant_fingerprint,
                    generation,
                    MAINTENANCE_POLICY_VERSION,
                    reason,
                    now_text
                ],
            )?;
            let row = projection_row(&blocked)?;
            append_audit(
                &blocked,
                "memory.private_maintenance_reconciled",
                "reconcile",
                "blocked",
                Some(&grant.grant_fingerprint),
                row.projection_id.as_deref(),
                generation,
                row.detector_digest.as_deref(),
                row.member_count,
                row.edge_count,
                Some(reason),
                &now_text,
            )?;
            blocked.commit()?;
            Ok(PrivateMaintenanceResult {
                schema: PRIVATE_MAINTENANCE_RESULT_SCHEMA.to_owned(),
                outcome: PrivateMaintenanceOutcome::Blocked,
                grant: Some(grant_public(&grant)),
                projection: projection_inspection(&projection_row(conn)?, now)?,
            })
        }
    }
}

fn reconcile_transaction(
    paths: &crate::MemoryPaths,
    conn: &Connection,
    now: OffsetDateTime,
    grant: &GrantRow,
) -> Result<PrivateMaintenanceProjectionInspection> {
    #[cfg(test)]
    fail_reconciliation_if_injected()?;
    ensure!(
        grant.policy_version == MAINTENANCE_POLICY_VERSION,
        "private maintenance policy mismatch"
    );
    let generation = lifecycle_generation(conn)?;
    let plan = plan_private_lifecycle_with_conn(
        paths,
        conn,
        MaintenancePlanRequest {
            schema: crate::MAINTENANCE_REQUEST_SCHEMA.to_owned(),
            evaluated_at: Some(crate::expiry::format_timestamp(now)?),
            record_ids: Vec::new(),
        },
        now,
    )?;
    ensure!(
        plan.authority.grant_fingerprint == grant.grant_fingerprint,
        "private maintenance grant changed while planning"
    );
    let derived = plan
        .action_groups
        .iter()
        .find(|group| group.kind == MaintenanceActionGroupKind::PrivateDerivedState)
        .context("private maintenance plan has no derived-state action group")?;
    ensure!(
        derived
            .actions
            .iter()
            .all(|action| action.class == MaintenanceActionClass::SuppressUnresolvedConflict),
        "private maintenance plan contains an unsupported derived action"
    );
    let projection_id = identity(
        "memzoi/private-maintenance/projection",
        &json!({
            "plan_id": plan.plan_id,
            "grant_fingerprint": grant.grant_fingerprint,
            "authoritative_generation": generation,
            "actions": derived.actions,
        }),
    )?;
    ensure!(
        lifecycle_generation(conn)? == generation,
        "private maintenance authoritative generation changed during planning"
    );
    ensure!(
        active_grant(conn)?
            .as_ref()
            .map(|row| row.grant_fingerprint.as_str())
            == Some(grant.grant_fingerprint.as_str()),
        "private maintenance grant changed during planning"
    );

    conn.execute("DELETE FROM private_conflict_set", [])?;
    let detector_version = plan
        .detectors
        .iter()
        .find(|detector| detector.kind == MaintenanceFindingKind::HighConfidenceContradiction)
        .map(|detector| detector.version.as_str())
        .context("private maintenance plan has no contradiction detector")?;
    let mut member_ids = BTreeSet::new();
    let mut edge_count = 0_usize;
    for action in &derived.actions {
        let finding = plan
            .findings
            .iter()
            .find(|finding| finding.finding_id == action.finding_id)
            .context("private maintenance action has no finding")?;
        let conflict_id = identity(
            "memzoi/private-maintenance/conflict",
            &json!({"projection_id": projection_id, "finding_id": finding.finding_id}),
        )?;
        conn.execute(
            "INSERT INTO private_conflict_set(
               conflict_id, projection_id, finding_id, comparison_set_digest,
               grant_fingerprint, detector_version, policy_version,
               reason_code, resolution_state, recall_effect
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                       'high_confidence_unresolved_contradiction', 'unresolved',
                       'suppress_all_automatic_recall')",
            rusqlite::params![
                conflict_id,
                projection_id,
                finding.finding_id,
                finding.comparison_set_digest,
                grant.grant_fingerprint,
                detector_version,
                MAINTENANCE_POLICY_VERSION,
            ],
        )?;
        for record_id in &finding.record_ids {
            let version = action
                .preconditions
                .record_versions
                .get(record_id)
                .and_then(|version| version.private_version_token())
                .context("private maintenance member has no private version")?;
            conn.execute(
                "INSERT INTO private_conflict_member(conflict_id, record_id, record_version)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![conflict_id, record_id, version],
            )?;
            member_ids.insert(record_id.clone());
        }
        for edge in &finding.conflict_edges {
            conn.execute(
                "INSERT INTO private_conflict_edge(
                   conflict_id, left_record_id, right_record_id, evidence_digest, reason_code
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    conflict_id,
                    edge.record_ids[0],
                    edge.record_ids[1],
                    edge.evidence_digest,
                    edge.reason_code,
                ],
            )?;
            edge_count += 1;
        }
    }
    let now_text = crate::expiry::format_timestamp(now)?;
    conn.execute(
        "UPDATE private_maintenance_projection
         SET state = 'current', grant_fingerprint = ?1, projection_id = ?2,
             plan_id = ?3, authoritative_generation = ?4, policy_version = ?5,
             detector_digest = ?6, not_after = ?7, reason_code = NULL,
             member_count = ?8, edge_count = ?9, updated_at = ?10
         WHERE singleton = 1",
        rusqlite::params![
            grant.grant_fingerprint,
            projection_id,
            plan.plan_id,
            generation,
            MAINTENANCE_POLICY_VERSION,
            plan.preconditions.detector_digest,
            plan.not_after,
            member_ids.len() as i64,
            edge_count as i64,
            now_text,
        ],
    )?;
    append_audit(
        conn,
        "memory.private_maintenance_reconciled",
        "reconcile",
        "current",
        Some(&grant.grant_fingerprint),
        Some(&projection_id),
        generation,
        Some(&plan.preconditions.detector_digest),
        member_ids.len() as i64,
        edge_count as i64,
        None,
        &now_text,
    )?;
    projection_inspection(&projection_row(conn)?, now)
}

pub(super) fn mirror_snapshot(conn: &Connection) -> Result<PrivateMaintenanceMirrorSnapshot> {
    Ok(PrivateMaintenanceMirrorSnapshot {
        grants: query_grants(conn)?,
        projection: projection_row(conn)?,
        sets: query_sets(conn)?,
        members: query_members(conn)?,
        edges: query_edges(conn)?,
    })
}

pub(super) fn replace_mirror(
    conn: &Connection,
    snapshot: &PrivateMaintenanceMirrorSnapshot,
) -> Result<()> {
    conn.execute("DELETE FROM private_conflict_set", [])?;
    conn.execute("DELETE FROM private_maintenance_grant", [])?;
    for row in &snapshot.grants {
        conn.execute(
            "INSERT INTO private_maintenance_grant(
               grant_id,grant_fingerprint,state,policy_version,authorized_at,revoked_at
             ) VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![
                row.grant_id,
                row.grant_fingerprint,
                row.state.as_str(),
                row.policy_version,
                row.authorized_at,
                row.revoked_at
            ],
        )?;
    }
    for row in &snapshot.sets {
        conn.execute(
            "INSERT INTO private_conflict_set VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                row.conflict_id,
                row.projection_id,
                row.finding_id,
                row.comparison_set_digest,
                row.grant_fingerprint,
                row.detector_version,
                row.policy_version,
                row.reason_code,
                row.resolution_state,
                row.recall_effect
            ],
        )?;
    }
    for row in &snapshot.members {
        conn.execute(
            "INSERT INTO private_conflict_member VALUES (?1,?2,?3)",
            rusqlite::params![row.conflict_id, row.record_id, row.record_version],
        )?;
    }
    for row in &snapshot.edges {
        conn.execute(
            "INSERT INTO private_conflict_edge VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![
                row.conflict_id,
                row.left_record_id,
                row.right_record_id,
                row.evidence_digest,
                row.reason_code
            ],
        )?;
    }
    let row = &snapshot.projection;
    conn.execute(
        "UPDATE private_maintenance_projection SET
           state=?1,grant_fingerprint=?2,projection_id=?3,plan_id=?4,
           authoritative_generation=?5,policy_version=?6,detector_digest=?7,
           not_after=?8,reason_code=?9,member_count=?10,edge_count=?11,updated_at=?12
         WHERE singleton=1",
        rusqlite::params![
            row.state.as_str(),
            row.grant_fingerprint,
            row.projection_id,
            row.plan_id,
            row.authoritative_generation,
            row.policy_version,
            row.detector_digest,
            row.not_after,
            row.reason_code,
            row.member_count,
            row.edge_count,
            row.updated_at
        ],
    )?;
    Ok(())
}

pub(super) fn conflict_participation(
    conn: &Connection,
    record_id: &str,
    evaluated_at: &str,
) -> Result<Vec<PrivateConflictParticipation>> {
    let mut statement = conn.prepare(
        "SELECT conflict.conflict_id, edge.left_record_id, edge.right_record_id,
                conflict.reason_code, conflict.resolution_state, conflict.recall_effect,
                conflict.detector_version, conflict.policy_version
         FROM private_conflict_set AS conflict
         JOIN private_maintenance_projection AS projection
           ON projection.singleton = 1
          AND projection.projection_id = conflict.projection_id
          AND projection.grant_fingerprint = conflict.grant_fingerprint
          AND projection.policy_version = conflict.policy_version
         JOIN private_conflict_edge AS edge ON edge.conflict_id = conflict.conflict_id
         CROSS JOIN private_lifecycle_generation AS generation
         WHERE generation.singleton = 1
           AND projection.state = 'current'
           AND projection.authoritative_generation = generation.generation
           AND projection.policy_version = ?3
           AND memzoi_timestamp_before(?2, projection.not_after) = 1
           AND (edge.left_record_id = ?1 OR edge.right_record_id = ?1)
         ORDER BY conflict.conflict_id, edge.left_record_id, edge.right_record_id",
    )?;
    let rows = statement.query_map(
        rusqlite::params![record_id, evaluated_at, MAINTENANCE_POLICY_VERSION],
        |row| {
            let left: String = row.get(1)?;
            let right: String = row.get(2)?;
            Ok(PrivateConflictParticipation {
                conflict_id: row.get(0)?,
                other_member_ids: vec![if left == record_id { right } else { left }],
                reason_code: row.get(3)?,
                resolution_state: row.get(4)?,
                recall_effect: row.get(5)?,
                detector_version: row.get(6)?,
                policy_version: row.get(7)?,
            })
        },
    )?;
    let mut grouped = BTreeMap::<String, PrivateConflictParticipation>::new();
    for item in rows {
        let item = item?;
        grouped
            .entry(item.conflict_id.clone())
            .and_modify(|existing| {
                existing
                    .other_member_ids
                    .extend(item.other_member_ids.clone())
            })
            .or_insert(item);
    }
    for item in grouped.values_mut() {
        item.other_member_ids.sort();
        item.other_member_ids.dedup();
    }
    Ok(grouped.into_values().collect())
}

fn active_grant(conn: &Connection) -> Result<Option<GrantRow>> {
    conn.query_row(
        "SELECT grant_id, grant_fingerprint, state, policy_version, authorized_at, revoked_at
         FROM private_maintenance_grant WHERE state = 'active'",
        [],
        |row| {
            Ok(GrantRow {
                grant_id: row.get(0)?,
                grant_fingerprint: row.get(1)?,
                state: PrivateMaintenanceGrantState::Active,
                policy_version: row.get(3)?,
                authorized_at: row.get(4)?,
                revoked_at: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn insert_grant(conn: &Connection, row: &GrantRow) -> Result<()> {
    conn.execute(
        "INSERT INTO private_maintenance_grant(
           grant_id, grant_fingerprint, state, policy_version, authorized_at, revoked_at
         ) VALUES (?1, ?2, 'active', ?3, ?4, NULL)",
        rusqlite::params![
            row.grant_id,
            row.grant_fingerprint,
            row.policy_version,
            row.authorized_at
        ],
    )?;
    Ok(())
}

fn projection_row(conn: &Connection) -> Result<ProjectionRow> {
    conn.query_row(
        "SELECT state, grant_fingerprint, projection_id, plan_id, authoritative_generation,
                policy_version, detector_digest, not_after, reason_code,
                member_count, edge_count, updated_at
         FROM private_maintenance_projection WHERE singleton = 1",
        [],
        |row| {
            let state: String = row.get(0)?;
            let state = match state.as_str() {
                "disabled" => PrivateMaintenanceProjectionState::Disabled,
                "current" => PrivateMaintenanceProjectionState::Current,
                "dirty" => PrivateMaintenanceProjectionState::Dirty,
                "blocked" => PrivateMaintenanceProjectionState::Blocked,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(ProjectionRow {
                state,
                grant_fingerprint: row.get(1)?,
                projection_id: row.get(2)?,
                plan_id: row.get(3)?,
                authoritative_generation: row.get(4)?,
                policy_version: row.get(5)?,
                detector_digest: row.get(6)?,
                not_after: row.get(7)?,
                reason_code: row.get(8)?,
                member_count: row.get(9)?,
                edge_count: row.get(10)?,
                updated_at: row.get(11)?,
            })
        },
    )
    .context("private maintenance projection is not initialized")
}

fn projection_is_expired(row: &ProjectionRow, now: OffsetDateTime) -> Result<bool> {
    let Some(not_after) = row.not_after.as_deref() else {
        return Ok(true);
    };
    Ok(now
        >= crate::private_lifecycle::parse_timestamp(
            not_after,
            "private maintenance projection not_after",
        )?)
}

fn projection_inspection(
    row: &ProjectionRow,
    now: OffsetDateTime,
) -> Result<PrivateMaintenanceProjectionInspection> {
    let state = if row.state == PrivateMaintenanceProjectionState::Current
        && projection_is_expired(row, now)?
    {
        PrivateMaintenanceProjectionState::Stale
    } else {
        row.state
    };
    Ok(PrivateMaintenanceProjectionInspection {
        state,
        grant_fingerprint: row.grant_fingerprint.clone(),
        projection_id: row.projection_id.clone(),
        plan_id: row.plan_id.clone(),
        authoritative_generation: u64::try_from(row.authoritative_generation)
            .context("private maintenance generation is negative")?,
        policy_version: row.policy_version.clone(),
        detector_digest: row.detector_digest.clone(),
        not_after: row.not_after.clone(),
        reason_code: row.reason_code.clone(),
        member_count: usize::try_from(row.member_count).context("member count is negative")?,
        edge_count: usize::try_from(row.edge_count).context("edge count is negative")?,
        updated_at: row.updated_at.clone(),
    })
}

fn grant_public(row: &GrantRow) -> PrivateMaintenanceGrant {
    PrivateMaintenanceGrant {
        schema: PRIVATE_MAINTENANCE_GRANT_SCHEMA.to_owned(),
        grant_id: row.grant_id.clone(),
        grant_fingerprint: row.grant_fingerprint.clone(),
        state: row.state,
        policy_version: row.policy_version.clone(),
        authorized_at: row.authorized_at.clone(),
        revoked_at: row.revoked_at.clone(),
    }
}

fn query_grants(conn: &Connection) -> Result<Vec<GrantRow>> {
    let mut stmt = conn.prepare(
        "SELECT grant_id,grant_fingerprint,state,policy_version,authorized_at,revoked_at
         FROM private_maintenance_grant ORDER BY authorized_at,grant_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let state: String = row.get(2)?;
        let state = match state.as_str() {
            "active" => PrivateMaintenanceGrantState::Active,
            "revoked" => PrivateMaintenanceGrantState::Revoked,
            _ => return Err(rusqlite::Error::InvalidQuery),
        };
        Ok(GrantRow {
            grant_id: row.get(0)?,
            grant_fingerprint: row.get(1)?,
            state,
            policy_version: row.get(3)?,
            authorized_at: row.get(4)?,
            revoked_at: row.get(5)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn query_sets(conn: &Connection) -> Result<Vec<ConflictSetRow>> {
    let mut stmt = conn.prepare(
        "SELECT conflict_id,projection_id,finding_id,comparison_set_digest,grant_fingerprint,
                detector_version,policy_version,reason_code,
                resolution_state,recall_effect FROM private_conflict_set ORDER BY conflict_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ConflictSetRow {
            conflict_id: r.get(0)?,
            projection_id: r.get(1)?,
            finding_id: r.get(2)?,
            comparison_set_digest: r.get(3)?,
            grant_fingerprint: r.get(4)?,
            detector_version: r.get(5)?,
            policy_version: r.get(6)?,
            reason_code: r.get(7)?,
            resolution_state: r.get(8)?,
            recall_effect: r.get(9)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn query_members(conn: &Connection) -> Result<Vec<ConflictMemberRow>> {
    let mut stmt = conn.prepare(
        "SELECT conflict_id,record_id,record_version FROM private_conflict_member
         ORDER BY conflict_id,record_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ConflictMemberRow {
            conflict_id: r.get(0)?,
            record_id: r.get(1)?,
            record_version: r.get(2)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn query_edges(conn: &Connection) -> Result<Vec<ConflictEdgeRow>> {
    let mut stmt = conn.prepare(
        "SELECT conflict_id,left_record_id,right_record_id,evidence_digest,reason_code
         FROM private_conflict_edge ORDER BY conflict_id,left_record_id,right_record_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ConflictEdgeRow {
            conflict_id: r.get(0)?,
            left_record_id: r.get(1)?,
            right_record_id: r.get(2)?,
            evidence_digest: r.get(3)?,
            reason_code: r.get(4)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
fn append_audit(
    conn: &Connection,
    event_type: &str,
    operation: &str,
    outcome: &str,
    grant_fingerprint: Option<&str>,
    projection_id: Option<&str>,
    authoritative_generation: i64,
    detector_digest: Option<&str>,
    member_count: i64,
    edge_count: i64,
    reason_code: Option<&str>,
    created_at: &str,
) -> Result<()> {
    let payload = json!({
        "schema": AUDIT_SCHEMA,
        "operation": operation,
        "outcome": outcome,
        "grant_fingerprint": grant_fingerprint,
        "projection_id": projection_id,
        "policy_version": MAINTENANCE_POLICY_VERSION,
        "detector_digest": detector_digest,
        "authoritative_generation": authoritative_generation,
        "member_count": member_count,
        "edge_count": edge_count,
        "reason_code": reason_code,
    });
    let keys = payload.as_object().expect("audit payload is an object");
    ensure!(
        keys.len() == 11,
        "private maintenance audit schema changed unexpectedly"
    );
    conn.execute(
        "INSERT INTO event_log(id,event_type,actor,data_class,payload_json,created_at)
         VALUES (?1,?2,?3,'private',?4,?5)",
        rusqlite::params![
            format!("evt_{}", Uuid::now_v7()),
            event_type,
            AUDIT_ACTOR,
            serde_json::to_string(&payload)?,
            created_at
        ],
    )?;
    Ok(())
}

fn identity(domain: &str, value: &impl Serialize) -> Result<String> {
    let bytes = serde_json_canonicalizer::to_vec(value)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_bytes());
    hasher.update(&[0]);
    hasher.update(&bytes);
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn blocked_reason(error: &anyhow::Error) -> &'static str {
    let message = error.to_string();
    if message.contains("limit") || message.contains("bound") {
        "bounds_exceeded"
    } else if message.contains("policy") {
        "policy_mismatch"
    } else if message.contains("generation") || message.contains("changed") {
        "stale_snapshot"
    } else {
        "detector_or_validation_failed"
    }
}

fn refresh_existing_index_locked(paths: &crate::MemoryPaths, shared: &Connection) -> Result<()> {
    if !paths.index_db_path.is_file() {
        return Ok(());
    }
    let index = crate::db::open_existing_database(&paths.index_db_path, false)?;
    shared_runtime::refresh_index_mirrors_locked(paths, shared, &index)
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use tempfile::TempDir;

    use super::*;
    use crate::{
        ContextPackInput, FixedClock, HandoffInput, InitRequest, LocalMemoryInput, MemoryLane,
        MemoryService, MemoryType, PrivateMaintenanceProjectionState,
    };

    fn fixture() -> Result<(TempDir, MemoryService, PrivateLifecycleService)> {
        let temp = TempDir::new()?;
        let project = temp.path().join("project");
        fs::create_dir(&project)?;
        let paths = crate::MemoryPaths::with_runtime_home(
            project.canonicalize()?,
            temp.path().join("runtime-home"),
        );
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let service = MemoryService::open_paths_with_clock(
            paths.clone(),
            FixedClock::from_rfc3339("2026-07-20T12:00:00Z")?,
        )?;
        for body in [
            "Authentication is required.",
            "Authentication is not required.",
        ] {
            service.create_local_memory(
                "test",
                LocalMemoryInput {
                    memory_type: MemoryType::Fact,
                    lane: MemoryLane::Semantic,
                    title: "Private conflict".to_owned(),
                    body: body.to_owned(),
                },
            )?;
        }
        let mut authority = PrivateLifecycleService::open_paths_for_authority(paths)?;
        authority.clock = Arc::new(FixedClock::from_rfc3339("2026-07-20T12:00:00Z")?);
        Ok((temp, service, authority))
    }

    #[test]
    fn consecutive_reconciliation_preserves_suppression_until_atomic_disable() -> Result<()> {
        let (_temp, service, authority) = fixture()?;
        assert_eq!(service.list_local_memory()?.len(), 2);

        let enabled = authority.enable_private_maintenance()?;
        assert_eq!(
            enabled.projection.state,
            PrivateMaintenanceProjectionState::Current
        );
        assert_eq!(enabled.projection.member_count, 2);
        assert_eq!(enabled.projection.edge_count, 1);
        assert!(service.list_local_memory()?.is_empty());

        let record_id = service.shared_conn.query_row(
            "SELECT id FROM memory_record WHERE destination = 'local' ORDER BY id LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )?;
        let inspected = authority.inspect_private_lifecycle_record(&record_id)?;
        assert!(inspected.base_eligibility.is_current);
        assert!(!inspected.effective_automatic_recall_eligibility.is_current);
        assert!(
            inspected
                .effective_automatic_recall_eligibility
                .exclusions
                .contains(&crate::CurrentAssertionExclusion::UnresolvedConflict)
        );
        assert_eq!(inspected.conflicts.len(), 1);

        let second = authority.reconcile_private_maintenance()?;
        assert_eq!(
            second.projection.state,
            PrivateMaintenanceProjectionState::Current
        );
        assert_eq!(
            second.projection.projection_id,
            enabled.projection.projection_id
        );
        assert_eq!(second.projection.member_count, 2);
        assert_eq!(second.projection.edge_count, 1);
        assert!(service.list_local_memory()?.is_empty());

        let disabled = authority.disable_private_maintenance()?;
        assert_eq!(
            disabled.projection.state,
            PrivateMaintenanceProjectionState::Disabled
        );
        assert_eq!(service.list_local_memory()?.len(), 2);
        Ok(())
    }

    #[test]
    fn generation_changing_write_installs_current_projection_in_index_backed_reads() -> Result<()> {
        let (_temp, service, authority) = fixture()?;
        authority.enable_private_maintenance()?;
        let visible = service.create_local_memory(
            "test",
            LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Mirror parity sentinel".to_owned(),
                body: "Mirror parity sentinel remains independently recallable.".to_owned(),
            },
        )?;

        for conn in [&service.shared_conn, &service.conn] {
            let state: String = conn.query_row(
                "SELECT state FROM private_maintenance_projection WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(state, "current");
        }
        let context = service.build_context_pack(ContextPackInput {
            task: "Mirror parity sentinel".to_owned(),
            include_local: true,
            ..ContextPackInput::default()
        })?;
        assert!(
            context
                .records
                .iter()
                .any(|item| item.record.id == visible.id)
        );
        let handoff = service.build_handoff_pack(HandoffInput {
            task: Some("Mirror parity sentinel".to_owned()),
            include_local: true,
            ..HandoffInput::default()
        })?;
        assert!(
            handoff
                .context
                .records
                .iter()
                .any(|item| item.record.id == visible.id)
        );
        Ok(())
    }

    #[test]
    fn private_path_scopes_are_loaded_before_conflict_detection() -> Result<()> {
        let (_temp, _service, authority) = fixture()?;
        let record_ids = authority
            .shared_conn
            .prepare("SELECT id FROM memory_record WHERE destination = 'local' ORDER BY id")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (index, (record_id, path)) in record_ids
            .iter()
            .zip(["frontend/**", "backend/**"])
            .enumerate()
        {
            authority.shared_conn.execute(
                "INSERT INTO memory_path(id, record_id, path) VALUES (?1, ?2, ?3)",
                rusqlite::params![format!("private-path-{index}"), record_id, path],
            )?;
        }

        let enabled = authority.enable_private_maintenance()?;
        assert_eq!(enabled.projection.member_count, 0);
        assert_eq!(enabled.projection.edge_count, 0);
        Ok(())
    }

    #[test]
    fn expired_projection_is_stale_until_an_audited_read_reconciles_it() -> Result<()> {
        let (_temp, mut service, mut authority) = fixture()?;
        let enabled = authority.enable_private_maintenance()?;
        assert_eq!(
            enabled.projection.not_after.as_deref(),
            Some("2026-07-21T12:00:00Z")
        );
        let record_id = authority.shared_conn.query_row(
            "SELECT id FROM memory_record WHERE destination = 'local' ORDER BY id LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )?;

        let boundary_clock = Arc::new(FixedClock::from_rfc3339("2026-07-21T12:00:00Z")?);
        authority.clock = boundary_clock.clone();
        service.clock = boundary_clock;
        let stale = authority.inspect_private_maintenance()?;
        assert_eq!(
            stale.projection.state,
            PrivateMaintenanceProjectionState::Stale
        );
        let record_inspection = authority.inspect_private_lifecycle_record(&record_id)?;
        assert!(record_inspection.conflicts.is_empty());
        assert!(
            record_inspection
                .effective_automatic_recall_eligibility
                .exclusions
                .iter()
                .any(|exclusion| matches!(
                    exclusion,
                    crate::CurrentAssertionExclusion::Safety { reason }
                        if reason == "private_maintenance_projection_stale"
                ))
        );
        assert!(
            !record_inspection
                .effective_automatic_recall_eligibility
                .exclusions
                .contains(&crate::CurrentAssertionExclusion::UnresolvedConflict)
        );

        let _ = service.build_context_pack(ContextPackInput {
            task: "Private conflict".to_owned(),
            include_local: true,
            ..ContextPackInput::default()
        })?;
        let refreshed = authority.inspect_private_maintenance()?;
        assert_eq!(
            refreshed.projection.state,
            PrivateMaintenanceProjectionState::Current
        );
        assert_eq!(
            refreshed.projection.not_after.as_deref(),
            Some("2026-07-22T12:00:00Z")
        );
        assert_eq!(refreshed.projection.edge_count, 1);
        Ok(())
    }

    #[test]
    fn inspect_and_reconcile_refuse_pending_shared_sync() -> Result<()> {
        let (_temp, _service, authority) = fixture()?;
        shared_runtime::inject_pending_shared_sync_marker(&authority.paths)?;
        for error in [
            authority
                .inspect_private_maintenance()
                .expect_err("inspection must reject pending shared sync"),
            authority
                .reconcile_private_maintenance()
                .expect_err("reconciliation must reject pending shared sync"),
        ] {
            assert!(
                error
                    .to_string()
                    .contains("read-only lifecycle access requires shared runtime recovery"),
                "{error:#}"
            );
        }
        Ok(())
    }

    #[test]
    fn maintenance_audits_use_the_exact_content_free_schema() -> Result<()> {
        let (_temp, _service, authority) = fixture()?;
        authority.enable_private_maintenance()?;
        authority.shared_conn.execute(
            "UPDATE private_lifecycle_generation SET generation = generation + 1
             WHERE singleton = 1",
            [],
        )?;
        inject_reconciliation_failure();
        authority.reconcile_private_maintenance()?;
        authority.disable_private_maintenance()?;
        let mut statement = authority.shared_conn.prepare(
            "SELECT payload_json FROM event_log
             WHERE event_type IN (
               'memory.private_maintenance_authority_changed',
               'memory.private_maintenance_reconciled'
             ) ORDER BY created_at,id",
        )?;
        let payloads = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(!payloads.is_empty());
        let expected = [
            "authoritative_generation",
            "detector_digest",
            "edge_count",
            "grant_fingerprint",
            "member_count",
            "operation",
            "outcome",
            "policy_version",
            "projection_id",
            "reason_code",
            "schema",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let mut outcomes = BTreeSet::new();
        for payload in payloads {
            let value: serde_json::Value = serde_json::from_str(&payload)?;
            outcomes.insert(
                value["outcome"]
                    .as_str()
                    .context("maintenance audit outcome is not a string")?
                    .to_owned(),
            );
            let keys = value
                .as_object()
                .context("maintenance audit payload is not an object")?
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            assert_eq!(keys, expected);
            assert!(!payload.contains("Authentication"));
            assert!(!payload.contains("Private conflict"));
        }
        assert_eq!(
            outcomes,
            ["blocked", "current", "disabled", "enabled"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        Ok(())
    }

    #[test]
    fn detector_failure_blocks_recall_without_replacing_prior_edges() -> Result<()> {
        let (_temp, service, authority) = fixture()?;
        authority.enable_private_maintenance()?;
        let prior_projection = authority.inspect_private_maintenance()?.projection;
        authority.shared_conn.execute(
            "UPDATE private_lifecycle_generation SET generation = generation + 1
             WHERE singleton = 1",
            [],
        )?;
        inject_reconciliation_failure();
        assert!(service.list_local_memory()?.is_empty());
        let blocked = authority.inspect_private_maintenance()?.projection;
        assert_eq!(blocked.state, PrivateMaintenanceProjectionState::Blocked);
        assert_eq!(blocked.projection_id, prior_projection.projection_id);
        assert_eq!(blocked.edge_count, 1);
        assert!(service.list_local_memory()?.is_empty());
        Ok(())
    }

    #[test]
    fn failed_initial_reconciliation_does_not_enable_authority() -> Result<()> {
        let (_temp, _service, authority) = fixture()?;
        inject_reconciliation_failure();
        assert!(authority.enable_private_maintenance().is_err());
        let inspection = authority.inspect_private_maintenance()?;
        assert!(inspection.active_grant.is_none());
        assert_eq!(
            inspection.projection.state,
            PrivateMaintenanceProjectionState::Disabled
        );
        Ok(())
    }

    #[test]
    fn reconciliation_keeps_the_surviving_pair_of_a_partial_conflict_graph() -> Result<()> {
        let (_temp, service, authority) = fixture()?;
        let third = service.create_local_memory(
            "test",
            LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Private conflict".to_owned(),
                body: "Authentication is required.".to_owned(),
            },
        )?;
        let initial = authority.enable_private_maintenance()?;
        assert_eq!(initial.projection.member_count, 3);
        assert_eq!(initial.projection.edge_count, 2);

        authority.shared_conn.execute(
            "UPDATE memory_record SET status = 'superseded' WHERE id = ?1",
            [&third.id],
        )?;
        let rebuilt = authority.reconcile_private_maintenance()?;
        assert_eq!(
            rebuilt.projection.state,
            PrivateMaintenanceProjectionState::Current
        );
        assert_eq!(rebuilt.projection.member_count, 2);
        assert_eq!(rebuilt.projection.edge_count, 1);
        assert!(service.list_local_memory()?.is_empty());
        Ok(())
    }

    #[test]
    fn record_bound_failure_retains_suppression_and_blocks_recall() -> Result<()> {
        let (_temp, service, authority) = fixture()?;
        authority.enable_private_maintenance()?;
        authority.shared_conn.execute_batch(&format!(
            "WITH RECURSIVE numbers(value) AS (
               SELECT 1 UNION ALL SELECT value + 1 FROM numbers WHERE value < {limit}
             )
             INSERT INTO memory_record(
               id,type,lane,destination,scope_kind,scope_id,visibility,title,body,status,
               confidence,retention_json,origin_json,content_hash,created_at,updated_at
             )
             SELECT printf('maintenance-bound-extra-%03d', numbers.value),
                    source.type,source.lane,source.destination,source.scope_kind,source.scope_id,
                    source.visibility,'Bound fixture','Independent bounded fixture.',source.status,
                    source.confidence,source.retention_json,source.origin_json,source.content_hash,
                    source.created_at,source.updated_at
             FROM numbers
             CROSS JOIN (
               SELECT * FROM memory_record WHERE destination = 'local' ORDER BY id LIMIT 1
             ) AS source;",
            limit = crate::MAINTENANCE_MAX_RECORDS - 1,
        ))?;

        let blocked = authority.reconcile_private_maintenance()?;
        assert_eq!(blocked.outcome, PrivateMaintenanceOutcome::Blocked);
        assert_eq!(
            blocked.projection.state,
            PrivateMaintenanceProjectionState::Blocked
        );
        assert_eq!(
            blocked.projection.reason_code.as_deref(),
            Some("bounds_exceeded")
        );
        assert_eq!(blocked.projection.edge_count, 1);
        assert!(service.list_local_memory()?.is_empty());
        Ok(())
    }

    #[test]
    fn grant_policy_and_detector_rotation_invalidate_predictably() -> Result<()> {
        let (_temp, _service, authority) = fixture()?;
        authority.enable_private_maintenance()?;
        authority.shared_conn.execute(
            "UPDATE private_maintenance_projection SET detector_digest = 'stale-detector'
             WHERE singleton = 1",
            [],
        )?;
        reconcile_if_dirty_locked(
            &authority.paths,
            &authority.shared_conn,
            authority.clock.now_utc(),
        )?;
        let detector_refreshed = authority.inspect_private_maintenance()?.projection;
        assert_eq!(
            detector_refreshed.detector_digest.as_deref(),
            Some(crate::maintenance::current_maintenance_detector_digest()?.as_str())
        );

        authority.shared_conn.execute(
            "UPDATE private_maintenance_grant SET policy_version = 'maintenance-policy/old'
             WHERE state = 'active'",
            [],
        )?;
        let policy_dirty = authority.inspect_private_maintenance()?.projection;
        assert_eq!(policy_dirty.state, PrivateMaintenanceProjectionState::Dirty);
        assert_eq!(
            policy_dirty.reason_code.as_deref(),
            Some("policy_version_changed")
        );
        reconcile_if_dirty_locked(
            &authority.paths,
            &authority.shared_conn,
            authority.clock.now_utc(),
        )?;
        let blocked = authority.inspect_private_maintenance()?.projection;
        assert_eq!(blocked.state, PrivateMaintenanceProjectionState::Blocked);
        assert_eq!(blocked.reason_code.as_deref(), Some("policy_mismatch"));

        authority.shared_conn.execute(
            "UPDATE private_maintenance_grant
             SET policy_version = ?1,
                 grant_fingerprint = 'blake3:7777777777777777777777777777777777777777777777777777777777777777'
             WHERE state = 'active'",
            [MAINTENANCE_POLICY_VERSION],
        )?;
        reconcile_if_dirty_locked(
            &authority.paths,
            &authority.shared_conn,
            authority.clock.now_utc(),
        )?;
        let rotated = authority.inspect_private_maintenance()?;
        assert_eq!(
            rotated.projection.state,
            PrivateMaintenanceProjectionState::Current
        );
        assert_eq!(
            rotated.projection.grant_fingerprint.as_deref(),
            Some("blake3:7777777777777777777777777777777777777777777777777777777777777777")
        );
        Ok(())
    }
}
