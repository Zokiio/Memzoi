use std::fs;

use memzoi_core::{
    InitRequest, MemoryDraft, MemoryLane, MemoryPaths, MemoryRecord, MemoryService, MemoryStatus,
    MemoryType, ScopeKind, Visibility, parse_okf_record_file, read_okf_record_files,
    write_memory_record_file,
};
use rusqlite::Connection;
use tempfile::TempDir;

#[test]
fn parses_example_memory_as_memzoi_okf_profile_record() -> anyhow::Result<()> {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/example-memory.md");
    let bundle = fixture.parent().expect("example has parent");

    let record =
        parse_okf_record_file(bundle, &fixture)?.expect("example-memory.md is a concept record");

    assert_eq!(record.concept_id, "example-memory");
    assert_eq!(record.draft.memory_type, MemoryType::Preference);
    assert_eq!(record.draft.scope_kind, ScopeKind::Repo);
    assert_eq!(record.draft.visibility, Visibility::Team);
    assert_eq!(record.status, MemoryStatus::Active);
    assert_eq!(record.draft.confidence, 1.0);
    assert_eq!(record.created, "2026-07-04T00:00:00Z");
    assert_eq!(record.updated, None);
    assert_eq!(record.draft.source_kind.as_deref(), Some("human"));
    assert_eq!(record.draft.title, "Swedish-first UI copy");
    assert_eq!(record.applies_to, vec!["apps/web/**"]);
    assert_eq!(record.draft.tags, vec!["frontend", "i18n"]);
    assert!(record.draft.body.contains("User-facing UI"));
    assert!(!record.draft.body.starts_with("# Swedish-first"));

    Ok(())
}

#[test]
fn skips_okf_reserved_index_and_log_files() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let records = temp.path().join("memory").join("records");
    fs::create_dir_all(&records)?;
    fs::write(records.join("index.md"), "# Memory records\n")?;
    fs::write(records.join("log.md"), "# Log\n")?;

    let parsed = read_okf_record_files(&records)?;

    assert!(parsed.is_empty());
    Ok(())
}

#[test]
fn rejects_unsafe_applies_to_paths() {
    let temp = TempDir::new().unwrap();
    let records = temp.path().join("records");
    fs::create_dir_all(&records).unwrap();
    let file = records.join("bad.md");
    fs::write(
        &file,
        r#"---
type: decision
title: Bad path
description: Bad path record.
timestamp: 2026-07-05T00:00:00Z
status: active
visibility: repo
confidence: confirmed
source: human-authored
applies_to:
  - ../secrets
---

# Bad path

Do not allow traversal.
"#,
    )
    .unwrap();

    let error = parse_okf_record_file(&records, &file)
        .unwrap_err()
        .to_string();

    assert!(error.contains("applies_to"), "got {error}");
}

#[test]
fn rejects_concept_ids_that_do_not_match_profile_rules() {
    let temp = TempDir::new().unwrap();
    let records = temp.path().join("records");
    fs::create_dir_all(&records).unwrap();
    let file = records.join("Bad_Segment.md");
    fs::write(
        &file,
        r#"---
type: decision
title: Bad concept path
description: Bad concept path record.
timestamp: 2026-07-05T00:00:00Z
status: active
visibility: repo
confidence: confirmed
source: human-authored
---

# Bad concept path

Do not allow uppercase or underscores in concept IDs.
"#,
    )
    .unwrap();

    let error = parse_okf_record_file(&records, &file)
        .unwrap_err()
        .to_string();

    assert!(error.contains("OKF concept id"), "got {error}");
}

#[test]
fn rendered_records_are_valid_yaml_and_preserve_core_fields() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let records = temp.path().join("records");
    let record = MemoryRecord {
        id: "team/install-risk".to_owned(),
        memory_type: MemoryType::Risk,
        lane: MemoryLane::Semantic,
        scope_kind: ScopeKind::Team,
        scope_id: Some("platform".to_owned()),
        visibility: Visibility::Team,
        title: "Risk: package install".to_owned(),
        body: "Package installs require review.".to_owned(),
        status: MemoryStatus::Superseded,
        confidence: 0.75,
        source_kind: Some("human-authored".to_owned()),
        source_ref: Some("issue://42".to_owned()),
        content_hash: "hash".to_owned(),
        created_at: "2026-07-05T00:00:00Z".to_owned(),
        updated_at: "2026-07-06T00:00:00Z".to_owned(),
        supersedes_id: Some("team/old-install-risk".to_owned()),
        expires_at: Some("2027-01-01".to_owned()),
    };

    let path = write_memory_record_file(&records, &record)?;
    let rendered = fs::read_to_string(&path)?;
    assert!(rendered.contains("type: risk\n"));
    assert!(rendered.contains("title: \"Risk: package install\"\n"));
    let parsed = parse_okf_record_file(&records, &path)?.expect("rendered record parses");

    assert_eq!(parsed.concept_id, "team/install-risk");
    assert_eq!(parsed.draft.title, "Risk: package install");
    assert_eq!(parsed.draft.scope_kind, ScopeKind::Team);
    assert_eq!(parsed.draft.scope_id.as_deref(), Some("platform"));
    assert_eq!(parsed.status, MemoryStatus::Superseded);
    assert_eq!(parsed.draft.source_ref.as_deref(), Some("issue://42"));
    assert_eq!(
        parsed.supersedes_id.as_deref(),
        Some("team/old-install-risk")
    );
    assert_eq!(parsed.expires_at.as_deref(), Some("2027-01-01"));

    Ok(())
}

#[test]
fn service_updates_canonical_files_for_supersede_and_tombstone() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = initialized_service(&temp)?;
    let records = service.paths().records_dir();

    let proposal = service.propose_memory(
        "agent:red-tests",
        memory_draft(
            "Canonical lifecycle source",
            "The original canonical file should stop being active.",
            Vec::new(),
        ),
    )?;
    service.approve_proposal(&proposal.id, "reviewer:human")?;
    let applied = service.apply_proposal(&proposal.id, "agent:applier")?;
    let applied_path = records.join(format!("{}.md", applied.id));

    let superseded = service.supersede_record(
        &applied.id,
        "agent:red-tests",
        memory_draft(
            "Canonical lifecycle replacement",
            "The replacement canonical file should be present.",
            Vec::new(),
        ),
    )?;
    let previous_markdown = fs::read_to_string(&applied_path)?;
    assert!(previous_markdown.contains("status: superseded\n"));

    let replacement_path = records.join(format!("{}.md", superseded.replacement.id));
    let replacement_markdown = fs::read_to_string(&replacement_path)?;
    assert!(replacement_markdown.contains("status: active\n"));
    assert!(replacement_markdown.contains("# Canonical lifecycle replacement"));

    service.tombstone_record(&superseded.replacement.id, "agent:red-tests", "obsolete")?;
    let tombstoned_markdown = fs::read_to_string(&replacement_path)?;
    assert!(tombstoned_markdown.contains("status: tombstoned\n"));

    Ok(())
}

#[test]
fn applied_records_use_path_concept_ids_for_canonical_files() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = initialized_service(&temp)?;

    let proposal = service.propose_memory(
        "agent:red-tests",
        memory_draft(
            "Use React Query in Active",
            "Use React Query for server state in apps/active.",
            Vec::new(),
        ),
    )?;
    service.approve_proposal(&proposal.id, "reviewer:human")?;

    let applied = service.apply_proposal(&proposal.id, "agent:applier")?;
    let canonical_path = service
        .paths()
        .records_dir()
        .join("use-react-query-in-active.md");

    assert_eq!(applied.id, "use-react-query-in-active");
    assert!(canonical_path.is_file());
    assert!(
        !service
            .paths()
            .records_dir()
            .join(format!("{}.md", proposal.id))
            .exists()
    );

    Ok(())
}

#[test]
fn apply_refuses_to_overwrite_existing_canonical_file_outside_db() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = initialized_service(&temp)?;
    let records_dir = service.paths().records_dir();
    let db_path = service.paths().db_path.clone();
    let existing_path = records_dir.join("use-pnpm.md");
    fs::create_dir_all(&records_dir)?;
    fs::write(&existing_path, "human-authored canonical memory\n")?;

    let proposal = service.propose_memory(
        "agent:red-tests",
        memory_draft(
            "Use pnpm",
            "This repo should keep the existing human-authored canonical source.",
            Vec::new(),
        ),
    )?;
    service.approve_proposal(&proposal.id, "reviewer:human")?;

    let error = service
        .apply_proposal(&proposal.id, "agent:applier")
        .expect_err("apply should not overwrite an existing canonical file")
        .to_string();
    assert!(
        error.contains("failed to create memory record"),
        "got {error}"
    );
    assert_eq!(
        fs::read_to_string(&existing_path)?,
        "human-authored canonical memory\n"
    );

    let conn = Connection::open(&db_path)?;
    let status: String = conn.query_row(
        "SELECT status FROM proposal WHERE id = ?1",
        [&proposal.id],
        |row| row.get(0),
    )?;
    let records: i64 =
        conn.query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))?;
    assert_eq!(status, "approved");
    assert_eq!(records, 0);

    Ok(())
}

#[test]
fn supersede_writes_previous_record_with_current_updated_timestamp() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = initialized_service(&temp)?;
    let records = service.paths().records_dir();
    let db_path = service.paths().db_path.clone();

    let proposal = service.propose_memory(
        "agent:red-tests",
        memory_draft(
            "Supersede timestamp source",
            "The superseded canonical file should carry the DB update timestamp.",
            Vec::new(),
        ),
    )?;
    service.approve_proposal(&proposal.id, "reviewer:human")?;
    let applied = service.apply_proposal(&proposal.id, "agent:applier")?;
    let old_updated = "2000-01-01T00:00:00Z";
    let conn = Connection::open(&db_path)?;
    conn.execute(
        "UPDATE memory_record SET updated_at = ?1 WHERE id = ?2",
        (old_updated, applied.id.as_str()),
    )?;
    drop(conn);

    service.supersede_record(
        &applied.id,
        "agent:red-tests",
        memory_draft(
            "Supersede timestamp replacement",
            "Replacement body.",
            Vec::new(),
        ),
    )?;

    let conn = Connection::open(&db_path)?;
    let db_updated: String = conn.query_row(
        "SELECT updated_at FROM memory_record WHERE id = ?1",
        [&applied.id],
        |row| row.get(0),
    )?;
    let previous_path = records.join(format!("{}.md", applied.id));
    let parsed = parse_okf_record_file(&records, &previous_path)?.expect("previous record parses");

    assert_ne!(db_updated, old_updated);
    assert_eq!(parsed.updated.as_deref(), Some(db_updated.as_str()));

    Ok(())
}

#[test]
fn supersede_refuses_existing_replacement_file_before_rewriting_previous() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = initialized_service(&temp)?;
    let records = service.paths().records_dir();
    let db_path = service.paths().db_path.clone();

    let proposal = service.propose_memory(
        "agent:red-tests",
        memory_draft(
            "Supersede collision source",
            "The previous canonical file should remain unchanged if the replacement path exists.",
            Vec::new(),
        ),
    )?;
    service.approve_proposal(&proposal.id, "reviewer:human")?;
    let applied = service.apply_proposal(&proposal.id, "agent:applier")?;
    let previous_path = records.join(format!("{}.md", applied.id));
    let previous_before = fs::read_to_string(&previous_path)?;

    let existing_replacement_path = records.join("existing-replacement-collision.md");
    fs::write(&existing_replacement_path, "human-authored replacement\n")?;

    let error = service
        .supersede_record(
            &applied.id,
            "agent:red-tests",
            memory_draft(
                "Existing replacement collision",
                "This replacement would collide with a canonical file outside the DB.",
                Vec::new(),
            ),
        )
        .expect_err("supersede should fail before rewriting previous file")
        .to_string();
    assert!(
        error.contains("canonical memory record already exists"),
        "got {error}"
    );

    assert_eq!(fs::read_to_string(&previous_path)?, previous_before);
    assert_eq!(
        fs::read_to_string(&existing_replacement_path)?,
        "human-authored replacement\n"
    );

    let conn = Connection::open(&db_path)?;
    let previous_status: String = conn.query_row(
        "SELECT status FROM memory_record WHERE id = ?1",
        [&applied.id],
        |row| row.get(0),
    )?;
    let replacement_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memory_record WHERE id = 'existing-replacement-collision'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(previous_status, "active");
    assert_eq!(replacement_count, 0);

    Ok(())
}

#[test]
fn apply_rolls_back_db_state_when_canonical_file_write_fails() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = initialized_service(&temp)?;
    let records_dir = service.paths().records_dir();
    let db_path = service.paths().db_path.clone();

    fs::remove_dir(&records_dir)?;
    fs::create_dir_all(records_dir.parent().expect("records dir has parent"))?;
    fs::write(&records_dir, "not a directory")?;
    let proposal = service.propose_memory(
        "agent:red-tests",
        memory_draft(
            "Rollback when canonical write fails",
            "The proposal should remain approved if canonical write fails.",
            Vec::new(),
        ),
    )?;
    service.approve_proposal(&proposal.id, "reviewer:human")?;

    let error = service
        .apply_proposal(&proposal.id, "agent:applier")
        .expect_err("apply should fail when canonical record path cannot be written")
        .to_string();
    assert!(
        error.contains("failed to create records directory"),
        "got {error}"
    );

    let conn = Connection::open(&db_path)?;
    let status: String = conn.query_row(
        "SELECT status FROM proposal WHERE id = ?1",
        [&proposal.id],
        |row| row.get(0),
    )?;
    let records: i64 =
        conn.query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))?;

    assert_eq!(status, "approved");
    assert_eq!(records, 0);

    Ok(())
}

#[test]
fn service_keeps_canonical_record_files_current_and_preserves_tags() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = initialized_service(&temp)?;
    let records = service.paths().records_dir();

    let proposal = service.propose_memory(
        "agent:red-tests",
        memory_draft(
            "Canonical record files preserve tags",
            "The canonical OKF file should retain draft tags.",
            vec!["canonical".to_owned(), "roundtrip".to_owned()],
        ),
    )?;
    service.approve_proposal(&proposal.id, "reviewer:human")?;
    let applied = service.apply_proposal(&proposal.id, "agent:applier")?;
    let applied_path = records.join(format!("{}.md", applied.id));
    let applied_markdown = fs::read_to_string(&applied_path)?;

    assert!(applied_markdown.contains("tags:\n"));
    assert!(applied_markdown.contains("  - canonical\n"));
    assert!(applied_markdown.contains("  - roundtrip\n"));
    let parsed_applied =
        parse_okf_record_file(&records, &applied_path)?.expect("applied record parses");
    assert_eq!(
        parsed_applied.draft.tags,
        vec!["canonical".to_owned(), "roundtrip".to_owned()]
    );

    let superseded = service.supersede_record(
        &applied.id,
        "agent:red-tests",
        memory_draft(
            "Canonical replacement survives rebuild",
            "The replacement file should become canonical.",
            vec!["replacement".to_owned()],
        ),
    )?;
    let previous_markdown = fs::read_to_string(&applied_path)?;
    assert!(previous_markdown.contains("status: superseded\n"));

    let replacement_path = records.join(format!("{}.md", superseded.replacement.id));
    let replacement_markdown = fs::read_to_string(&replacement_path)?;
    assert!(replacement_markdown.contains("status: active\n"));
    assert!(replacement_markdown.contains("# Canonical replacement survives rebuild"));

    service.tombstone_record(&superseded.replacement.id, "agent:red-tests", "obsolete")?;
    let tombstoned_markdown = fs::read_to_string(&replacement_path)?;
    assert!(tombstoned_markdown.contains("status: tombstoned\n"));

    Ok(())
}

#[test]
fn service_preserves_path_bindings_when_rewriting_canonical_files() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let service = initialized_service(&temp)?;
    let records = service.paths().records_dir();
    let db_path = service.paths().db_path.clone();

    let supersede_proposal = service.propose_memory(
        "agent:red-tests",
        memory_draft(
            "Path scoped supersede source",
            "The superseded file should keep its path scope.",
            Vec::new(),
        ),
    )?;
    service.approve_proposal(&supersede_proposal.id, "reviewer:human")?;
    let supersede_source = service.apply_proposal(&supersede_proposal.id, "agent:applier")?;
    attach_memory_path(&db_path, &supersede_source.id, "apps/web/**")?;

    service.supersede_record(
        &supersede_source.id,
        "agent:red-tests",
        memory_draft(
            "Path scoped replacement",
            "The replacement should not erase the old path metadata.",
            Vec::new(),
        ),
    )?;
    let superseded_markdown =
        fs::read_to_string(records.join(format!("{}.md", supersede_source.id)))?;
    assert!(superseded_markdown.contains("status: superseded\n"));
    assert!(superseded_markdown.contains("applies_to:\n"));
    assert!(superseded_markdown.contains("  - \"apps/web/**\"\n"));

    let tombstone_proposal = service.propose_memory(
        "agent:red-tests",
        memory_draft(
            "Path scoped tombstone source",
            "The tombstoned file should keep its path scope.",
            Vec::new(),
        ),
    )?;
    service.approve_proposal(&tombstone_proposal.id, "reviewer:human")?;
    let tombstone_source = service.apply_proposal(&tombstone_proposal.id, "agent:applier")?;
    attach_memory_path(
        &db_path,
        &tombstone_source.id,
        "crates/memzoi-core/src/service.rs",
    )?;

    service.tombstone_record(&tombstone_source.id, "agent:red-tests", "obsolete")?;
    let tombstoned_markdown =
        fs::read_to_string(records.join(format!("{}.md", tombstone_source.id)))?;
    assert!(tombstoned_markdown.contains("status: tombstoned\n"));
    assert!(tombstoned_markdown.contains("applies_to:\n"));
    assert!(tombstoned_markdown.contains("  - crates/memzoi-core/src/service.rs\n"));

    Ok(())
}

fn memory_draft(title: &str, body: &str, tags: Vec<String>) -> MemoryDraft {
    MemoryDraft {
        memory_type: MemoryType::Fact,
        lane: MemoryLane::Semantic,
        scope_kind: ScopeKind::Repo,
        scope_id: None,
        visibility: Visibility::Repo,
        title: title.to_owned(),
        body: body.to_owned(),
        tags,
        source_kind: Some("test".to_owned()),
        source_ref: Some("okf-profile-test".to_owned()),
        confidence: 0.9,
    }
}

fn attach_memory_path(
    db_path: &std::path::Path,
    record_id: &str,
    path: &str,
) -> anyhow::Result<()> {
    let conn = Connection::open(db_path)?;
    conn.execute(
        "INSERT INTO memory_path(id, record_id, path, line_start, line_end)
         VALUES (?1, ?2, ?3, NULL, NULL)",
        rusqlite::params![format!("path-{record_id}"), record_id, path],
    )?;
    Ok(())
}

fn initialized_service(temp: &TempDir) -> anyhow::Result<MemoryService> {
    let paths = MemoryPaths::with_runtime_home(
        temp.path().canonicalize()?,
        temp.path().join(".memzoi-runtime"),
    );
    MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
    MemoryService::open_paths(paths)
}
