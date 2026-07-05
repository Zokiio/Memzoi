use crate::error::{CoreError, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppendEvent {
    pub event_type: String,
    pub actor: String,
    pub payload: Value,
    pub record_id: Option<String>,
    pub proposal_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryEvent {
    pub id: String,
    pub event_type: String,
    pub actor: String,
    pub payload: Value,
    pub record_id: Option<String>,
    pub proposal_id: Option<String>,
    pub created_at: String,
}

pub fn append_event(conn: &Connection, input: AppendEvent) -> Result<MemoryEvent> {
    let event = MemoryEvent {
        id: format!("evt_{}", Uuid::now_v7()),
        event_type: input.event_type,
        actor: input.actor,
        payload: input.payload,
        record_id: input.record_id,
        proposal_id: input.proposal_id,
        created_at: now_utc()?,
    };

    conn.execute(
        "INSERT INTO event_log (id, event_type, actor, payload_json, record_id, proposal_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            event.id,
            event.event_type,
            event.actor,
            serde_json::to_string(&event.payload)?,
            event.record_id,
            event.proposal_id,
            event.created_at,
        ],
    )
    .map_err(|source| CoreError::DbContext {
        context: format!("failed to append event {}", event.event_type),
        source,
    })?;

    Ok(event)
}

#[cfg(test)]
pub(crate) fn list_events(conn: &Connection) -> Result<Vec<MemoryEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, event_type, actor, payload_json, record_id, proposal_id, created_at
         FROM event_log
         ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let payload_json: String = row.get(3)?;
        let payload = serde_json::from_str(&payload_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        Ok(MemoryEvent {
            id: row.get(0)?,
            event_type: row.get(1)?,
            actor: row.get(2)?,
            payload,
            record_id: row.get(4)?,
            proposal_id: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn now_utc() -> Result<String> {
    Ok(OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use crate::{
        events::{AppendEvent, append_event, list_events},
        init_database, open_database,
    };

    #[test]
    fn now_utc_returns_parseable_rfc3339_timestamp() -> anyhow::Result<()> {
        let timestamp = super::now_utc()?;

        time::OffsetDateTime::parse(&timestamp, &time::format_description::well_known::Rfc3339)?;

        Ok(())
    }

    #[test]
    fn appended_json_payloads_round_trip_in_append_order() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;

        let first = append_event(
            &conn,
            AppendEvent {
                event_type: "memory.proposed".to_owned(),
                actor: "agent:red-tests".to_owned(),
                payload: json!({
                    "proposal_id": "prop-first",
                    "title": "Prefer deterministic tests",
                    "tags": ["rust", "tests"]
                }),
                record_id: None,
                proposal_id: Some("prop-first".to_owned()),
            },
        )?;
        let second = append_event(
            &conn,
            AppendEvent {
                event_type: "memory.applied".to_owned(),
                actor: "agent:red-tests".to_owned(),
                payload: json!({
                    "proposal_id": "prop-second",
                    "record_id": "mem-second",
                    "status": "active"
                }),
                record_id: Some("mem-second".to_owned()),
                proposal_id: Some("prop-second".to_owned()),
            },
        )?;

        let events = list_events(&conn)?;

        assert_eq!(
            events
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            vec![first.id.as_str(), second.id.as_str()]
        );
        assert_eq!(events[0].event_type, "memory.proposed");
        assert_eq!(events[0].actor, "agent:red-tests");
        assert_eq!(events[0].proposal_id.as_deref(), Some("prop-first"));
        assert_eq!(events[0].record_id, None);
        assert_eq!(
            events[0].payload,
            json!({
                "proposal_id": "prop-first",
                "title": "Prefer deterministic tests",
                "tags": ["rust", "tests"]
            })
        );
        assert_eq!(events[1].event_type, "memory.applied");
        assert_eq!(events[1].record_id.as_deref(), Some("mem-second"));
        assert_eq!(
            events[1].payload,
            json!({
                "proposal_id": "prop-second",
                "record_id": "mem-second",
                "status": "active"
            })
        );

        Ok(())
    }

    #[test]
    fn appending_events_never_replaces_an_existing_event() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;

        let original = append_event(
            &conn,
            AppendEvent {
                event_type: "memory.proposed".to_owned(),
                actor: "agent:first".to_owned(),
                payload: json!({ "version": 1, "proposal_id": "prop-same" }),
                record_id: None,
                proposal_id: Some("prop-same".to_owned()),
            },
        )?;
        let later = append_event(
            &conn,
            AppendEvent {
                event_type: "memory.proposed".to_owned(),
                actor: "agent:second".to_owned(),
                payload: json!({ "version": 2, "proposal_id": "prop-same" }),
                record_id: None,
                proposal_id: Some("prop-same".to_owned()),
            },
        )?;

        let events = list_events(&conn)?;

        assert_eq!(events.len(), 2, "append_event must append a new row");
        assert_ne!(
            original.id, later.id,
            "each appended event needs a distinct id"
        );
        assert_eq!(events[0].id, original.id);
        assert_eq!(events[0].actor, "agent:first");
        assert_eq!(
            events[0].payload,
            json!({ "version": 1, "proposal_id": "prop-same" })
        );
        assert_eq!(events[1].id, later.id);
        assert_eq!(events[1].actor, "agent:second");
        assert_eq!(
            events[1].payload,
            json!({ "version": 2, "proposal_id": "prop-same" })
        );

        Ok(())
    }

    fn initialized_database() -> anyhow::Result<(TempDir, rusqlite::Connection)> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("memory.db");
        let conn = open_database(&db_path)?;
        init_database(&conn)?;
        Ok((temp, conn))
    }
}
