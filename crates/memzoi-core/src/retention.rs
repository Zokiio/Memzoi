use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, functions::FunctionFlags};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use crate::models::{MemoryLane, MemoryStatus};

pub const SQL_RETENTION_STATE: &str = "memzoi_retention_state";

const SESSION_INACTIVITY_HOURS: i64 = 24;
const SESSION_MAXIMUM_DAYS: i64 = 7;
const EPISODIC_ORDINARY_DAYS: i64 = 30;

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

/// Build the shared SQL boundary for ordinary-use eligibility.
///
/// Every SQL-backed recall, admission, duplicate, and conflict route must use
/// this complete boundary rather than composing status and retention clauses
/// independently. Later lifecycle policies can extend this one builder.
pub(crate) fn current_assertion_sql(table_alias: &str, evaluated_at_parameter: &str) -> String {
    format!(
        "{table_alias}.status = 'active'\n         AND memzoi_retention_state(\n               {table_alias}.id,\n               {table_alias}.lane,\n               {table_alias}.retention_json,\n               {evaluated_at_parameter}\n             ) = 'current'"
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

#[derive(Debug, Clone, Copy)]
struct Boundary {
    at: OffsetDateTime,
    reason: RetentionReason,
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
}
