use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    events::{AppendEvent, append_event},
    models::{
        MemoryCitation, MemoryDestination, MemoryPath, MemoryRecord, MemoryType, ScopeKind,
        SearchResult,
    },
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SearchInput {
    pub query: String,
    pub scope_kind: Option<ScopeKind>,
    pub scope_id: Option<String>,
    pub memory_type: Option<MemoryType>,
    pub destination: Option<MemoryDestination>,
    pub path_prefix: Option<String>,
    pub limit: usize,
    pub include_inactive: bool,
}

pub fn search_memory(conn: &Connection, input: SearchInput) -> Result<Vec<SearchResult>> {
    let fts_query = fts_query(&input.query);
    if fts_query.is_empty() {
        return Ok(Vec::new());
    }
    let path_prefix = input
        .path_prefix
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned);

    let limit = normalized_limit(input.limit);
    let status_filter = if input.include_inactive {
        "1 = 1"
    } else {
        "memory_record.status = 'active'"
    };
    let scope_filter = if input.scope_kind.is_some() {
        "memory_record.scope_kind = ?2"
    } else {
        "1 = 1"
    };
    let scope_id_filter = if input.scope_id.is_some() {
        "memory_record.scope_id = ?3"
    } else {
        "1 = 1"
    };
    let type_filter = if input.memory_type.is_some() {
        "memory_record.type = ?4"
    } else {
        "1 = 1"
    };
    let destination = input.destination.unwrap_or(MemoryDestination::Repo);
    let destination_filter = "memory_record.destination = ?7";
    let path_filter = if path_prefix.is_some() {
        "EXISTS (
            SELECT 1 FROM memory_path
            WHERE memory_path.record_id = memory_record.id
              AND (
                memory_path.path = ?5
                OR memory_path.path LIKE ?6
                OR ?5 LIKE memory_path.path || '/%'
                OR (
                    memory_path.path LIKE '%/**'
                    AND (
                        ?5 = substr(memory_path.path, 1, length(memory_path.path) - 3)
                        OR ?5 LIKE substr(memory_path.path, 1, length(memory_path.path) - 2) || '%'
                    )
                )
                OR (
                    ?5 LIKE '%/**'
                    AND (
                        memory_path.path = substr(?5, 1, length(?5) - 3)
                        OR memory_path.path LIKE substr(?5, 1, length(?5) - 2) || '%'
                    )
                )
              )
        )"
    } else {
        "1 = 1"
    };

    let sql = format!(
        "SELECT memory_record.id, memory_record.type, memory_record.lane, memory_record.destination,
                memory_record.scope_kind, memory_record.scope_id, memory_record.visibility,
                memory_record.title, memory_record.body, memory_record.status,
                memory_record.confidence, memory_record.source_kind, memory_record.source_ref,
                memory_record.content_hash, memory_record.created_at, memory_record.updated_at,
                memory_record.supersedes_id, memory_record.expires_at, bm25(memory_fts) AS rank
         FROM memory_fts
         JOIN memory_record ON memory_record.rowid = memory_fts.rowid
         WHERE memory_fts MATCH ?1
           AND {status_filter}
           AND {destination_filter}
           AND {scope_filter}
           AND {scope_id_filter}
           AND {type_filter}
           AND {path_filter}
         ORDER BY rank ASC, memory_record.updated_at DESC, memory_record.id ASC
         LIMIT ?8"
    );

    let scope_kind = input.scope_kind.map(|value| value.as_str().to_owned());
    let memory_type = input.memory_type.map(|value| value.as_str().to_owned());
    let destination = destination.as_str().to_owned();
    let path_like = path_prefix
        .as_ref()
        .map(|path| format!("{}/%", path.trim_end_matches('/')));
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare memory search")?;
    let rows = stmt
        .query_map(
            params![
                fts_query,
                scope_kind,
                input.scope_id,
                memory_type,
                path_prefix,
                path_like,
                destination,
                limit as i64,
            ],
            |row| {
                let record = record_from_row(row)?;
                let rank: f64 = row.get(18)?;
                Ok((record, rank))
            },
        )
        .context("failed to execute memory search")?;

    let mut results = Vec::new();
    for row in rows {
        let (record, rank) = row.context("failed to read memory search row")?;
        let paths = load_paths(conn, record.id.as_str())?;
        let citation = citation_for(&record, paths.first());
        results.push(SearchResult {
            score: -rank,
            snippet: Some(snippet(&record, &input.query)),
            rationale: Some("fts5 title/body match".to_owned()),
            ranking: None,
            citations: vec![citation],
            paths,
            record,
        });
    }

    append_event(
        conn,
        AppendEvent {
            event_type: "memory.searched".to_owned(),
            actor: "memzoi-core".to_owned(),
            payload: json!({
                "query": input.query,
                "scope_kind": scope_kind,
                "type": memory_type,
                "destination": destination,
                "path_prefix": input.path_prefix,
                "limit": limit,
                "result_ids": results.iter().map(|result| result.record.id.as_str()).collect::<Vec<_>>(),
            }),
            record_id: None,
            proposal_id: None,
        },
    )?;

    Ok(results)
}

pub(crate) fn load_paths(conn: &Connection, record_id: &str) -> Result<Vec<MemoryPath>> {
    let mut stmt = conn.prepare(
        "SELECT path, symbol, line_start, line_end
         FROM memory_path
         WHERE record_id = ?1
         ORDER BY path ASC, symbol ASC",
    )?;
    let rows = stmt.query_map([record_id], |row| {
        Ok(MemoryPath {
            path: row.get(0)?,
            symbol: row.get(1)?,
            line_start: row.get(2)?,
            line_end: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn citation_for(record: &MemoryRecord, path: Option<&MemoryPath>) -> MemoryCitation {
    MemoryCitation {
        record_id: record.id.clone(),
        memory_type: record.memory_type,
        scope_kind: record.scope_kind,
        destination: record.destination,
        visibility: record.visibility,
        source_kind: record.source_kind.clone(),
        source_ref: record.source_ref.clone(),
        path: path.map(|path| path.path.clone()),
    }
}

pub(crate) fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRecord> {
    Ok(MemoryRecord {
        id: row.get(0)?,
        memory_type: parse_cell(row, 1)?,
        lane: parse_cell(row, 2)?,
        destination: parse_cell(row, 3)?,
        scope_kind: parse_cell(row, 4)?,
        scope_id: row.get(5)?,
        visibility: parse_cell(row, 6)?,
        title: row.get(7)?,
        body: row.get(8)?,
        status: parse_cell(row, 9)?,
        confidence: row.get(10)?,
        source_kind: row.get(11)?,
        source_ref: row.get(12)?,
        content_hash: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
        supersedes_id: row.get(16)?,
        expires_at: row.get(17)?,
    })
}

fn parse_cell<T>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T>
where
    T: std::str::FromStr<Err = String>,
{
    let raw: String = row.get(index)?;
    raw.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        )
    })
}

pub(crate) fn path_matches_request(stored_path: &str, requested_path: &str) -> bool {
    let stored_path = stored_path.trim().trim_end_matches('/');
    let requested_path = requested_path.trim().trim_end_matches('/');
    if stored_path.is_empty() || requested_path.is_empty() {
        return false;
    }
    if stored_path == requested_path {
        return true;
    }
    if let Some(glob_base) = stored_path.strip_suffix("/**") {
        return path_is_or_is_under(requested_path, glob_base);
    }
    if let Some(glob_base) = requested_path.strip_suffix("/**") {
        return path_is_or_is_under(stored_path, glob_base);
    }
    path_is_or_is_under(requested_path, stored_path)
        || path_is_or_is_under(stored_path, requested_path)
}

fn path_is_or_is_under(path: &str, base: &str) -> bool {
    path == base
        || path
            .strip_prefix(base)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn fts_query(query: &str) -> String {
    let terms = query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "")))
        .collect::<Vec<_>>();
    terms.join(" OR ")
}

fn normalized_limit(limit: usize) -> usize {
    if limit == 0 { 10 } else { limit.min(100) }
}

fn snippet(record: &MemoryRecord, query: &str) -> String {
    let lower_query = query.to_ascii_lowercase();
    let lower_title = record.title.to_ascii_lowercase();
    if lower_title.contains(&lower_query) {
        record.title.clone()
    } else {
        record.body.chars().take(240).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use rusqlite::{Connection, params};
    use serde_json::Value;
    use tempfile::TempDir;

    use super::{SearchInput, search_memory};
    use crate::{
        init_database,
        models::{MemoryStatus, MemoryType, ScopeKind},
        open_database,
    };

    #[test]
    fn search_memory_returns_active_title_and_body_matches_and_suppresses_inactive_statuses()
    -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-active-title",
                memory_type: MemoryType::Decision,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title: "Zircon index routing decision",
                body: "Use the repo-local indexer when routing context for Rust crates.",
                path: Some("crates/memzoi-core/src/search.rs"),
                source_ref: Some("proposal://active-title"),
            },
        )?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-active-body",
                memory_type: MemoryType::Fact,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title: "Indexer body match",
                body: "The zircon token must also be found when it appears only in the body.",
                path: Some("crates/memzoi-core/src/context.rs"),
                source_ref: Some("proposal://active-body"),
            },
        )?;
        for status in [
            MemoryStatus::Superseded,
            MemoryStatus::Tombstoned,
            MemoryStatus::Redacted,
            MemoryStatus::Expired,
        ] {
            insert_memory(
                &conn,
                MemoryFixture {
                    id: status_record_id(status),
                    memory_type: MemoryType::Decision,
                    scope_kind: ScopeKind::Repo,
                    status,
                    title: "Inactive zircon memory",
                    body: "Inactive records still hit FTS but must be suppressed by default.",
                    path: Some("crates/memzoi-core/src/search.rs"),
                    source_ref: Some("proposal://inactive"),
                },
            )?;
        }

        let results = search_memory(
            &conn,
            SearchInput {
                query: "zircon".to_owned(),
                limit: 10,
                ..SearchInput::default()
            },
        )?;

        let ids = result_ids(&results)?;
        assert_eq!(
            ids,
            HashSet::from(["rec-active-title".to_owned(), "rec-active-body".to_owned(),]),
            "search should return active title/body FTS matches and suppress inactive statuses"
        );

        let json = serde_json::to_value(&results)?;
        let title_match = json
            .as_array()
            .and_then(|items| {
                items.iter().find(|item| {
                    item.get("record")
                        .and_then(|record| record.get("id"))
                        .and_then(Value::as_str)
                        == Some("rec-active-title")
                })
            })
            .expect("SearchResult JSON should include rec-active-title");
        let record = title_match
            .get("record")
            .and_then(Value::as_object)
            .expect("SearchResult JSON should expose the matched record");
        assert_eq!(
            record.get("id").and_then(Value::as_str),
            Some("rec-active-title")
        );
        assert_eq!(
            record.get("title").and_then(Value::as_str),
            Some("Zircon index routing decision")
        );
        assert_eq!(
            record
                .get("type")
                .or_else(|| record.get("memory_type"))
                .and_then(Value::as_str),
            Some("decision")
        );
        assert_eq!(
            record
                .get("scope")
                .or_else(|| record.get("scope_kind"))
                .and_then(Value::as_str),
            Some("repo")
        );
        assert_path_metadata_if_exposed(title_match, "crates/memzoi-core/src/search.rs");

        Ok(())
    }

    #[test]
    fn search_memory_filters_scope_type_and_path_prefix_before_applying_limit() -> anyhow::Result<()>
    {
        let (_temp, conn) = initialized_database()?;
        for (id, memory_type, scope_kind, path) in [
            (
                "rec-repo-decision-search",
                MemoryType::Decision,
                ScopeKind::Repo,
                "crates/search/src/lib.rs",
            ),
            (
                "rec-repo-decision-context",
                MemoryType::Decision,
                ScopeKind::Repo,
                "crates/search/src/context.rs",
            ),
            (
                "rec-wrong-type",
                MemoryType::Fact,
                ScopeKind::Repo,
                "crates/search/src/lib.rs",
            ),
            (
                "rec-wrong-scope",
                MemoryType::Decision,
                ScopeKind::Team,
                "crates/search/src/lib.rs",
            ),
            (
                "rec-wrong-path",
                MemoryType::Decision,
                ScopeKind::Repo,
                "apps/active/src/search.tsx",
            ),
        ] {
            insert_memory(
                &conn,
                MemoryFixture {
                    id,
                    memory_type,
                    scope_kind,
                    status: MemoryStatus::Active,
                    title: "Routing recall rule",
                    body: "The recall token should be filtered by scope, memory type, and path before limit is applied.",
                    path: Some(path),
                    source_ref: Some("proposal://filters"),
                },
            )?;
        }

        let unbounded = search_memory(
            &conn,
            SearchInput {
                query: "recall".to_owned(),
                scope_kind: Some(ScopeKind::Repo),
                memory_type: Some(MemoryType::Decision),
                path_prefix: Some("crates/search/".to_owned()),
                limit: 10,
                ..SearchInput::default()
            },
        )?;
        assert_eq!(
            result_ids(&unbounded)?,
            HashSet::from([
                "rec-repo-decision-search".to_owned(),
                "rec-repo-decision-context".to_owned(),
            ]),
            "scope_kind, memory_type, and path_prefix filters should exclude otherwise matching records"
        );

        let limited = search_memory(
            &conn,
            SearchInput {
                query: "recall".to_owned(),
                scope_kind: Some(ScopeKind::Repo),
                memory_type: Some(MemoryType::Decision),
                path_prefix: Some("crates/search/".to_owned()),
                limit: 1,
                ..SearchInput::default()
            },
        )?;
        assert_eq!(limited.len(), 1, "limit should cap the filtered result set");
        assert!(
            ["rec-repo-decision-search", "rec-repo-decision-context"]
                .contains(&limited[0].record.id.as_str()),
            "the limited result must still come from the filtered candidate set: {limited:?}"
        );

        Ok(())
    }

    #[test]
    fn search_memory_path_filter_matches_trailing_double_star_scope() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-web-glob-scope",
                memory_type: MemoryType::Procedure,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title: "Web glob routing recall",
                body: "Webglob scoped guidance applies when editing the React application entry point.",
                path: Some("apps/web/**"),
                source_ref: Some("issue://web-glob"),
            },
        )?;

        let results = search_memory(
            &conn,
            SearchInput {
                query: "webglob".to_owned(),
                scope_kind: Some(ScopeKind::Repo),
                path_prefix: Some("apps/web/src/App.tsx".to_owned()),
                limit: 10,
                ..SearchInput::default()
            },
        )?;

        assert_eq!(
            results.len(),
            1,
            "stored path apps/web/** should match requested file apps/web/src/App.tsx"
        );
        assert_eq!(results[0].record.id, "rec-web-glob-scope");
        assert!(
            results[0]
                .paths
                .iter()
                .any(|path| path.path == "apps/web/**"),
            "search result should expose the stored glob path: {results:?}"
        );

        Ok(())
    }

    fn initialized_database() -> anyhow::Result<(TempDir, Connection)> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("memory.db");
        let conn = open_database(&db_path)?;
        init_database(&conn)?;
        Ok((temp, conn))
    }

    #[derive(Debug, Clone, Copy)]
    struct MemoryFixture<'a> {
        id: &'a str,
        memory_type: MemoryType,
        scope_kind: ScopeKind,
        status: MemoryStatus,
        title: &'a str,
        body: &'a str,
        path: Option<&'a str>,
        source_ref: Option<&'a str>,
    }

    fn insert_memory(conn: &Connection, memory: MemoryFixture<'_>) -> anyhow::Result<()> {
        conn.execute(
            "INSERT INTO memory_record(
                id, type, scope_kind, visibility, title, body, status, confidence,
                source_kind, source_ref, content_hash, expires_at
             ) VALUES (?1, ?2, ?3, 'repo', ?4, ?5, ?6, 0.91, 'test', ?7, ?8, ?9)",
            params![
                memory.id,
                memory.memory_type.as_str(),
                memory.scope_kind.as_str(),
                memory.title,
                memory.body,
                memory.status.as_str(),
                memory.source_ref,
                format!("hash-{}", memory.id),
                if memory.status == MemoryStatus::Expired {
                    Some("2000-01-01T00:00:00.000Z")
                } else {
                    None
                },
            ],
        )?;

        if let Some(path) = memory.path {
            conn.execute(
                "INSERT INTO memory_path(id, record_id, path, line_start, line_end)
                 VALUES (?1, ?2, ?3, 3, 8)",
                params![format!("path-{}", memory.id), memory.id, path],
            )?;
        }

        Ok(())
    }

    fn status_record_id(status: MemoryStatus) -> &'static str {
        match status {
            MemoryStatus::Superseded => "rec-superseded",
            MemoryStatus::Tombstoned => "rec-tombstoned",
            MemoryStatus::Redacted => "rec-redacted",
            MemoryStatus::Expired => "rec-expired",
            _ => unreachable!("only inactive terminal statuses are fixture ids"),
        }
    }

    fn result_ids(results: &[crate::models::SearchResult]) -> anyhow::Result<HashSet<String>> {
        Ok(results
            .iter()
            .map(|result| result.record.id.clone())
            .collect())
    }

    fn assert_path_metadata_if_exposed(result: &Value, expected_prefix: &str) {
        if let Some(paths) = result.get("paths").and_then(Value::as_array) {
            assert!(
                paths.iter().any(|path| path
                    .get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|path| path.starts_with(expected_prefix))),
                "exposed SearchResult paths should include the memory_path prefix {expected_prefix}: {result}"
            );
        }

        if let Some(citations) = result.get("citations").and_then(Value::as_array) {
            assert!(
                citations.iter().any(|citation| citation
                    .get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|path| path.starts_with(expected_prefix))),
                "exposed SearchResult citations should include memory_path metadata for {expected_prefix}: {result}"
            );
        }
    }
}
