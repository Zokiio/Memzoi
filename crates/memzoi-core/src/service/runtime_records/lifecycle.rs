use anyhow::{Context, Result, bail, ensure};
use rusqlite::Connection;
use serde_json::json;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    MemoryDestination, MemoryEvent, MemoryLane, MemoryRecord, MemoryStatus, RetentionState,
    evaluate_retention,
    events::{AppendEvent, append_event},
};

use super::{
    CloseCheckpointCommand, ContinueCheckpointCommand,
    private_lifecycle_storage::PrivateLifecycleStorage, query,
};

#[derive(Debug)]
pub(in crate::service) struct CheckpointLifecycleMutation {
    pub record: MemoryRecord,
    pub event: Option<MemoryEvent>,
    pub applied: bool,
}

pub(super) fn checkpoint_for_lifecycle(conn: &Connection, record_id: &str) -> Result<MemoryRecord> {
    let record = query::record_by_id(conn, record_id)?
        .with_context(|| format!("checkpoint not found: {record_id}"))?;
    ensure_checkpoint_shape(&record)?;
    Ok(record)
}

pub(super) fn checkpoint_record_version(conn: &Connection, record_id: &str) -> Result<String> {
    let record = checkpoint_for_lifecycle(conn, record_id)?;
    ensure_checkpoint_shape(&record)?;
    super::private_lifecycle_storage::private_record_version(conn, record_id)
}

pub(super) fn continue_checkpoint(
    conn: &Connection,
    actor: &str,
    command: &ContinueCheckpointCommand,
    now: OffsetDateTime,
    timestamp: &str,
) -> Result<CheckpointLifecycleMutation> {
    validate_operation(&command.operation_id, &command.expected_version)?;
    let previous = checkpoint_for_lifecycle(conn, &command.checkpoint_id)?;
    ensure_expected_version(conn, &previous, &command.expected_version)?;
    let previous_version = checkpoint_record_version(conn, &previous.id)?;
    ensure!(
        previous.retention.closed_at.is_none(),
        "checkpoint {} is closed and cannot be reopened",
        previous.id
    );

    ensure!(
        query::checkpoint_record(conn, &previous.id, now)?.is_some(),
        "checkpoint {} is not a current assertion and cannot be continued",
        previous.id
    );
    let previous_lease_anchor = previous
        .retention
        .last_continued_at
        .as_deref()
        .or(previous.retention.started_at.as_deref())
        .context("validated session checkpoint is missing its lease anchor")?;
    let previous_lease_anchor = OffsetDateTime::parse(previous_lease_anchor, &Rfc3339)
        .with_context(|| format!("checkpoint {} has an invalid lease anchor", previous.id))?;
    ensure!(
        now > previous_lease_anchor,
        "checkpoint {} continuation must be later than its previous lease anchor",
        previous.id
    );

    let mut retention = previous.retention.clone();
    retention.last_continued_at = Some(timestamp.to_owned());
    update_retention(conn, &previous.id, &retention, timestamp)?;
    let record = checkpoint_for_lifecycle(conn, &previous.id)?;
    let record_version = checkpoint_record_version(conn, &record.id)?;
    let event = append_event(
        conn,
        AppendEvent {
            event_type: "memory.checkpoint_continued".to_owned(),
            actor: actor.to_owned(),
            payload: json!({
                "record_id": &record.id,
                "operation_id": &command.operation_id,
                "continued_at": timestamp,
                "previous_record_version": previous_version,
                "record_version": record_version,
            }),
            record_id: Some(record.id.clone()),
            proposal_id: None,
        },
    )?;
    Ok(CheckpointLifecycleMutation {
        record,
        event: Some(event),
        applied: true,
    })
}

pub(super) fn close_checkpoint(
    conn: &Connection,
    actor: &str,
    command: &CloseCheckpointCommand,
    now: OffsetDateTime,
    timestamp: &str,
) -> Result<CheckpointLifecycleMutation> {
    validate_operation(&command.operation_id, &command.expected_version)?;
    let previous = checkpoint_for_lifecycle(conn, &command.checkpoint_id)?;
    ensure_expected_version(conn, &previous, &command.expected_version)?;
    let previous_version = checkpoint_record_version(conn, &previous.id)?;
    let lifecycle = PrivateLifecycleStorage::new(conn).require_state(&previous.id)?;
    ensure!(
        !lifecycle.quarantined,
        "checkpoint {} is quarantined and cannot be closed",
        previous.id
    );
    ensure!(
        previous.status == MemoryStatus::Active,
        "checkpoint {} has lifecycle status {} and cannot be closed",
        previous.id,
        previous.status.as_str()
    );
    // Validate every retention fact even though closure is allowed after the
    // temporal boundary. Closing query-only history records a terminal fact;
    // it never makes the record current again.
    evaluate_retention(&previous.id, previous.lane, &previous.retention, now)?;

    if previous.retention.closed_at.is_some() {
        return Ok(CheckpointLifecycleMutation {
            record: previous,
            event: None,
            applied: false,
        });
    }

    let mut retention = previous.retention.clone();
    retention.closed_at = Some(timestamp.to_owned());
    update_retention(conn, &previous.id, &retention, timestamp)?;
    let record = checkpoint_for_lifecycle(conn, &previous.id)?;
    let record_version = checkpoint_record_version(conn, &record.id)?;
    let event = append_event(
        conn,
        AppendEvent {
            event_type: "memory.checkpoint_closed".to_owned(),
            actor: actor.to_owned(),
            payload: json!({
                "record_id": &record.id,
                "operation_id": &command.operation_id,
                "closed_at": timestamp,
                "previous_record_version": previous_version,
                "record_version": record_version,
            }),
            record_id: Some(record.id.clone()),
            proposal_id: None,
        },
    )?;
    Ok(CheckpointLifecycleMutation {
        record,
        event: Some(event),
        applied: true,
    })
}

pub(super) fn ensure_successor_predecessor(
    conn: &Connection,
    record: &MemoryRecord,
    expected_version: &str,
    now: OffsetDateTime,
) -> Result<()> {
    ensure_expected_version(conn, record, expected_version)?;
    ensure!(
        record.status == MemoryStatus::Active,
        "checkpoint {} has lifecycle status {} and cannot be succeeded",
        record.id,
        record.status.as_str()
    );
    let lifecycle = PrivateLifecycleStorage::new(conn).require_state(&record.id)?;
    ensure!(
        !lifecycle.quarantined,
        "checkpoint {} is quarantined and cannot be succeeded",
        record.id
    );
    let retention = evaluate_retention(&record.id, record.lane, &record.retention, now)?;
    ensure!(
        record.retention.closed_at.is_some() || retention.state == RetentionState::QueryOnly,
        "checkpoint {} is still current; close it before creating a successor",
        record.id
    );
    Ok(())
}

pub(super) fn ensure_expected_version(
    conn: &Connection,
    record: &MemoryRecord,
    expected: &str,
) -> Result<()> {
    let actual = checkpoint_record_version(conn, &record.id)?;
    if actual != expected {
        bail!(
            "checkpoint {} version mismatch: expected {}, current {}",
            record.id,
            expected,
            actual
        );
    }
    Ok(())
}

fn update_retention(
    conn: &Connection,
    record_id: &str,
    retention: &crate::RetentionFacts,
    updated_at: &str,
) -> Result<()> {
    let retention_json = serde_json::to_string(retention)?;
    let changed = conn.execute(
        "UPDATE memory_record
         SET retention_json = ?1, updated_at = ?2
         WHERE id = ?3
           AND destination = 'session'
           AND lane = 'session'
           AND source_kind = 'memzoi-checkpoint'",
        rusqlite::params![retention_json, updated_at, record_id],
    )?;
    ensure!(
        changed == 1,
        "checkpoint {record_id} changed during lifecycle update"
    );
    Ok(())
}

fn ensure_checkpoint_shape(record: &MemoryRecord) -> Result<()> {
    ensure!(
        record.destination == MemoryDestination::Session
            && record.lane == MemoryLane::Session
            && record.source_kind.as_deref() == Some("memzoi-checkpoint"),
        "record {} is not a runtime session checkpoint",
        record.id
    );
    Ok(())
}

fn validate_operation(operation_id: &str, expected_version: &str) -> Result<()> {
    ensure!(!operation_id.trim().is_empty(), "operation_id is required");
    ensure!(
        !expected_version.trim().is_empty(),
        "expected checkpoint version is required"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use time::format_description::well_known::Rfc3339;

    use crate::{OriginDescriptor, OriginRoute, RecordLineage, RecordLineageKind, db};

    use super::*;
    use crate::service::runtime_records::{CheckpointInput, write};

    fn at(value: &str) -> OffsetDateTime {
        OffsetDateTime::parse(value, &Rfc3339).expect("test timestamp")
    }

    fn checkpoint(conn: &Connection, id: &str, started_at: &str) -> MemoryRecord {
        write::create_checkpoint_with_metadata_avoiding(
            conn,
            "test",
            &CheckpointInput {
                task: id.to_owned(),
                note: "Explicit checkpoint state".to_owned(),
            },
            started_at,
            OriginDescriptor::new(format!("test:{id}"), OriginRoute::CheckpointCreate),
            None,
            &BTreeSet::new(),
        )
        .expect("create checkpoint")
    }

    #[test]
    fn continuation_changes_version_and_exact_boundary_is_rejected() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        db::init_database(&conn)?;
        crate::retention::register_sqlite_functions(&conn)?;
        let original = checkpoint(&conn, "continuation", "2026-07-01T00:00:00Z");
        let initial_version = checkpoint_record_version(&conn, &original.id)?;
        assert_eq!(initial_version.len(), 32);
        assert!(
            initial_version
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        let continued = continue_checkpoint(
            &conn,
            "owner",
            &ContinueCheckpointCommand {
                operation_id: "continue-1".to_owned(),
                checkpoint_id: original.id.clone(),
                expected_version: initial_version.clone(),
            },
            at("2026-07-01T12:00:00Z"),
            "2026-07-01T12:00:00Z",
        )?;
        assert!(continued.applied);
        assert_ne!(
            checkpoint_record_version(&conn, &continued.record.id)?,
            initial_version
        );
        assert_eq!(
            continued.record.retention.last_continued_at.as_deref(),
            Some("2026-07-01T12:00:00Z")
        );

        let boundary = checkpoint(&conn, "exact-boundary", "2026-07-02T00:00:00Z");
        let error = continue_checkpoint(
            &conn,
            "owner",
            &ContinueCheckpointCommand {
                operation_id: "continue-at-boundary".to_owned(),
                checkpoint_id: boundary.id.clone(),
                expected_version: checkpoint_record_version(&conn, &boundary.id)?,
            },
            at("2026-07-03T00:00:00Z"),
            "2026-07-03T00:00:00Z",
        )
        .expect_err("the inclusive 24-hour boundary is query-only");
        assert!(format!("{error:#}").contains("not a current assertion"));
        Ok(())
    }

    #[test]
    fn quarantined_checkpoint_cannot_be_continued_closed_or_succeeded() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        db::init_database(&conn)?;
        crate::retention::register_sqlite_functions(&conn)?;
        let original = checkpoint(&conn, "quarantined", "2026-07-01T00:00:00Z");
        conn.execute(
            "UPDATE private_lifecycle_state
             SET quarantined = 1,
                 quarantine_reason_code = 'owner_quarantine',
                 quarantine_event_id = 'event-quarantine'
             WHERE record_id = ?1",
            [&original.id],
        )?;
        let version = checkpoint_record_version(&conn, &original.id)?;

        let error = continue_checkpoint(
            &conn,
            "owner",
            &ContinueCheckpointCommand {
                operation_id: "continue-quarantined".to_owned(),
                checkpoint_id: original.id.clone(),
                expected_version: version.clone(),
            },
            at("2026-07-01T12:00:00Z"),
            "2026-07-01T12:00:00Z",
        )
        .expect_err("quarantined checkpoint must not be continued");
        assert!(format!("{error:#}").contains("not a current assertion"));

        let error = close_checkpoint(
            &conn,
            "owner",
            &CloseCheckpointCommand {
                operation_id: "close-quarantined".to_owned(),
                checkpoint_id: original.id.clone(),
                expected_version: version.clone(),
            },
            at("2026-07-01T12:00:00Z"),
            "2026-07-01T12:00:00Z",
        )
        .expect_err("quarantined checkpoint must not be closed");
        assert!(format!("{error:#}").contains("quarantined"));
        assert_eq!(checkpoint_record_version(&conn, &original.id)?, version);
        assert!(
            checkpoint_for_lifecycle(&conn, &original.id)?
                .retention
                .closed_at
                .is_none()
        );
        let close_events: i64 = conn.query_row(
            "SELECT COUNT(*) FROM event_log
             WHERE record_id = ?1 AND event_type = 'memory.checkpoint_closed'",
            [&original.id],
            |row| row.get(0),
        )?;
        assert_eq!(close_events, 0);

        let error =
            ensure_successor_predecessor(&conn, &original, &version, at("2026-07-03T00:00:00Z"))
                .expect_err("quarantined checkpoint must not be used as a successor predecessor");
        assert!(format!("{error:#}").contains("quarantined"));
        Ok(())
    }

    #[test]
    fn closure_is_terminal_and_state_idempotent() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        db::init_database(&conn)?;
        crate::retention::register_sqlite_functions(&conn)?;
        let original = checkpoint(&conn, "closure", "2026-07-01T00:00:00Z");
        let closed = close_checkpoint(
            &conn,
            "owner",
            &CloseCheckpointCommand {
                operation_id: "close-1".to_owned(),
                checkpoint_id: original.id.clone(),
                expected_version: checkpoint_record_version(&conn, &original.id)?,
            },
            at("2026-07-01T01:00:00Z"),
            "2026-07-01T01:00:00Z",
        )?;
        assert!(closed.applied);
        assert_eq!(
            closed.record.retention.closed_at.as_deref(),
            Some("2026-07-01T01:00:00Z")
        );

        let closed_version = checkpoint_record_version(&conn, &closed.record.id)?;
        let repeated = close_checkpoint(
            &conn,
            "owner",
            &CloseCheckpointCommand {
                operation_id: "close-2".to_owned(),
                checkpoint_id: original.id.clone(),
                expected_version: closed_version.clone(),
            },
            at("2026-07-01T02:00:00Z"),
            "2026-07-01T02:00:00Z",
        )?;
        assert!(!repeated.applied);
        assert_eq!(
            checkpoint_record_version(&conn, &repeated.record.id)?,
            closed_version
        );

        let error = continue_checkpoint(
            &conn,
            "owner",
            &ContinueCheckpointCommand {
                operation_id: "reopen".to_owned(),
                checkpoint_id: original.id,
                expected_version: closed_version,
            },
            at("2026-07-01T02:00:00Z"),
            "2026-07-01T02:00:00Z",
        )
        .expect_err("closed checkpoints never reopen");
        assert!(format!("{error:#}").contains("cannot be reopened"));
        Ok(())
    }

    #[test]
    fn successor_requires_terminal_predecessor_and_preserves_lineage() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        db::init_database(&conn)?;
        crate::retention::register_sqlite_functions(&conn)?;
        let predecessor = checkpoint(&conn, "predecessor", "2026-07-01T00:00:00Z");
        let active_version = checkpoint_record_version(&conn, &predecessor.id)?;
        assert!(
            ensure_successor_predecessor(
                &conn,
                &predecessor,
                &active_version,
                at("2026-07-01T01:00:00Z")
            )
            .is_err()
        );
        let closed = close_checkpoint(
            &conn,
            "owner",
            &CloseCheckpointCommand {
                operation_id: "close-predecessor".to_owned(),
                checkpoint_id: predecessor.id.clone(),
                expected_version: active_version,
            },
            at("2026-07-01T01:00:00Z"),
            "2026-07-01T01:00:00Z",
        )?;
        ensure_successor_predecessor(
            &conn,
            &closed.record,
            &checkpoint_record_version(&conn, &closed.record.id)?,
            at("2026-07-01T01:00:00Z"),
        )?;

        let successor = write::create_checkpoint_with_metadata_avoiding(
            &conn,
            "owner",
            &CheckpointInput {
                task: "successor".to_owned(),
                note: "Continued work with explicit handoff".to_owned(),
            },
            "2026-07-01T01:00:00Z",
            OriginDescriptor::new("test:successor", OriginRoute::CheckpointSuccessor),
            Some(RecordLineage {
                kind: RecordLineageKind::SessionSuccessor,
                predecessor_id: predecessor.id.clone(),
            }),
            &BTreeSet::new(),
        )?;
        assert_eq!(
            successor.lineage,
            Some(RecordLineage {
                kind: RecordLineageKind::SessionSuccessor,
                predecessor_id: predecessor.id,
            })
        );
        Ok(())
    }
}
