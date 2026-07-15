use anyhow::{Result, bail};
use rusqlite::Connection;
use serde_json::json;

use crate::{
    CapturePlan, CaptureProvenance, CaptureReview, MemoryDestination, MemoryRecord, MemoryStatus,
    Visibility,
    events::{AppendEvent, append_event},
};

use super::super::{InsertMode, insert_memory_record_row, next_prefixed_record_id};

pub(super) fn capture_provenance(
    plan: &CapturePlan,
    review: &CaptureReview,
    decision: &crate::CaptureReviewDecision,
    candidate: &crate::CaptureCandidate,
    actor: &str,
) -> CaptureProvenance {
    let original = plan
        .candidates
        .iter()
        .find(|original| original.candidate_id == decision.candidate_id)
        .expect("validated capture review decision must name a plan candidate");
    CaptureProvenance {
        schema: crate::CAPTURE_PROVENANCE_SCHEMA.to_owned(),
        plan_id: review.plan_id.clone(),
        review_id: review.review_id.clone(),
        claim_id: original.claim_id.clone(),
        reviewed_claim_id: candidate.claim_id.clone(),
        candidate_id: decision.candidate_id.clone(),
        reviewed_candidate_id: candidate.candidate_id.clone(),
        extraction: candidate.extraction.clone(),
        evidence: candidate.evidence.clone(),
        confidence: candidate.confidence.to_string(),
        classification: candidate.classification.clone(),
        destination: candidate.classification.destination,
        sensitivity: candidate.classification.sensitivity,
        review_outcome: decision.outcome,
        review_reason_code: decision.reason_code.clone(),
        reviewed_by: review.reviewed_by.clone(),
        reviewed_at: review.reviewed_at.clone(),
        routed_by: actor.to_owned(),
    }
}

pub(super) fn create_capture_runtime_with_conn(
    conn: &Connection,
    actor: &str,
    candidate: &crate::CaptureCandidate,
    destination: MemoryDestination,
    now: &str,
    provenance: CaptureProvenance,
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
    let record = MemoryRecord {
        id: next_prefixed_record_id(conn, prefix, &candidate.memory.title)?,
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
        expires_at: None,
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
