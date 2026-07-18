use anyhow::{Result, bail};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    context::{self, ContextPackInput},
    models::{HandoffPack, ProposalInboxSummary},
    proposals::{self, ProposalStatus},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HandoffInput {
    pub task: Option<String>,
    pub path_prefix: Option<String>,
    pub token_budget: Option<usize>,
    pub include_local: bool,
    pub include_session: bool,
}

#[cfg(test)]
pub fn build_handoff_pack(conn: &Connection, input: HandoffInput) -> Result<HandoffPack> {
    build_handoff_pack_at(conn, input, OffsetDateTime::now_utc())
}

pub(crate) fn build_handoff_pack_at(
    conn: &Connection,
    input: HandoffInput,
    now: OffsetDateTime,
) -> Result<HandoffPack> {
    let path_prefix = normalize_optional(input.path_prefix);
    let task = effective_task(input.task, path_prefix.as_deref())?;
    let context = context::build_context_pack_at(
        conn,
        ContextPackInput {
            task: task.clone(),
            path_prefix: path_prefix.clone(),
            token_budget: input.token_budget,
            include_local: input.include_local,
            include_session: input.include_session,
        },
        now,
    )?;
    let proposal_inbox = proposal_inbox_summary(proposals::open_proposal_counts(conn)?);

    Ok(HandoffPack {
        id: format!("handoff_{}", Uuid::now_v7()),
        task,
        path_prefix,
        token_budget: input.token_budget,
        include_local: input.include_local,
        include_session: input.include_session,
        proposal_inbox,
        created_at: context.created_at.clone(),
        context,
    })
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn effective_task(task: Option<String>, path_prefix: Option<&str>) -> Result<String> {
    if let Some(task) = normalize_optional(task) {
        return Ok(task);
    }
    if let Some(path_prefix) = path_prefix {
        return Ok(format!("Handoff for path {path_prefix}"));
    }
    bail!("handoff requires --task or --path")
}

fn proposal_inbox_summary(
    counts: std::collections::BTreeMap<ProposalStatus, usize>,
) -> ProposalInboxSummary {
    let pending = *counts.get(&ProposalStatus::Pending).unwrap_or(&0);
    let validated = *counts.get(&ProposalStatus::Validated).unwrap_or(&0);
    let approved = *counts.get(&ProposalStatus::Approved).unwrap_or(&0);
    ProposalInboxSummary {
        source: "db".to_owned(),
        open_total: pending + validated + approved,
        pending,
        validated,
        approved,
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};
    use serde_json::Value;
    use tempfile::TempDir;

    use super::{HandoffInput, build_handoff_pack};
    use crate::{
        MemoryDraft, Visibility, init_database,
        models::{MemoryDestination, MemoryLane, MemoryStatus, MemoryType, ScopeKind},
        open_database, proposals,
    };

    #[test]
    fn handoff_requires_task_or_path() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        let error = build_handoff_pack(&conn, HandoffInput::default())
            .expect_err("handoff should require a task or path");
        assert!(
            error.to_string().contains("--task or --path"),
            "error should explain the required input: {error:#}"
        );
        Ok(())
    }

    #[test]
    fn path_only_handoff_uses_deterministic_task_fallback() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-handoff-path",
                memory_type: MemoryType::Warning,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title: "Handoff path warning",
                body: "Path-scoped handoff memory should appear even without task text.",
                path: Some("crates/memzoi-core/src/handoff.rs"),
            },
        )?;

        let pack = build_handoff_pack(
            &conn,
            HandoffInput {
                task: None,
                path_prefix: Some("crates/memzoi-core/src/handoff.rs".to_owned()),
                token_budget: Some(120),
                include_local: false,
                include_session: false,
            },
        )?;
        let json = serde_json::to_value(&pack)?;

        assert_eq!(
            pack.task,
            "Handoff for path crates/memzoi-core/src/handoff.rs"
        );
        assert_eq!(pack.context.task, pack.task);
        assert_eq!(
            record_ids_from_context(&json),
            vec!["rec-handoff-path".to_owned()],
            "path-only handoff should include path-scoped context records: {json}"
        );

        Ok(())
    }

    #[test]
    fn handoff_is_repo_only_by_default_and_layers_runtime_on_opt_in() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-handoff-repo",
                memory_type: MemoryType::Decision,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title: "Layered handoff repo decision",
                body: "Layered handoff should include repo memory by default.",
                path: None,
            },
        )?;
        insert_memory_with_destination(
            &conn,
            MemoryFixture {
                id: "rec-handoff-local",
                memory_type: MemoryType::Preference,
                scope_kind: ScopeKind::Personal,
                status: MemoryStatus::Active,
                title: "Layered handoff local preference",
                body: "Layered handoff should include local memory only when requested.",
                path: None,
            },
            MemoryDestination::Local,
            MemoryLane::Semantic,
        )?;
        insert_memory_with_destination(
            &conn,
            MemoryFixture {
                id: "rec-handoff-session",
                memory_type: MemoryType::Episode,
                scope_kind: ScopeKind::Personal,
                status: MemoryStatus::Active,
                title: "Layered handoff session checkpoint",
                body: "Layered handoff should include session memory only when requested.",
                path: None,
            },
            MemoryDestination::Session,
            MemoryLane::Session,
        )?;

        let default_pack = build_handoff_pack(
            &conn,
            HandoffInput {
                task: Some("layered handoff".to_owned()),
                token_budget: Some(200),
                ..HandoffInput::default()
            },
        )?;
        let default_json = serde_json::to_value(&default_pack)?;
        assert_eq!(
            record_ids_from_context(&default_json),
            vec!["rec-handoff-repo".to_owned()],
            "handoff should be repo-only by default: {default_json}"
        );

        let layered_pack = build_handoff_pack(
            &conn,
            HandoffInput {
                task: Some("layered handoff".to_owned()),
                token_budget: Some(240),
                include_local: true,
                include_session: true,
                ..HandoffInput::default()
            },
        )?;
        let layered_json = serde_json::to_value(&layered_pack)?;
        let ids = record_ids_from_context(&layered_json);
        assert!(ids.iter().any(|id| id == "rec-handoff-repo"));
        assert!(ids.iter().any(|id| id == "rec-handoff-local"));
        assert!(ids.iter().any(|id| id == "rec-handoff-session"));
        assert_eq!(
            layered_json["context"]["policy"]["requested_destinations"],
            serde_json::json!(["repo", "local", "session"])
        );

        Ok(())
    }

    #[test]
    fn handoff_reports_db_proposal_inbox_counts() -> anyhow::Result<()> {
        let (_temp, conn) = initialized_database()?;
        insert_memory(
            &conn,
            MemoryFixture {
                id: "rec-handoff-counts",
                memory_type: MemoryType::Fact,
                scope_kind: ScopeKind::Repo,
                status: MemoryStatus::Active,
                title: "Proposal inbox handoff fact",
                body: "Proposal inbox metadata belongs in handoff output.",
                path: None,
            },
        )?;
        let pending =
            proposals::propose_memory(&conn, "test", draft("Pending proposal inbox fixture"))?;
        let validated =
            proposals::propose_memory(&conn, "test", draft("Validated proposal inbox fixture"))?;
        conn.execute(
            "UPDATE proposal SET status = 'validated' WHERE id = ?1",
            [validated.id.as_str()],
        )?;
        let approved =
            proposals::propose_memory(&conn, "test", draft("Approved proposal inbox fixture"))?;
        proposals::approve_proposal(&conn, approved.id.as_str(), "test")?;

        let pack = build_handoff_pack(
            &conn,
            HandoffInput {
                task: Some("proposal inbox handoff".to_owned()),
                token_budget: Some(120),
                ..HandoffInput::default()
            },
        )?;

        assert_eq!(pack.proposal_inbox.source, "db");
        assert_eq!(pack.proposal_inbox.open_total, 3);
        assert_eq!(pack.proposal_inbox.pending, 1);
        assert_eq!(pack.proposal_inbox.validated, 1);
        assert_eq!(pack.proposal_inbox.approved, 1);
        assert!(!pending.id.is_empty());

        Ok(())
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
        let now = crate::events::now_utc()?;
        let retention = serde_json::to_string(&crate::retention_facts_for_creation(
            lane, &now, None, None,
        )?)?;
        let route = match destination {
            MemoryDestination::Repo => crate::OriginRoute::RepositoryMaterialization,
            MemoryDestination::Local => crate::OriginRoute::LocalMemory,
            MemoryDestination::Session => crate::OriginRoute::CheckpointCreate,
            _ => crate::OriginRoute::OwnerCommand,
        };
        let origin = serde_json::to_string(&crate::OriginDescriptor::new(
            format!("test-handoff:{}", memory.id),
            route,
        ))?;
        conn.execute(
            "INSERT INTO memory_record(
                id, type, lane, destination, scope_kind, visibility, title, body, status, confidence,
                source_kind, source_ref, retention_json, origin_json, content_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0.88, 'test', NULL, ?10, ?11, ?12)",
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
                retention,
                origin,
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

    fn draft(title: &str) -> MemoryDraft {
        MemoryDraft {
            memory_type: MemoryType::Fact,
            lane: MemoryLane::Semantic,
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            visibility: Visibility::Repo,
            title: title.to_owned(),
            body: "Proposal inbox fixture body.".to_owned(),
            tags: Vec::new(),
            source_kind: Some("test".to_owned()),
            source_ref: None,
            sensitivity: crate::OkfProposalSensitivity::RepoSafe,
            content_class: crate::RepositoryContentClass::GeneralRepoKnowledge,
            confidence: 0.9,
        }
    }

    fn record_ids_from_context(json: &Value) -> Vec<String> {
        json.get("context")
            .and_then(|context| context.get("records"))
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("handoff JSON should include context.records: {json}"))
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

    fn initialized_database() -> anyhow::Result<(TempDir, Connection)> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("memory.db");
        let conn = open_database(&db_path)?;
        init_database(&conn)?;
        Ok((temp, conn))
    }
}
