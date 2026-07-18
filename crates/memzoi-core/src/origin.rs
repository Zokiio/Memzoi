use std::str::FromStr;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::MemoryDestination;

const ORIGIN_FINGERPRINT_DOMAIN: &[u8] = b"memzoi/origin-input-fingerprint\0";

/// Portable source-event identity stored with a record or repository artifact.
///
/// The local repository authority key is deliberately absent. It is composed
/// into [`OriginIdentity`] only at the runtime boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginDescriptor {
    pub origin_key: String,
    pub route: OriginRoute,
}

impl OriginDescriptor {
    pub fn new(origin_key: impl Into<String>, route: OriginRoute) -> Self {
        Self {
            origin_key: origin_key.into(),
            route,
        }
    }

    pub fn owner_command(operation_id: impl Into<String>, route: OriginRoute) -> Self {
        Self::new(format!("owner-command:{}", operation_id.into()), route)
    }

    pub fn validate(&self) -> Result<()> {
        require_non_empty("origin_key", &self.origin_key)?;
        if self.origin_key == "owner-command:" {
            bail!("owner command operation_id must not be empty");
        }
        Ok(())
    }
}

/// Runtime identity whose uniqueness is scoped to one local repository authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginIdentity {
    pub repository_key: String,
    pub origin_key: String,
    pub route: OriginRoute,
}

impl OriginIdentity {
    pub fn new(repository_key: impl Into<String>, descriptor: OriginDescriptor) -> Self {
        Self {
            repository_key: repository_key.into(),
            origin_key: descriptor.origin_key,
            route: descriptor.route,
        }
    }

    pub fn descriptor(&self) -> OriginDescriptor {
        OriginDescriptor {
            origin_key: self.origin_key.clone(),
            route: self.route,
        }
    }

    pub fn validate(&self) -> Result<()> {
        validate_identity(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginRoute {
    Capture,
    Import,
    SessionEnd,
    LocalMemory,
    CheckpointCreate,
    CheckpointContinue,
    CheckpointClose,
    CheckpointSuccessor,
    RepositoryProposal,
    RepositoryMaterialization,
    OwnerCommand,
}

impl OriginRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Import => "import",
            Self::SessionEnd => "session_end",
            Self::LocalMemory => "local_memory",
            Self::CheckpointCreate => "checkpoint_create",
            Self::CheckpointContinue => "checkpoint_continue",
            Self::CheckpointClose => "checkpoint_close",
            Self::CheckpointSuccessor => "checkpoint_successor",
            Self::RepositoryProposal => "repository_proposal",
            Self::RepositoryMaterialization => "repository_materialization",
            Self::OwnerCommand => "owner_command",
        }
    }
}

impl FromStr for OriginRoute {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "capture" => Ok(Self::Capture),
            "import" => Ok(Self::Import),
            "session_end" => Ok(Self::SessionEnd),
            "local_memory" => Ok(Self::LocalMemory),
            "checkpoint_create" => Ok(Self::CheckpointCreate),
            "checkpoint_continue" => Ok(Self::CheckpointContinue),
            "checkpoint_close" => Ok(Self::CheckpointClose),
            "checkpoint_successor" => Ok(Self::CheckpointSuccessor),
            "repository_proposal" => Ok(Self::RepositoryProposal),
            "repository_materialization" => Ok(Self::RepositoryMaterialization),
            "owner_command" => Ok(Self::OwnerCommand),
            other => bail!("unsupported origin route {other:?}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginOutcomeKind {
    Created,
    ExistingDuplicateNoWrite,
    ConflictNoWrite,
    NeedsReviewNoWrite,
    RejectedNoWrite,
    /// Reserved for #124's content-free no-resurrection barrier.
    Erased,
}

impl OriginOutcomeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::ExistingDuplicateNoWrite => "existing_duplicate_no_write",
            Self::ConflictNoWrite => "conflict_no_write",
            Self::NeedsReviewNoWrite => "needs_review_no_write",
            Self::RejectedNoWrite => "rejected_no_write",
            Self::Erased => "erased",
        }
    }
}

impl FromStr for OriginOutcomeKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "created" => Ok(Self::Created),
            "existing_duplicate_no_write" => Ok(Self::ExistingDuplicateNoWrite),
            "conflict_no_write" => Ok(Self::ConflictNoWrite),
            "needs_review_no_write" => Ok(Self::NeedsReviewNoWrite),
            "rejected_no_write" => Ok(Self::RejectedNoWrite),
            "erased" => Ok(Self::Erased),
            other => bail!("unsupported origin outcome kind {other:?}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginOutcome {
    pub identity: OriginIdentity,
    pub input_fingerprint: String,
    pub outcome: OriginOutcomeKind,
    pub destination: Option<MemoryDestination>,
    pub record_id: Option<String>,
    pub proposal_id: Option<String>,
    pub lifecycle_event_id: Option<String>,
    pub recorded_at: String,
}

impl OriginOutcome {
    pub fn new(
        identity: OriginIdentity,
        input_fingerprint: impl Into<String>,
        outcome: OriginOutcomeKind,
        recorded_at: impl Into<String>,
    ) -> Self {
        Self {
            identity,
            input_fingerprint: input_fingerprint.into(),
            outcome,
            destination: None,
            record_id: None,
            proposal_id: None,
            lifecycle_event_id: None,
            recorded_at: recorded_at.into(),
        }
    }

    pub fn with_destination(mut self, destination: MemoryDestination) -> Self {
        self.destination = Some(destination);
        self
    }

    pub fn with_record_id(mut self, record_id: impl Into<String>) -> Self {
        self.record_id = Some(record_id.into());
        self
    }

    pub fn with_proposal_id(mut self, proposal_id: impl Into<String>) -> Self {
        self.proposal_id = Some(proposal_id.into());
        self
    }

    pub fn with_lifecycle_event_id(mut self, lifecycle_event_id: impl Into<String>) -> Self {
        self.lifecycle_event_id = Some(lifecycle_event_id.into());
        self
    }

    pub fn validate(&self) -> Result<()> {
        validate_outcome(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedOrigin {
    pub identity: OriginIdentity,
    pub input_fingerprint: String,
    pub prepared_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginLookup {
    Unseen,
    Prepared(PreparedOrigin),
    Replay(OriginOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginPreparation {
    Acquired,
    Pending(PreparedOrigin),
    Replay(OriginOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordLineageKind {
    Renewal,
    SessionSuccessor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordLineage {
    pub kind: RecordLineageKind,
    pub predecessor_id: String,
}

impl RecordLineage {
    pub fn validate(&self) -> Result<()> {
        require_non_empty("predecessor_id", &self.predecessor_id)
    }
}

/// Produces a route-bound, domain-separated digest of canonical typed input.
pub fn origin_input_fingerprint<T>(route: OriginRoute, value: &T) -> Result<String>
where
    T: Serialize,
{
    let canonical =
        serde_json_canonicalizer::to_vec(value).context("failed to canonicalize origin input")?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(ORIGIN_FINGERPRINT_DOMAIN);
    hasher.update(route.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(&canonical);
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn lookup_origin(
    conn: &Connection,
    identity: &OriginIdentity,
    input_fingerprint: &str,
) -> Result<OriginLookup> {
    validate_identity(identity)?;
    validate_fingerprint(input_fingerprint)?;

    let stored = conn
        .query_row(
            "SELECT route, input_fingerprint, state, outcome_kind,
                    destination, record_id, proposal_id, lifecycle_event_id,
                    prepared_at, recorded_at
             FROM origin_outcome
             WHERE repository_key = ?1 AND origin_key = ?2",
            params![identity.repository_key, identity.origin_key],
            |row| {
                Ok(StoredOrigin {
                    route: row.get(0)?,
                    input_fingerprint: row.get(1)?,
                    state: row.get(2)?,
                    outcome_kind: row.get(3)?,
                    destination: row.get(4)?,
                    record_id: row.get(5)?,
                    proposal_id: row.get(6)?,
                    lifecycle_event_id: row.get(7)?,
                    prepared_at: row.get(8)?,
                    recorded_at: row.get(9)?,
                })
            },
        )
        .optional()
        .context("failed to look up origin outcome")?;

    let Some(stored) = stored else {
        return Ok(OriginLookup::Unseen);
    };
    if stored.route != identity.route.as_str() || stored.input_fingerprint != input_fingerprint {
        bail!(
            "origin_reuse_mismatch: origin key {:?} is already bound to different typed input",
            identity.origin_key
        );
    }

    match stored.state.as_str() {
        "prepared" => Ok(OriginLookup::Prepared(PreparedOrigin {
            identity: identity.clone(),
            input_fingerprint: stored.input_fingerprint,
            prepared_at: stored.prepared_at,
        })),
        "finalized" => Ok(OriginLookup::Replay(stored.into_outcome(identity.clone())?)),
        other => bail!(
            "invalid origin registry state {other:?} for origin {:?}",
            identity.origin_key
        ),
    }
}

/// Claims an unseen origin in the caller's transaction before admission checks.
pub fn prepare_origin(
    conn: &Connection,
    identity: &OriginIdentity,
    input_fingerprint: &str,
    prepared_at: &str,
) -> Result<OriginPreparation> {
    validate_identity(identity)?;
    validate_fingerprint(input_fingerprint)?;
    validate_timestamp("prepared_at", prepared_at)?;

    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO origin_outcome(
               repository_key, origin_key, route, input_fingerprint,
               state, prepared_at
             ) VALUES (?1, ?2, ?3, ?4, 'prepared', ?5)",
            params![
                identity.repository_key,
                identity.origin_key,
                identity.route.as_str(),
                input_fingerprint,
                prepared_at,
            ],
        )
        .context("failed to prepare origin outcome")?;
    if inserted == 1 {
        return Ok(OriginPreparation::Acquired);
    }

    match lookup_origin(conn, identity, input_fingerprint)? {
        OriginLookup::Prepared(prepared) => Ok(OriginPreparation::Pending(prepared)),
        OriginLookup::Replay(outcome) => Ok(OriginPreparation::Replay(outcome)),
        OriginLookup::Unseen => bail!("origin registry changed while preparing an outcome"),
    }
}

/// Finalizes a previously prepared origin. An already-finalized origin always
/// returns its recorded outcome rather than allowing history to be rewritten.
pub fn finalize_origin(conn: &Connection, outcome: &OriginOutcome) -> Result<OriginOutcome> {
    validate_outcome(outcome)?;

    match lookup_origin(conn, &outcome.identity, &outcome.input_fingerprint)? {
        OriginLookup::Unseen => bail!(
            "origin_not_prepared: origin key {:?} has no prepared outcome",
            outcome.identity.origin_key
        ),
        OriginLookup::Replay(recorded) => return Ok(recorded),
        OriginLookup::Prepared(_) => {}
    }

    let updated = conn
        .execute(
            "UPDATE origin_outcome
             SET state = 'finalized', outcome_kind = ?1, destination = ?2,
                 record_id = ?3, proposal_id = ?4, lifecycle_event_id = ?5,
                 recorded_at = ?6
             WHERE repository_key = ?7 AND origin_key = ?8 AND state = 'prepared'",
            params![
                outcome.outcome.as_str(),
                outcome.destination.map(MemoryDestination::as_str),
                outcome.record_id,
                outcome.proposal_id,
                outcome.lifecycle_event_id,
                outcome.recorded_at,
                outcome.identity.repository_key,
                outcome.identity.origin_key,
            ],
        )
        .context("failed to finalize origin outcome")?;
    if updated == 1 {
        return Ok(outcome.clone());
    }

    match lookup_origin(conn, &outcome.identity, &outcome.input_fingerprint)? {
        OriginLookup::Replay(recorded) => Ok(recorded),
        _ => bail!("origin registry changed while finalizing an outcome"),
    }
}

/// Records an outcome directly in the caller's transaction.
///
/// Repository file workflows should use `prepare_origin` and `finalize_origin`
/// separately around their journal/recovery boundary.
pub fn record_origin_outcome(conn: &Connection, outcome: &OriginOutcome) -> Result<OriginOutcome> {
    validate_outcome(outcome)?;
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO origin_outcome(
               repository_key, origin_key, route, input_fingerprint,
               state, outcome_kind, destination, record_id, proposal_id,
               lifecycle_event_id, prepared_at, recorded_at
             ) VALUES (?1, ?2, ?3, ?4, 'finalized', ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![
                outcome.identity.repository_key,
                outcome.identity.origin_key,
                outcome.identity.route.as_str(),
                outcome.input_fingerprint,
                outcome.outcome.as_str(),
                outcome.destination.map(MemoryDestination::as_str),
                outcome.record_id,
                outcome.proposal_id,
                outcome.lifecycle_event_id,
                outcome.recorded_at,
            ],
        )
        .context("failed to record origin outcome")?;
    if inserted == 1 {
        return Ok(outcome.clone());
    }

    match lookup_origin(conn, &outcome.identity, &outcome.input_fingerprint)? {
        OriginLookup::Replay(recorded) => Ok(recorded),
        OriginLookup::Prepared(_) => bail!(
            "origin_operation_pending: origin key {:?} already has a prepared outcome",
            outcome.identity.origin_key
        ),
        OriginLookup::Unseen => bail!("origin registry changed while recording an outcome"),
    }
}

pub(crate) fn finalized_origin_outcomes(
    conn: &Connection,
    repository_key: &str,
) -> Result<Vec<OriginOutcome>> {
    require_non_empty("repository_key", repository_key)?;
    let mut statement = conn.prepare(
        "SELECT origin_key, route, input_fingerprint
         FROM origin_outcome
         WHERE repository_key = ?1 AND state = 'finalized'
         ORDER BY origin_key",
    )?;
    let rows = statement.query_map([repository_key], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut outcomes = Vec::new();
    for row in rows {
        let (origin_key, route, fingerprint) = row?;
        let identity = OriginIdentity {
            repository_key: repository_key.to_owned(),
            origin_key,
            route: route.parse()?,
        };
        match lookup_origin(conn, &identity, &fingerprint)? {
            OriginLookup::Replay(outcome) => outcomes.push(outcome),
            OriginLookup::Prepared(_) | OriginLookup::Unseen => {
                bail!("finalized origin query returned a non-finalized row")
            }
        }
    }
    Ok(outcomes)
}

fn validate_identity(identity: &OriginIdentity) -> Result<()> {
    require_non_empty("repository_key", &identity.repository_key)?;
    identity.descriptor().validate()
}

fn validate_fingerprint(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("origin input fingerprint must be 64 lowercase hexadecimal characters");
    }
    Ok(())
}

fn validate_outcome(outcome: &OriginOutcome) -> Result<()> {
    validate_identity(&outcome.identity)?;
    validate_fingerprint(&outcome.input_fingerprint)?;
    validate_timestamp("recorded_at", &outcome.recorded_at)?;
    for (label, value) in [
        ("record_id", outcome.record_id.as_deref()),
        ("proposal_id", outcome.proposal_id.as_deref()),
        ("lifecycle_event_id", outcome.lifecycle_event_id.as_deref()),
    ] {
        if let Some(value) = value {
            require_non_empty(label, value)?;
        }
    }
    Ok(())
}

fn require_non_empty(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{label} must not be empty");
    }
    Ok(())
}

fn validate_timestamp(label: &str, value: &str) -> Result<()> {
    require_non_empty(label, value)?;
    OffsetDateTime::parse(value, &Rfc3339)
        .with_context(|| format!("{label} must be an RFC 3339 timestamp with a timezone"))?;
    Ok(())
}

struct StoredOrigin {
    route: String,
    input_fingerprint: String,
    state: String,
    outcome_kind: Option<String>,
    destination: Option<String>,
    record_id: Option<String>,
    proposal_id: Option<String>,
    lifecycle_event_id: Option<String>,
    prepared_at: String,
    recorded_at: Option<String>,
}

impl StoredOrigin {
    fn into_outcome(self, identity: OriginIdentity) -> Result<OriginOutcome> {
        let outcome = self
            .outcome_kind
            .context("finalized origin outcome is missing outcome_kind")?
            .parse()?;
        let destination = self
            .destination
            .map(|value| {
                MemoryDestination::from_str(&value)
                    .map_err(|error| anyhow::anyhow!("invalid origin destination: {error}"))
            })
            .transpose()?;
        let recorded_at = self
            .recorded_at
            .context("finalized origin outcome is missing recorded_at")?;
        Ok(OriginOutcome {
            identity,
            input_fingerprint: self.input_fingerprint,
            outcome,
            destination,
            record_id: self.record_id,
            proposal_id: self.proposal_id,
            lifecycle_event_id: self.lifecycle_event_id,
            recorded_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use serde::Serialize;

    use super::*;
    use crate::init_database;

    #[derive(Serialize)]
    struct FixtureInput<'a> {
        title: &'a str,
        count: u32,
    }

    fn identity(repository_key: &str, origin_key: &str) -> OriginIdentity {
        OriginIdentity::new(
            repository_key,
            OriginDescriptor::new(origin_key, OriginRoute::CheckpointClose),
        )
    }

    fn initialized() -> anyhow::Result<Connection> {
        let conn = Connection::open_in_memory()?;
        init_database(&conn)?;
        Ok(conn)
    }

    #[test]
    fn fingerprint_is_canonical_route_bound_and_domain_separated() -> anyhow::Result<()> {
        let typed = FixtureInput {
            title: "same",
            count: 2,
        };
        let reordered = serde_json::json!({"count": 2, "title": "same"});

        let first = origin_input_fingerprint(OriginRoute::Capture, &typed)?;
        let second = origin_input_fingerprint(OriginRoute::Capture, &reordered)?;
        let other_route = origin_input_fingerprint(OriginRoute::Import, &typed)?;

        assert_eq!(first, second);
        assert_ne!(first, other_route);
        assert_eq!(first.len(), 64);
        Ok(())
    }

    #[test]
    fn portable_descriptor_excludes_the_local_repository_authority() -> anyhow::Result<()> {
        let identity = identity("private-local-repository-key", "event-1");
        let portable = serde_json::to_value(identity.descriptor())?;

        assert_eq!(portable["origin_key"], "event-1");
        assert_eq!(portable["route"], "checkpoint_close");
        assert!(portable.get("repository_key").is_none());
        Ok(())
    }

    #[test]
    fn exact_origin_replays_but_changed_input_fails_closed() -> anyhow::Result<()> {
        let conn = initialized()?;
        let identity = identity("repo-a", "event-1");
        let fingerprint = origin_input_fingerprint(
            identity.route,
            &FixtureInput {
                title: "first",
                count: 1,
            },
        )?;
        let outcome = OriginOutcome::new(
            identity.clone(),
            &fingerprint,
            OriginOutcomeKind::Created,
            "2026-07-18T10:00:00Z",
        )
        .with_destination(MemoryDestination::Session)
        .with_record_id("checkpoint-1");

        assert_eq!(record_origin_outcome(&conn, &outcome)?, outcome);
        assert_eq!(
            lookup_origin(&conn, &identity, &fingerprint)?,
            OriginLookup::Replay(outcome.clone())
        );
        assert_eq!(record_origin_outcome(&conn, &outcome)?, outcome);

        let changed = origin_input_fingerprint(
            identity.route,
            &FixtureInput {
                title: "changed",
                count: 1,
            },
        )?;
        let error = lookup_origin(&conn, &identity, &changed).unwrap_err();
        assert!(format!("{error:#}").contains("origin_reuse_mismatch"));
        let prepare_error =
            prepare_origin(&conn, &identity, &changed, "2026-07-18T10:01:00Z").unwrap_err();
        assert!(format!("{prepare_error:#}").contains("origin_reuse_mismatch"));
        let stored_rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM origin_outcome", [], |row| row.get(0))?;
        assert_eq!(stored_rows, 1, "mismatch must perform zero writes");
        Ok(())
    }

    #[test]
    fn uniqueness_crosses_destinations_but_not_repository_authorities() -> anyhow::Result<()> {
        let conn = initialized()?;
        let repo_a = identity("repo-a", "shared-event");
        let repo_b = identity("repo-b", "shared-event");
        let fingerprint = origin_input_fingerprint(repo_a.route, &"payload")?;
        let local = OriginOutcome::new(
            repo_a.clone(),
            &fingerprint,
            OriginOutcomeKind::Created,
            "2026-07-18T10:00:00Z",
        )
        .with_destination(MemoryDestination::Local)
        .with_record_id("local-1");
        record_origin_outcome(&conn, &local)?;

        let attempted_repo = OriginOutcome::new(
            repo_a.clone(),
            &fingerprint,
            OriginOutcomeKind::Created,
            "2026-07-18T10:01:00Z",
        )
        .with_destination(MemoryDestination::Repo)
        .with_record_id("repo-1");
        assert_eq!(record_origin_outcome(&conn, &attempted_repo)?, local);

        let independent = OriginOutcome::new(
            repo_b,
            &fingerprint,
            OriginOutcomeKind::Created,
            "2026-07-18T10:02:00Z",
        )
        .with_destination(MemoryDestination::Repo)
        .with_record_id("repo-2");
        assert_eq!(record_origin_outcome(&conn, &independent)?, independent);
        Ok(())
    }

    #[test]
    fn preparation_and_finalization_follow_the_callers_transaction() -> anyhow::Result<()> {
        let mut conn = initialized()?;
        let operation_identity = identity("repo-a", "transactional-event");
        let fingerprint = origin_input_fingerprint(operation_identity.route, &"payload")?;

        {
            let tx = conn.transaction()?;
            assert_eq!(
                prepare_origin(
                    &tx,
                    &operation_identity,
                    &fingerprint,
                    "2026-07-18T10:00:00Z"
                )?,
                OriginPreparation::Acquired
            );
            tx.rollback()?;
        }
        assert_eq!(
            lookup_origin(&conn, &operation_identity, &fingerprint)?,
            OriginLookup::Unseen
        );

        let outcome = OriginOutcome::new(
            operation_identity.clone(),
            &fingerprint,
            OriginOutcomeKind::RejectedNoWrite,
            "2026-07-18T10:01:00Z",
        );
        {
            let tx = conn.transaction()?;
            assert_eq!(
                prepare_origin(
                    &tx,
                    &operation_identity,
                    &fingerprint,
                    "2026-07-18T10:01:00Z"
                )?,
                OriginPreparation::Acquired
            );
            assert_eq!(finalize_origin(&tx, &outcome)?, outcome);
            tx.commit()?;
        }
        assert_eq!(
            lookup_origin(&conn, &operation_identity, &fingerprint)?,
            OriginLookup::Replay(outcome)
        );

        let rolled_back_identity = identity("repo-a", "rolled-back-final-outcome");
        let rolled_back_fingerprint =
            origin_input_fingerprint(rolled_back_identity.route, &"rolled back")?;
        let rolled_back_outcome = OriginOutcome::new(
            rolled_back_identity.clone(),
            &rolled_back_fingerprint,
            OriginOutcomeKind::ExistingDuplicateNoWrite,
            "2026-07-18T10:02:00Z",
        );
        {
            let tx = conn.transaction()?;
            record_origin_outcome(&tx, &rolled_back_outcome)?;
            tx.rollback()?;
        }
        assert_eq!(
            lookup_origin(&conn, &rolled_back_identity, &rolled_back_fingerprint)?,
            OriginLookup::Unseen
        );
        Ok(())
    }

    #[test]
    fn prepared_entries_are_content_free_and_visible_to_recovery() -> anyhow::Result<()> {
        let conn = initialized()?;
        let identity = identity("repo-a", "prepared-event");
        let fingerprint = origin_input_fingerprint(identity.route, &"payload")?;
        assert_eq!(
            prepare_origin(&conn, &identity, &fingerprint, "2026-07-18T10:00:00Z")?,
            OriginPreparation::Acquired
        );

        let pending = match prepare_origin(&conn, &identity, &fingerprint, "2026-07-18T10:05:00Z")?
        {
            OriginPreparation::Pending(pending) => pending,
            other => panic!("expected pending preparation, got {other:?}"),
        };
        assert_eq!(pending.prepared_at, "2026-07-18T10:00:00Z");

        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(origin_outcome)")?
            .query_map([], |row| row.get(1))?
            .collect::<rusqlite::Result<_>>()?;
        for prohibited in ["title", "body", "transcript", "evidence", "source_material"] {
            assert!(!columns.iter().any(|column| column == prohibited));
        }
        Ok(())
    }

    #[test]
    fn erased_is_a_content_free_replay_barrier() -> anyhow::Result<()> {
        let conn = initialized()?;
        let identity = identity("repo-a", "erased-event");
        let fingerprint = origin_input_fingerprint(identity.route, &"payload")?;
        let erased = OriginOutcome::new(
            identity.clone(),
            &fingerprint,
            OriginOutcomeKind::Erased,
            "2026-07-18T10:00:00Z",
        )
        .with_record_id("removed-record")
        .with_lifecycle_event_id("erasure-event");
        record_origin_outcome(&conn, &erased)?;

        assert_eq!(
            lookup_origin(&conn, &identity, &fingerprint)?,
            OriginLookup::Replay(erased)
        );
        Ok(())
    }
}
