use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    events::{AppendEvent, append_event, now_utc},
    models::{
        ContextPack, ContextPackBudget, ContextPackIncludedItem, ContextPackOmittedItem,
        ContextPackPolicy, ContextPackWarning, MemoryCitation, MemoryDestination, MemoryLane,
        MemoryPath, MemoryRecord, MemoryType, SearchRanking, SearchRankingSignals, SearchResult,
    },
    search::{
        SearchInput, citation_for, load_paths, path_matches_request, record_from_row, search_memory,
    },
};

const MAX_OMITTED_ITEMS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContextPackInput {
    pub task: String,
    pub path_prefix: Option<String>,
    pub token_budget: Option<usize>,
    pub include_local: bool,
    pub include_session: bool,
}

pub fn build_context_pack(conn: &Connection, input: ContextPackInput) -> Result<ContextPack> {
    let path_prefix = input
        .path_prefix
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned);
    let requested_destinations = requested_destinations(&input);
    let mut candidates = HashMap::<String, SearchResult>::new();

    for destination in &requested_destinations {
        for result in search_memory(
            conn,
            SearchInput {
                query: input.task.clone(),
                destination: Some(*destination),
                limit: 50,
                include_inactive: false,
                ..SearchInput::default()
            },
        )? {
            insert_candidate(&mut candidates, result);
        }
    }

    if let Some(path_prefix) = path_prefix.as_deref() {
        for destination in &requested_destinations {
            for result in path_candidates(conn, *destination, path_prefix, 50)? {
                insert_candidate(&mut candidates, result);
            }
        }
    }

    let ranked = candidates
        .into_values()
        .map(|mut result| {
            let ranking = rank_result(&result, path_prefix.as_deref());
            result.score = ranking.score;
            result.rationale = Some(ranking.reasons.join("; "));
            result.ranking = Some(ranking);
            result
        })
        .collect::<Vec<_>>();

    let mut ranked = deduplicate_candidates(ranked);
    ranked.sort_by(compare_ranked_results);

    let budget = input.token_budget.unwrap_or(400).max(1);
    let candidate_records = ranked.len();
    let selection = select_for_budget(ranked, budget);
    let citations = selection
        .selected
        .iter()
        .map(|item| primary_citation(&item.result))
        .collect::<Vec<_>>();
    let selected = selection
        .selected
        .iter()
        .map(|item| item.result.clone())
        .collect::<Vec<_>>();
    let (prompt, estimated_used, truncated) = render_prompt(&selected, &citations, budget);
    let included = included_items(&selection.selected, &citations);
    let omitted = omitted_items(&selection.omitted);
    let warnings: Vec<ContextPackWarning> = Vec::new();
    let included_destinations = included_destinations(&selected, &requested_destinations);

    let pack = ContextPack {
        id: format!("ctx_{}", Uuid::now_v7()),
        task: input.task.clone(),
        prompt,
        records: selected,
        citations,
        token_budget: input.token_budget,
        policy: ContextPackPolicy {
            include_local: input.include_local,
            include_session: input.include_session,
            requested_destinations: requested_destinations.clone(),
            included_destinations,
        },
        budget: ContextPackBudget {
            requested: input.token_budget,
            effective: budget,
            estimated_used,
            estimate_unit: "approx_words".to_owned(),
            candidate_records,
            selected_records: 0,
            rendered_words: estimated_used,
            truncated,
        },
        included,
        omitted,
        warnings,
        next_queries: Vec::new(),
        created_at: now_utc()?,
    };
    let mut pack = pack;
    pack.budget.selected_records = pack.records.len();

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
                "include_local": input.include_local,
                "include_session": input.include_session,
                "requested_destinations": pack.policy.requested_destinations.iter().map(|destination| destination.as_str()).collect::<Vec<_>>(),
                "included_destinations": pack.policy.included_destinations.iter().map(|destination| destination.as_str()).collect::<Vec<_>>(),
                "candidate_records": pack.budget.candidate_records,
                "selected_records": pack.budget.selected_records,
                "record_ids": pack.records.iter().map(|result| result.record.id.as_str()).collect::<Vec<_>>(),
            }),
            record_id: None,
            proposal_id: None,
        },
    )?;

    Ok(pack)
}

#[derive(Debug, Clone)]
struct SelectedContextItem {
    result: SearchResult,
    estimated_size: usize,
}

#[derive(Debug, Clone)]
struct ContextSelection {
    selected: Vec<SelectedContextItem>,
    omitted: Vec<SelectedContextItem>,
}

fn requested_destinations(input: &ContextPackInput) -> Vec<MemoryDestination> {
    let mut destinations = vec![MemoryDestination::Repo];
    if input.include_local {
        destinations.push(MemoryDestination::Local);
    }
    if input.include_session {
        destinations.push(MemoryDestination::Session);
    }
    destinations
}

fn insert_candidate(candidates: &mut HashMap<String, SearchResult>, result: SearchResult) {
    candidates
        .entry(result.record.id.clone())
        .and_modify(|existing| {
            if result_has_fts_match(&result) && !result_has_fts_match(existing) {
                *existing = result.clone();
            }
        })
        .or_insert(result);
}

fn path_candidates(
    conn: &Connection,
    destination: MemoryDestination,
    path_prefix: &str,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let requested_path = path_prefix.trim().trim_end_matches('/').to_owned();
    if requested_path.is_empty() {
        return Ok(Vec::new());
    }
    let path_like = format!("{}/%", requested_path);
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT memory_record.id, memory_record.type, memory_record.lane,
                    memory_record.destination, memory_record.scope_kind, memory_record.scope_id,
                    memory_record.visibility, memory_record.title, memory_record.body,
                    memory_record.status, memory_record.confidence, memory_record.source_kind,
                    memory_record.source_ref, memory_record.content_hash, memory_record.created_at,
                    memory_record.updated_at, memory_record.supersedes_id, memory_record.expires_at
             FROM memory_path
             JOIN memory_record ON memory_record.id = memory_path.record_id
             WHERE memory_record.status = 'active'
               AND memory_record.destination = ?1
               AND (
                 memory_path.path = ?2
                 OR memory_path.path LIKE ?3
                 OR ?2 LIKE memory_path.path || '/%'
                 OR (
                     memory_path.path LIKE '%/**'
                     AND (
                         ?2 = substr(memory_path.path, 1, length(memory_path.path) - 3)
                         OR ?2 LIKE substr(memory_path.path, 1, length(memory_path.path) - 2) || '%'
                     )
                 )
                 OR (
                     ?2 LIKE '%/**'
                     AND (
                         memory_path.path = substr(?2, 1, length(?2) - 3)
                         OR memory_path.path LIKE substr(?2, 1, length(?2) - 2) || '%'
                     )
                 )
               )
             ORDER BY memory_record.updated_at DESC, memory_record.id ASC
             LIMIT ?4",
        )
        .context("failed to prepare path-scoped context candidate query")?;
    let rows = stmt
        .query_map(
            params![
                destination.as_str(),
                requested_path,
                path_like,
                limit as i64
            ],
            record_from_row,
        )
        .context("failed to execute path-scoped context candidate query")?;

    let mut results = Vec::new();
    for row in rows {
        let record = row.context("failed to read path-scoped context candidate")?;
        let paths = load_paths(conn, record.id.as_str())?;
        if !paths
            .iter()
            .any(|path| path_matches_request(&path.path, path_prefix))
        {
            continue;
        }
        let citation = citation_for(&record, paths.first());
        results.push(SearchResult {
            record,
            score: 0.0,
            snippet: None,
            rationale: Some("path match".to_owned()),
            ranking: None,
            paths,
            citations: vec![citation],
        });
    }

    Ok(results)
}

fn rank_result(result: &SearchResult, requested_path: Option<&str>) -> SearchRanking {
    let fts_match = result_has_fts_match(result);
    let fts_score = if fts_match {
        normalized_fts_score(result.score)
    } else {
        0.0
    };
    let path_score = requested_path
        .map(|path| best_path_score(&result.paths, path))
        .unwrap_or(0);
    let type_priority = memory_type_priority(result.record.memory_type);
    let lane_priority = lane_priority(result.record.lane);
    let destination_priority = destination_priority(result.record.destination);
    let confidence = normalized_confidence(result.record.confidence);
    let score = path_score as f64 * 10_000.0
        + type_priority as f64 * 1_000.0
        + if fts_match { 500.0 } else { 0.0 }
        + lane_priority as f64 * 100.0
        + destination_priority as f64 * 10.0
        + confidence * 25.0
        + fts_score;

    let mut reasons = Vec::new();
    if fts_match {
        reasons.push("task text matched title/body".to_owned());
    }
    if requested_path.is_some() {
        if path_score > 0 {
            reasons.push("path matched requested path".to_owned());
        } else {
            reasons.push("no path match".to_owned());
        }
    }
    reasons.push(format!(
        "{} memory type priority",
        result.record.memory_type.as_str()
    ));
    reasons.push(format!(
        "{} destination",
        result.record.destination.as_str()
    ));

    SearchRanking {
        score,
        signals: SearchRankingSignals {
            fts_match,
            fts_score,
            path_score,
            type_priority,
            lane_priority,
            destination_priority,
            confidence,
        },
        reasons,
    }
}

fn result_has_fts_match(result: &SearchResult) -> bool {
    result
        .rationale
        .as_deref()
        .is_some_and(|rationale| rationale.contains("fts5"))
}

fn normalized_fts_score(score: f64) -> f64 {
    if score.is_finite() {
        score.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

fn normalized_confidence(confidence: f64) -> f64 {
    if confidence.is_finite() {
        confidence.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn best_path_score(paths: &[MemoryPath], requested_path: &str) -> i64 {
    paths
        .iter()
        .map(|path| path_score(&path.path, requested_path))
        .max()
        .unwrap_or(0)
}

fn path_score(stored_path: &str, requested_path: &str) -> i64 {
    let stored_path = stored_path.trim().trim_end_matches('/');
    let requested_path = requested_path.trim().trim_end_matches('/');
    if stored_path.is_empty() || requested_path.is_empty() {
        return 0;
    }
    if stored_path == requested_path {
        return 5;
    }
    if let Some(base) = stored_path.strip_suffix("/**") {
        return if path_is_or_is_under(requested_path, base) {
            4
        } else {
            0
        };
    }
    if let Some(base) = requested_path.strip_suffix("/**") {
        return if path_is_or_is_under(stored_path, base) {
            4
        } else {
            0
        };
    }
    if path_is_or_is_under(requested_path, stored_path) {
        return 3;
    }
    if path_is_or_is_under(stored_path, requested_path) {
        return 2;
    }
    0
}

fn path_is_or_is_under(path: &str, base: &str) -> bool {
    path == base
        || path
            .strip_prefix(base)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn memory_type_priority(memory_type: MemoryType) -> i64 {
    match memory_type {
        MemoryType::Risk => 9,
        MemoryType::Warning => 8,
        MemoryType::FailedAttempt => 7,
        MemoryType::Decision => 6,
        MemoryType::Procedure => 5,
        MemoryType::Preference => 4,
        MemoryType::Fact => 3,
        MemoryType::Episode | MemoryType::Relationship => 2,
        MemoryType::InstructionProjection => 1,
    }
}

fn lane_priority(lane: MemoryLane) -> i64 {
    match lane {
        MemoryLane::Procedural | MemoryLane::Session => 3,
        MemoryLane::Semantic => 2,
        MemoryLane::Episodic => 1,
    }
}

fn destination_priority(destination: MemoryDestination) -> i64 {
    match destination {
        MemoryDestination::Repo => 3,
        MemoryDestination::Local => 2,
        MemoryDestination::Session => 1,
        MemoryDestination::Discard | MemoryDestination::NeedsReview => 0,
    }
}

fn deduplicate_candidates(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let by_hash = best_by_key(results, |result| {
        Some(format!("hash:{}", result.record.content_hash))
    });
    best_by_key(by_hash, |result| {
        Some(format!(
            "text:{}\n{}",
            normalize_text(&result.record.title),
            normalize_text(&result.record.body)
        ))
    })
}

fn best_by_key<F>(results: Vec<SearchResult>, key_for: F) -> Vec<SearchResult>
where
    F: Fn(&SearchResult) -> Option<String>,
{
    let mut best = BTreeMap::<String, SearchResult>::new();
    let mut unkeyed = Vec::new();
    for result in results {
        let Some(key) = key_for(&result) else {
            unkeyed.push(result);
            continue;
        };
        best.entry(key)
            .and_modify(|existing| {
                if compare_ranked_results(&result, existing) == Ordering::Less {
                    *existing = result.clone();
                }
            })
            .or_insert(result);
    }
    unkeyed.extend(best.into_values());
    unkeyed
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn compare_ranked_results(left: &SearchResult, right: &SearchResult) -> Ordering {
    ranking_score(right)
        .total_cmp(&ranking_score(left))
        .then_with(|| {
            destination_priority(right.record.destination)
                .cmp(&destination_priority(left.record.destination))
        })
        .then_with(|| right.record.updated_at.cmp(&left.record.updated_at))
        .then_with(|| left.record.id.cmp(&right.record.id))
}

fn ranking_score(result: &SearchResult) -> f64 {
    result
        .ranking
        .as_ref()
        .map(|ranking| ranking.score)
        .unwrap_or(result.score)
}

fn select_for_budget(results: Vec<SearchResult>, budget: usize) -> ContextSelection {
    let mut selected = Vec::new();
    let mut omitted = Vec::new();
    let mut used = 0usize;

    for result in results {
        let estimate = estimate_result(&result);
        let item = SelectedContextItem {
            result,
            estimated_size: estimate,
        };
        if selected.is_empty() || used + estimate <= budget {
            used += estimate;
            selected.push(item);
        } else {
            omitted.push(item);
        }
    }

    ContextSelection { selected, omitted }
}

fn included_items(
    selected: &[SelectedContextItem],
    citations: &[MemoryCitation],
) -> Vec<ContextPackIncludedItem> {
    selected
        .iter()
        .zip(citations)
        .map(|(item, citation)| ContextPackIncludedItem {
            record_id: item.result.record.id.clone(),
            title: item.result.record.title.clone(),
            memory_type: item.result.record.memory_type,
            lane: item.result.record.lane,
            scope_kind: item.result.record.scope_kind,
            path: item.result.paths.first().map(|path| path.path.clone()),
            citation: citation.clone(),
            provenance: item.result.record.destination,
            destination: item.result.record.destination,
            score: item.result.score,
            rationale: item.result.rationale.clone(),
            estimated_size: item.estimated_size,
        })
        .collect()
}

fn omitted_items(omitted: &[SelectedContextItem]) -> Vec<ContextPackOmittedItem> {
    omitted
        .iter()
        .take(MAX_OMITTED_ITEMS)
        .map(|item| ContextPackOmittedItem {
            record_id: item.result.record.id.clone(),
            title: item.result.record.title.clone(),
            memory_type: item.result.record.memory_type,
            lane: item.result.record.lane,
            destination: item.result.record.destination,
            estimated_size: item.estimated_size,
            reason: "budget_exceeded".to_owned(),
        })
        .collect()
}

fn primary_citation(result: &SearchResult) -> MemoryCitation {
    result.citations.first().cloned().unwrap_or(MemoryCitation {
        record_id: result.record.id.clone(),
        memory_type: result.record.memory_type,
        scope_kind: result.record.scope_kind,
        destination: result.record.destination,
        visibility: result.record.visibility,
        source_kind: result.record.source_kind.clone(),
        source_ref: result.record.source_ref.clone(),
        path: result.paths.first().map(|path| path.path.clone()),
    })
}

fn estimate_result(result: &SearchResult) -> usize {
    estimate_words(&result.record.title) + estimate_words(&result.record.body) + 6
}

fn render_prompt(
    results: &[SearchResult],
    citations: &[MemoryCitation],
    budget: usize,
) -> (String, usize, bool) {
    let mut lines = vec!["# Memzoi Context".to_owned()];

    for (result, citation) in results.iter().zip(citations) {
        lines.push(render_line(result, citation));
    }

    let rendered = lines.join("\n");
    let rendered_words = estimate_words(&rendered);
    if rendered_words <= budget {
        return (rendered, rendered_words, false);
    }
    let truncated = truncate_words(&rendered, budget);
    let truncated_words = estimate_words(&truncated);
    (truncated, truncated_words, true)
}

fn render_line(result: &SearchResult, citation: &MemoryCitation) -> String {
    let provenance = if result.record.destination == MemoryDestination::Repo {
        result.record.scope_kind.as_str().to_owned()
    } else {
        format!(
            "{}; destination={}",
            result.record.scope_kind.as_str(),
            result.record.destination.as_str()
        )
    };
    format!(
        "- [{}] ({}/{}) {}: {}\n  Source: {}",
        citation.record_id,
        result.record.memory_type.as_str(),
        provenance,
        result.record.title,
        result.record.body,
        citation
            .source_ref
            .as_deref()
            .unwrap_or_else(|| source_label(&result.record)),
    )
}

fn source_label(record: &MemoryRecord) -> &str {
    match record.destination {
        MemoryDestination::Repo => "unknown",
        MemoryDestination::Local => "local memory",
        MemoryDestination::Session => "session memory",
        MemoryDestination::Discard | MemoryDestination::NeedsReview => "non-repo memory",
    }
}

fn included_destinations(
    results: &[SearchResult],
    requested_destinations: &[MemoryDestination],
) -> Vec<MemoryDestination> {
    requested_destinations
        .iter()
        .copied()
        .filter(|destination| {
            results
                .iter()
                .any(|result| result.record.destination == *destination)
        })
        .collect()
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
        models::{MemoryDestination, MemoryLane, MemoryStatus, MemoryType, ScopeKind},
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
                include_local: false,
                include_session: false,
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
                title: "Need zircon context path procedure",
                body: "When editing context.rs, start from path-bound zircon context procedure guidance before global recall.",
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
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-z-path-over-budget",
                memory_type: MemoryType::Procedure,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title: "Zircon supplemental path note",
                body: "Supplemental zircon recall guidance contains enough words to exceed the remaining context budget after the primary path-bound procedure has already been selected for rendering.",
                path: Some("crates/memzoi-core/src/context.rs"),
                source_ref: Some("issue://path-over-budget"),
            },
        )?;

        let pack = build_context_pack(
            &conn,
            ContextPackInput {
                task: "Need zircon context procedure while editing context.rs".to_owned(),
                path_prefix: Some("crates/memzoi-core/src/context.rs".to_owned()),
                token_budget: Some(40),
                include_local: false,
                include_session: false,
            },
        )?;
        let json = serde_json::to_value(&pack)?;
        let ids = record_ids_from_pack(&json);

        assert_eq!(json["budget"]["requested"], 40);
        assert_eq!(json["budget"]["effective"], 40);
        assert_eq!(json["budget"]["estimate_unit"], "approx_words");
        assert!(
            json["budget"]["estimated_used"]
                .as_u64()
                .is_some_and(|used| used > 0),
            "context JSON should include estimated budget use: {json}"
        );

        let included = json["included"]
            .as_array()
            .unwrap_or_else(|| panic!("context JSON should include included metadata: {json}"));
        assert_eq!(
            included
                .first()
                .and_then(|item| item.get("record_id"))
                .and_then(Value::as_str),
            Some("rec-path-relevant"),
            "included metadata should preserve selected record order: {json}"
        );
        assert_eq!(included[0]["type"], "procedure");
        assert_eq!(included[0]["lane"], "semantic");
        assert_eq!(included[0]["provenance"], "repo");
        assert_eq!(included[0]["destination"], "repo");
        assert_eq!(included[0]["citation"]["record_id"], "rec-path-relevant");
        assert_eq!(
            included[0]["path"], "crates/memzoi-core/src/context.rs",
            "included metadata should expose selected path provenance: {json}"
        );

        let omitted = json["omitted"]
            .as_array()
            .unwrap_or_else(|| panic!("context JSON should include omitted metadata: {json}"));
        assert!(
            omitted.iter().any(|item| {
                item.get("record_id").and_then(Value::as_str) == Some("rec-z-path-over-budget")
                    && item.get("reason").and_then(Value::as_str) == Some("budget_exceeded")
                    && item.get("destination").and_then(Value::as_str) == Some("repo")
            }),
            "budget-excluded repo records should be listed as omitted metadata: {json}"
        );

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
            prompt.contains("path-bound zircon context procedure guidance")
                || prompt.contains("Need zircon context path procedure"),
            "prompt-ready text should include the path-relevant memory content: {prompt:?}"
        );

        Ok(())
    }

    #[test]
    fn build_context_pack_excludes_runtime_matches_without_exposing_content_or_counts()
    -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-repo-zircon",
                memory_type: MemoryType::Decision,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title: "Zircon repo decision",
                body: "Repo zircon memory may appear in global context.",
                path: Some("crates/memzoi-core/src/context.rs"),
                source_ref: Some("issue://repo"),
            },
        )?;
        insert_memory_with_destination(
            &conn,
            MemoryFixture {
                id: "rec-local-zircon",
                memory_type: MemoryType::Fact,
                scope_kind: ScopeKind::Personal,
                status: MemoryStatus::Active,
                title: "Local zircon private title",
                body: "Local zircon private body must never leak into global context packs.",
                path: Some("crates/memzoi-core/src/context.rs"),
                source_ref: Some("local://private"),
            },
            MemoryDestination::Local,
            MemoryLane::Semantic,
        )?;
        insert_memory_with_destination(
            &conn,
            MemoryFixture {
                id: "rec-session-zircon",
                memory_type: MemoryType::Episode,
                scope_kind: ScopeKind::Personal,
                status: MemoryStatus::Active,
                title: "Session zircon private title",
                body: "Session zircon checkpoint body must never leak into global context packs.",
                path: Some("crates/memzoi-core/src/context.rs"),
                source_ref: Some("session://private"),
            },
            MemoryDestination::Session,
            MemoryLane::Session,
        )?;

        let pack = build_context_pack(
            &conn,
            ContextPackInput {
                task: "Need zircon context".to_owned(),
                path_prefix: Some("crates/memzoi-core/src/context.rs".to_owned()),
                token_budget: Some(160),
                include_local: false,
                include_session: false,
            },
        )?;
        let json = serde_json::to_value(&pack)?;
        let rendered = serde_json::to_string(&json)?;

        assert!(
            !rendered.contains("Local zircon private")
                && !rendered.contains("Session zircon private")
                && !rendered.contains("local://private")
                && !rendered.contains("session://private"),
            "runtime memory content and refs must not leak into global context JSON: {json}"
        );
        let warnings = json["warnings"]
            .as_array()
            .unwrap_or_else(|| panic!("context JSON should include warnings: {json}"));
        assert!(
            warnings.is_empty(),
            "runtime memory must not be counted or exposed unless explicitly opted in: {json}"
        );
        assert_eq!(
            record_ids_from_pack(&json),
            vec!["rec-repo-zircon".to_owned()],
            "global context records should remain repo-only: {json}"
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
                include_local: false,
                include_session: false,
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

    #[test]
    fn build_context_pack_includes_local_and_session_only_when_explicitly_allowed()
    -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-layered-repo",
                memory_type: MemoryType::Decision,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title: "Layered alpha repo decision",
                body: "Layered alpha context includes repo memory by default.",
                path: None,
                source_ref: Some("issue://layered-repo"),
            },
        )?;
        insert_memory_with_destination(
            &conn,
            MemoryFixture {
                id: "rec-layered-local",
                memory_type: MemoryType::Preference,
                scope_kind: ScopeKind::Personal,
                status: MemoryStatus::Active,
                title: "Layered alpha local preference",
                body: "Layered alpha context includes local memory only when requested.",
                path: None,
                source_ref: Some("local://layered"),
            },
            MemoryDestination::Local,
            MemoryLane::Semantic,
        )?;
        insert_memory_with_destination(
            &conn,
            MemoryFixture {
                id: "rec-layered-session",
                memory_type: MemoryType::Episode,
                scope_kind: ScopeKind::Personal,
                status: MemoryStatus::Active,
                title: "Layered alpha session checkpoint",
                body: "Layered alpha context includes session memory only when requested.",
                path: None,
                source_ref: Some("session://layered"),
            },
            MemoryDestination::Session,
            MemoryLane::Session,
        )?;

        let default_pack = build_context_pack(
            &conn,
            ContextPackInput {
                task: "layered alpha context".to_owned(),
                path_prefix: None,
                token_budget: Some(200),
                include_local: false,
                include_session: false,
            },
        )?;
        let default_json = serde_json::to_value(&default_pack)?;
        assert_eq!(
            record_ids_from_pack(&default_json),
            vec!["rec-layered-repo".to_owned()],
            "context should be repo-only by default: {default_json}"
        );
        assert_eq!(
            default_json["policy"]["requested_destinations"],
            serde_json::json!(["repo"])
        );

        let layered_pack = build_context_pack(
            &conn,
            ContextPackInput {
                task: "layered alpha context".to_owned(),
                path_prefix: None,
                token_budget: Some(240),
                include_local: true,
                include_session: true,
            },
        )?;
        let layered_json = serde_json::to_value(&layered_pack)?;
        let ids = record_ids_from_pack(&layered_json);
        assert!(ids.iter().any(|id| id == "rec-layered-repo"));
        assert!(ids.iter().any(|id| id == "rec-layered-local"));
        assert!(ids.iter().any(|id| id == "rec-layered-session"));
        assert_eq!(
            layered_json["policy"]["requested_destinations"],
            serde_json::json!(["repo", "local", "session"])
        );
        let prompt = prompt_text(&layered_json).unwrap_or_else(|| {
            panic!("context pack JSON should include prompt-ready text: {layered_json}")
        });
        assert!(
            prompt.contains("destination=local") && prompt.contains("destination=session"),
            "prompt should label non-repo memory provenance: {prompt:?}"
        );

        Ok(())
    }

    #[test]
    fn build_context_pack_surfaces_path_scoped_governance_memory_without_fts_match()
    -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        for (id, memory_type, title) in [
            ("rec-path-risk", MemoryType::Risk, "Tax calculation risk"),
            (
                "rec-path-warning",
                MemoryType::Warning,
                "Totals update warning",
            ),
            (
                "rec-path-failed-attempt",
                MemoryType::FailedAttempt,
                "Previous totals attempt",
            ),
            ("rec-path-fact", MemoryType::Fact, "Invoice rounding fact"),
        ] {
            insert_memory(
                &conn,
                MemoryFixture {
                    id,
                    memory_type,
                    scope_kind: ScopeKind::Repo,
                    status: MemoryStatus::Active,
                    title,
                    body: "Changing this file previously broke production totals.",
                    path: Some("apps/api/src/billing/invoice.rs"),
                    source_ref: Some("issue://billing-governance"),
                },
            )?;
        }

        let pack = build_context_pack(
            &conn,
            ContextPackInput {
                task: "change invoice rounding".to_owned(),
                path_prefix: Some("apps/api/src/billing/invoice.rs".to_owned()),
                token_budget: Some(240),
                include_local: false,
                include_session: false,
            },
        )?;
        let json = serde_json::to_value(&pack)?;
        let ids = record_ids_from_pack(&json);
        assert_eq!(
            ids.iter().take(3).map(String::as_str).collect::<Vec<_>>(),
            vec![
                "rec-path-risk",
                "rec-path-warning",
                "rec-path-failed-attempt"
            ],
            "path-scoped governance records should rank above lower-priority facts: {json}"
        );
        let first = json["records"]
            .as_array()
            .and_then(|records| records.first())
            .unwrap_or_else(|| panic!("context records should include first record: {json}"));
        assert_eq!(first["ranking"]["signals"]["path_score"].as_i64(), Some(5));
        assert_eq!(
            first["ranking"]["signals"]["type_priority"].as_i64(),
            Some(9)
        );

        Ok(())
    }

    #[test]
    fn build_context_pack_deduplicates_by_text_and_prefers_repo_on_tie() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        let title = "Duplicate beta context fact";
        let body = "Duplicate beta context body should appear only once.";
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-duplicate-repo",
                memory_type: MemoryType::Fact,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title,
                body,
                path: None,
                source_ref: Some("issue://duplicate-repo"),
            },
        )?;
        insert_memory_with_destination(
            &conn,
            MemoryFixture {
                id: "rec-duplicate-local",
                memory_type: MemoryType::Fact,
                scope_kind: ScopeKind::Personal,
                status: MemoryStatus::Active,
                title,
                body,
                path: None,
                source_ref: Some("local://duplicate"),
            },
            MemoryDestination::Local,
            MemoryLane::Semantic,
        )?;

        let pack = build_context_pack(
            &conn,
            ContextPackInput {
                task: "duplicate beta context".to_owned(),
                path_prefix: None,
                token_budget: Some(120),
                include_local: true,
                include_session: false,
            },
        )?;
        let json = serde_json::to_value(&pack)?;
        assert_eq!(
            record_ids_from_pack(&json),
            vec!["rec-duplicate-repo".to_owned()],
            "deduplication should keep canonical repo memory over duplicate local memory: {json}"
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
        insert_memory_with_destination(conn, memory, MemoryDestination::Repo, MemoryLane::Semantic)
    }

    fn insert_memory_with_destination(
        conn: &Connection,
        memory: MemoryFixture<'_>,
        destination: MemoryDestination,
        lane: MemoryLane,
    ) -> anyhow::Result<()> {
        let visibility = if destination == MemoryDestination::Repo {
            "repo"
        } else {
            "private"
        };
        conn.execute(
            "INSERT INTO memory_record(
                id, type, lane, destination, scope_kind, visibility, title, body, status, confidence,
                source_kind, source_ref, content_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0.88, 'test', ?10, ?11)",
            params![
                memory.id,
                memory.memory_type.as_str(),
                lane.as_str(),
                destination.as_str(),
                memory.scope_kind.as_str(),
                visibility,
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
