use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::BTreeMap, str::FromStr};
use uuid::Uuid;

use crate::events::{AppendEvent, append_event, now_utc};
use crate::models::{MemoryLane, MemoryRecord, MemoryStatus, MemoryType, ScopeKind, Visibility};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Validated,
    Approved,
    Rejected,
    Applied,
}

impl ProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Validated => "validated",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Applied => "applied",
        }
    }

    pub fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::Validated | Self::Approved)
    }
}

impl FromStr for ProposalStatus {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "validated" => Ok(Self::Validated),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            "applied" => Ok(Self::Applied),
            other => bail!(
                "invalid proposal status {other:?}; expected open, pending, validated, approved, rejected, applied, or all"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalStatusFilter {
    Open,
    Status(ProposalStatus),
    All,
}

impl FromStr for ProposalStatusFilter {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "open" => Ok(Self::Open),
            "all" => Ok(Self::All),
            status => Ok(Self::Status(status.parse()?)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryDraft {
    pub memory_type: MemoryType,
    #[serde(default)]
    pub lane: MemoryLane,
    pub scope_kind: ScopeKind,
    pub scope_id: Option<String>,
    pub visibility: Visibility,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub source_kind: Option<String>,
    pub source_ref: Option<String>,
    pub confidence: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub operation: String,
    pub payload: MemoryDraft,
    pub status: ProposalStatus,
    pub actor: String,
    pub validation: Option<ValidationResult>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationResult {
    pub is_valid: bool,
    pub issues: Vec<ValidationIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
    pub record_id: Option<String>,
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupersedeResult {
    pub previous: MemoryRecord,
    pub replacement: MemoryRecord,
}

pub fn propose_memory(conn: &Connection, actor: &str, draft: MemoryDraft) -> Result<Proposal> {
    validate_draft_shape(&draft)?;
    let id = format!("prop_{}", Uuid::now_v7());
    let now = now_utc()?;
    conn.execute(
        "INSERT INTO proposal (id, operation, payload_json, status, actor, created_at, updated_at)
         VALUES (?1, 'create', ?2, 'pending', ?3, ?4, ?4)",
        params![id, serde_json::to_string(&draft)?, actor, now],
    )?;
    append_event(
        conn,
        AppendEvent {
            event_type: "memory.proposed".to_owned(),
            actor: actor.to_owned(),
            payload: json!({"proposal_id": id, "title": draft.title}),
            record_id: None,
            proposal_id: Some(id.clone()),
        },
    )?;
    load_proposal(conn, &id)
}

pub fn list_proposals(conn: &Connection, filter: ProposalStatusFilter) -> Result<Vec<Proposal>> {
    let mut stmt = match filter {
        ProposalStatusFilter::Open => conn.prepare(
            "SELECT id, operation, payload_json, status, actor, validation_json, created_at, updated_at
             FROM proposal
             WHERE status IN ('pending', 'validated', 'approved')
             ORDER BY created_at ASC, id ASC",
        )?,
        ProposalStatusFilter::Status(_) => conn.prepare(
            "SELECT id, operation, payload_json, status, actor, validation_json, created_at, updated_at
             FROM proposal
             WHERE status = ?1
             ORDER BY created_at ASC, id ASC",
        )?,
        ProposalStatusFilter::All => conn.prepare(
            "SELECT id, operation, payload_json, status, actor, validation_json, created_at, updated_at
             FROM proposal
             ORDER BY created_at ASC, id ASC",
        )?,
    };
    let rows = match filter {
        ProposalStatusFilter::Status(status) => stmt
            .query_map(params![status.as_str()], proposal_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        ProposalStatusFilter::Open | ProposalStatusFilter::All => stmt
            .query_map([], proposal_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    };
    rows.into_iter().map(proposal_from_row).collect()
}

pub fn load_proposal_public(conn: &Connection, id: &str) -> Result<Proposal> {
    load_proposal(conn, id)
}

pub fn open_proposal_counts(conn: &Connection) -> Result<BTreeMap<ProposalStatus, usize>> {
    let mut stmt = conn.prepare(
        "SELECT status, COUNT(*)
         FROM proposal
         WHERE status IN ('pending', 'validated', 'approved')
         GROUP BY status",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut counts = BTreeMap::new();
    for row in rows {
        let (status, count) = row?;
        counts.insert(parse_proposal_status(&status)?, count as usize);
    }
    Ok(counts)
}

pub fn validate_proposal(conn: &Connection, proposal_id: &str) -> Result<ValidationResult> {
    let proposal = load_proposal(conn, proposal_id)?;
    let hash = content_hash(&proposal.payload);
    let mut issues = Vec::new();
    if proposal.payload.title.trim().is_empty() {
        issues.push(ValidationIssue {
            code: "missing_title".to_owned(),
            message: "title is required".to_owned(),
            record_id: None,
            content_hash: None,
        });
    }
    if proposal.payload.body.trim().is_empty() {
        issues.push(ValidationIssue {
            code: "missing_body".to_owned(),
            message: "body is required".to_owned(),
            record_id: None,
            content_hash: None,
        });
    }
    for record_id in duplicate_record_ids(conn, &hash)? {
        issues.push(ValidationIssue {
            code: "duplicate_content_hash".to_owned(),
            message: "an active memory already has the same content hash".to_owned(),
            record_id: Some(record_id),
            content_hash: Some(hash.clone()),
        });
    }
    let result = ValidationResult {
        is_valid: issues.is_empty(),
        issues,
    };
    conn.execute(
        "UPDATE proposal SET validation_json = ?1, updated_at = ?2 WHERE id = ?3",
        params![serde_json::to_string(&result)?, now_utc()?, proposal_id],
    )?;
    Ok(result)
}

pub fn approve_proposal(conn: &Connection, proposal_id: &str, actor: &str) -> Result<Proposal> {
    set_proposal_status(conn, proposal_id, ProposalStatus::Approved)?;
    append_event(
        conn,
        AppendEvent {
            event_type: "proposal.approved".to_owned(),
            actor: actor.to_owned(),
            payload: json!({"proposal_id": proposal_id}),
            record_id: None,
            proposal_id: Some(proposal_id.to_owned()),
        },
    )?;
    load_proposal(conn, proposal_id)
}

pub fn reject_proposal(
    conn: &Connection,
    proposal_id: &str,
    actor: &str,
    reason: &str,
) -> Result<Proposal> {
    set_proposal_status(conn, proposal_id, ProposalStatus::Rejected)?;
    append_event(
        conn,
        AppendEvent {
            event_type: "proposal.rejected".to_owned(),
            actor: actor.to_owned(),
            payload: json!({"proposal_id": proposal_id, "reason": reason}),
            record_id: None,
            proposal_id: Some(proposal_id.to_owned()),
        },
    )?;
    load_proposal(conn, proposal_id)
}

pub fn apply_proposal(conn: &Connection, proposal_id: &str, actor: &str) -> Result<MemoryRecord> {
    let proposal = load_proposal(conn, proposal_id)?;
    match proposal.status {
        ProposalStatus::Approved => {}
        ProposalStatus::Rejected => bail!("rejected proposal {proposal_id} cannot be applied"),
        ProposalStatus::Applied => bail!("proposal {proposal_id} is already applied"),
        ProposalStatus::Pending | ProposalStatus::Validated => {
            bail!("proposal {proposal_id} must be approved before apply")
        }
    }
    let record = insert_record(conn, &proposal.payload, None, Some(proposal_id))?;
    set_proposal_status(conn, proposal_id, ProposalStatus::Applied)?;
    append_event(
        conn,
        AppendEvent {
            event_type: "memory.applied".to_owned(),
            actor: actor.to_owned(),
            payload: json!({"proposal_id": proposal_id, "record_id": record.id}),
            record_id: Some(record.id.clone()),
            proposal_id: Some(proposal_id.to_owned()),
        },
    )?;
    Ok(record)
}

pub fn supersede_record(
    conn: &Connection,
    record_id: &str,
    actor: &str,
    replacement: MemoryDraft,
) -> Result<SupersedeResult> {
    let replacement = insert_record(conn, &replacement, Some(record_id), None)?;
    let superseded_at = now_utc()?;
    conn.execute(
        "UPDATE memory_record SET status = 'superseded', updated_at = ?1 WHERE id = ?2",
        params![superseded_at, record_id],
    )?;
    let previous = load_record(conn, record_id)?;
    append_event(
        conn,
        AppendEvent {
            event_type: "memory.superseded".to_owned(),
            actor: actor.to_owned(),
            payload: json!({"previous_record_id": record_id, "replacement_record_id": replacement.id}),
            record_id: Some(replacement.id.clone()),
            proposal_id: None,
        },
    )?;
    Ok(SupersedeResult {
        previous,
        replacement,
    })
}

pub fn tombstone_record(
    conn: &Connection,
    record_id: &str,
    actor: &str,
    reason: &str,
) -> Result<MemoryRecord> {
    conn.execute(
        "UPDATE memory_record SET status = 'tombstoned', updated_at = ?1 WHERE id = ?2",
        params![now_utc()?, record_id],
    )?;
    let record = load_record(conn, record_id)?;
    append_event(
        conn,
        AppendEvent {
            event_type: "memory.tombstoned".to_owned(),
            actor: actor.to_owned(),
            payload: json!({"record_id": record_id, "reason": reason}),
            record_id: Some(record_id.to_owned()),
            proposal_id: None,
        },
    )?;
    Ok(record)
}

fn validate_draft_shape(draft: &MemoryDraft) -> Result<()> {
    if draft.title.trim().is_empty() {
        bail!("title is required");
    }
    if draft.body.trim().is_empty() {
        bail!("body is required");
    }
    Ok(())
}

type ProposalRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
);

fn load_proposal(conn: &Connection, id: &str) -> Result<Proposal> {
    let row = conn
        .query_row(
            "SELECT id, operation, payload_json, status, actor, validation_json, created_at, updated_at
             FROM proposal WHERE id = ?1",
            [id],
            proposal_row,
        )
        .optional()?
        .with_context(|| format!("proposal not found: {id}"))?;
    proposal_from_row(row)
}

fn proposal_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProposalRow> {
    Ok((
        row.get::<_, String>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, String>(3)?,
        row.get::<_, String>(4)?,
        row.get::<_, Option<String>>(5)?,
        row.get::<_, String>(6)?,
        row.get::<_, String>(7)?,
    ))
}

fn proposal_from_row(row: ProposalRow) -> Result<Proposal> {
    Ok(Proposal {
        id: row.0,
        operation: row.1,
        payload: serde_json::from_str(&row.2)?,
        status: parse_proposal_status(&row.3)?,
        actor: row.4,
        validation: row.5.map(|json| serde_json::from_str(&json)).transpose()?,
        created_at: row.6,
        updated_at: row.7,
    })
}

fn set_proposal_status(conn: &Connection, id: &str, status: ProposalStatus) -> Result<()> {
    let changed = conn.execute(
        "UPDATE proposal SET status = ?1, updated_at = ?2 WHERE id = ?3",
        params![status.as_str(), now_utc()?, id],
    )?;
    if changed == 0 {
        bail!("proposal not found: {id}");
    }
    Ok(())
}

fn insert_record(
    conn: &Connection,
    draft: &MemoryDraft,
    supersedes_id: Option<&str>,
    proposal_id: Option<&str>,
) -> Result<MemoryRecord> {
    validate_draft_shape(draft)?;
    let id = next_record_id(conn, draft)?;
    let now = now_utc()?;
    let hash = content_hash(draft);
    conn.execute(
        "INSERT INTO memory_record (
          id, type, lane, scope_kind, scope_id, visibility, title, body, status, confidence,
          source_kind, source_ref, content_hash, created_at, updated_at, supersedes_id
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'active', ?9, ?10, ?11, ?12, ?13, ?13, ?14)",
        params![
            id,
            draft.memory_type.as_str(),
            draft.lane.as_str(),
            draft.scope_kind.as_str(),
            draft.scope_id,
            draft.visibility.as_str(),
            draft.title.trim(),
            draft.body.trim(),
            draft.confidence,
            draft.source_kind,
            proposal_id.or(draft.source_ref.as_deref()),
            hash,
            now,
            supersedes_id,
        ],
    )?;
    for tag in &draft.tags {
        conn.execute(
            "INSERT OR IGNORE INTO memory_tag(record_id, tag) VALUES (?1, ?2)",
            params![id, tag],
        )?;
    }
    load_record(conn, &id)
}

fn load_record(conn: &Connection, id: &str) -> Result<MemoryRecord> {
    conn.query_row(
        "SELECT id, type, lane, scope_kind, scope_id, visibility, title, body, status, confidence,
                source_kind, source_ref, content_hash, created_at, updated_at, supersedes_id, expires_at
         FROM memory_record WHERE id = ?1",
        [id],
        |row| {
            let memory_type: String = row.get(1)?;
            let lane: String = row.get(2)?;
            let scope_kind: String = row.get(3)?;
            let visibility: String = row.get(5)?;
            let status: String = row.get(8)?;
            Ok(MemoryRecord {
                id: row.get(0)?,
                memory_type: parse_memory_type(&memory_type)?,
                lane: parse_model_enum(&lane)?,
                scope_kind: parse_scope_kind(&scope_kind)?,
                scope_id: row.get(4)?,
                visibility: parse_visibility(&visibility)?,
                title: row.get(6)?,
                body: row.get(7)?,
                status: parse_memory_status(&status)?,
                confidence: row.get(9)?,
                source_kind: row.get(10)?,
                source_ref: row.get(11)?,
                content_hash: row.get(12)?,
                created_at: row.get(13)?,
                updated_at: row.get(14)?,
                supersedes_id: row.get(15)?,
                expires_at: row.get(16)?,
            })
        },
    )
    .optional()?
    .with_context(|| format!("memory record not found: {id}"))
}

fn duplicate_record_ids(conn: &Connection, content_hash: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM memory_record WHERE content_hash = ?1 AND status NOT IN ('tombstoned', 'redacted') ORDER BY id",
    )?;
    let rows = stmt.query_map([content_hash], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn content_hash(draft: &MemoryDraft) -> String {
    blake3::hash(draft.body.as_bytes()).to_hex().to_string()
}

fn next_record_id(conn: &Connection, draft: &MemoryDraft) -> Result<String> {
    let base = title_to_concept_id(&draft.title);
    if !record_exists(conn, &base)? {
        return Ok(base);
    }
    for suffix in 2.. {
        let candidate = format!("{base}-{suffix}");
        if !record_exists(conn, &candidate)? {
            return Ok(candidate);
        }
    }
    unreachable!("unbounded suffix search returns")
}

fn record_exists(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_record WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?)
}

fn title_to_concept_id(title: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for ch in title.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            previous_dash = false;
        } else if !previous_dash && !slug.is_empty() {
            slug.push('-');
            previous_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        format!("memory-{}", Uuid::now_v7())
    } else {
        slug
    }
}

fn parse_proposal_status(value: &str) -> rusqlite::Result<ProposalStatus> {
    value.parse().map_err(|_| {
        rusqlite::Error::InvalidParameterName(format!("invalid proposal status: {value}"))
    })
}

fn parse_memory_type(value: &str) -> rusqlite::Result<MemoryType> {
    parse_model_enum(value)
}

fn parse_memory_status(value: &str) -> rusqlite::Result<MemoryStatus> {
    parse_model_enum(value)
}

fn parse_scope_kind(value: &str) -> rusqlite::Result<ScopeKind> {
    parse_model_enum(value)
}

fn parse_visibility(value: &str) -> rusqlite::Result<Visibility> {
    parse_model_enum(value)
}

fn parse_model_enum<T>(value: &str) -> rusqlite::Result<T>
where
    T: FromStr<Err = String>,
{
    value.parse().map_err(rusqlite::Error::InvalidParameterName)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use crate::{
        events::list_events,
        init_database,
        models::{MemoryLane, MemoryStatus, MemoryType, ScopeKind, Visibility},
        open_database,
        proposals::{
            MemoryDraft, ProposalStatus, ProposalStatusFilter, apply_proposal, approve_proposal,
            list_proposals, load_proposal, open_proposal_counts, propose_memory, reject_proposal,
            supersede_record, tombstone_record, validate_proposal,
        },
    };

    #[test]
    fn propose_creates_pending_proposal_and_proposed_event() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        let draft = sample_memory_draft("Prefer deterministic tests");

        let proposal = propose_memory(&conn, "agent:red-tests", draft.clone())?;

        assert_eq!(proposal.status, ProposalStatus::Pending);
        assert_eq!(proposal.operation, "create");
        assert_eq!(proposal.actor, "agent:red-tests");
        assert_eq!(proposal.payload.title, draft.title);
        assert_eq!(proposal.payload.body, draft.body);
        assert_eq!(proposal.payload.memory_type, MemoryType::Fact);
        assert_eq!(proposal.payload.scope_kind, ScopeKind::Repo);
        assert_eq!(proposal.payload.visibility, Visibility::Repo);

        let events = list_events(&conn)?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "memory.proposed");
        assert_eq!(events[0].actor, "agent:red-tests");
        assert_eq!(events[0].proposal_id.as_deref(), Some(proposal.id.as_str()));
        assert_eq!(events[0].record_id, None);
        assert_eq!(events[0].payload["proposal_id"], proposal.id);
        assert_eq!(events[0].payload["title"], draft.title);

        Ok(())
    }

    #[test]
    fn approve_then_apply_creates_active_record_and_auditable_events() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        let proposal = propose_memory(
            &conn,
            "agent:red-tests",
            sample_memory_draft("Keep proposal approvals explicit"),
        )?;

        let validation = validate_proposal(&conn, proposal.id.as_str())?;
        assert!(
            validation.is_valid,
            "new unique proposal should validate: {validation:?}"
        );

        let approved = approve_proposal(&conn, proposal.id.as_str(), "reviewer:human")?;
        assert_eq!(approved.status, ProposalStatus::Approved);

        let record = apply_proposal(&conn, proposal.id.as_str(), "agent:applier")?;
        assert_eq!(record.status, MemoryStatus::Active);
        assert_eq!(record.title, "Keep proposal approvals explicit");
        assert_eq!(record.memory_type, MemoryType::Fact);
        assert_eq!(record.scope_kind, ScopeKind::Repo);
        assert_eq!(record.visibility, Visibility::Repo);
        assert_eq!(record.supersedes_id, None);

        let event_types = list_events(&conn)?
            .into_iter()
            .map(|event| event.event_type)
            .collect::<Vec<_>>();
        assert_eq!(
            event_types,
            vec!["memory.proposed", "proposal.approved", "memory.applied"]
        );

        Ok(())
    }

    #[test]
    fn rejected_proposal_cannot_be_applied() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        let proposal = propose_memory(
            &conn,
            "agent:red-tests",
            sample_memory_draft("Reject unsafe memory writes"),
        )?;

        let rejected = reject_proposal(
            &conn,
            proposal.id.as_str(),
            "reviewer:human",
            "The claim is not supported by the repo.",
        )?;
        assert_eq!(rejected.status, ProposalStatus::Rejected);

        let error = apply_proposal(&conn, proposal.id.as_str(), "agent:applier")
            .expect_err("rejected proposals must not create active memory records");
        assert!(
            error.to_string().contains("rejected"),
            "apply error should explain the rejected proposal, got {error:?}"
        );

        let event_types = list_events(&conn)?
            .into_iter()
            .map(|event| event.event_type)
            .collect::<Vec<_>>();
        assert_eq!(event_types, vec!["memory.proposed", "proposal.rejected"]);

        Ok(())
    }

    #[test]
    fn validated_proposal_loads_and_still_requires_approval_before_apply() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        let proposal = propose_memory(
            &conn,
            "agent:red-tests",
            sample_memory_draft("Validate before approval"),
        )?;
        conn.execute(
            "UPDATE proposal SET status = 'validated' WHERE id = ?1",
            [proposal.id.as_str()],
        )?;

        let loaded = load_proposal(&conn, proposal.id.as_str())?;
        assert_eq!(loaded.status, ProposalStatus::Validated);
        let error = apply_proposal(&conn, proposal.id.as_str(), "agent:applier")
            .expect_err("validated proposals still require explicit approval");
        assert!(
            error.to_string().contains("must be approved before apply"),
            "unexpected error: {error:#}"
        );

        Ok(())
    }

    #[test]
    fn validation_flags_duplicate_blake3_content_hashes() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        let draft = sample_memory_draft("Duplicate content hashes are rejected");
        let proposal = propose_memory(&conn, "agent:red-tests", draft.clone())?;
        approve_proposal(&conn, proposal.id.as_str(), "reviewer:human")?;
        let original = apply_proposal(&conn, proposal.id.as_str(), "agent:applier")?;

        let duplicate = propose_memory(&conn, "agent:red-tests", draft)?;
        let validation = validate_proposal(&conn, duplicate.id.as_str())?;

        let expected_hash = blake3::hash("Duplicate content hashes are rejected".as_bytes())
            .to_hex()
            .to_string();
        assert_eq!(original.content_hash, expected_hash);
        assert!(
            !validation.is_valid,
            "exact duplicate content should not validate"
        );
        assert!(
            validation.issues.iter().any(|issue| {
                issue.code == "duplicate_content_hash"
                    && issue.content_hash.as_deref() == Some(expected_hash.as_str())
                    && issue.record_id.as_deref() == Some(original.id.as_str())
            }),
            "validation should include duplicate_content_hash issue with the conflicting record id and blake3 hash: {validation:?}"
        );

        Ok(())
    }

    #[test]
    fn proposal_inbox_filters_order_counts_and_missing_show_errors() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        let pending = propose_memory(
            &conn,
            "agent:red-tests",
            sample_memory_draft("Pending inbox proposal"),
        )?;
        let validated = propose_memory(
            &conn,
            "agent:red-tests",
            sample_memory_draft("Validated inbox proposal"),
        )?;
        let approved = propose_memory(
            &conn,
            "agent:red-tests",
            sample_memory_draft("Approved inbox proposal"),
        )?;
        let rejected = propose_memory(
            &conn,
            "agent:red-tests",
            sample_memory_draft("Rejected inbox proposal"),
        )?;
        let applied = propose_memory(
            &conn,
            "agent:red-tests",
            sample_memory_draft("Applied inbox proposal"),
        )?;

        set_proposal_fixture_status(
            &conn,
            pending.id.as_str(),
            ProposalStatus::Pending,
            "2026-07-05T00:00:01Z",
        )?;
        set_proposal_fixture_status(
            &conn,
            validated.id.as_str(),
            ProposalStatus::Validated,
            "2026-07-05T00:00:02Z",
        )?;
        approve_proposal(&conn, approved.id.as_str(), "reviewer:human")?;
        set_proposal_fixture_status(
            &conn,
            approved.id.as_str(),
            ProposalStatus::Approved,
            "2026-07-05T00:00:03Z",
        )?;
        reject_proposal(
            &conn,
            rejected.id.as_str(),
            "reviewer:human",
            "not supported",
        )?;
        set_proposal_fixture_status(
            &conn,
            rejected.id.as_str(),
            ProposalStatus::Rejected,
            "2026-07-05T00:00:04Z",
        )?;
        approve_proposal(&conn, applied.id.as_str(), "reviewer:human")?;
        apply_proposal(&conn, applied.id.as_str(), "agent:applier")?;
        set_proposal_fixture_status(
            &conn,
            applied.id.as_str(),
            ProposalStatus::Applied,
            "2026-07-05T00:00:05Z",
        )?;

        let open = list_proposals(&conn, ProposalStatusFilter::Open)?;
        assert_eq!(
            open.iter()
                .map(|proposal| (proposal.id.as_str(), proposal.status))
                .collect::<Vec<_>>(),
            vec![
                (pending.id.as_str(), ProposalStatus::Pending),
                (validated.id.as_str(), ProposalStatus::Validated),
                (approved.id.as_str(), ProposalStatus::Approved),
            ]
        );

        let all = list_proposals(&conn, ProposalStatusFilter::All)?;
        assert_eq!(
            all.iter()
                .map(|proposal| proposal.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                pending.id.as_str(),
                validated.id.as_str(),
                approved.id.as_str(),
                rejected.id.as_str(),
                applied.id.as_str(),
            ]
        );

        let approved_only = list_proposals(
            &conn,
            ProposalStatusFilter::Status(ProposalStatus::Approved),
        )?;
        assert_eq!(
            approved_only
                .iter()
                .map(|proposal| proposal.id.as_str())
                .collect::<Vec<_>>(),
            vec![approved.id.as_str()]
        );

        let counts = open_proposal_counts(&conn)?;
        assert_eq!(counts.get(&ProposalStatus::Pending), Some(&1));
        assert_eq!(counts.get(&ProposalStatus::Validated), Some(&1));
        assert_eq!(counts.get(&ProposalStatus::Approved), Some(&1));
        assert_eq!(counts.get(&ProposalStatus::Rejected), None);
        assert_eq!(counts.get(&ProposalStatus::Applied), None);

        let error = load_proposal(&conn, "prop_missing")
            .expect_err("missing proposal lookup should explain what was not found");
        assert!(
            error
                .to_string()
                .contains("proposal not found: prop_missing"),
            "missing proposal error should include the requested id: {error:#}"
        );

        Ok(())
    }

    #[test]
    fn supersede_and_tombstone_update_current_state_and_append_events() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        let proposal = propose_memory(
            &conn,
            "agent:red-tests",
            sample_memory_draft("Original memory body"),
        )?;
        approve_proposal(&conn, proposal.id.as_str(), "reviewer:human")?;
        let original = apply_proposal(&conn, proposal.id.as_str(), "agent:applier")?;

        let replacement_draft = MemoryDraft {
            title: "Replacement memory body".to_owned(),
            body: "Replacement memory body".to_owned(),
            ..sample_memory_draft("Replacement memory body")
        };
        let superseded = supersede_record(
            &conn,
            original.id.as_str(),
            "agent:red-tests",
            replacement_draft,
        )?;
        assert_eq!(superseded.previous.status, MemoryStatus::Superseded);
        assert_eq!(superseded.previous.id, original.id);
        assert_eq!(superseded.replacement.status, MemoryStatus::Active);
        assert_eq!(
            superseded.replacement.supersedes_id.as_deref(),
            Some(original.id.as_str())
        );

        let tombstoned = tombstone_record(
            &conn,
            superseded.replacement.id.as_str(),
            "agent:red-tests",
            "The replacement is obsolete.",
        )?;
        assert_eq!(tombstoned.status, MemoryStatus::Tombstoned);
        assert_eq!(tombstoned.id, superseded.replacement.id);

        let event_types = list_events(&conn)?
            .into_iter()
            .map(|event| event.event_type)
            .collect::<Vec<_>>();
        assert_eq!(
            event_types,
            vec![
                "memory.proposed",
                "proposal.approved",
                "memory.applied",
                "memory.superseded",
                "memory.tombstoned",
            ]
        );

        Ok(())
    }

    fn set_proposal_fixture_status(
        conn: &rusqlite::Connection,
        proposal_id: &str,
        status: ProposalStatus,
        created_at: &str,
    ) -> anyhow::Result<()> {
        conn.execute(
            "UPDATE proposal
             SET status = ?1, created_at = ?2, updated_at = ?2
             WHERE id = ?3",
            rusqlite::params![status.as_str(), created_at, proposal_id],
        )?;
        Ok(())
    }

    fn initialized_database() -> anyhow::Result<(TempDir, rusqlite::Connection)> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("memory.db");
        let conn = open_database(&db_path)?;
        init_database(&conn)?;
        Ok((temp, conn))
    }

    fn sample_memory_draft(body: &str) -> MemoryDraft {
        MemoryDraft {
            memory_type: MemoryType::Fact,
            lane: MemoryLane::Semantic,
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            visibility: Visibility::Repo,
            title: body.to_owned(),
            body: body.to_owned(),
            tags: vec!["rust".to_owned(), "tests".to_owned()],
            source_kind: Some("test".to_owned()),
            source_ref: Some("proposal-red-tests".to_owned()),
            confidence: 0.82,
        }
    }
}
