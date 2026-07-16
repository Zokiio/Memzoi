use crate::{
    error::{CoreError, Result as CoreResult},
    models::MemoryEvent,
};
use rusqlite::{Connection, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
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

pub fn append_event(conn: &Connection, input: AppendEvent) -> CoreResult<MemoryEvent> {
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
pub(crate) fn for_each_event(
    conn: &Connection,
    mut visit: impl FnMut(MemoryEvent) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, event_type, actor, payload_json, record_id, proposal_id, created_at
         FROM event_log
         ORDER BY created_at ASC, id ASC",
    )?;
    let mut rows = stmt.query([])?;

    while let Some(row) = rows.next()? {
        visit(memory_event_from_row(row)?)?;
    }

    Ok(())
}

pub(crate) fn for_each_merged_event(
    first: &Connection,
    second: &Connection,
    mut visit: impl FnMut(MemoryEvent) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    const ORDERED_EVENTS: &str =
        "SELECT id, event_type, actor, payload_json, record_id, proposal_id, created_at
         FROM event_log
         ORDER BY created_at ASC, id ASC";

    let second_ids = {
        let mut stmt = second.prepare("SELECT id FROM event_log")?;
        let ids = stmt.query_map([], |row| row.get::<_, String>(0))?;
        ids.collect::<rusqlite::Result<HashSet<_>>>()?
    };
    let mut first_stmt = first.prepare(ORDERED_EVENTS)?;
    let mut second_stmt = second.prepare(ORDERED_EVENTS)?;
    let mut first_rows = first_stmt.query([])?;
    let mut second_rows = second_stmt.query([])?;
    let mut first_event = next_encoded_event(&mut first_rows)?;
    let mut second_event = next_encoded_event(&mut second_rows)?;

    loop {
        match (first_event.take(), second_event.take()) {
            (Some(left), Some(right)) => {
                let left_key = (left.created_at.as_str(), left.id.as_str());
                let right_key = (right.created_at.as_str(), right.id.as_str());
                match left_key.cmp(&right_key) {
                    std::cmp::Ordering::Less => {
                        second_event = Some(right);
                        if !second_ids.contains(&left.id) {
                            visit(left.into_event()?)?;
                        }
                        first_event = next_encoded_event(&mut first_rows)?;
                    }
                    std::cmp::Ordering::Greater => {
                        first_event = Some(left);
                        visit(right.into_event()?)?;
                        second_event = next_encoded_event(&mut second_rows)?;
                    }
                    std::cmp::Ordering::Equal => {
                        visit(right.into_event()?)?;
                        first_event = next_encoded_event(&mut first_rows)?;
                        second_event = next_encoded_event(&mut second_rows)?;
                    }
                }
            }
            (Some(event), None) => {
                if !second_ids.contains(&event.id) {
                    visit(event.into_event()?)?;
                }
                first_event = next_encoded_event(&mut first_rows)?;
            }
            (None, Some(event)) => {
                visit(event.into_event()?)?;
                second_event = next_encoded_event(&mut second_rows)?;
            }
            (None, None) => return Ok(()),
        }
    }
}

fn next_encoded_event(rows: &mut rusqlite::Rows<'_>) -> anyhow::Result<Option<EncodedMemoryEvent>> {
    rows.next()?
        .map(EncodedMemoryEvent::from_row)
        .transpose()
        .map_err(Into::into)
}

struct EncodedMemoryEvent {
    id: String,
    event_type: String,
    actor: String,
    payload_json: String,
    record_id: Option<String>,
    proposal_id: Option<String>,
    created_at: String,
}

impl EncodedMemoryEvent {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            event_type: row.get(1)?,
            actor: row.get(2)?,
            payload_json: row.get(3)?,
            record_id: row.get(4)?,
            proposal_id: row.get(5)?,
            created_at: row.get(6)?,
        })
    }

    fn into_event(self) -> rusqlite::Result<MemoryEvent> {
        let payload = serde_json::from_str(&self.payload_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        Ok(MemoryEvent {
            id: self.id,
            event_type: self.event_type,
            actor: self.actor,
            payload,
            record_id: self.record_id,
            proposal_id: self.proposal_id,
            created_at: self.created_at,
        })
    }
}

#[cfg(test)]
pub(crate) fn list_events(conn: &Connection) -> anyhow::Result<Vec<MemoryEvent>> {
    let mut events = Vec::new();
    for_each_event(conn, |event| {
        events.push(event);
        Ok(())
    })?;
    Ok(events)
}

#[cfg(test)]
fn memory_event_from_row(row: &Row<'_>) -> rusqlite::Result<MemoryEvent> {
    EncodedMemoryEvent::from_row(row)?.into_event()
}

pub(crate) fn now_utc() -> CoreResult<String> {
    Ok(OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use crate::{
        events::{AppendEvent, append_event, for_each_event, for_each_merged_event, list_events},
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
    fn for_each_event_stops_after_consumer_error() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;

        let first = append_event(
            &conn,
            AppendEvent {
                event_type: "memory.proposed".to_owned(),
                actor: "agent:streaming-test".to_owned(),
                payload: json!({ "sequence": 1 }),
                record_id: None,
                proposal_id: None,
            },
        )?;
        append_event(
            &conn,
            AppendEvent {
                event_type: "memory.applied".to_owned(),
                actor: "agent:streaming-test".to_owned(),
                payload: json!({ "sequence": 2 }),
                record_id: None,
                proposal_id: None,
            },
        )?;

        let mut observed = Vec::new();
        let result = for_each_event(&conn, |event| {
            observed.push(event.id);
            Err(anyhow::anyhow!("stop after first event"))
        });

        assert_eq!(observed, vec![first.id]);
        let error = result.expect_err("the consumer error should stop traversal");
        assert_eq!(error.to_string(), "stop after first event");

        Ok(())
    }

    #[test]
    fn merged_event_stream_is_ordered_deduplicated_and_stops_early() -> anyhow::Result<()> {
        let (_first_temp, first) = initialized_database()?;
        let (_second_temp, second) = initialized_database()?;
        insert_test_event(&first, "evt-first", "first", "2026-07-14T00:00:00Z")?;
        insert_test_event(&first, "evt-shared", "shared", "2026-07-14T00:01:00Z")?;
        insert_test_event(&second, "evt-shared", "index", "2026-07-14T00:01:30Z")?;
        insert_test_event(&first, "evt-equal", "shared-equal", "2026-07-14T00:01:45Z")?;
        insert_test_event(&second, "evt-equal", "index-equal", "2026-07-14T00:01:45Z")?;
        insert_test_event(&second, "evt-last", "last", "2026-07-14T00:02:00Z")?;
        second.pragma_update(None, "ignore_check_constraints", "ON")?;
        second.execute(
            "INSERT INTO event_log (
               id, event_type, actor, payload_json, record_id, proposal_id, created_at
             ) VALUES (
               'evt-malformed', 'test.event', 'malformed', '{', NULL, NULL,
               '2026-07-14T00:03:00Z'
             )",
            [],
        )?;
        second.pragma_update(None, "ignore_check_constraints", "OFF")?;

        let mut merged = Vec::new();
        for_each_merged_event(&first, &second, |event| {
            merged.push((event.id, event.actor));
            Ok(())
        })
        .expect_err("a full traversal should eventually decode the malformed fixture");
        assert_eq!(
            merged,
            vec![
                ("evt-first".to_owned(), "first".to_owned()),
                ("evt-shared".to_owned(), "index".to_owned()),
                ("evt-equal".to_owned(), "index-equal".to_owned()),
                ("evt-last".to_owned(), "last".to_owned()),
            ]
        );

        let mut observed = Vec::new();
        let error = for_each_merged_event(&first, &second, |event| {
            observed.push(event.id);
            Err(anyhow::anyhow!("stop merged traversal"))
        })
        .expect_err("the consumer error should stop the merged traversal");
        assert_eq!(observed, vec!["evt-first"]);
        assert_eq!(error.to_string(), "stop merged traversal");
        Ok(())
    }

    #[test]
    fn merged_event_stream_does_not_decode_an_unvisited_later_head() -> anyhow::Result<()> {
        let (_first_temp, first) = initialized_database()?;
        let (_second_temp, second) = initialized_database()?;
        insert_test_event(&first, "evt-first", "first", "2026-07-14T00:00:00Z")?;
        second.pragma_update(None, "ignore_check_constraints", "ON")?;
        second.execute(
            "INSERT INTO event_log (
               id, event_type, actor, payload_json, record_id, proposal_id, created_at
             ) VALUES (
               'evt-malformed', 'test.event', 'malformed', '{', NULL, NULL,
               '2026-07-14T00:01:00Z'
             )",
            [],
        )?;
        second.pragma_update(None, "ignore_check_constraints", "OFF")?;

        let mut observed = Vec::new();
        let error = for_each_merged_event(&first, &second, |event| {
            observed.push(event.id);
            Err(anyhow::anyhow!("stop before later malformed event"))
        })
        .expect_err("the visitor should stop before the later event is decoded");

        assert_eq!(observed, vec!["evt-first"]);
        assert_eq!(error.to_string(), "stop before later malformed event");
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

    fn insert_test_event(
        conn: &rusqlite::Connection,
        id: &str,
        actor: &str,
        created_at: &str,
    ) -> anyhow::Result<()> {
        conn.execute(
            "INSERT INTO event_log (
               id, event_type, actor, payload_json, record_id, proposal_id, created_at
             ) VALUES (?1, 'test.event', ?2, '{}', NULL, NULL, ?3)",
            rusqlite::params![id, actor, created_at],
        )?;
        Ok(())
    }
}
