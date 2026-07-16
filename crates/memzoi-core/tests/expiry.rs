use std::fs;

use memzoi_core::{
    ContextPackInput, ExportFormat, ExportInput, FixedClock, HandoffInput, InitRequest,
    MemoryPaths, MemoryService, PrecheckInput, ScopeKind, SearchInput,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

const EXPIRY: &str = "2026-07-10T14:00:00+02:00";
const BEFORE_EXPIRY: &str = "2026-07-10T11:59:59.999999999Z";
const AT_EXPIRY: &str = "2026-07-10T12:00:00Z";
const TOKEN: &str = "authoritativeexpiry";

#[test]
fn expiry_is_authoritative_across_rebuild_and_every_normal_read_surface() -> anyhow::Result<()> {
    let fixture = ExpiryFixture::new()?;
    let canonical_before = fs::read_to_string(&fixture.canonical_path)?;

    let before = fixture.service_at(BEFORE_EXPIRY)?;
    assert_eq!(before.search_memory(repo_search())?.len(), 1);
    assert_eq!(before.list_local_memory()?.len(), 1);
    assert_eq!(before.list_checkpoints()?.len(), 1);

    let agents_export = before.export(ExportInput {
        format: ExportFormat::AgentsMd,
        scope_kind: ScopeKind::Repo,
    })?;
    assert!(fs::read_to_string(&agents_export.written_paths[0])?.contains("rec-expiring-risk"));
    let okf_export = before.export(ExportInput {
        format: ExportFormat::Okf,
        scope_kind: ScopeKind::Repo,
    })?;
    assert_eq!(okf_export.written_paths.len(), 1);
    let stale_okf_export = okf_export.written_paths[0].clone();
    assert!(stale_okf_export.exists());
    drop(before);

    let at_boundary = fixture.service_at(AT_EXPIRY)?;
    assert!(
        at_boundary.repo_index_drift()?.is_current(),
        "expiry must not masquerade as canonical/index drift"
    );
    assert!(at_boundary.search_memory(repo_search())?.is_empty());
    assert!(
        at_boundary
            .search_local_memory(TOKEN.to_owned(), 10)?
            .is_empty()
    );
    assert!(at_boundary.list_local_memory()?.is_empty());
    assert!(at_boundary.list_checkpoints()?.is_empty());
    assert!(
        at_boundary.show_checkpoint("rec-expiring-session").is_err(),
        "expired checkpoints must not be consumable by ID through the normal show surface"
    );

    let context = at_boundary.build_context_pack(ContextPackInput {
        task: format!("review {TOKEN} behavior"),
        path_prefix: Some("crates/memzoi-core/src/search.rs".to_owned()),
        token_budget: Some(400),
        include_local: true,
        include_session: true,
    })?;
    assert!(
        context.records.is_empty(),
        "expired context leaked: {context:?}"
    );
    assert!(!context.prompt.contains(TOKEN));

    let handoff = at_boundary.build_handoff_pack(HandoffInput {
        task: Some(format!("handoff {TOKEN} behavior")),
        path_prefix: Some("crates/memzoi-core/src/search.rs".to_owned()),
        token_budget: Some(400),
        include_local: true,
        include_session: true,
    })?;
    assert!(
        handoff.context.records.is_empty(),
        "expired handoff leaked: {handoff:?}"
    );

    let warnings = at_boundary.precheck(PrecheckInput {
        path: Some("crates/memzoi-core/src/search.rs".to_owned()),
        action: Some(format!("change {TOKEN} danger handling")),
        command: None,
        scope_kind: Some(ScopeKind::Repo),
    })?;
    assert!(
        warnings.is_empty(),
        "expired precheck warning leaked: {warnings:?}"
    );

    let agents_export = at_boundary.export(ExportInput {
        format: ExportFormat::AgentsMd,
        scope_kind: ScopeKind::Repo,
    })?;
    assert!(!fs::read_to_string(&agents_export.written_paths[0])?.contains(TOKEN));
    let okf_export = at_boundary.export(ExportInput {
        format: ExportFormat::Okf,
        scope_kind: ScopeKind::Repo,
    })?;
    assert!(okf_export.written_paths.is_empty());
    assert!(
        !stale_okf_export.exists(),
        "rerunning a generated export must remove a record that has since expired"
    );

    let diagnostic = at_boundary.inspect_expiry("rec-expiring-risk")?;
    assert_eq!(diagnostic.record.body, canonical_body());
    assert_eq!(diagnostic.record.status.as_str(), "active");
    assert!(diagnostic.expired);
    assert!(diagnostic.excluded_from_normal_reads);
    assert_eq!(
        diagnostic.effective_expires_at.as_deref(),
        Some("2026-07-10T12:00:00Z")
    );
    assert!(diagnostic.reason.contains("at or after expiry"));

    assert_eq!(
        fs::read_to_string(&fixture.canonical_path)?,
        canonical_before,
        "read-time expiry must not rewrite canonical memory"
    );
    assert!(canonical_before.contains("status: active"));
    assert!(canonical_before.contains(&format!("expires: {EXPIRY}")));

    Ok(())
}

struct ExpiryFixture {
    _temp: TempDir,
    paths: MemoryPaths,
    canonical_path: std::path::PathBuf,
}

impl ExpiryFixture {
    fn new() -> anyhow::Result<Self> {
        let temp = TempDir::new()?;
        let paths = MemoryPaths::with_runtime_home(
            temp.path().canonicalize()?,
            temp.path().join(".memzoi-runtime"),
        );
        MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
        let canonical_path = paths.records_dir().join("rec-expiring-risk.md");
        fs::write(&canonical_path, canonical_record())?;

        MemoryService::rebuild_paths(paths.clone())?;
        insert_runtime_records(&paths.shared_db_path)?;
        MemoryService::rebuild_paths(paths.clone())?;

        Ok(Self {
            _temp: temp,
            paths,
            canonical_path,
        })
    }

    fn service_at(&self, now: &str) -> anyhow::Result<MemoryService> {
        MemoryService::open_paths_with_clock(self.paths.clone(), FixedClock::from_rfc3339(now)?)
    }
}

fn repo_search() -> SearchInput {
    SearchInput {
        query: TOKEN.to_owned(),
        scope_kind: Some(ScopeKind::Repo),
        limit: 10,
        ..SearchInput::default()
    }
}

fn canonical_record() -> String {
    format!(
        r#"---
type: risk
lane: semantic
title: Expiring authoritative risk
description: Expiring authoritative risk for cross-surface tests.
timestamp: 2026-07-01T00:00:00Z
status: active
visibility: repo
content_class: general_repo_knowledge
confidence: 0.9
scope: repo
source: test
expires: {EXPIRY}
applies_to:
  - crates/memzoi-core/src/search.rs
---

# Expiring authoritative risk

{}
"#,
        canonical_body()
    )
}

fn canonical_body() -> &'static str {
    "The authoritativeexpiry danger warning must disappear from normal reads at its expiry."
}

fn insert_runtime_records(db_path: &std::path::Path) -> anyhow::Result<()> {
    let conn = Connection::open(db_path)?;
    for (id, destination, lane, memory_type, title, body, source_kind) in [
        (
            "rec-expiring-local",
            "local",
            "semantic",
            "preference",
            "Expiring local preference",
            "The authoritativeexpiry local preference must disappear at expiry.",
            "memzoi-local",
        ),
        (
            "rec-expiring-session",
            "session",
            "session",
            "episode",
            "Expiring session checkpoint",
            "The authoritativeexpiry session checkpoint must disappear at expiry.",
            "memzoi-checkpoint",
        ),
    ] {
        conn.execute(
            "INSERT INTO memory_record(
                id, type, lane, destination, scope_kind, visibility, title, body, status,
                confidence, source_kind, content_hash, created_at, updated_at, expires_at
             ) VALUES (?1, ?2, ?3, ?4, 'personal', 'private', ?5, ?6, 'active',
                       1.0, ?7, ?8, '2026-07-01T00:00:00Z', '2026-07-01T00:00:00Z', ?9)",
            params![
                id,
                memory_type,
                lane,
                destination,
                title,
                body,
                source_kind,
                format!("hash-{id}"),
                EXPIRY,
            ],
        )?;
    }
    Ok(())
}
