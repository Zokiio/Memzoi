use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    events::{AppendEvent, append_event, now_utc},
    models::{ContextPack, MemoryCitation, SearchResult},
    search::{SearchInput, path_matches_request, search_memory},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContextPackInput {
    pub task: String,
    pub path_prefix: Option<String>,
    pub token_budget: Option<usize>,
}

pub fn build_context_pack(conn: &Connection, input: ContextPackInput) -> Result<ContextPack> {
    let mut results = search_memory(
        conn,
        SearchInput {
            query: input.task.clone(),
            limit: 50,
            include_inactive: false,
            ..SearchInput::default()
        },
    )?;

    if let Some(path_prefix) = input.path_prefix.as_deref() {
        results.sort_by(|left, right| {
            let left_match = result_matches_path(left, path_prefix);
            let right_match = result_matches_path(right, path_prefix);
            right_match
                .cmp(&left_match)
                .then_with(|| right.score.total_cmp(&left.score))
                .then_with(|| left.record.id.cmp(&right.record.id))
        });
    }

    let budget = input.token_budget.unwrap_or(400).max(1);
    let selected = select_for_budget(results, budget);
    let citations = selected.iter().map(primary_citation).collect::<Vec<_>>();
    let prompt = render_prompt(&selected, &citations, budget);

    let pack = ContextPack {
        id: format!("ctx_{}", Uuid::now_v7()),
        task: input.task.clone(),
        prompt,
        records: selected,
        citations,
        token_budget: input.token_budget,
        created_at: now_utc()?,
    };

    append_event(
        conn,
        AppendEvent {
            event_type: "memory.context_pack_built".to_owned(),
            actor: "memzoi-core".to_owned(),
            payload: json!({
                "context_pack_id": pack.id,
                "task": input.task,
                "path_prefix": input.path_prefix,
                "token_budget": input.token_budget,
                "record_ids": pack.records.iter().map(|result| result.record.id.as_str()).collect::<Vec<_>>(),
            }),
            record_id: None,
            proposal_id: None,
        },
    )?;

    Ok(pack)
}

fn result_matches_path(result: &SearchResult, path_prefix: &str) -> bool {
    result
        .paths
        .iter()
        .any(|path| path_matches_request(&path.path, path_prefix))
}

fn select_for_budget(results: Vec<SearchResult>, budget: usize) -> Vec<SearchResult> {
    let mut selected = Vec::new();
    let mut used = 0usize;

    for result in results {
        let estimate =
            estimate_words(&result.record.title) + estimate_words(&result.record.body) + 6;
        if selected.is_empty() || used + estimate <= budget {
            used += estimate;
            selected.push(result);
        }
        if used >= budget {
            break;
        }
    }

    selected
}

fn primary_citation(result: &SearchResult) -> MemoryCitation {
    result.citations.first().cloned().unwrap_or(MemoryCitation {
        record_id: result.record.id.clone(),
        memory_type: result.record.memory_type,
        scope_kind: result.record.scope_kind,
        source_ref: result.record.source_ref.clone(),
        path: result.paths.first().map(|path| path.path.clone()),
    })
}

fn render_prompt(results: &[SearchResult], citations: &[MemoryCitation], budget: usize) -> String {
    let mut lines = vec!["# Memzoi Context".to_owned()];

    for (result, citation) in results.iter().zip(citations) {
        let line = format!(
            "- [{}] ({}/{}) {}: {}\n  Source: {}",
            citation.record_id,
            result.record.memory_type.as_str(),
            result.record.scope_kind.as_str(),
            result.record.title,
            result.record.body,
            citation.source_ref.as_deref().unwrap_or("unknown"),
        );
        lines.push(line);
    }

    truncate_words(&lines.join("\n"), budget)
}

fn estimate_words(text: &str) -> usize {
    text.split_whitespace().count()
}

fn truncate_words(text: &str, budget: usize) -> String {
    let mut words = text.split_whitespace().collect::<Vec<_>>();
    if words.len() <= budget {
        return text.to_owned();
    }
    words.truncate(budget);
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};
    use serde_json::Value;
    use tempfile::TempDir;

    use super::{ContextPackInput, build_context_pack};
    use crate::{
        init_database,
        models::{MemoryStatus, MemoryType, ScopeKind},
        open_database,
    };

    #[test]
    fn build_context_pack_returns_task_relevant_active_records_with_citations() -> anyhow::Result<()>
    {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-active-routing",
                memory_type: MemoryType::Decision,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title: "Zircon routing decision",
                body: "Use the shard-local Rust index when routing zircon context packs.",
                path: Some("crates/memzoi-core/src/context.rs"),
                source_ref: Some("issue://4242#decision"),
            },
        )?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-tombstoned-routing",
                memory_type: MemoryType::Decision,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Tombstoned,
                title: "Obsolete zircon routing decision",
                body: "This inactive memory still matches zircon routing but must not enter a pack.",
                path: Some("crates/memzoi-core/src/context.rs"),
                source_ref: Some("issue://old"),
            },
        )?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-unrelated-active",
                memory_type: MemoryType::Fact,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title: "Unrelated release note",
                body: "The release process uses signed artifacts and staged rollout gates.",
                path: Some("crates/memzoi-core/src/release.rs"),
                source_ref: Some("issue://release"),
            },
        )?;

        let pack = build_context_pack(
            &conn,
            ContextPackInput {
                task: "Implement zircon routing context for the Rust indexer".to_owned(),
                path_prefix: Some("crates/memzoi-core/src/context.rs".to_owned()),
                token_budget: Some(160),
            },
        )?;
        let json = serde_json::to_value(&pack)?;

        let ids = record_ids_from_pack(&json);
        assert!(
            ids.iter().any(|id| id == "rec-active-routing"),
            "task-relevant active record should be included in the pack: {json}"
        );
        assert!(
            !ids.iter().any(|id| id == "rec-tombstoned-routing"),
            "inactive records must be suppressed from context packs: {json}"
        );

        let citation = citation_for_record(&json, "rec-active-routing").unwrap_or_else(|| {
            panic!("pack should cite included record rec-active-routing: {json}")
        });
        assert_json_string_field(citation, &["record_id", "id"], "rec-active-routing");
        assert_json_string_field(citation, &["type", "memory_type"], "decision");
        assert_json_string_field(citation, &["scope", "scope_kind"], "repo");
        assert_json_string_field(citation, &["source_ref"], "issue://4242#decision");

        Ok(())
    }

    #[test]
    fn build_context_pack_honors_token_budget_and_prioritizes_path_relevant_records()
    -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-path-relevant",
                memory_type: MemoryType::Procedure,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title: "Zircon context path procedure",
                body: "When editing context.rs, start from path-bound zircon guidance before global recall.",
                path: Some("crates/memzoi-core/src/context.rs"),
                source_ref: Some("issue://path-relevant"),
            },
        )?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-unrelated-path",
                memory_type: MemoryType::Procedure,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title: "Zircon global procedure",
                body: "Global zircon recall describes background preferences for unrelated surfaces and should rank after path-bound guidance when a path is supplied.",
                path: Some("crates/memzoi-cli/src/main.rs"),
                source_ref: Some("issue://global"),
            },
        )?;

        let pack = build_context_pack(
            &conn,
            ContextPackInput {
                task: "Need zircon context procedure while editing context.rs".to_owned(),
                path_prefix: Some("crates/memzoi-core/src/context.rs".to_owned()),
                token_budget: Some(40),
            },
        )?;
        let json = serde_json::to_value(&pack)?;
        let ids = record_ids_from_pack(&json);

        assert_eq!(
            ids.first().map(String::as_str),
            Some("rec-path-relevant"),
            "path-relevant memory should rank ahead of unrelated matching memory when path is supplied: {json}"
        );

        let prompt = prompt_text(&json).unwrap_or_else(|| {
            panic!("context pack JSON should include prompt-ready text: {json}")
        });
        let approximate_tokens = prompt.split_whitespace().count();
        assert!(
            approximate_tokens <= 60,
            "token_budget should cap prompt-ready text approximately; got {approximate_tokens} words for budget 40: {prompt:?}"
        );
        assert!(
            prompt.contains("path-bound zircon guidance")
                || prompt.contains("Zircon context path procedure"),
            "prompt-ready text should include the path-relevant memory content: {prompt:?}"
        );

        Ok(())
    }

    #[test]
    fn build_context_pack_prioritizes_trailing_double_star_path_for_requested_file()
    -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        for (id, path) in [
            ("rec-a-distractor", "packages/server/src/App.tsx"),
            ("rec-z-web-glob", "apps/web/**"),
        ] {
            insert_memory(
                &conn,
                MemoryFixture {
                    id,
                    memory_type: MemoryType::Procedure,
                    scope_kind: ScopeKind::Repo,
                    status: MemoryStatus::Active,
                    title: "Web glob ranking procedure",
                    body: "Webglob ranking guidance applies while editing the React application entry point.",
                    path: Some(path),
                    source_ref: Some("issue://web-glob-ranking"),
                },
            )?;
        }

        let pack = build_context_pack(
            &conn,
            ContextPackInput {
                task: "Need webglob ranking guidance for App.tsx".to_owned(),
                path_prefix: Some("apps/web/src/App.tsx".to_owned()),
                token_budget: Some(80),
            },
        )?;
        let json = serde_json::to_value(&pack)?;
        let ids = record_ids_from_pack(&json);

        assert_eq!(
            ids.first().map(String::as_str),
            Some("rec-z-web-glob"),
            "stored path apps/web/** should rank ahead of an otherwise equal distractor for apps/web/src/App.tsx: {json}"
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
                source_kind, source_ref, content_hash
             ) VALUES (?1, ?2, ?3, 'repo', ?4, ?5, ?6, 0.88, 'test', ?7, ?8)",
            params![
                memory.id,
                memory.memory_type.as_str(),
                memory.scope_kind.as_str(),
                memory.title,
                memory.body,
                memory.status.as_str(),
                memory.source_ref,
                format!("hash-{}", memory.id),
            ],
        )?;

        if let Some(path) = memory.path {
            conn.execute(
                "INSERT INTO memory_path(id, record_id, path, line_start, line_end)
                 VALUES (?1, ?2, ?3, 11, 19)",
                params![format!("path-{}", memory.id), memory.id, path],
            )?;
        }

        Ok(())
    }

    fn record_ids_from_pack(json: &Value) -> Vec<String> {
        json.get("records")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("context pack JSON should include records array: {json}"))
            .iter()
            .map(record_id_from_value)
            .collect()
    }

    fn record_id_from_value(value: &Value) -> String {
        value
            .get("record")
            .and_then(|record| record.get("id"))
            .or_else(|| value.get("record_id"))
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("record entry should expose a record id: {value}"))
            .to_owned()
    }

    fn citation_for_record<'a>(json: &'a Value, record_id: &str) -> Option<&'a Value> {
        json.get("citations")
            .and_then(Value::as_array)?
            .iter()
            .find(|citation| {
                citation
                    .get("record_id")
                    .or_else(|| citation.get("id"))
                    .and_then(Value::as_str)
                    == Some(record_id)
            })
    }

    fn assert_json_string_field(value: &Value, keys: &[&str], expected: &str) {
        let actual = keys
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_str));
        assert_eq!(
            actual,
            Some(expected),
            "expected one of {keys:?} to equal {expected:?} in {value}"
        );
    }

    fn prompt_text(json: &Value) -> Option<&str> {
        ["prompt", "prompt_text", "context", "text", "rendered"]
            .into_iter()
            .find_map(|key| json.get(key).and_then(Value::as_str))
    }
}
