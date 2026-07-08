use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    events::{AppendEvent, append_event},
    models::{MemoryCitation, MemoryType, PrecheckWarning, ScopeKind, SearchResult},
    search::{SearchInput, search_memory},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PrecheckInput {
    pub path: Option<String>,
    pub action: Option<String>,
    pub command: Option<String>,
    pub scope_kind: Option<ScopeKind>,
}

pub fn precheck(conn: &Connection, input: PrecheckInput) -> Result<Vec<PrecheckWarning>> {
    let query = precheck_query(&input);
    if query.is_empty() {
        append_precheck_event(conn, &input, &[])?;
        return Ok(Vec::new());
    }

    let results = search_memory(
        conn,
        SearchInput {
            query,
            scope_kind: input.scope_kind,
            path_prefix: input.path.clone(),
            limit: 25,
            include_inactive: false,
            ..SearchInput::default()
        },
    )?;
    let warnings = results
        .into_iter()
        .filter(is_governance_memory)
        .map(warning_from_result)
        .collect::<Vec<_>>();

    append_precheck_event(conn, &input, &warnings)?;
    Ok(warnings)
}

fn precheck_query(input: &PrecheckInput) -> String {
    input
        .action
        .as_deref()
        .or(input.command.as_deref())
        .or(input.path.as_deref())
        .unwrap_or_default()
        .to_owned()
}

fn is_governance_memory(result: &SearchResult) -> bool {
    matches!(
        result.record.memory_type,
        MemoryType::Risk | MemoryType::Warning | MemoryType::FailedAttempt
    )
}

fn warning_from_result(result: SearchResult) -> PrecheckWarning {
    let severity = severity_for(result.record.memory_type).to_owned();
    let citation = result
        .citations
        .first()
        .cloned()
        .unwrap_or_else(|| MemoryCitation {
            record_id: result.record.id.clone(),
            memory_type: result.record.memory_type,
            scope_kind: result.record.scope_kind,
            destination: result.record.destination,
            visibility: result.record.visibility,
            source_kind: result.record.source_kind.clone(),
            source_ref: result.record.source_ref.clone(),
            path: result.paths.first().map(|path| path.path.clone()),
        });
    let suggested_next_step = match result.record.memory_type {
        MemoryType::Risk => "Review the cited risk before editing and consider a targeted test.",
        MemoryType::Warning => "Review the cited warning before proceeding.",
        MemoryType::FailedAttempt => {
            "Check the failed-attempt memory and avoid repeating the same approach."
        }
        _ => "Review the cited memory before proceeding.",
    }
    .to_owned();

    PrecheckWarning {
        id: format!("warn_{}", result.record.id),
        record_id: result.record.id,
        message: format!("{}: {}", result.record.title, result.record.body),
        severity,
        citations: vec![citation],
        suggested_next_step,
    }
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
        conn.execute(
            "INSERT INTO memory_path(id, record_id, path, line_start, line_end)
             VALUES (?1, ?2, ?3, 1, 12)",
            params![format!("path-{}", memory.id), memory.id, memory.path],
        )?;
        Ok(())
    }
}
