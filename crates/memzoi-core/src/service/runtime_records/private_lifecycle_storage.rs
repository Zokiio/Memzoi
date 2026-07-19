use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

const LIFECYCLE_SINGLETON: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::service) struct PrivateLifecycleState {
    pub record_id: String,
    pub automatic_recall_until: Option<String>,
    pub validity_until: Option<String>,
    pub retain_until: Option<String>,
    pub pinned: bool,
    pub quarantined: bool,
    pub quarantine_reason_code: Option<String>,
    pub record_version: String,
    pub automatic_recall_event_id: Option<String>,
    pub validity_event_id: Option<String>,
    pub retention_event_id: Option<String>,
    pub quarantine_event_id: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::service) enum PrivateLifecycleRelationKind {
    RenewedBy,
    CorrectedBy,
    SupersededBy,
    ConsolidatedInto,
    ContradictionResolvedBy,
}

impl PrivateLifecycleRelationKind {
    pub(in crate::service) const fn as_str(self) -> &'static str {
        match self {
            Self::RenewedBy => "renewed_by",
            Self::CorrectedBy => "corrected_by",
            Self::SupersededBy => "superseded_by",
            Self::ConsolidatedInto => "consolidated_into",
            Self::ContradictionResolvedBy => "contradiction_resolved_by",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "renewed_by" => Ok(Self::RenewedBy),
            "corrected_by" => Ok(Self::CorrectedBy),
            "superseded_by" => Ok(Self::SupersededBy),
            "consolidated_into" => Ok(Self::ConsolidatedInto),
            "contradiction_resolved_by" => Ok(Self::ContradictionResolvedBy),
            _ => bail!("unsupported private lifecycle relation kind: {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::service) struct PrivateLifecycleRelation {
    pub id: String,
    pub relation_kind: PrivateLifecycleRelationKind,
    pub subject_record_id: String,
    pub related_record_id: String,
    pub application_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::service) enum OwnerActionGrantState {
    Active,
    Consumed,
    Revoked,
}

impl OwnerActionGrantState {
    pub(in crate::service) const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Consumed => "consumed",
            Self::Revoked => "revoked",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "consumed" => Ok(Self::Consumed),
            "revoked" => Ok(Self::Revoked),
            _ => bail!("unsupported owner-action grant state: {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::service) struct OwnerActionGrantRow {
    pub grant_id: String,
    pub request_id: String,
    /// Canonical strict request JSON. This is the exact grant binding.
    pub request_json: String,
    pub state: OwnerActionGrantState,
    pub authorized_at: String,
    pub expires_at: String,
    pub revoked_at: Option<String>,
    pub consumed_at: Option<String>,
    pub consumed_application_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::service) struct PrivateLifecycleApplicationRow {
    pub application_id: String,
    pub operation_id: String,
    pub request_id: String,
    pub grant_id: String,
    pub result_json: String,
    pub lifecycle_generation: i64,
    pub applied_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::service) enum RevokeGrantOutcome {
    Revoked,
    AlreadyRevoked,
    AlreadyConsumed,
    Missing,
}

pub(in crate::service) struct PrivateLifecycleStorage<'a> {
    conn: &'a Connection,
}

impl<'a> PrivateLifecycleStorage<'a> {
    pub(in crate::service) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub(in crate::service) fn state(
        &self,
        record_id: &str,
    ) -> Result<Option<PrivateLifecycleState>> {
        self.conn
            .query_row(
                "SELECT record_id, automatic_recall_until, validity_until, retain_until,
                        pinned, quarantined, quarantine_reason_code, record_version,
                        automatic_recall_event_id, validity_event_id, retention_event_id,
                        quarantine_event_id, updated_at
                 FROM private_lifecycle_state
                 WHERE record_id = ?1",
                [record_id],
                private_lifecycle_state_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(in crate::service) fn require_state(
        &self,
        record_id: &str,
    ) -> Result<PrivateLifecycleState> {
        self.state(record_id)?
            .with_context(|| format!("private lifecycle state not found: {record_id}"))
    }

    /// Replaces the independently authorized lifecycle facts while preserving
    /// the row identity. The schema trigger installs a fresh random version;
    /// callers must supply their injected-clock timestamp in `updated_at`.
    pub(in crate::service) fn update_state_facts(
        &self,
        state: &PrivateLifecycleState,
    ) -> Result<PrivateLifecycleState> {
        let changed = self.conn.execute(
            "UPDATE private_lifecycle_state
             SET automatic_recall_until = ?1,
                 validity_until = ?2,
                 retain_until = ?3,
                 pinned = ?4,
                 quarantined = ?5,
                 quarantine_reason_code = ?6,
                 automatic_recall_event_id = ?7,
                 validity_event_id = ?8,
                 retention_event_id = ?9,
                 quarantine_event_id = ?10,
                 updated_at = ?11
             WHERE record_id = ?12",
            rusqlite::params![
                state.automatic_recall_until,
                state.validity_until,
                state.retain_until,
                state.pinned,
                state.quarantined,
                state.quarantine_reason_code,
                state.automatic_recall_event_id,
                state.validity_event_id,
                state.retention_event_id,
                state.quarantine_event_id,
                state.updated_at,
                state.record_id,
            ],
        )?;
        ensure!(
            changed == 1,
            "private lifecycle state not found: {}",
            state.record_id
        );
        self.require_state(&state.record_id)
    }

    pub(in crate::service) fn relation(
        &self,
        id: &str,
    ) -> Result<Option<PrivateLifecycleRelation>> {
        self.conn
            .query_row(
                "SELECT id, relation_kind, subject_record_id, related_record_id,
                        application_id, created_at
                 FROM private_lifecycle_relation
                 WHERE id = ?1",
                [id],
                private_lifecycle_relation_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(in crate::service) fn relations_for_record(
        &self,
        record_id: &str,
    ) -> Result<Vec<PrivateLifecycleRelation>> {
        let mut statement = self.conn.prepare(
            "SELECT id, relation_kind, subject_record_id, related_record_id,
                    application_id, created_at
             FROM private_lifecycle_relation
             WHERE subject_record_id = ?1 OR related_record_id = ?1
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = statement.query_map([record_id], private_lifecycle_relation_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub(super) fn relations_for_subject(
        &self,
        record_id: &str,
    ) -> Result<Vec<PrivateLifecycleRelation>> {
        let mut statement = self.conn.prepare(
            "SELECT id, relation_kind, subject_record_id, related_record_id,
                    application_id, created_at
             FROM private_lifecycle_relation
             WHERE subject_record_id = ?1
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = statement.query_map([record_id], private_lifecycle_relation_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub(in crate::service) fn insert_relation(
        &self,
        relation: &PrivateLifecycleRelation,
    ) -> Result<()> {
        let inserted = self.conn.execute(
            "INSERT INTO private_lifecycle_relation(
               id, relation_kind, subject_record_id, related_record_id, application_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                relation.id,
                relation.relation_kind.as_str(),
                relation.subject_record_id,
                relation.related_record_id,
                relation.application_id,
                relation.created_at,
            ],
        )?;
        ensure!(inserted == 1, "private lifecycle relation was not inserted");
        Ok(())
    }

    pub(in crate::service) fn grant(&self, grant_id: &str) -> Result<Option<OwnerActionGrantRow>> {
        self.conn
            .query_row(
                "SELECT grant_id, request_id, request_json, state, authorized_at, expires_at,
                        revoked_at, consumed_at, consumed_application_id
                 FROM owner_action_grant
                 WHERE grant_id = ?1",
                [grant_id],
                owner_action_grant_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Returns active grants whose stored canonical request is byte-for-byte
    /// identical to the proposed binding. Callers parse and compare expiry
    /// instants in Rust; RFC 3339 text is not chronologically ordered when
    /// fractional-second representations differ.
    pub(in crate::service) fn identical_active_grants(
        &self,
        request_id: &str,
        request_json: &str,
    ) -> Result<Vec<OwnerActionGrantRow>> {
        let mut statement = self.conn.prepare(
            "SELECT grant_id, request_id, request_json, state, authorized_at, expires_at,
                        revoked_at, consumed_at, consumed_application_id
                 FROM owner_action_grant
                 WHERE request_id = ?1
                   AND request_json = ?2
                   AND state = 'active'
                 ORDER BY authorized_at ASC, grant_id ASC",
        )?;
        let rows = statement.query_map(
            rusqlite::params![request_id, request_json],
            owner_action_grant_from_row,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Inserts authority only. There are intentionally no triggers from this
    /// table into record state, lifecycle generations, mirrors, or event logs.
    pub(in crate::service) fn insert_grant(&self, grant: &OwnerActionGrantRow) -> Result<()> {
        ensure!(
            grant.state == OwnerActionGrantState::Active
                && grant.revoked_at.is_none()
                && grant.consumed_at.is_none()
                && grant.consumed_application_id.is_none(),
            "new owner-action grants must be active and unused"
        );
        let authorized_at =
            crate::private_lifecycle::parse_timestamp(&grant.authorized_at, "authorized_at")?;
        let expires_at =
            crate::private_lifecycle::parse_timestamp(&grant.expires_at, "expires_at")?;
        ensure!(
            expires_at > authorized_at,
            "owner grant expiry must be later than authorization time"
        );
        let inserted = self.conn.execute(
            "INSERT INTO owner_action_grant(
               grant_id, request_id, request_json, state, authorized_at, expires_at,
               revoked_at, consumed_at, consumed_application_id
             ) VALUES (?1, ?2, ?3, 'active', ?4, ?5, NULL, NULL, NULL)",
            rusqlite::params![
                grant.grant_id,
                grant.request_id,
                grant.request_json,
                grant.authorized_at,
                grant.expires_at,
            ],
        )?;
        ensure!(inserted == 1, "owner-action grant was not inserted");
        Ok(())
    }

    pub(in crate::service) fn revoke_grant(
        &self,
        grant_id: &str,
        revoked_at: &str,
    ) -> Result<RevokeGrantOutcome> {
        let Some(grant) = self.grant(grant_id)? else {
            return Ok(RevokeGrantOutcome::Missing);
        };
        match grant.state {
            OwnerActionGrantState::Revoked => Ok(RevokeGrantOutcome::AlreadyRevoked),
            OwnerActionGrantState::Consumed => Ok(RevokeGrantOutcome::AlreadyConsumed),
            OwnerActionGrantState::Active => {
                let changed = self.conn.execute(
                    "UPDATE owner_action_grant
                     SET state = 'revoked', revoked_at = ?1
                     WHERE grant_id = ?2 AND state = 'active'",
                    rusqlite::params![revoked_at, grant_id],
                )?;
                ensure!(changed == 1, "owner-action grant changed during revocation");
                Ok(RevokeGrantOutcome::Revoked)
            }
        }
    }

    /// Consumes exactly one active grant. The caller is responsible for using
    /// the same transaction for record changes, receipt insertion, and commit.
    pub(in crate::service) fn consume_active_grant(
        &self,
        grant_id: &str,
        application_id: &str,
        consumed_at: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE owner_action_grant
             SET state = 'consumed', consumed_at = ?1, consumed_application_id = ?2
             WHERE grant_id = ?3 AND state = 'active'",
            rusqlite::params![consumed_at, application_id, grant_id],
        )? == 1)
    }

    pub(in crate::service) fn application_by_operation_id(
        &self,
        operation_id: &str,
    ) -> Result<Option<PrivateLifecycleApplicationRow>> {
        self.conn
            .query_row(
                "SELECT application_id, operation_id, request_id, grant_id, result_json,
                        lifecycle_generation, applied_at
                 FROM private_lifecycle_application
                 WHERE operation_id = ?1",
                [operation_id],
                private_lifecycle_application_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(in crate::service) fn insert_application(
        &self,
        application: &PrivateLifecycleApplicationRow,
    ) -> Result<()> {
        let inserted = self.conn.execute(
            "INSERT INTO private_lifecycle_application(
               application_id, operation_id, request_id, grant_id, result_json,
               lifecycle_generation, applied_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                application.application_id,
                application.operation_id,
                application.request_id,
                application.grant_id,
                application.result_json,
                application.lifecycle_generation,
                application.applied_at,
            ],
        )?;
        ensure!(
            inserted == 1,
            "private lifecycle application receipt was not inserted"
        );
        Ok(())
    }
}

pub(in crate::service) fn lifecycle_generation(conn: &Connection) -> Result<i64> {
    conn.query_row(
        "SELECT generation FROM private_lifecycle_generation WHERE singleton = ?1",
        [LIFECYCLE_SINGLETON],
        |row| row.get(0),
    )
    .context("private lifecycle generation is not initialized")
}

/// Explicitly advances the generation inside the caller's transaction. Most
/// record/state/relation writes advance it through schema triggers; this helper
/// is available for a lifecycle mutation that otherwise has no triggering row.
#[allow(dead_code)]
pub(in crate::service) fn advance_lifecycle_generation(conn: &Connection) -> Result<i64> {
    let changed = conn.execute(
        "UPDATE private_lifecycle_generation
         SET generation = generation + 1
         WHERE singleton = ?1",
        [LIFECYCLE_SINGLETON],
    )?;
    ensure!(
        changed == 1,
        "private lifecycle generation is not initialized"
    );
    lifecycle_generation(conn)
}

pub(in crate::service) fn set_lifecycle_generation(
    conn: &Connection,
    generation: i64,
) -> Result<()> {
    ensure!(
        generation >= 0,
        "private lifecycle generation cannot be negative"
    );
    let changed = conn.execute(
        "UPDATE private_lifecycle_generation SET generation = ?1 WHERE singleton = ?2",
        rusqlite::params![generation, LIFECYCLE_SINGLETON],
    )?;
    ensure!(
        changed == 1,
        "private lifecycle generation is not initialized"
    );
    Ok(())
}

pub(super) fn private_record_version(conn: &Connection, record_id: &str) -> Result<String> {
    PrivateLifecycleStorage::new(conn)
        .require_state(record_id)
        .map(|state| state.record_version)
}

pub(super) fn ensure_private_record_version(
    conn: &Connection,
    record_id: &str,
    expected_version: &str,
) -> Result<()> {
    ensure!(
        !expected_version.trim().is_empty(),
        "expected private record version is required"
    );
    let current = private_record_version(conn, record_id)?;
    ensure!(
        current == expected_version,
        "private record {record_id} version mismatch: expected {expected_version}, current {current}"
    );
    Ok(())
}

/// Rotates a private version token in the caller's transaction. This is for
/// lifecycle relation/state mutations implemented outside the schema-triggered
/// paths; ordinary memory-record writes rotate automatically.
#[allow(dead_code)]
pub(super) fn rotate_private_record_version(
    conn: &Connection,
    record_id: &str,
    updated_at: &str,
) -> Result<String> {
    let changed = conn.execute(
        "UPDATE private_lifecycle_state
         SET record_version = lower(hex(randomblob(16))), updated_at = ?1
         WHERE record_id = ?2",
        rusqlite::params![updated_at, record_id],
    )?;
    ensure!(
        changed == 1,
        "private lifecycle state not found: {record_id}"
    );
    advance_lifecycle_generation(conn)?;
    conn.execute(
        "INSERT OR REPLACE INTO runtime_mirror_state(singleton, revision)
         VALUES (1, lower(hex(randomblob(16))))",
        [],
    )?;
    private_record_version(conn, record_id)
}

fn private_lifecycle_state_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<PrivateLifecycleState> {
    Ok(PrivateLifecycleState {
        record_id: row.get(0)?,
        automatic_recall_until: row.get(1)?,
        validity_until: row.get(2)?,
        retain_until: row.get(3)?,
        pinned: row.get(4)?,
        quarantined: row.get(5)?,
        quarantine_reason_code: row.get(6)?,
        record_version: row.get(7)?,
        automatic_recall_event_id: row.get(8)?,
        validity_event_id: row.get(9)?,
        retention_event_id: row.get(10)?,
        quarantine_event_id: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn private_lifecycle_relation_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<PrivateLifecycleRelation> {
    let relation_kind: String = row.get(1)?;
    let relation_kind = PrivateLifecycleRelationKind::parse(&relation_kind).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, error.into())
    })?;
    Ok(PrivateLifecycleRelation {
        id: row.get(0)?,
        relation_kind,
        subject_record_id: row.get(2)?,
        related_record_id: row.get(3)?,
        application_id: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn owner_action_grant_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OwnerActionGrantRow> {
    let state: String = row.get(3)?;
    let state = OwnerActionGrantState::parse(&state).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, error.into())
    })?;
    Ok(OwnerActionGrantRow {
        grant_id: row.get(0)?,
        request_id: row.get(1)?,
        request_json: row.get(2)?,
        state,
        authorized_at: row.get(4)?,
        expires_at: row.get(5)?,
        revoked_at: row.get(6)?,
        consumed_at: row.get(7)?,
        consumed_application_id: row.get(8)?,
    })
}

fn private_lifecycle_application_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<PrivateLifecycleApplicationRow> {
    Ok(PrivateLifecycleApplicationRow {
        application_id: row.get(0)?,
        operation_id: row.get(1)?,
        request_id: row.get(2)?,
        grant_id: row.get(3)?,
        result_json: row.get(4)?,
        lifecycle_generation: row.get(5)?,
        applied_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use rusqlite::Connection;

    use crate::{MemoryLane, MemoryType, db};

    use super::*;
    use crate::service::runtime_records::{LocalMemoryInput, RuntimeRecords};

    fn grant(id: &str, request_id: &str) -> OwnerActionGrantRow {
        OwnerActionGrantRow {
            grant_id: id.to_owned(),
            request_id: request_id.to_owned(),
            request_json: format!(r#"{{"request_id":"{request_id}"}}"#),
            state: OwnerActionGrantState::Active,
            authorized_at: "2026-07-19T10:00:00Z".to_owned(),
            expires_at: "2026-07-19T11:00:00Z".to_owned(),
            revoked_at: None,
            consumed_at: None,
            consumed_application_id: None,
        }
    }

    #[test]
    fn grant_writes_do_not_change_lifecycle_or_mirror_state() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        db::init_database(&conn)?;
        let before_generation = lifecycle_generation(&conn)?;
        let before_revision: Option<String> = conn
            .query_row(
                "SELECT revision FROM runtime_mirror_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;

        let store = PrivateLifecycleStorage::new(&conn);
        let grant = grant("grant-1", "request-1");
        store.insert_grant(&grant)?;

        assert_eq!(lifecycle_generation(&conn)?, before_generation);
        assert_eq!(
            conn.query_row(
                "SELECT revision FROM runtime_mirror_state WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?,
            before_revision
        );
        assert_eq!(
            store.identical_active_grants(&grant.request_id, &grant.request_json)?,
            vec![grant]
        );
        Ok(())
    }

    #[test]
    fn grant_insert_compares_fractional_timestamps_chronologically() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        db::init_database(&conn)?;
        let store = PrivateLifecycleStorage::new(&conn);

        let mut valid = grant("grant-fractional-valid", "request-fractional-valid");
        valid.authorized_at = "2026-07-19T10:00:00.1Z".to_owned();
        valid.expires_at = "2026-07-19T10:00:00.1001Z".to_owned();
        store.insert_grant(&valid)?;

        let mut invalid = grant("grant-fractional-invalid", "request-fractional-invalid");
        invalid.authorized_at = "2026-07-19T10:00:00.1001Z".to_owned();
        invalid.expires_at = "2026-07-19T10:00:00.1Z".to_owned();
        let error = store
            .insert_grant(&invalid)
            .expect_err("chronologically non-increasing authority must be rejected");
        assert!(format!("{error:#}").contains("expiry must be later"));
        assert!(store.grant(&invalid.grant_id)?.is_none());
        Ok(())
    }

    #[test]
    fn grant_transitions_and_operation_receipts_are_exact() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        db::init_database(&conn)?;
        let store = PrivateLifecycleStorage::new(&conn);
        store.insert_grant(&grant("grant-revoked", "request-revoked"))?;
        assert_eq!(
            store.revoke_grant("grant-revoked", "2026-07-19T10:15:00Z")?,
            RevokeGrantOutcome::Revoked
        );
        assert_eq!(
            store.revoke_grant("grant-revoked", "2026-07-19T10:16:00Z")?,
            RevokeGrantOutcome::AlreadyRevoked
        );

        store.insert_grant(&grant("grant-applied", "request-applied"))?;
        let generation = advance_lifecycle_generation(&conn)?;
        let application = PrivateLifecycleApplicationRow {
            application_id: "application-1".to_owned(),
            operation_id: "operation-1".to_owned(),
            request_id: "request-applied".to_owned(),
            grant_id: "grant-applied".to_owned(),
            result_json: r#"{"applied":true}"#.to_owned(),
            lifecycle_generation: generation,
            applied_at: "2026-07-19T10:20:00Z".to_owned(),
        };
        store.insert_application(&application)?;
        assert!(store.consume_active_grant(
            "grant-applied",
            &application.application_id,
            &application.applied_at
        )?);
        assert!(!store.consume_active_grant(
            "grant-applied",
            "application-2",
            "2026-07-19T10:21:00Z"
        )?);
        assert_eq!(
            store.application_by_operation_id("operation-1")?,
            Some(application)
        );
        assert_eq!(
            store.revoke_grant("grant-applied", "2026-07-19T10:22:00Z")?,
            RevokeGrantOutcome::AlreadyConsumed
        );
        Ok(())
    }

    #[test]
    fn private_records_receive_random_versions_and_relations_rotate_both() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        db::init_database(&conn)?;
        let records = RuntimeRecords::new(&conn);
        let first = records.create_local(
            "test",
            &LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "First private record".to_owned(),
                body: "First body".to_owned(),
            },
            "2026-07-19T10:00:00Z",
        )?;
        let second = records.create_local_avoiding(
            "test",
            &LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Second private record".to_owned(),
                body: "Second body".to_owned(),
            },
            "2026-07-19T10:00:00Z",
            &BTreeSet::new(),
        )?;
        let first_version = records.private_record_version(&first.id)?;
        let second_version = records.private_record_version(&second.id)?;
        assert_eq!(first_version.len(), 32);
        assert_ne!(first_version, second_version);

        PrivateLifecycleStorage::new(&conn).insert_relation(&PrivateLifecycleRelation {
            id: "relation-1".to_owned(),
            relation_kind: PrivateLifecycleRelationKind::SupersededBy,
            subject_record_id: first.id.clone(),
            related_record_id: second.id.clone(),
            application_id: "application-1".to_owned(),
            created_at: "2026-07-19T10:30:00Z".to_owned(),
        })?;
        assert_ne!(records.private_record_version(&first.id)?, first_version);
        assert_ne!(records.private_record_version(&second.id)?, second_version);
        Ok(())
    }

    #[test]
    fn private_child_changes_rotate_version_and_lifecycle_generation() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        db::init_database(&conn)?;
        let records = RuntimeRecords::new(&conn);
        let record = records.create_local(
            "test",
            &LocalMemoryInput {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Versioned private child facts".to_owned(),
                body: "Tags, paths, and capture provenance are part of the exact snapshot."
                    .to_owned(),
            },
            "2026-07-19T10:00:00Z",
        )?;

        let mut prior_version = records.private_record_version(&record.id)?;
        let mut prior_generation = lifecycle_generation(&conn)?;
        let mut assert_rotated = || -> Result<()> {
            let next_version = records.private_record_version(&record.id)?;
            let next_generation = lifecycle_generation(&conn)?;
            assert_ne!(next_version, prior_version);
            assert_eq!(next_generation, prior_generation + 1);
            prior_version = next_version;
            prior_generation = next_generation;
            Ok(())
        };

        conn.execute(
            "INSERT INTO memory_tag(record_id, tag) VALUES (?1, 'owner-selected')",
            [&record.id],
        )?;
        assert_rotated()?;
        conn.execute(
            "UPDATE memory_tag SET tag = 'owner-confirmed'
             WHERE record_id = ?1 AND tag = 'owner-selected'",
            [&record.id],
        )?;
        assert_rotated()?;
        conn.execute(
            "DELETE FROM memory_tag WHERE record_id = ?1 AND tag = 'owner-confirmed'",
            [&record.id],
        )?;
        assert_rotated()?;

        conn.execute(
            "INSERT INTO memory_path(id, record_id, repo_id, path, symbol, line_start, line_end)
             VALUES ('private-path', ?1, 'repo-fixture', 'src/old.rs', 'old_symbol', 1, 2)",
            [&record.id],
        )?;
        assert_rotated()?;
        conn.execute(
            "UPDATE memory_path SET path = 'src/new.rs', symbol = 'new_symbol'
             WHERE id = 'private-path'",
            [],
        )?;
        assert_rotated()?;
        conn.execute("DELETE FROM memory_path WHERE id = 'private-path'", [])?;
        assert_rotated()?;

        conn.execute(
            "INSERT INTO memory_capture(record_id, provenance_json)
             VALUES (?1, '{\"source\":\"first\"}')",
            [&record.id],
        )?;
        assert_rotated()?;
        conn.execute(
            "UPDATE memory_capture SET provenance_json = '{\"source\":\"second\"}'
             WHERE record_id = ?1",
            [&record.id],
        )?;
        assert_rotated()?;
        conn.execute(
            "DELETE FROM memory_capture WHERE record_id = ?1",
            [&record.id],
        )?;
        assert_rotated()?;
        Ok(())
    }
}
