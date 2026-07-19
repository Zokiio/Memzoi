use std::collections::BTreeSet;

use anyhow::{Result, bail};
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

use crate::{
    CaptureCandidate, CaptureProvenance, MemoryDestination, MemoryEventDataClass, MemoryLane,
    MemoryRecord, MemoryStatus, MemoryType, OriginDescriptor, OriginRoute, RecordLineage,
    RetentionFacts, ScopeKind, Visibility,
    events::{AppendEvent, append_event},
};

use super::{CheckpointInput, LocalMemoryInput};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InsertMode {
    Create,
    RestoreIfAbsent,
}

#[cfg(test)]
pub(super) fn create_local_memory(
    conn: &Connection,
    actor: &str,
    input: &LocalMemoryInput,
    now: &str,
) -> Result<MemoryRecord> {
    create_local_memory_avoiding(conn, actor, input, now, &BTreeSet::new())
}

pub(super) fn create_local_memory_avoiding(
    conn: &Connection,
    actor: &str,
    input: &LocalMemoryInput,
    now: &str,
    reserved_ids: &BTreeSet<String>,
) -> Result<MemoryRecord> {
    create_local_memory_with_metadata_avoiding(
        conn,
        actor,
        input,
        now,
        OriginDescriptor::new(
            format!("runtime-local:{}", Uuid::now_v7()),
            OriginRoute::LocalMemory,
        ),
        None,
        reserved_ids,
    )
}

pub(super) fn create_local_memory_with_metadata_avoiding(
    conn: &Connection,
    actor: &str,
    input: &LocalMemoryInput,
    now: &str,
    origin: OriginDescriptor,
    lineage: Option<RecordLineage>,
    reserved_ids: &BTreeSet<String>,
) -> Result<MemoryRecord> {
    let id = next_private_record_id(conn, "local", reserved_ids)?;
    create_local_memory_with_metadata_and_id(conn, actor, input, now, origin, lineage, id)
}

pub(super) fn create_local_memory_with_id_for_trusted_recall_eval(
    conn: &Connection,
    actor: &str,
    id: &str,
    input: &LocalMemoryInput,
    now: &str,
) -> Result<MemoryRecord> {
    create_local_memory_with_metadata_and_id(
        conn,
        actor,
        input,
        now,
        OriginDescriptor::new(
            format!("trusted-recall-eval:{id}"),
            OriginRoute::LocalMemory,
        ),
        None,
        id.to_owned(),
    )
}

fn create_local_memory_with_metadata_and_id(
    conn: &Connection,
    actor: &str,
    input: &LocalMemoryInput,
    now: &str,
    origin: OriginDescriptor,
    lineage: Option<RecordLineage>,
    id: String,
) -> Result<MemoryRecord> {
    validate_local_memory_input(input)?;
    validate_private_record_id(&id, "local")?;
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
        retention: retention_facts_for_lane(input.lane, now),
        origin,
        lineage,
    };
    insert_memory_record_row(conn, &record, InsertMode::Create)?;
    append_event(
        conn,
        AppendEvent {
            event_type: "memory.local_created".to_owned(),
            actor: actor.to_owned(),
            data_class: MemoryEventDataClass::Private,
            payload: json!({
                "record_id": &record.id,
                "destination": record.destination.as_str(),
            }),
            record_id: Some(record.id.clone()),
            proposal_id: None,
        },
    )?;
    Ok(record)
}

pub(super) fn create_checkpoint_avoiding(
    conn: &Connection,
    actor: &str,
    input: &CheckpointInput,
    now: &str,
    reserved_ids: &BTreeSet<String>,
) -> Result<MemoryRecord> {
    create_checkpoint_with_metadata_avoiding(
        conn,
        actor,
        input,
        now,
        OriginDescriptor::owner_command(Uuid::now_v7().to_string(), OriginRoute::CheckpointCreate),
        None,
        reserved_ids,
    )
}

pub(super) fn create_checkpoint_with_metadata_avoiding(
    conn: &Connection,
    actor: &str,
    input: &CheckpointInput,
    now: &str,
    origin: OriginDescriptor,
    lineage: Option<RecordLineage>,
    reserved_ids: &BTreeSet<String>,
) -> Result<MemoryRecord> {
    let id = next_private_record_id(conn, "session", reserved_ids)?;
    create_checkpoint_with_metadata_and_id(conn, actor, input, now, origin, lineage, id)
}

pub(super) fn create_checkpoint_with_id_for_trusted_recall_eval(
    conn: &Connection,
    actor: &str,
    id: &str,
    input: &CheckpointInput,
    now: &str,
) -> Result<MemoryRecord> {
    create_checkpoint_with_metadata_and_id(
        conn,
        actor,
        input,
        now,
        OriginDescriptor::new(
            format!("trusted-recall-eval:{id}"),
            OriginRoute::CheckpointCreate,
        ),
        None,
        id.to_owned(),
    )
}

fn create_checkpoint_with_metadata_and_id(
    conn: &Connection,
    actor: &str,
    input: &CheckpointInput,
    now: &str,
    origin: OriginDescriptor,
    lineage: Option<RecordLineage>,
    id: String,
) -> Result<MemoryRecord> {
    validate_checkpoint_input(input)?;
    validate_private_record_id(&id, "session")?;
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
        retention: retention_facts_for_lane(MemoryLane::Session, now),
        origin,
        lineage,
    };
    insert_memory_record_row(conn, &record, InsertMode::Create)?;
    let record_version = super::lifecycle::checkpoint_record_version(conn, &record.id)?;
    append_event(
        conn,
        AppendEvent {
            event_type: "memory.checkpoint_created".to_owned(),
            actor: actor.to_owned(),
            data_class: MemoryEventDataClass::Private,
            payload: json!({
                "record_id": &record.id,
                "destination": record.destination.as_str(),
                "record_version": record_version,
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

fn validate_private_record_id(id: &str, prefix: &str) -> Result<()> {
    if id.len() > 256 || !id.starts_with(&format!("{prefix}-")) || id.contains('/') {
        bail!("private record id must be a bounded {prefix}-* identifier");
    }
    crate::validate_canonical_record_id(id)?;
    Ok(())
}

fn next_private_record_id(
    conn: &Connection,
    prefix: &str,
    reserved_ids: &BTreeSet<String>,
) -> Result<String> {
    loop {
        // Private record identifiers cross lifecycle-plan and audit boundaries.
        // Keep them opaque: neither title/task text nor content participates in
        // the identifier. UUID v4 also avoids exposing creation timestamps.
        let candidate = format!("{prefix}-{}", Uuid::new_v4());
        if !reserved_ids.contains(&candidate) && !record_id_exists(conn, &candidate)? {
            return Ok(candidate);
        }
    }
}

fn record_id_exists(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_record WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?)
}

pub(super) fn insert_memory_record_row(
    conn: &Connection,
    record: &MemoryRecord,
    mode: InsertMode,
) -> Result<bool> {
    let verb = match mode {
        InsertMode::Create => "INSERT INTO",
        InsertMode::RestoreIfAbsent => "INSERT OR IGNORE INTO",
    };
    let sql = format!(
        "{verb} memory_record (
          id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
          confidence, source_kind, source_ref, proposal_id, content_hash, created_at, updated_at,
          supersedes_id, retention_json, origin_json, lineage_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)"
    );
    let inserted = conn.execute(
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
            serde_json::to_string(&record.retention)?,
            serde_json::to_string(&record.origin)?,
            record
                .lineage
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        ],
    )?;
    if inserted == 1 {
        crate::capture::store_capture_provenance(conn, &record.id, record.capture.as_ref())?;
    }
    Ok(inserted == 1)
}

pub(super) fn create_capture(
    conn: &Connection,
    actor: &str,
    candidate: &CaptureCandidate,
    destination: MemoryDestination,
    now: &str,
    provenance: CaptureProvenance,
    reserved_ids: &BTreeSet<String>,
) -> Result<MemoryRecord> {
    if !matches!(
        destination,
        MemoryDestination::Local | MemoryDestination::Session
    ) {
        bail!("capture runtime writes require local or session destination");
    }
    let prefix = match destination {
        MemoryDestination::Local => "local",
        MemoryDestination::Session => "session",
        _ => unreachable!("destination is checked above"),
    };
    let origin = OriginDescriptor::new(
        format!("capture:{}", provenance.claim_id),
        OriginRoute::Capture,
    );
    let record = MemoryRecord {
        id: next_private_record_id(conn, prefix, reserved_ids)?,
        memory_type: candidate.memory.memory_type,
        lane: candidate.memory.lane,
        destination,
        scope_kind: candidate.memory.scope.kind,
        scope_id: candidate.memory.scope.id.clone(),
        visibility: Visibility::Private,
        title: candidate.memory.title.trim().to_owned(),
        body: candidate.memory.body.trim().to_owned(),
        status: MemoryStatus::Active,
        confidence: candidate.confidence,
        source_kind: Some("memzoi-capture".to_owned()),
        source_ref: candidate
            .evidence
            .first()
            .map(crate::CaptureEvidence::durable_reference),
        proposal_id: None,
        capture: Some(provenance),
        content_hash: crate::import::content_hash(&candidate.memory.body),
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
        supersedes_id: None,
        retention: retention_facts_for_lane(candidate.memory.lane, now),
        origin,
        lineage: None,
    };
    insert_memory_record_row(conn, &record, InsertMode::Create)?;
    for tag in &candidate.memory.tags {
        conn.execute(
            "INSERT OR IGNORE INTO memory_tag(record_id, tag) VALUES (?1, ?2)",
            rusqlite::params![record.id, tag],
        )?;
    }
    for (index, path) in candidate.memory.scope.paths.iter().enumerate() {
        conn.execute(
            "INSERT INTO memory_path(id, record_id, path, line_start, line_end)
             VALUES (?1, ?2, ?3, NULL, NULL)",
            rusqlite::params![
                format!("{}_capture_path_{index}", record.id),
                record.id,
                path
            ],
        )?;
    }
    append_event(
        conn,
        AppendEvent {
            event_type: "memory.capture_routed".to_owned(),
            actor: actor.to_owned(),
            data_class: match destination {
                MemoryDestination::Repo => MemoryEventDataClass::Repository,
                MemoryDestination::Local | MemoryDestination::Session => {
                    MemoryEventDataClass::Private
                }
                MemoryDestination::Discard | MemoryDestination::NeedsReview => {
                    MemoryEventDataClass::Private
                }
            },
            payload: json!({
                "record_id": &record.id,
                "destination": destination.as_str(),
                "plan_id": &record.capture.as_ref().expect("capture provenance").plan_id,
                "review_id": &record.capture.as_ref().expect("capture provenance").review_id,
            }),
            record_id: Some(record.id.clone()),
            proposal_id: None,
        },
    )?;
    Ok(record)
}

fn retention_facts_for_lane(lane: MemoryLane, now: &str) -> RetentionFacts {
    RetentionFacts {
        occurred_at: (lane == MemoryLane::Episodic).then(|| now.to_owned()),
        started_at: (lane == MemoryLane::Session).then(|| now.to_owned()),
        last_continued_at: None,
        closed_at: None,
        explicit_expires_at: None,
        episodic_extension: None,
    }
}
