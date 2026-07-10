use anyhow::{Context, Result, bail};
use rusqlite::{Connection, functions::FunctionFlags};
use serde::{Deserialize, Serialize};
use time::{Date, Month, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use crate::models::{MemoryRecord, MemoryStatus};

pub(crate) const SQL_IS_EXPIRED: &str = "memzoi_is_expired";

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
    pub effective_expires_at: Option<String>,
    pub expired: bool,
    pub excluded_from_normal_reads: bool,
    pub reason: String,
}

pub(crate) fn register_sqlite_functions(conn: &Connection) -> Result<()> {
    conn.create_scalar_function(
        SQL_IS_EXPIRED,
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let expires_at = context.get::<Option<String>>(0)?;
            let now = context.get::<String>(1)?;
            let Some(expires_at) = expires_at else {
                return Ok(0_i64);
            };
            let expires_at = parse_expires_at(&expires_at)
                .map_err(|error| rusqlite::Error::UserFunctionError(error.into()))?;
            let now = OffsetDateTime::parse(&now, &Rfc3339)
                .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))?;
            Ok(i64::from(now >= expires_at))
        },
    )?;
    Ok(())
}

/// Parse canonical expiry syntax.
///
/// A date-only value means midnight at the start of that date in UTC. A
/// timestamp must be RFC 3339 and include `Z` or an explicit numeric offset.
pub(crate) fn parse_expires_at(value: &str) -> Result<OffsetDateTime> {
    let value = value.trim();
    if value.is_empty() {
        bail!("expires_at cannot be empty");
    }

    if value.len() == 10
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
    {
        let year = value[0..4]
            .parse::<i32>()
            .with_context(|| format!("expires_at date has an invalid year: {value}"))?;
        let month = value[5..7]
            .parse::<u8>()
            .with_context(|| format!("expires_at date has an invalid month: {value}"))?;
        let day = value[8..10]
            .parse::<u8>()
            .with_context(|| format!("expires_at date has an invalid day: {value}"))?;
        let month = Month::try_from(month)
            .with_context(|| format!("expires_at date has an invalid month: {value}"))?;
        let date = Date::from_calendar_date(year, month, day)
            .with_context(|| format!("expires_at date is invalid: {value}"))?;
        return Ok(date.midnight().assume_utc());
    }

    OffsetDateTime::parse(value, &Rfc3339).with_context(|| {
        format!("expires_at must be YYYY-MM-DD or an RFC 3339 timestamp with a timezone: {value}")
    })
}

pub(crate) fn format_timestamp(timestamp: OffsetDateTime) -> Result<String> {
    timestamp
        .to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .context("failed to format expiry evaluation timestamp")
}

pub(crate) fn is_expired(expires_at: Option<&str>, now: OffsetDateTime) -> Result<bool> {
    expires_at
        .map(parse_expires_at)
        .transpose()
        .map(|expires_at| expires_at.is_some_and(|expires_at| now >= expires_at))
}

pub(crate) fn diagnose(record: MemoryRecord, now: OffsetDateTime) -> Result<ExpiryDiagnostic> {
    let effective = record
        .expires_at
        .as_deref()
        .map(parse_expires_at)
        .transpose()?;
    let expired = effective.is_some_and(|expires_at| now >= expires_at);
    let evaluated_at = format_timestamp(now)?;
    let effective_expires_at = effective.map(format_timestamp).transpose()?;
    let excluded_from_normal_reads = record.status != MemoryStatus::Active || expired;
    let reason = if record.status != MemoryStatus::Active {
        format!(
            "excluded from normal reads because record status is {}",
            record.status.as_str()
        )
    } else if expired {
        format!(
            "excluded from normal reads because evaluation time {evaluated_at} is at or after expiry {}",
            effective_expires_at.as_deref().unwrap_or_default()
        )
    } else if let Some(effective_expires_at) = effective_expires_at.as_deref() {
        format!(
            "included in normal reads because evaluation time {evaluated_at} is before expiry {effective_expires_at}"
        )
    } else {
        "included in normal reads because the active record has no expiry".to_owned()
    };

    Ok(ExpiryDiagnostic {
        record,
        evaluated_at,
        effective_expires_at,
        expired,
        excluded_from_normal_reads,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};

    use super::{is_expired, parse_expires_at};

    fn instant(value: &str) -> anyhow::Result<OffsetDateTime> {
        Ok(OffsetDateTime::parse(value, &Rfc3339)?)
    }

    #[test]
    fn expiry_boundary_is_inclusive() -> anyhow::Result<()> {
        let expires_at = "2026-07-10T12:00:00Z";
        assert!(!is_expired(
            Some(expires_at),
            instant("2026-07-10T11:59:59.999999999Z")?
        )?);
        assert!(is_expired(
            Some(expires_at),
            instant("2026-07-10T12:00:00Z")?
        )?);
        assert!(is_expired(
            Some(expires_at),
            instant("2026-07-10T12:00:00.000000001Z")?
        )?);
        Ok(())
    }

    #[test]
    fn expiry_offsets_are_compared_as_instants() -> anyhow::Result<()> {
        assert!(is_expired(
            Some("2026-07-10T14:00:00+02:00"),
            instant("2026-07-10T12:00:00Z")?
        )?);
        assert!(!is_expired(
            Some("2026-07-10T14:00:01+02:00"),
            instant("2026-07-10T12:00:00Z")?
        )?);
        Ok(())
    }

    #[test]
    fn date_only_expiry_is_start_of_day_utc() -> anyhow::Result<()> {
        assert_eq!(
            parse_expires_at("2026-07-10")?,
            instant("2026-07-10T00:00:00Z")?
        );
        assert!(!is_expired(
            Some("2026-07-10"),
            instant("2026-07-09T23:59:59.999999999Z")?
        )?);
        assert!(is_expired(
            Some("2026-07-10"),
            instant("2026-07-10T00:00:00Z")?
        )?);
        Ok(())
    }

    #[test]
    fn timestamp_requires_timezone() {
        let error = parse_expires_at("2026-07-10T12:00:00")
            .expect_err("timestamps without a timezone must be rejected");
        assert!(error.to_string().contains("with a timezone"));
    }
}
