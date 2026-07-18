use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use crate::{CurrentAssertionExclusion, MemoryRecord, RetentionDecision};

/// Supplies the instant used to decide whether records have expired.
///
/// Production services use [`SystemClock`]. Tests and embedding applications can
/// inject a deterministic implementation with `MemoryService::open_with_clock`
/// or `MemoryService::open_paths_with_clock`.
pub trait Clock: Send + Sync {
    fn now_utc(&self) -> OffsetDateTime;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedClock {
    now: OffsetDateTime,
}

impl FixedClock {
    pub fn new(now: OffsetDateTime) -> Self {
        Self { now }
    }

    pub fn from_rfc3339(value: &str) -> Result<Self> {
        let now = OffsetDateTime::parse(value.trim(), &Rfc3339)
            .with_context(|| format!("fixed clock value must be an RFC 3339 timestamp: {value}"))?;
        Ok(Self::new(now))
    }
}

impl Clock for FixedClock {
    fn now_utc(&self) -> OffsetDateTime {
        self.now
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpiryDiagnostic {
    pub record: MemoryRecord,
    pub evaluated_at: String,
    pub retention: RetentionDecision,
    pub current_assertion: bool,
    pub exclusions: Vec<CurrentAssertionExclusion>,
    pub excluded_from_normal_reads: bool,
    pub reason: String,
}

pub(crate) fn format_timestamp(timestamp: OffsetDateTime) -> Result<String> {
    timestamp
        .to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .context("failed to format expiry evaluation timestamp")
}

pub(crate) fn diagnose(record: MemoryRecord, now: OffsetDateTime) -> Result<ExpiryDiagnostic> {
    let evaluated_at = format_timestamp(now)?;
    let decision = crate::evaluate_current_assertion(
        &record.id,
        record.status,
        record.lane,
        &record.retention,
        now,
        Vec::new(),
    )?;
    let excluded_from_normal_reads = !decision.is_current;
    let reason = if decision.is_current {
        format!(
            "included in ordinary reads: retention is {} at {evaluated_at} and no current-assertion exclusion applies",
            decision.retention.state.as_str()
        )
    } else {
        format!(
            "excluded from ordinary reads by current-assertion exclusions: {}",
            serde_json::to_string(&decision.exclusions)?
        )
    };

    Ok(ExpiryDiagnostic {
        record,
        evaluated_at,
        retention: decision.retention,
        current_assertion: decision.is_current,
        exclusions: decision.exclusions,
        excluded_from_normal_reads,
        reason,
    })
}
