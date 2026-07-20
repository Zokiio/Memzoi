use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, functions::FunctionFlags};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use crate::models::{MemoryLane, MemoryStatus};

pub const SQL_RETENTION_STATE: &str = "memzoi_retention_state";
pub const SQL_PRIVATE_CURRENT_ASSERTION: &str = "memzoi_private_current_assertion";

const SESSION_INACTIVITY_HOURS: i64 = 24;
const SESSION_MAXIMUM_DAYS: i64 = 7;
const EPISODIC_ORDINARY_DAYS: i64 = 30;
const EPISODIC_MAXIMUM_RECALL_DAYS: i64 = 90;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionFacts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_continued_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explicit_expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episodic_extension: Option<EpisodicRetentionExtension>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpisodicRetentionExtension {
    pub until: String,
    pub authorization_event_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionDecision {
    pub state: RetentionState,
    pub effective_boundary: Option<String>,
    pub reason: RetentionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionState {
    Current,
    QueryOnly,
}

impl RetentionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::QueryOnly => "query_only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionReason {
    NoAgeLimit,
    ExplicitExpiry,
    SessionClosed,
    SessionInactivityLease,
    SessionMaximumAge,
    EpisodicOrdinaryWindow,
    EpisodicAuthorizedExtension,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentAssertionDecision {
    pub is_current: bool,
    pub retention: RetentionDecision,
    pub exclusions: Vec<CurrentAssertionExclusion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CurrentAssertionExclusion {
    LifecycleStatus { status: MemoryStatus },
    Retention { reason: RetentionReason },
    Applicability { reason: String },
    UnresolvedConflict,
    Quarantine { reason: String },
    Safety { reason: String },
}

/// Evaluate only the current lane-retention policy.
///
/// Invalid facts are errors rather than a third retention state. Every error
/// includes `record_id` so a caller can identify the artifact that must be
/// upgraded or removed.
pub fn evaluate_retention(
    record_id: &str,
    lane: MemoryLane,
    facts: &RetentionFacts,
    evaluated_at: OffsetDateTime,
) -> Result<RetentionDecision> {
    evaluate_retention_inner(lane, facts, evaluated_at)
        .with_context(|| format!("record {record_id} has invalid retention facts"))
}

/// Compose retention with every currently-known ordinary-use exclusion.
///
/// #119 supplies lifecycle status and retention. Later policies contribute
/// applicability, conflict, quarantine, and safety exclusions through the
/// same boundary instead of changing the meaning of retention.
pub fn evaluate_current_assertion(
    record_id: &str,
    status: MemoryStatus,
    lane: MemoryLane,
    facts: &RetentionFacts,
    evaluated_at: OffsetDateTime,
    additional_exclusions: Vec<CurrentAssertionExclusion>,
) -> Result<CurrentAssertionDecision> {
    let retention = evaluate_retention(record_id, lane, facts, evaluated_at)?;
    let mut exclusions = Vec::with_capacity(additional_exclusions.len() + 2);
    if status != MemoryStatus::Active {
        exclusions.push(CurrentAssertionExclusion::LifecycleStatus { status });
    }
    if retention.state == RetentionState::QueryOnly {
        exclusions.push(CurrentAssertionExclusion::Retention {
            reason: retention.reason,
        });
    }
    exclusions.extend(additional_exclusions);

    Ok(CurrentAssertionDecision {
        is_current: exclusions.is_empty(),
        retention,
        exclusions,
    })
}

/// Construct complete current-format retention facts at a creation boundary.
///
/// `occurred_at` is meaningful only for episodic records; when it is absent,
/// the service clock timestamp is the occurrence. Session start always comes
/// from the service clock so callers cannot manufacture an older lease.
pub fn retention_facts_for_creation(
    lane: MemoryLane,
    created_at: &str,
    occurred_at: Option<&str>,
    explicit_expires_at: Option<&str>,
) -> Result<RetentionFacts> {
    parse_timestamp(created_at, "created_at")?;
    if let Some(value) = explicit_expires_at {
        parse_timestamp(value, "explicit_expires_at")?;
    }
    let mut facts = RetentionFacts {
        occurred_at: None,
        started_at: None,
        last_continued_at: None,
        closed_at: None,
        explicit_expires_at: explicit_expires_at.map(str::to_owned),
        episodic_extension: None,
    };
    match lane {
        MemoryLane::Session => {
            ensure!(
                occurred_at.is_none(),
                "occurred_at is not valid for the session lane"
            );
            facts.started_at = Some(created_at.to_owned());
        }
        MemoryLane::Episodic => {
            let occurred_at = occurred_at.unwrap_or(created_at);
            parse_timestamp(occurred_at, "occurred_at")?;
            facts.occurred_at = Some(occurred_at.to_owned());
        }
        MemoryLane::Semantic | MemoryLane::Procedural => {
            ensure!(
                occurred_at.is_none(),
                "occurred_at is not valid for the {} lane",
                lane.as_str()
            );
        }
    }
    // Reuse the policy validator so creation cannot emit a shape that an
    // ordinary read would later reject.
    evaluate_retention(
        "<new-record>",
        lane,
        &facts,
        parse_timestamp(created_at, "created_at")?,
    )?;
    Ok(facts)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PrivateLifecycleReadFacts<'a> {
    pub automatic_recall_until: Option<&'a str>,
    pub validity_until: Option<&'a str>,
    pub automatic_recall_event_id: Option<&'a str>,
    pub validity_event_id: Option<&'a str>,
    pub quarantined: bool,
    pub quarantine_reason: Option<&'a str>,
}

/// Evaluate private ordinary-use eligibility without conflating the recall,
/// validity, physical-retention, and quarantine clocks.
///
/// Only lifecycle facts paired with an authorizing event are accepted. The
/// persisted `retain_until` and indefinite-pin facts intentionally do not
/// participate in this boundary: they govern physical retention, not recall.
pub(crate) fn evaluate_private_current_assertion(
    record_id: &str,
    lane: MemoryLane,
    facts: &RetentionFacts,
    evaluated_at: OffsetDateTime,
    lifecycle: PrivateLifecycleReadFacts<'_>,
) -> Result<bool> {
    Ok(evaluate_private_current_assertion_decision(
        record_id,
        MemoryStatus::Active,
        lane,
        facts,
        evaluated_at,
        lifecycle,
    )?
    .is_current)
}

/// Return the complete effective decision used by private planning and action
/// revalidation. The retention field reflects the authorized recall/validity
/// boundary rather than the superseded base boundary.
pub(crate) fn evaluate_private_current_assertion_decision(
    record_id: &str,
    status: MemoryStatus,
    lane: MemoryLane,
    facts: &RetentionFacts,
    evaluated_at: OffsetDateTime,
    lifecycle: PrivateLifecycleReadFacts<'_>,
) -> Result<CurrentAssertionDecision> {
    // Validate the complete base facts even when quarantine would exclude the
    // record. Invalid current-schema data must fail closed rather than merely
    // disappearing from one read route.
    let base = evaluate_retention(record_id, lane, facts, evaluated_at)?;
    let automatic_recall_until = verified_lifecycle_boundary(
        lifecycle.automatic_recall_until,
        lifecycle.automatic_recall_event_id,
        "automatic_recall_until",
    )?;
    let validity_until = verified_lifecycle_boundary(
        lifecycle.validity_until,
        lifecycle.validity_event_id,
        "validity_until",
    )?;
    let retention = match lane {
        MemoryLane::Session => {
            ensure!(
                automatic_recall_until.is_none(),
                "automatic_recall_until is not valid for the session lane"
            );
            ensure!(
                validity_until.is_none(),
                "validity_until is not valid for the session lane"
            );
            base
        }
        MemoryLane::Episodic => {
            let occurred = required_timestamp(
                facts.occurred_at.as_deref(),
                "occurred_at",
                MemoryLane::Episodic,
            )?;
            let ordinary_recall = add_duration(
                occurred,
                Duration::days(EPISODIC_ORDINARY_DAYS),
                "ordinary episodic window",
            )?;
            let maximum_recall = add_duration(
                occurred,
                Duration::days(EPISODIC_MAXIMUM_RECALL_DAYS),
                "maximum episodic recall window",
            )?;
            let recall_boundary = match automatic_recall_until {
                Some(at) => {
                    ensure!(
                        at > ordinary_recall,
                        "automatic_recall_until must extend the ordinary episodic window"
                    );
                    ensure!(
                        at <= maximum_recall,
                        "automatic_recall_until exceeds the 90-day episodic recall cap"
                    );
                    Boundary {
                        at,
                        reason: RetentionReason::EpisodicAuthorizedExtension,
                    }
                }
                None => Boundary {
                    at: ordinary_recall,
                    reason: RetentionReason::EpisodicOrdinaryWindow,
                },
            };
            let validity_boundary =
                extended_validity_boundary(facts, validity_until, lane)?.map(|at| Boundary {
                    at,
                    reason: RetentionReason::ExplicitExpiry,
                });
            decision_from_boundary(
                Some(match validity_boundary {
                    Some(validity) if validity.at < recall_boundary.at => validity,
                    _ => recall_boundary,
                }),
                evaluated_at,
            )?
        }
        MemoryLane::Semantic | MemoryLane::Procedural => {
            ensure!(
                automatic_recall_until.is_none(),
                "automatic_recall_until is only valid for the episodic lane"
            );
            let validity_boundary =
                extended_validity_boundary(facts, validity_until, lane)?.map(|at| Boundary {
                    at,
                    reason: RetentionReason::ExplicitExpiry,
                });
            decision_from_boundary(validity_boundary, evaluated_at)?
        }
    };

    let mut exclusions = Vec::with_capacity(3);
    if status != MemoryStatus::Active {
        exclusions.push(CurrentAssertionExclusion::LifecycleStatus { status });
    }
    if retention.state == RetentionState::QueryOnly {
        exclusions.push(CurrentAssertionExclusion::Retention {
            reason: retention.reason,
        });
    }
    match (lifecycle.quarantined, lifecycle.quarantine_reason) {
        (true, Some(reason)) if !reason.trim().is_empty() => {
            exclusions.push(CurrentAssertionExclusion::Quarantine {
                reason: reason.to_owned(),
            });
        }
        (true, _) => bail!("quarantined private record has no reason"),
        (false, None) => {}
        (false, Some(_)) => bail!("non-quarantined private record has a quarantine reason"),
    }

    Ok(CurrentAssertionDecision {
        is_current: exclusions.is_empty(),
        retention,
        exclusions,
    })
}

fn verified_lifecycle_boundary(
    boundary: Option<&str>,
    event_id: Option<&str>,
    field: &str,
) -> Result<Option<OffsetDateTime>> {
    match (boundary, event_id) {
        (None, None) => Ok(None),
        (Some(boundary), Some(event_id)) => {
            ensure!(
                !event_id.trim().is_empty(),
                "{field} authorizing event ID cannot be empty"
            );
            Ok(Some(parse_timestamp(boundary, field)?))
        }
        (Some(_), None) => bail!("{field} is missing its authorizing event ID"),
        (None, Some(_)) => bail!("{field} authorizing event ID has no lifecycle boundary"),
    }
}

fn extended_validity_boundary(
    facts: &RetentionFacts,
    lifecycle_boundary: Option<OffsetDateTime>,
    lane: MemoryLane,
) -> Result<Option<OffsetDateTime>> {
    let base = facts
        .explicit_expires_at
        .as_deref()
        .map(|value| parse_timestamp(value, "explicit_expires_at"))
        .transpose()?;
    match lifecycle_boundary {
        Some(until) => {
            let base = base.with_context(|| {
                format!("validity_until requires an existing finite boundary for the {lane} lane")
            })?;
            ensure!(
                until > base,
                "validity_until must extend the existing finite boundary"
            );
            Ok(Some(until))
        }
        None => Ok(base),
    }
}

/// Build the shared SQL boundary for ordinary-use eligibility.
///
/// Every SQL-backed recall, admission, duplicate, and conflict route must use
/// this complete boundary rather than composing status and retention clauses
/// independently. Later lifecycle policies can extend this one builder.
fn base_current_assertion_sql(table_alias: &str, evaluated_at_parameter: &str) -> String {
    format!(
        "{table_alias}.status = 'active'\n         AND CASE\n               WHEN {table_alias}.destination = 'repo' THEN\n                 memzoi_retention_state(\n                   {table_alias}.id,\n                   {table_alias}.lane,\n                   {table_alias}.retention_json,\n                   {evaluated_at_parameter}\n                 ) = 'current'\n               WHEN {table_alias}.destination IN ('local', 'session') THEN EXISTS (\n                 SELECT 1\n                 FROM private_lifecycle_state AS private_lifecycle\n                 WHERE private_lifecycle.record_id = {table_alias}.id\n                   AND (\n                     private_lifecycle.automatic_recall_until IS NULL\n                     OR EXISTS (\n                       SELECT 1 FROM event_log AS recall_authority\n                       WHERE recall_authority.id = private_lifecycle.automatic_recall_event_id\n                         AND recall_authority.event_type = 'memory.private_lifecycle_applied'\n                         AND EXISTS (\n                           SELECT 1\n                           FROM json_each(recall_authority.payload_json, '$.target_record_ids') AS recall_target\n                           WHERE recall_target.type = 'text'\n                             AND recall_target.value = {table_alias}.id\n                         )\n                         AND EXISTS (\n                           SELECT 1\n                           FROM json_each(recall_authority.payload_json, '$.action_kinds') AS recall_action\n                           WHERE recall_action.type = 'text'\n                             AND recall_action.value IN ('extend_automatic_recall', 'correct')\n                         )\n                     )\n                   )\n                   AND (\n                     private_lifecycle.validity_until IS NULL\n                     OR EXISTS (\n                       SELECT 1 FROM event_log AS validity_authority\n                       WHERE validity_authority.id = private_lifecycle.validity_event_id\n                         AND validity_authority.event_type = 'memory.private_lifecycle_applied'\n                         AND EXISTS (\n                           SELECT 1\n                           FROM json_each(validity_authority.payload_json, '$.target_record_ids') AS validity_target\n                           WHERE validity_target.type = 'text'\n                             AND validity_target.value = {table_alias}.id\n                         )\n                         AND EXISTS (\n                           SELECT 1\n                           FROM json_each(validity_authority.payload_json, '$.action_kinds') AS validity_action\n                           WHERE validity_action.type = 'text'\n                             AND validity_action.value IN ('extend_validity', 'correct')\n                         )\n                     )\n                   )\n                   AND (\n                     private_lifecycle.quarantined = 0\n                     OR EXISTS (\n                       SELECT 1 FROM event_log AS quarantine_authority\n                       WHERE quarantine_authority.id = private_lifecycle.quarantine_event_id\n                         AND quarantine_authority.event_type = 'memory.private_lifecycle_applied'\n                         AND EXISTS (\n                           SELECT 1\n                           FROM json_each(quarantine_authority.payload_json, '$.target_record_ids') AS quarantine_target\n                           WHERE quarantine_target.type = 'text'\n                             AND quarantine_target.value = {table_alias}.id\n                         )\n                         AND EXISTS (\n                           SELECT 1\n                           FROM json_each(quarantine_authority.payload_json, '$.action_kinds') AS quarantine_action\n                           WHERE quarantine_action.type = 'text'\n                             AND quarantine_action.value IN ('quarantine', 'correct')\n                         )\n                     )\n                   )\n                   AND memzoi_private_current_assertion(\n                         {table_alias}.id,\n                         {table_alias}.lane,\n                         {table_alias}.retention_json,\n                         private_lifecycle.automatic_recall_until,\n                         private_lifecycle.validity_until,\n                         private_lifecycle.automatic_recall_event_id,\n                         private_lifecycle.validity_event_id,\n                         private_lifecycle.quarantined,\n                         private_lifecycle.quarantine_reason_code,\n                         {evaluated_at_parameter}\n                       ) = 1\n               )\n               ELSE 0\n             END"
    )
}

/// Build effective ordinary-use eligibility from the stable base decision and
/// the current derived conflict projection. Reconciliation deliberately calls
/// the base evaluator above, so suppression can never erase its own evidence.
pub(crate) fn current_assertion_sql(table_alias: &str, evaluated_at_parameter: &str) -> String {
    let base = base_current_assertion_sql(table_alias, evaluated_at_parameter);
    let other_base = base_current_assertion_sql("conflict_other_record", evaluated_at_parameter);
    let policy_version = crate::MAINTENANCE_POLICY_VERSION;
    format!(
        "({base})\n         AND (\n           {table_alias}.destination = 'repo'\n           OR (\n             EXISTS (\n               SELECT 1\n               FROM private_maintenance_projection AS recall_projection\n               CROSS JOIN private_lifecycle_generation AS recall_generation\n               WHERE recall_projection.singleton = 1\n                 AND recall_generation.singleton = 1\n                 AND (\n                   recall_projection.state = 'disabled'\n                   OR (\n                     recall_projection.state = 'current'\n                     AND recall_projection.authoritative_generation = recall_generation.generation\n                     AND recall_projection.policy_version = '{policy_version}'\n                     AND {evaluated_at_parameter} < recall_projection.not_after\n                   )\n                 )\n             )\n             AND NOT EXISTS (\n               SELECT 1\n               FROM private_maintenance_projection AS conflict_projection\n               JOIN private_conflict_set AS conflict_set\n                 ON conflict_set.projection_id = conflict_projection.projection_id\n                AND conflict_set.grant_fingerprint = conflict_projection.grant_fingerprint\n                AND conflict_set.policy_version = conflict_projection.policy_version\n               JOIN private_conflict_edge AS conflict_edge\n                 ON conflict_edge.conflict_id = conflict_set.conflict_id\n               JOIN private_conflict_member AS conflict_self\n                 ON conflict_self.conflict_id = conflict_set.conflict_id\n                AND conflict_self.record_id = {table_alias}.id\n               JOIN private_lifecycle_state AS conflict_self_lifecycle\n                 ON conflict_self_lifecycle.record_id = conflict_self.record_id\n                AND conflict_self_lifecycle.record_version = conflict_self.record_version\n               JOIN private_conflict_member AS conflict_other\n                 ON conflict_other.conflict_id = conflict_set.conflict_id\n                AND conflict_other.record_id = CASE\n                  WHEN conflict_edge.left_record_id = {table_alias}.id\n                    THEN conflict_edge.right_record_id\n                  ELSE conflict_edge.left_record_id\n                END\n               JOIN memory_record AS conflict_other_record\n                 ON conflict_other_record.id = conflict_other.record_id\n               JOIN private_lifecycle_state AS conflict_other_lifecycle\n                 ON conflict_other_lifecycle.record_id = conflict_other.record_id\n                AND conflict_other_lifecycle.record_version = conflict_other.record_version\n               CROSS JOIN private_lifecycle_generation AS conflict_generation\n               WHERE conflict_projection.singleton = 1\n                 AND conflict_generation.singleton = 1\n                 AND conflict_projection.state = 'current'\n                 AND conflict_projection.authoritative_generation = conflict_generation.generation\n                 AND conflict_projection.policy_version = '{policy_version}'\n                 AND {evaluated_at_parameter} < conflict_projection.not_after\n                 AND (\n                   conflict_edge.left_record_id = {table_alias}.id\n                   OR conflict_edge.right_record_id = {table_alias}.id\n                 )\n                 AND ({other_base})\n             )\n           )\n         )"
    )
}

pub(crate) fn register_sqlite_functions(conn: &Connection) -> Result<()> {
    conn.create_scalar_function(
        SQL_RETENTION_STATE,
        4,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let record_id = context.get::<String>(0)?;
            let result = (|| -> Result<RetentionState> {
                let lane = context
                    .get::<String>(1)?
                    .parse::<MemoryLane>()
                    .map_err(anyhow::Error::msg)?;
                let retention_json = context.get::<String>(2)?;
                let facts = serde_json::from_str::<RetentionFacts>(&retention_json)
                    .context("retention_json does not match the current retention schema")?;
                let evaluated_at = parse_timestamp(&context.get::<String>(3)?, "evaluated_at")?;
                Ok(evaluate_retention(&record_id, lane, &facts, evaluated_at)?.state)
            })();

            result
                .map(|state| state.as_str().to_owned())
                .map_err(|error| {
                    rusqlite::Error::UserFunctionError(
                        anyhow::anyhow!(
                            "record {record_id} retention evaluation failed: {error:#}"
                        )
                        .into(),
                    )
                })
        },
    )?;
    conn.create_scalar_function(
        SQL_PRIVATE_CURRENT_ASSERTION,
        10,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let record_id = context.get::<String>(0)?;
            let result = (|| -> Result<bool> {
                let lane = context
                    .get::<String>(1)?
                    .parse::<MemoryLane>()
                    .map_err(anyhow::Error::msg)?;
                let retention_json = context.get::<String>(2)?;
                let facts = serde_json::from_str::<RetentionFacts>(&retention_json)
                    .context("retention_json does not match the current retention schema")?;
                let automatic_recall_until = context.get::<Option<String>>(3)?;
                let validity_until = context.get::<Option<String>>(4)?;
                let automatic_recall_event_id = context.get::<Option<String>>(5)?;
                let validity_event_id = context.get::<Option<String>>(6)?;
                let quarantined = match context.get::<i64>(7)? {
                    0 => false,
                    1 => true,
                    value => bail!("quarantined must be 0 or 1, got {value}"),
                };
                let quarantine_reason = context.get::<Option<String>>(8)?;
                let evaluated_at = parse_timestamp(&context.get::<String>(9)?, "evaluated_at")?;
                evaluate_private_current_assertion(
                    &record_id,
                    lane,
                    &facts,
                    evaluated_at,
                    PrivateLifecycleReadFacts {
                        automatic_recall_until: automatic_recall_until.as_deref(),
                        validity_until: validity_until.as_deref(),
                        automatic_recall_event_id: automatic_recall_event_id.as_deref(),
                        validity_event_id: validity_event_id.as_deref(),
                        quarantined,
                        quarantine_reason: quarantine_reason.as_deref(),
                    },
                )
            })();

            result.map(i64::from).map_err(|error| {
                rusqlite::Error::UserFunctionError(
                    anyhow::anyhow!(
                        "record {record_id} private current-assertion evaluation failed: {error:#}"
                    )
                    .into(),
                )
            })
        },
    )?;
    Ok(())
}

fn evaluate_retention_inner(
    lane: MemoryLane,
    facts: &RetentionFacts,
    evaluated_at: OffsetDateTime,
) -> Result<RetentionDecision> {
    let explicit = facts
        .explicit_expires_at
        .as_deref()
        .map(|value| parse_timestamp(value, "explicit_expires_at"))
        .transpose()?;

    let boundary = match lane {
        MemoryLane::Session => session_boundary(facts, explicit)?,
        MemoryLane::Episodic => episodic_boundary(facts, explicit)?,
        MemoryLane::Semantic | MemoryLane::Procedural => durable_boundary(lane, facts, explicit)?,
    };

    decision_from_boundary(boundary, evaluated_at)
}

#[derive(Debug, Clone, Copy)]
struct Boundary {
    at: OffsetDateTime,
    reason: RetentionReason,
}

fn decision_from_boundary(
    boundary: Option<Boundary>,
    evaluated_at: OffsetDateTime,
) -> Result<RetentionDecision> {
    let (effective_boundary, reason) = match boundary {
        Some(boundary) => (Some(format_timestamp(boundary.at)?), boundary.reason),
        None => (None, RetentionReason::NoAgeLimit),
    };
    let state = match boundary {
        Some(boundary) if evaluated_at >= boundary.at => RetentionState::QueryOnly,
        _ => RetentionState::Current,
    };
    Ok(RetentionDecision {
        state,
        effective_boundary,
        reason,
    })
}

fn session_boundary(
    facts: &RetentionFacts,
    explicit: Option<OffsetDateTime>,
) -> Result<Option<Boundary>> {
    reject_field(
        facts.occurred_at.as_ref(),
        "occurred_at",
        MemoryLane::Session,
    )?;
    reject_field(
        facts.episodic_extension.as_ref(),
        "episodic_extension",
        MemoryLane::Session,
    )?;

    let started = required_timestamp(
        facts.started_at.as_deref(),
        "started_at",
        MemoryLane::Session,
    )?;
    let continued = facts
        .last_continued_at
        .as_deref()
        .map(|value| parse_timestamp(value, "last_continued_at"))
        .transpose()?;
    let closed = facts
        .closed_at
        .as_deref()
        .map(|value| parse_timestamp(value, "closed_at"))
        .transpose()?;

    let maximum = add_duration(
        started,
        Duration::days(SESSION_MAXIMUM_DAYS),
        "session maximum age",
    )?;
    if let Some(continued) = continued {
        ensure!(
            continued >= started,
            "last_continued_at precedes started_at"
        );
        ensure!(
            continued < maximum,
            "last_continued_at is at or after the seven-day session cap"
        );
    }
    if let Some(closed) = closed {
        ensure!(closed >= started, "closed_at precedes started_at");
    }

    let lease_base = continued.unwrap_or(started);
    let lease = add_duration(
        lease_base,
        Duration::hours(SESSION_INACTIVITY_HOURS),
        "session inactivity lease",
    )?;
    let mut candidates = vec![
        Boundary {
            at: lease,
            reason: RetentionReason::SessionInactivityLease,
        },
        Boundary {
            at: maximum,
            reason: RetentionReason::SessionMaximumAge,
        },
    ];
    if let Some(closed) = closed {
        candidates.push(Boundary {
            at: closed,
            reason: RetentionReason::SessionClosed,
        });
    }
    if let Some(explicit) = explicit {
        candidates.push(Boundary {
            at: explicit,
            reason: RetentionReason::ExplicitExpiry,
        });
    }
    Ok(candidates.into_iter().min_by_key(|boundary| boundary.at))
}

fn episodic_boundary(
    facts: &RetentionFacts,
    explicit: Option<OffsetDateTime>,
) -> Result<Option<Boundary>> {
    reject_session_fields(facts, MemoryLane::Episodic)?;
    let occurred = required_timestamp(
        facts.occurred_at.as_deref(),
        "occurred_at",
        MemoryLane::Episodic,
    )?;
    let ordinary = add_duration(
        occurred,
        Duration::days(EPISODIC_ORDINARY_DAYS),
        "ordinary episodic window",
    )?;
    ensure!(
        facts.episodic_extension.is_none(),
        "episodic_extension is unsupported until an owner-authorized extension event can be verified"
    );
    let policy = Boundary {
        at: ordinary,
        reason: RetentionReason::EpisodicOrdinaryWindow,
    };

    Ok(Some(min_with_explicit(policy, explicit)))
}

fn durable_boundary(
    lane: MemoryLane,
    facts: &RetentionFacts,
    explicit: Option<OffsetDateTime>,
) -> Result<Option<Boundary>> {
    reject_field(facts.occurred_at.as_ref(), "occurred_at", lane)?;
    reject_session_fields(facts, lane)?;
    reject_field(
        facts.episodic_extension.as_ref(),
        "episodic_extension",
        lane,
    )?;
    Ok(explicit.map(|at| Boundary {
        at,
        reason: RetentionReason::ExplicitExpiry,
    }))
}

fn reject_session_fields(facts: &RetentionFacts, lane: MemoryLane) -> Result<()> {
    reject_field(facts.started_at.as_ref(), "started_at", lane)?;
    reject_field(facts.last_continued_at.as_ref(), "last_continued_at", lane)?;
    reject_field(facts.closed_at.as_ref(), "closed_at", lane)
}

fn reject_field<T>(field: Option<&T>, name: &str, lane: MemoryLane) -> Result<()> {
    ensure!(
        field.is_none(),
        "{name} is not valid for the {} lane",
        lane.as_str()
    );
    Ok(())
}

fn required_timestamp(
    value: Option<&str>,
    field: &str,
    lane: MemoryLane,
) -> Result<OffsetDateTime> {
    let value = value
        .ok_or_else(|| anyhow::anyhow!("{field} is required for the {} lane", lane.as_str()))?;
    parse_timestamp(value, field)
}

fn min_with_explicit(policy: Boundary, explicit: Option<OffsetDateTime>) -> Boundary {
    match explicit {
        Some(explicit) if explicit < policy.at => Boundary {
            at: explicit,
            reason: RetentionReason::ExplicitExpiry,
        },
        _ => policy,
    }
}

fn parse_timestamp(value: &str, field: &str) -> Result<OffsetDateTime> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{field} cannot be empty");
    }
    OffsetDateTime::parse(value, &Rfc3339)
        .with_context(|| format!("{field} must be an RFC 3339 timestamp with a timezone: {value}"))
}

fn format_timestamp(timestamp: OffsetDateTime) -> Result<String> {
    timestamp
        .to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .context("failed to format retention timestamp")
}

fn add_duration(
    timestamp: OffsetDateTime,
    duration: Duration,
    boundary_name: &str,
) -> Result<OffsetDateTime> {
    timestamp
        .checked_add(duration)
        .ok_or_else(|| anyhow::anyhow!("{boundary_name} is outside the supported timestamp range"))
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    fn instant(value: &str) -> Result<OffsetDateTime> {
        Ok(OffsetDateTime::parse(value, &Rfc3339)?)
    }

    fn facts() -> RetentionFacts {
        RetentionFacts {
            occurred_at: None,
            started_at: None,
            last_continued_at: None,
            closed_at: None,
            explicit_expires_at: None,
            episodic_extension: None,
        }
    }

    fn private_lifecycle() -> PrivateLifecycleReadFacts<'static> {
        PrivateLifecycleReadFacts {
            automatic_recall_until: None,
            validity_until: None,
            automatic_recall_event_id: None,
            validity_event_id: None,
            quarantined: false,
            quarantine_reason: None,
        }
    }

    #[test]
    fn episodic_ordinary_boundary_is_inclusive() -> Result<()> {
        let facts = RetentionFacts {
            occurred_at: Some("2026-01-01T00:00:00Z".to_owned()),
            ..facts()
        };
        let before = evaluate_retention(
            "episode-1",
            MemoryLane::Episodic,
            &facts,
            instant("2026-01-30T23:59:59.999999999Z")?,
        )?;
        let at = evaluate_retention(
            "episode-1",
            MemoryLane::Episodic,
            &facts,
            instant("2026-01-31T00:00:00Z")?,
        )?;

        assert_eq!(before.state, RetentionState::Current);
        assert_eq!(at.state, RetentionState::QueryOnly);
        assert_eq!(at.reason, RetentionReason::EpisodicOrdinaryWindow);
        assert_eq!(
            at.effective_boundary.as_deref(),
            Some("2026-01-31T00:00:00Z")
        );
        Ok(())
    }

    #[test]
    fn episodic_extension_is_rejected_without_authorization_verification() -> Result<()> {
        let facts = RetentionFacts {
            occurred_at: Some("2026-01-01T00:00:00Z".to_owned()),
            episodic_extension: Some(EpisodicRetentionExtension {
                until: "2026-03-31T00:00:00Z".to_owned(),
                authorization_event_id: "event-owner-grant".to_owned(),
            }),
            ..facts()
        };
        let error = evaluate_retention(
            "episode-unverified-extension",
            MemoryLane::Episodic,
            &facts,
            instant("2026-02-01T00:00:00Z")?,
        )
        .expect_err("unverified extensions must fail");
        let message = format!("{error:#}");
        assert!(message.contains("episode-unverified-extension"));
        assert!(message.contains("owner-authorized extension event"));
        Ok(())
    }

    #[test]
    fn explicit_expiry_shortens_episodic_window() -> Result<()> {
        let facts = RetentionFacts {
            occurred_at: Some("2026-01-01T00:00:00Z".to_owned()),
            explicit_expires_at: Some("2026-01-10T12:00:00+02:00".to_owned()),
            ..facts()
        };
        let decision = evaluate_retention(
            "episode-explicit",
            MemoryLane::Episodic,
            &facts,
            instant("2026-01-10T10:00:00Z")?,
        )?;
        assert_eq!(decision.state, RetentionState::QueryOnly);
        assert_eq!(decision.reason, RetentionReason::ExplicitExpiry);
        assert_eq!(
            decision.effective_boundary.as_deref(),
            Some("2026-01-10T10:00:00Z")
        );
        Ok(())
    }

    #[test]
    fn session_uses_latest_continuation_lease_but_never_exceeds_seven_days() -> Result<()> {
        let continued = RetentionFacts {
            started_at: Some("2026-01-01T00:00:00Z".to_owned()),
            last_continued_at: Some("2026-01-06T12:00:00Z".to_owned()),
            ..facts()
        };
        let lease = evaluate_retention(
            "session-continued",
            MemoryLane::Session,
            &continued,
            instant("2026-01-07T12:00:00Z")?,
        )?;
        assert_eq!(lease.state, RetentionState::QueryOnly);
        assert_eq!(lease.reason, RetentionReason::SessionInactivityLease);

        let near_cap = RetentionFacts {
            last_continued_at: Some("2026-01-07T23:00:00Z".to_owned()),
            ..continued
        };
        let cap = evaluate_retention(
            "session-cap",
            MemoryLane::Session,
            &near_cap,
            instant("2026-01-08T00:00:00Z")?,
        )?;
        assert_eq!(cap.state, RetentionState::QueryOnly);
        assert_eq!(cap.reason, RetentionReason::SessionMaximumAge);
        Ok(())
    }

    #[test]
    fn closed_session_is_query_only_at_closure() -> Result<()> {
        let facts = RetentionFacts {
            started_at: Some("2026-01-01T00:00:00Z".to_owned()),
            closed_at: Some("2026-01-01T12:00:00Z".to_owned()),
            ..facts()
        };
        let decision = evaluate_retention(
            "session-closed",
            MemoryLane::Session,
            &facts,
            instant("2026-01-01T12:00:00Z")?,
        )?;
        assert_eq!(decision.state, RetentionState::QueryOnly);
        assert_eq!(decision.reason, RetentionReason::SessionClosed);
        Ok(())
    }

    #[test]
    fn durable_lanes_have_no_age_ttl() -> Result<()> {
        for lane in [MemoryLane::Semantic, MemoryLane::Procedural] {
            let decision =
                evaluate_retention("durable", lane, &facts(), instant("2099-01-01T00:00:00Z")?)?;
            assert_eq!(decision.state, RetentionState::Current);
            assert_eq!(decision.reason, RetentionReason::NoAgeLimit);
            assert_eq!(decision.effective_boundary, None);
        }
        Ok(())
    }

    #[test]
    fn private_episodic_recall_and_validity_extensions_are_independent() -> Result<()> {
        let facts = RetentionFacts {
            occurred_at: Some("2026-01-01T00:00:00Z".to_owned()),
            explicit_expires_at: Some("2026-01-20T00:00:00Z".to_owned()),
            ..facts()
        };
        let recall_only = PrivateLifecycleReadFacts {
            automatic_recall_until: Some("2026-03-01T00:00:00Z"),
            automatic_recall_event_id: Some("event-recall"),
            ..private_lifecycle()
        };
        assert!(
            !evaluate_private_current_assertion(
                "private-episode",
                MemoryLane::Episodic,
                &facts,
                instant("2026-02-01T00:00:00Z")?,
                recall_only,
            )?,
            "recall authority must not extend an expired validity clock"
        );

        let recall_and_validity = PrivateLifecycleReadFacts {
            validity_until: Some("2026-04-01T00:00:00Z"),
            validity_event_id: Some("event-validity"),
            ..recall_only
        };
        assert!(evaluate_private_current_assertion(
            "private-episode",
            MemoryLane::Episodic,
            &facts,
            instant("2026-02-01T00:00:00Z")?,
            recall_and_validity,
        )?);
        assert!(
            !evaluate_private_current_assertion(
                "private-episode",
                MemoryLane::Episodic,
                &facts,
                instant("2026-03-01T00:00:00Z")?,
                recall_and_validity,
            )?,
            "validity authority must not extend an expired recall clock"
        );
        Ok(())
    }

    #[test]
    fn private_lifecycle_overrides_require_verified_bounded_authority() -> Result<()> {
        let episodic = RetentionFacts {
            occurred_at: Some("2026-01-01T00:00:00Z".to_owned()),
            ..facts()
        };
        let unverified = PrivateLifecycleReadFacts {
            automatic_recall_until: Some("2026-03-01T00:00:00Z"),
            ..private_lifecycle()
        };
        let error = evaluate_private_current_assertion(
            "private-unverified",
            MemoryLane::Episodic,
            &episodic,
            instant("2026-02-01T00:00:00Z")?,
            unverified,
        )
        .expect_err("an unverified override must fail closed");
        assert!(format!("{error:#}").contains("authorizing event ID"));

        let over_cap = PrivateLifecycleReadFacts {
            automatic_recall_until: Some("2026-04-02T00:00:00Z"),
            automatic_recall_event_id: Some("event-over-cap"),
            ..private_lifecycle()
        };
        let error = evaluate_private_current_assertion(
            "private-over-cap",
            MemoryLane::Episodic,
            &episodic,
            instant("2026-02-01T00:00:00Z")?,
            over_cap,
        )
        .expect_err("recall authority beyond 90 days must fail closed");
        assert!(format!("{error:#}").contains("90-day"));

        let quarantined = PrivateLifecycleReadFacts {
            quarantined: true,
            quarantine_reason: Some("owner_quarantine"),
            ..private_lifecycle()
        };
        assert!(!evaluate_private_current_assertion(
            "private-quarantine",
            MemoryLane::Semantic,
            &facts(),
            instant("2026-02-01T00:00:00Z")?,
            quarantined,
        )?);
        Ok(())
    }

    #[test]
    fn current_assertion_composes_status_retention_and_future_exclusions() -> Result<()> {
        let facts = RetentionFacts {
            occurred_at: Some("2026-01-01T00:00:00Z".to_owned()),
            ..facts()
        };
        let decision = evaluate_current_assertion(
            "episode-composed",
            MemoryStatus::Superseded,
            MemoryLane::Episodic,
            &facts,
            instant("2026-02-01T00:00:00Z")?,
            vec![CurrentAssertionExclusion::UnresolvedConflict],
        )?;

        assert!(!decision.is_current);
        assert_eq!(decision.exclusions.len(), 3);
        assert!(matches!(
            decision.exclusions[0],
            CurrentAssertionExclusion::LifecycleStatus {
                status: MemoryStatus::Superseded
            }
        ));
        assert!(matches!(
            decision.exclusions[1],
            CurrentAssertionExclusion::Retention { .. }
        ));
        Ok(())
    }

    #[test]
    fn invalid_facts_identify_the_record() -> Result<()> {
        let error = evaluate_retention(
            "record-needs-upgrade",
            MemoryLane::Episodic,
            &facts(),
            instant("2026-01-01T00:00:00Z")?,
        )
        .expect_err("missing occurred_at must fail");
        assert!(error.to_string().contains("record-needs-upgrade"));
        Ok(())
    }

    #[test]
    fn sqlite_function_returns_state_and_wraps_errors_with_record_id() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        register_sqlite_functions(&conn)?;
        let current: String = conn.query_row(
            "SELECT memzoi_retention_state(?1, ?2, ?3, ?4)",
            rusqlite::params![
                "episode-sql",
                "episodic",
                serde_json::to_string(&RetentionFacts {
                    occurred_at: Some("2026-01-01T00:00:00Z".to_owned()),
                    ..facts()
                })?,
                "2026-01-30T00:00:00Z",
            ],
            |row| row.get(0),
        )?;
        assert_eq!(current, "current");

        let session: String = conn.query_row(
            "SELECT memzoi_retention_state(?1, ?2, ?3, ?4)",
            rusqlite::params![
                "session-sql",
                "session",
                serde_json::to_string(&RetentionFacts {
                    started_at: Some("2026-07-10T12:00:00Z".to_owned()),
                    ..facts()
                })?,
                "2026-07-10T12:00:00Z",
            ],
            |row| row.get(0),
        )?;
        assert_eq!(session, "current");

        let error = conn
            .query_row::<String, _, _>(
                "SELECT memzoi_retention_state(?1, ?2, ?3, ?4)",
                rusqlite::params![
                    "episode-bad-sql",
                    "episodic",
                    serde_json::to_string(&facts())?,
                    "2026-01-30T00:00:00Z",
                ],
                |row| row.get(0),
            )
            .expect_err("invalid facts must surface as a SQL error");
        assert!(
            format!("{error:?}").contains("episode-bad-sql"),
            "{error:?}"
        );
        Ok(())
    }

    #[test]
    fn central_sql_requires_private_state_and_enforces_quarantine_and_validity() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        register_sqlite_functions(&conn)?;
        conn.execute_batch(
            "CREATE TABLE memory_record (
               id TEXT PRIMARY KEY,
               status TEXT NOT NULL,
               lane TEXT NOT NULL,
               destination TEXT NOT NULL,
               retention_json TEXT NOT NULL
             );
             CREATE TABLE private_lifecycle_state (
               record_id TEXT PRIMARY KEY,
               record_version TEXT NOT NULL DEFAULT '00000000000000000000000000000000',
               automatic_recall_until TEXT,
               validity_until TEXT,
               automatic_recall_event_id TEXT,
               validity_event_id TEXT,
               quarantined INTEGER NOT NULL,
               quarantine_reason_code TEXT,
               quarantine_event_id TEXT
             );
             CREATE TABLE event_log (
               id TEXT PRIMARY KEY,
               event_type TEXT NOT NULL,
               payload_json TEXT NOT NULL
             );
             CREATE TABLE private_lifecycle_generation (
               singleton INTEGER PRIMARY KEY,
               generation INTEGER NOT NULL
             );
             INSERT INTO private_lifecycle_generation VALUES (1, 0);
             CREATE TABLE private_maintenance_projection (
               singleton INTEGER PRIMARY KEY,
               state TEXT NOT NULL,
               grant_fingerprint TEXT,
               projection_id TEXT,
               authoritative_generation INTEGER NOT NULL,
               policy_version TEXT NOT NULL,
               not_after TEXT
             );
             INSERT INTO private_maintenance_projection VALUES (
               1, 'disabled', NULL, NULL, 0,
               'maintenance-policy/1', NULL
             );
             CREATE TABLE private_conflict_set (
               conflict_id TEXT PRIMARY KEY,
               projection_id TEXT NOT NULL,
               grant_fingerprint TEXT NOT NULL,
               policy_version TEXT NOT NULL
             );
             CREATE TABLE private_conflict_member (
               conflict_id TEXT NOT NULL,
               record_id TEXT NOT NULL,
               record_version TEXT NOT NULL
             );
             CREATE TABLE private_conflict_edge (
               conflict_id TEXT NOT NULL,
               left_record_id TEXT NOT NULL,
               right_record_id TEXT NOT NULL
             );",
        )?;
        let durable_expired = serde_json::to_string(&RetentionFacts {
            explicit_expires_at: Some("2026-01-10T00:00:00Z".to_owned()),
            ..facts()
        })?;
        for (id, destination) in [
            ("repo-current", "repo"),
            ("private-current", "local"),
            ("private-missing-state", "local"),
        ] {
            conn.execute(
                "INSERT INTO memory_record(id, status, lane, destination, retention_json)
                 VALUES (?1, 'active', 'semantic', ?2, ?3)",
                rusqlite::params![id, destination, serde_json::to_string(&facts())?],
            )?;
        }
        conn.execute(
            "INSERT INTO memory_record(id, status, lane, destination, retention_json)
             VALUES ('private-validity', 'active', 'semantic', 'local', ?1)",
            [&durable_expired],
        )?;
        conn.execute(
            "INSERT INTO private_lifecycle_state(
               record_id, automatic_recall_until, validity_until,
               automatic_recall_event_id, validity_event_id, quarantined
             ) VALUES ('private-current', NULL, NULL, NULL, NULL, 0)",
            [],
        )?;
        conn.execute(
            "INSERT INTO private_lifecycle_state(
               record_id, automatic_recall_until, validity_until,
               automatic_recall_event_id, validity_event_id, quarantined
             ) VALUES (
               'private-validity', NULL, '2026-03-01T00:00:00Z',
               NULL, 'event-validity', 0
             )",
            [],
        )?;

        let predicate = current_assertion_sql("memory_record", "?1");
        let query = format!("SELECT id FROM memory_record WHERE {predicate} ORDER BY id");
        let eligible = |at: &str| -> Result<Vec<String>> {
            let mut statement = conn.prepare(&query)?;
            let rows = statement.query_map([at], |row| row.get(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        };
        assert_eq!(
            eligible("2026-02-01T00:00:00Z")?,
            vec!["private-current".to_owned(), "repo-current".to_owned()],
            "a fabricated lifecycle event ID must fail closed"
        );
        conn.execute(
            "INSERT INTO event_log(id, event_type, payload_json)
             VALUES (
               'event-validity',
               'memory.private_lifecycle_applied',
               '{\"action_kinds\":[\"extend_validity\"],\"target_record_ids\":[\"another-record\"]}'
             )",
            [],
        )?;
        assert_eq!(
            eligible("2026-02-01T00:00:00Z")?,
            vec!["private-current".to_owned(), "repo-current".to_owned()],
            "an event for a different target must fail closed"
        );
        conn.execute(
            "UPDATE event_log
             SET payload_json =
               '{\"action_kinds\":[\"extend_validity\"],\"target_record_ids\":[\"private-validity\"]}'
             WHERE id = 'event-validity'",
            [],
        )?;
        assert_eq!(
            eligible("2026-02-01T00:00:00Z")?,
            vec![
                "private-current".to_owned(),
                "private-validity".to_owned(),
                "repo-current".to_owned(),
            ],
            "missing private lifecycle state must fail closed"
        );

        conn.execute(
            "UPDATE private_lifecycle_state
             SET quarantined = 1,
                 quarantine_reason_code = 'owner_quarantine',
                 quarantine_event_id = 'fabricated-quarantine-event'
             WHERE record_id = 'private-current'",
            [],
        )?;
        assert_eq!(
            eligible("2026-02-01T00:00:00Z")?,
            vec!["private-validity".to_owned(), "repo-current".to_owned()]
        );
        Ok(())
    }
}
