use std::{cmp::Ordering, collections::BTreeMap};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    events::{AppendEvent, append_event},
    models::{
        MemoryCitation, MemoryDestination, MemoryPath, MemoryType, PrecheckWarning, ScopeKind,
        SearchResult,
    },
    search::{
        SearchInput, citation_for, load_paths, path_matches_request, record_from_row, search_memory,
    },
};

const PRECHECK_RESULT_LIMIT: usize = 25;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PrecheckInput {
    pub path: Option<String>,
    pub action: Option<String>,
    pub command: Option<String>,
    pub scope_kind: Option<ScopeKind>,
}

pub fn precheck(conn: &Connection, input: PrecheckInput) -> Result<Vec<PrecheckWarning>> {
    let requested_path = normalized_path(input.path.as_deref());
    let mut candidates = BTreeMap::<String, PrecheckCandidate>::new();

    if let Some(requested_path) = requested_path {
        for (result, path_score) in
            path_governance_candidates(conn, input.scope_kind, requested_path)?
        {
            candidates.insert(
                result.record.id.clone(),
                PrecheckCandidate {
                    result,
                    path_score,
                    lexical_score: None,
                },
            );
        }
    }

    if let Some(query) = lexical_query(&input) {
        for result in search_memory(
            conn,
            SearchInput {
                query,
                scope_kind: input.scope_kind,
                path_prefix: requested_path.map(ToOwned::to_owned),
                limit: 100,
                include_inactive: false,
                ..SearchInput::default()
            },
        )?
        .into_iter()
        .filter(is_governance_memory)
        {
            let lexical_score = result.score;
            candidates
                .entry(result.record.id.clone())
                .and_modify(|candidate| {
                    candidate.lexical_score = Some(
                        candidate
                            .lexical_score
                            .map_or(lexical_score, |score| score.max(lexical_score)),
                    );
                })
                .or_insert(PrecheckCandidate {
                    result,
                    path_score: 0,
                    lexical_score: Some(lexical_score),
                });
        }
    }

    let mut candidates = candidates.into_values().collect::<Vec<_>>();
    candidates.sort_by(compare_precheck_candidates);
    candidates.truncate(PRECHECK_RESULT_LIMIT);
    let warnings = candidates
        .into_iter()
        .map(|candidate| warning_from_result(candidate.result))
        .collect::<Result<Vec<_>>>()?;

    append_precheck_event(conn, &input, &warnings)?;
    Ok(warnings)
}

#[derive(Debug)]
struct PrecheckCandidate {
    result: SearchResult,
    path_score: i64,
    lexical_score: Option<f64>,
}

fn normalized_path(path: Option<&str>) -> Option<&str> {
    path.map(str::trim).filter(|path| !path.is_empty())
}

fn lexical_query(input: &PrecheckInput) -> Option<String> {
    let query = [input.action.as_deref(), input.command.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!query.is_empty()).then_some(query)
}

fn path_governance_candidates(
    conn: &Connection,
    scope_kind: Option<ScopeKind>,
    requested_path: &str,
) -> Result<Vec<(SearchResult, i64)>> {
    let requested_path = requested_path.trim().trim_end_matches('/').to_owned();
    if requested_path.is_empty() {
        return Ok(Vec::new());
    }

    let path_like = format!("{requested_path}/%");
    let scope_kind = scope_kind.map(|value| value.as_str().to_owned());
    // SQL narrows the indexed candidates; path_matches_request below remains
    // authoritative for the documented exact/directory/trailing-glob rules.
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
               AND memory_record.type IN ('risk', 'warning', 'failed_attempt')
               AND (?2 IS NULL OR memory_record.scope_kind = ?2)
               AND (
                 memory_path.path = ?3
                 OR memory_path.path LIKE ?4
                 OR ?3 LIKE memory_path.path || '/%'
                 OR (
                     memory_path.path LIKE '%/**'
                     AND (
                         ?3 = substr(memory_path.path, 1, length(memory_path.path) - 3)
                         OR ?3 LIKE substr(memory_path.path, 1, length(memory_path.path) - 2) || '%'
                     )
                 )
                 OR (
                     ?3 LIKE '%/**'
                     AND (
                         memory_path.path = substr(?3, 1, length(?3) - 3)
                         OR memory_path.path LIKE substr(?3, 1, length(?3) - 2) || '%'
                     )
                 )
               )
             ORDER BY memory_record.updated_at DESC, memory_record.id ASC",
        )
        .context("failed to prepare path-scoped precheck candidate query")?;
    let rows = stmt
        .query_map(
            params![
                MemoryDestination::Repo.as_str(),
                scope_kind,
                requested_path,
                path_like,
            ],
            record_from_row,
        )
        .context("failed to execute path-scoped precheck candidate query")?;

    let mut results = Vec::new();
    for row in rows {
        let record = row.context("failed to read path-scoped precheck candidate")?;
        let paths = load_paths(conn, record.id.as_str())?;
        let Some((matching_path, path_score)) = best_matching_path(&paths, &requested_path) else {
            continue;
        };
        let citation = citation_for(&record, Some(matching_path))?;
        results.push((
            SearchResult {
                record,
                score: path_score as f64,
                snippet: None,
                rationale: Some("path binding match".to_owned()),
                ranking: None,
                paths,
                citations: vec![citation],
            },
            path_score,
        ));
    }

    Ok(results)
}

fn best_matching_path<'a>(
    paths: &'a [MemoryPath],
    requested_path: &str,
) -> Option<(&'a MemoryPath, i64)> {
    let mut best: Option<(&MemoryPath, i64)> = None;
    for path in paths {
        if !path_matches_request(&path.path, requested_path) {
            continue;
        }
        let score = path_match_score(&path.path, requested_path);
        if score == 0 {
            continue;
        }
        let replace = match best {
            None => true,
            Some((best_path, best_score)) => {
                score > best_score
                    || (score == best_score && path.path.as_str() < best_path.path.as_str())
            }
        };
        if replace {
            best = Some((path, score));
        }
    }
    best
}

fn path_match_score(stored_path: &str, requested_path: &str) -> i64 {
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

fn compare_precheck_candidates(left: &PrecheckCandidate, right: &PrecheckCandidate) -> Ordering {
    right
        .path_score
        .cmp(&left.path_score)
        .then_with(|| {
            right
                .lexical_score
                .is_some()
                .cmp(&left.lexical_score.is_some())
        })
        .then_with(|| lexical_score(right).total_cmp(&lexical_score(left)))
        .then_with(|| {
            governance_priority(right.result.record.memory_type)
                .cmp(&governance_priority(left.result.record.memory_type))
        })
        .then_with(|| {
            right
                .result
                .record
                .confidence
                .total_cmp(&left.result.record.confidence)
        })
        .then_with(|| {
            right
                .result
                .record
                .updated_at
                .cmp(&left.result.record.updated_at)
        })
        .then_with(|| left.result.record.id.cmp(&right.result.record.id))
}

fn lexical_score(candidate: &PrecheckCandidate) -> f64 {
    candidate.lexical_score.unwrap_or(0.0)
}

fn governance_priority(memory_type: MemoryType) -> i64 {
    match memory_type {
        MemoryType::Risk => 3,
        MemoryType::Warning => 2,
        MemoryType::FailedAttempt => 1,
        _ => 0,
    }
}

fn is_governance_memory(result: &SearchResult) -> bool {
    matches!(
        result.record.memory_type,
        MemoryType::Risk | MemoryType::Warning | MemoryType::FailedAttempt
    )
}

fn warning_from_result(result: SearchResult) -> Result<PrecheckWarning> {
    let severity = severity_for(result.record.memory_type).to_owned();
    let citation = match result.citations.first().cloned() {
        Some(citation) => citation,
        None => MemoryCitation {
            record_id: result.record.id.clone(),
            memory_type: result.record.memory_type,
            scope_kind: result.record.scope_kind,
            provenance: result.record.destination.policy().plane.ok_or_else(|| {
                anyhow::anyhow!(
                    "precheck result '{}' has a destination without a memory plane",
                    result.record.id
                )
            })?,
            destination: result.record.destination,
            visibility: result.record.visibility,
            source_kind: result.record.source_kind.clone(),
            source_ref: result.record.source_ref.clone(),
            path: result.paths.first().map(|path| path.path.clone()),
        },
    };
    let suggested_next_step = match result.record.memory_type {
        MemoryType::Risk => "Review the cited risk before editing and consider a targeted test.",
        MemoryType::Warning => "Review the cited warning before proceeding.",
        MemoryType::FailedAttempt => {
            "Check the failed-attempt memory and avoid repeating the same approach."
        }
        _ => "Review the cited memory before proceeding.",
    }
    .to_owned();

    Ok(PrecheckWarning {
        id: format!("warn_{}", result.record.id),
        record_id: result.record.id,
        message: format!("{}: {}", result.record.title, result.record.body),
        severity,
        citations: vec![citation],
        suggested_next_step,
    })
}

fn severity_for(memory_type: MemoryType) -> &'static str {
    match memory_type {
        MemoryType::Risk => "high",
        MemoryType::Warning | MemoryType::FailedAttempt => "warning",
        _ => "info",
    }
}

fn append_precheck_event(
    conn: &Connection,
    input: &PrecheckInput,
    warnings: &[PrecheckWarning],
) -> Result<()> {
    append_event(
        conn,
        AppendEvent {
            event_type: "memory.precheck_ran".to_owned(),
            actor: "memzoi-core".to_owned(),
            payload: json!({
                "path": input.path,
                "action": input.action,
                "command": input.command,
                "scope_kind": input.scope_kind.map(|scope| scope.as_str()),
                "warning_ids": warnings.iter().map(|warning| warning.id.as_str()).collect::<Vec<_>>(),
                "record_ids": warnings.iter().map(|warning| warning.record_id.as_str()).collect::<Vec<_>>(),
            }),
            record_id: None,
            proposal_id: None,
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};
    use tempfile::TempDir;

    use super::{PrecheckInput, precheck};
    use crate::{
        init_database,
        models::{MemoryStatus, MemoryType, ScopeKind},
        open_database,
    };

    #[test]
    fn precheck_path_only_returns_exact_governance_without_lexical_overlap() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "risk-exact-ledger",
                memory_type: MemoryType::Risk,
                title: "Preserve ledger invariants",
                body: "Changing the rounding order can silently alter settled totals.",
                path: "apps/api/src/billing/invoice.rs",
                source_ref: "issue://ledger-invariant",
            },
        )?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "warning-unrelated-auth",
                memory_type: MemoryType::Warning,
                title: "Keep the migration lock held",
                body: "Concurrent schema changes can strand partially applied state.",
                path: "apps/api/src/auth/mod.rs",
                source_ref: "issue://auth-migration-lock",
            },
        )?;

        let warnings = precheck(
            &conn,
            PrecheckInput {
                path: Some("apps/api/src/billing/invoice.rs".to_owned()),
                ..PrecheckInput::default()
            },
        )?;

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].record_id, "risk-exact-ledger");
        assert_eq!(
            warnings[0].citations[0].path.as_deref(),
            Some("apps/api/src/billing/invoice.rs")
        );
        Ok(())
    }

    #[test]
    fn precheck_path_only_applies_directory_and_trailing_double_star_bindings() -> anyhow::Result<()>
    {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "warning-billing-directory",
                memory_type: MemoryType::Warning,
                title: "Preserve settlement ordering",
                body: "The settlement pipeline depends on a stable calculation sequence.",
                path: "apps/api/src/billing",
                source_ref: "runbook://settlement-ordering",
            },
        )?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "risk-web-glob",
                memory_type: MemoryType::Risk,
                title: "Hydration boundary is fragile",
                body: "Server and client render phases must continue to agree.",
                path: "apps/web/**",
                source_ref: "issue://hydration-boundary",
            },
        )?;

        let directory_warnings = precheck(
            &conn,
            PrecheckInput {
                path: Some("apps/api/src/billing/invoice.rs".to_owned()),
                ..PrecheckInput::default()
            },
        )?;
        let parent_directory_warnings = precheck(
            &conn,
            PrecheckInput {
                path: Some("apps/api/src".to_owned()),
                ..PrecheckInput::default()
            },
        )?;
        let glob_warnings = precheck(
            &conn,
            PrecheckInput {
                path: Some("apps/web/src/App.tsx".to_owned()),
                ..PrecheckInput::default()
            },
        )?;

        assert_eq!(
            directory_warnings
                .iter()
                .map(|warning| warning.record_id.as_str())
                .collect::<Vec<_>>(),
            ["warning-billing-directory"]
        );
        assert_eq!(
            parent_directory_warnings
                .iter()
                .map(|warning| warning.record_id.as_str())
                .collect::<Vec<_>>(),
            ["warning-billing-directory"],
            "a requested directory should include records bound to descendants"
        );
        assert_eq!(
            glob_warnings
                .iter()
                .map(|warning| warning.record_id.as_str())
                .collect::<Vec<_>>(),
            ["risk-web-glob"]
        );
        assert_eq!(
            glob_warnings[0].citations[0].path.as_deref(),
            Some("apps/web/**")
        );
        Ok(())
    }

    #[test]
    fn precheck_uses_lexical_matches_as_an_additional_signal_without_duplicates()
    -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "warning-key-rotation",
                memory_type: MemoryType::Warning,
                title: "Signing key rotation warning",
                body: "Rotate signing keys only after publishing the overlap window.",
                path: "config/keys.toml",
                source_ref: "runbook://key-rotation",
            },
        )?;
        insert_path(
            &conn,
            "warning-key-rotation",
            "config",
            "path-warning-key-rotation-directory",
        )?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "risk-key-config",
                memory_type: MemoryType::Risk,
                title: "Preserve trust continuity",
                body: "A malformed transition can invalidate already issued credentials.",
                path: "config/keys.toml",
                source_ref: "issue://trust-continuity",
            },
        )?;

        let warnings = precheck(
            &conn,
            PrecheckInput {
                path: Some("config/keys.toml".to_owned()),
                action: Some("rotate signing keys".to_owned()),
                ..PrecheckInput::default()
            },
        )?;

        assert_eq!(
            warnings
                .iter()
                .map(|warning| warning.record_id.as_str())
                .collect::<Vec<_>>(),
            ["warning-key-rotation", "risk-key-config"],
            "lexical relevance should rank the matching warning without excluding path-only risk"
        );
        assert_eq!(
            warnings
                .iter()
                .filter(|warning| warning.record_id == "warning-key-rotation")
                .count(),
            1,
            "a record found through both path and lexical recall must be emitted once"
        );
        assert_eq!(
            warnings[0].citations[0].path.as_deref(),
            Some("config/keys.toml"),
            "the citation should use the most specific applicable binding"
        );
        Ok(())
    }

    #[test]
    fn precheck_path_recall_suppresses_inactive_out_of_scope_and_unrelated_records()
    -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "warning-active-repo",
                memory_type: MemoryType::Warning,
                title: "Keep rollout overlap",
                body: "Removing overlap can interrupt active sessions.",
                path: "config/deploy.toml",
                source_ref: "runbook://deploy-overlap",
            },
        )?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "risk-inactive-repo",
                memory_type: MemoryType::Risk,
                title: "Old rollout hazard",
                body: "This lifecycle record is no longer active.",
                path: "config/deploy.toml",
                source_ref: "issue://old-rollout",
            },
        )?;
        conn.execute(
            "UPDATE memory_record SET status = ?1 WHERE id = ?2",
            params![MemoryStatus::Tombstoned.as_str(), "risk-inactive-repo"],
        )?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "risk-project-scope",
                memory_type: MemoryType::Risk,
                title: "Project rollout hazard",
                body: "This warning belongs to a different requested scope.",
                path: "config/deploy.toml",
                source_ref: "issue://project-rollout",
            },
        )?;
        conn.execute(
            "UPDATE memory_record SET scope_kind = ?1 WHERE id = ?2",
            params![ScopeKind::Project.as_str(), "risk-project-scope"],
        )?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "fact-deploy-config",
                memory_type: MemoryType::Fact,
                title: "Deploy configuration location",
                body: "This informational record must not become a precheck warning.",
                path: "config/deploy.toml",
                source_ref: "doc://deploy-config",
            },
        )?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "warning-unrelated-deploy",
                memory_type: MemoryType::Warning,
                title: "Rotate deploy keys carefully",
                body: "The same action text must not bypass path applicability.",
                path: "config/other.toml",
                source_ref: "runbook://other-deploy",
            },
        )?;

        let warnings = precheck(
            &conn,
            PrecheckInput {
                path: Some("config/deploy.toml".to_owned()),
                action: Some("rotate deploy keys".to_owned()),
                scope_kind: Some(ScopeKind::Repo),
                ..PrecheckInput::default()
            },
        )?;

        assert_eq!(
            warnings
                .iter()
                .map(|warning| warning.record_id.as_str())
                .collect::<Vec<_>>(),
            ["warning-active-repo"]
        );
        Ok(())
    }

    #[test]
    fn precheck_warns_for_risky_path_memory_and_cites_record() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "risk-billing-invoice",
                memory_type: MemoryType::Risk,
                title: "Billing file is fragile",
                body: "Editing invoice rounding previously broke tax calculation.",
                path: "apps/api/src/billing/invoice.rs",
                source_ref: "issue://billing-risk#invoice",
            },
        )?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "warning-auth-command",
                memory_type: MemoryType::Warning,
                title: "Auth command warning",
                body: "Do not run auth migrations while smoke tests are active.",
                path: "apps/api/src/auth/mod.rs",
                source_ref: "issue://auth-warning",
            },
        )?;

        let warnings = precheck(
            &conn,
            PrecheckInput {
                path: Some("apps/api/src/billing/invoice.rs".to_owned()),
                action: Some("change invoice rounding".to_owned()),
                ..PrecheckInput::default()
            },
        )?;

        assert_eq!(
            warnings.len(),
            1,
            "only path/action matching risk should warn"
        );
        assert_eq!(warnings[0].record_id, "risk-billing-invoice");
        assert_eq!(warnings[0].severity, "high");
        assert_eq!(warnings[0].citations[0].record_id, "risk-billing-invoice");
        assert!(warnings[0].message.contains("Billing file is fragile"));
        Ok(())
    }

    #[test]
    fn precheck_surfaces_risk_for_trailing_double_star_path_scope() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "risk-web-glob-scope",
                memory_type: MemoryType::Risk,
                title: "Web glob edit risk",
                body: "Changing webglob hydration behavior previously broke the app shell.",
                path: "apps/web/**",
                source_ref: "issue://web-glob-risk",
            },
        )?;

        let warnings = precheck(
            &conn,
            PrecheckInput {
                path: Some("apps/web/src/App.tsx".to_owned()),
                action: Some("change webglob hydration".to_owned()),
                ..PrecheckInput::default()
            },
        )?;

        assert_eq!(
            warnings.len(),
            1,
            "stored path apps/web/** should surface a matching risk for apps/web/src/App.tsx"
        );
        assert_eq!(warnings[0].record_id, "risk-web-glob-scope");
        assert_eq!(warnings[0].severity, "high");
        assert!(
            warnings[0]
                .citations
                .iter()
                .any(|citation| citation.path.as_deref() == Some("apps/web/**")),
            "precheck warning should cite the stored glob path: {warnings:?}"
        );

        Ok(())
    }

    #[test]
    fn precheck_warns_for_failed_attempt_with_similar_action() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "failed-cache-reset",
                memory_type: MemoryType::FailedAttempt,
                title: "Cache reset failed attempt",
                body: "Running cache reset without draining workers caused stale lock files.",
                path: "crates/cache/src/reset.rs",
                source_ref: "runbook://cache-reset",
            },
        )?;

        let warnings = precheck(
            &conn,
            PrecheckInput {
                path: Some("crates/cache/src/reset.rs".to_owned()),
                action: Some("run cache reset".to_owned()),
                ..PrecheckInput::default()
            },
        )?;

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].record_id, "failed-cache-reset");
        assert_eq!(warnings[0].severity, "warning");
        Ok(())
    }

    #[test]
    fn precheck_warns_for_command_only_input() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "warning-npm-install",
                memory_type: MemoryType::Warning,
                title: "npm install warning",
                body: "Running npm install mutates lockfiles; use the package manager already configured by the repo.",
                path: "package.json",
                source_ref: "runbook://package-manager",
            },
        )?;

        let warnings = precheck(
            &conn,
            PrecheckInput {
                command: Some("npm install".to_owned()),
                ..PrecheckInput::default()
            },
        )?;

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].record_id, "warning-npm-install");
        assert_eq!(warnings[0].severity, "warning");
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
        title: &'a str,
        body: &'a str,
        path: &'a str,
        source_ref: &'a str,
    }

    fn insert_memory(conn: &Connection, memory: MemoryFixture<'_>) -> anyhow::Result<()> {
        conn.execute(
            "INSERT INTO memory_record(
                id, type, scope_kind, visibility, title, body, status, confidence,
                source_kind, source_ref, content_hash
             ) VALUES (?1, ?2, ?3, 'repo', ?4, ?5, ?6, 0.93, 'test', ?7, ?8)",
            params![
                memory.id,
                memory.memory_type.as_str(),
                ScopeKind::Repo.as_str(),
                memory.title,
                memory.body,
                MemoryStatus::Active.as_str(),
                memory.source_ref,
                format!("hash-{}", memory.id),
            ],
        )?;
        insert_path(conn, memory.id, memory.path, &format!("path-{}", memory.id))?;
        Ok(())
    }

    fn insert_path(
        conn: &Connection,
        record_id: &str,
        path: &str,
        path_id: &str,
    ) -> anyhow::Result<()> {
        conn.execute(
            "INSERT INTO memory_path(id, record_id, path, line_start, line_end)
             VALUES (?1, ?2, ?3, 1, 12)",
            params![path_id, record_id, path],
        )?;
        Ok(())
    }
}
