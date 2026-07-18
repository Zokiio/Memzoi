use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use time::OffsetDateTime;

use crate::{MemoryDestination, MemoryRecord, expiry, retention, search};

const RECORD_COLUMNS: &str =
    "id, type, lane, destination, scope_kind, scope_id, visibility, title, body, status,
     confidence, source_kind, source_ref, content_hash, created_at, updated_at,
     supersedes_id, proposal_id, retention_json, origin_json, lineage_json";

pub(super) fn indexed_active_records_for_destination(
    conn: &Connection,
    destination: MemoryDestination,
) -> Result<Vec<MemoryRecord>> {
    let sql = format!(
        "SELECT {RECORD_COLUMNS}
         FROM memory_record
         WHERE status = 'active'
           AND destination = ?1
         ORDER BY updated_at DESC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([destination.as_str()], search::record_from_row)?;
    let mut records = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    load_capture_provenance_for_records(conn, &mut records)?;
    Ok(records)
}

pub(super) fn indexed_non_runtime_record_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id
         FROM memory_record
         WHERE destination NOT IN ('local', 'session')
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn active_records_for_destination(
    conn: &Connection,
    destination: MemoryDestination,
    now: OffsetDateTime,
) -> Result<Vec<MemoryRecord>> {
    let current_assertion = retention::current_assertion_sql("memory_record", "?2");
    let sql = format!(
        "SELECT {RECORD_COLUMNS}
         FROM memory_record
         WHERE destination = ?1
           AND {current_assertion}
         ORDER BY updated_at DESC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![destination.as_str(), expiry::format_timestamp(now)?],
        search::record_from_row,
    )?;
    let mut records = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    load_capture_provenance_for_records(conn, &mut records)?;
    Ok(records)
}

pub(super) fn active_checkpoint_records(
    conn: &Connection,
    now: OffsetDateTime,
) -> Result<Vec<MemoryRecord>> {
    let current_assertion = retention::current_assertion_sql("memory_record", "?1");
    let sql = format!(
        "SELECT {RECORD_COLUMNS}
         FROM memory_record
         WHERE destination = 'session'
           AND source_kind = 'memzoi-checkpoint'
           AND {current_assertion}
         ORDER BY created_at DESC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([expiry::format_timestamp(now)?], search::record_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn checkpoint_record(
    conn: &Connection,
    record_id: &str,
    now: OffsetDateTime,
) -> Result<Option<MemoryRecord>> {
    let current_assertion = retention::current_assertion_sql("memory_record", "?2");
    let sql = format!(
        "SELECT {RECORD_COLUMNS}
         FROM memory_record
         WHERE id = ?1
           AND destination = 'session'
           AND source_kind = 'memzoi-checkpoint'
           AND {current_assertion}"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_row(
        rusqlite::params![record_id, expiry::format_timestamp(now)?],
        search::record_from_row,
    )
    .optional()
    .map_err(Into::into)
}

pub(super) fn record_by_id(conn: &Connection, record_id: &str) -> Result<Option<MemoryRecord>> {
    let sql = format!(
        "SELECT {RECORD_COLUMNS}
         FROM memory_record
         WHERE id = ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut record = stmt
        .query_row([record_id], search::record_from_row)
        .optional()
        .map_err(anyhow::Error::from)?;
    drop(stmt);
    if let Some(record) = &mut record {
        record.capture = crate::capture::load_capture_provenance(conn, &record.id)?;
    }
    Ok(record)
}

fn load_capture_provenance_for_records(
    conn: &Connection,
    records: &mut [MemoryRecord],
) -> Result<()> {
    for record in records {
        record.capture = crate::capture::load_capture_provenance(conn, &record.id)?;
    }
    Ok(())
}

pub(super) fn records_for_runtime_preservation(conn: &Connection) -> Result<Vec<MemoryRecord>> {
    let sql = format!(
        "SELECT {RECORD_COLUMNS}
         FROM memory_record
         WHERE destination IN ('local', 'session')
         ORDER BY updated_at DESC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], search::record_from_row)?;
    let mut records = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    load_capture_provenance_for_records(conn, &mut records)?;
    Ok(records)
}
