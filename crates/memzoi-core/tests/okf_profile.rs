use std::{
    fs,
    path::{Path, PathBuf},
};

use memzoi_core::{
    InitRequest, MemoryDraft, MemoryLane, MemoryPaths, MemoryService, MemoryStatus, MemoryType,
    OkfProposalAction, OkfProposalSensitivity, OkfProposalStatus, ScopeKind, Visibility,
    parse_okf_proposal_markdown, parse_okf_record_file, parse_okf_record_markdown,
    read_okf_proposal_files, read_okf_record_files,
};
use rusqlite::Connection;
use tempfile::TempDir;

#[test]
fn parses_example_memory_as_memzoi_okf_profile_record() -> anyhow::Result<()> {
    let fixture = examples_root().join("example-memory.md");
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
fn parses_okf_proposal_examples_as_review_packets() -> anyhow::Result<()> {
    let proposals = read_okf_proposal_files(examples_root().join("proposals"))?;

    assert!(
        proposals.len() >= 5,
        "expected at least the required proposal fixtures"
    );
    let semantic = proposals
        .iter()
        .find(|proposal| proposal.id == "mem_2026_07_06_auth_001")
        .expect("semantic create proposal is present");
    assert_eq!(semantic.file_id, "mem_2026_07_06_auth_001");
    assert_eq!(semantic.status, OkfProposalStatus::Proposed);
    assert_eq!(semantic.proposal.action, OkfProposalAction::Create);
    assert_eq!(semantic.memory_type, MemoryType::Decision);
    assert_eq!(semantic.lane, MemoryLane::Semantic);
    assert_eq!(semantic.scope_kind, ScopeKind::Project);
    assert_eq!(semantic.applies_to, vec!["src/auth/**"]);
    assert_eq!(semantic.sensitivity, OkfProposalSensitivity::RepoSafe);
    assert!(semantic.body.contains("## Review notes"));

    let episodic = proposals
        .iter()
        .find(|proposal| proposal.id == "mem_2026_07_06_auth_handoff")
        .expect("episodic handoff proposal is present");
    assert_eq!(episodic.memory_type, MemoryType::Episode);
    assert_eq!(episodic.lane, MemoryLane::Episodic);

    let procedural = proposals
        .iter()
        .find(|proposal| proposal.id == "mem_2026_07_06_testing_procedure")
        .expect("procedural proposal is present");
    assert_eq!(procedural.memory_type, MemoryType::Procedure);
    assert_eq!(procedural.lane, MemoryLane::Procedural);

    let supersede = proposals
        .iter()
        .find(|proposal| proposal.proposal.action == OkfProposalAction::Supersede)
        .expect("supersede proposal is present");
    assert_eq!(
        supersede.supersedes,
        vec!["semantic/decisions/auth-client-validation"]
    );

    let tombstone = proposals
        .iter()
        .find(|proposal| proposal.proposal.action == OkfProposalAction::Tombstone)
        .expect("tombstone proposal is present");
    assert_eq!(
        tombstone.proposal.target.as_deref(),
        Some("semantic/decisions/auth-client-validation")
    );

    Ok(())
}

#[test]
fn compact_canonical_example_parses_without_proposal_metadata() -> anyhow::Result<()> {
    let fixture = examples_root().join("compact-canonical-from-proposal.md");
    let bundle = fixture.parent().expect("example has parent");

    let record =
        parse_okf_record_file(bundle, &fixture)?.expect("compact canonical example is a record");

    assert_eq!(record.draft.memory_type, MemoryType::Decision);
    assert_eq!(record.draft.lane, MemoryLane::Semantic);
    assert_eq!(record.status, MemoryStatus::Active);
    assert_eq!(
        record.draft.source_ref.as_deref(),
        Some("mem_2026_07_06_auth_001")
    );
    assert_eq!(
        record.proposal_id, None,
        "legacy canonical records without proposal_id remain readable"
    );
    assert!(
        !record.draft.body.contains("proposal:"),
        "canonical record body should not carry proposal metadata"
    );

    Ok(())
}

#[test]
fn rejects_unknown_proposal_schema_values() {
    for (field, markdown, expected) in [
        (
            "lane",
            proposal_markdown(
                "create",
                "mystery",
                "proposed",
                "repo-safe",
                "supersedes: []",
                "",
            ),
            "unknown memory lane",
        ),
        (
            "action",
            proposal_markdown(
                "update",
                "semantic",
                "proposed",
                "repo-safe",
                "supersedes: []",
                "",
            ),
            "unknown OKF proposal action",
        ),
        (
            "status",
            proposal_markdown(
                "create",
                "semantic",
                "approved",
                "repo-safe",
                "supersedes: []",
                "",
            ),
            "unknown OKF proposal status",
        ),
        (
            "sensitivity",
            proposal_markdown(
                "create",
                "semantic",
                "proposed",
                "public",
                "supersedes: []",
                "",
            ),
            "unknown OKF proposal sensitivity",
        ),
    ] {
        let error = parse_okf_proposal_markdown(
            Path::new("/bundle"),
            Path::new("/bundle/mem_test_proposal.md"),
            &markdown,
        )
        .unwrap_err();

        assert!(
            error.to_string().contains(expected),
            "{field} error should contain {expected:?}, got {error:#}"
        );
    }
}

#[test]
fn legacy_proposal_without_sensitivity_is_read_as_unknown() -> anyhow::Result<()> {
    let markdown = proposal_markdown(
        "create",
        "semantic",
        "proposed",
        "repo-safe",
        "supersedes: []",
        "",
    )
    .replace("sensitivity: repo-safe\n", "");
    let parsed = parse_okf_proposal_markdown(
        Path::new("/bundle"),
        Path::new("/bundle/mem_test_proposal.md"),
        &markdown,
    )?
    .expect("legacy proposal should remain reviewable");

    assert_eq!(parsed.sensitivity, OkfProposalSensitivity::Unknown);
    Ok(())
}

#[test]
fn legacy_action_shapes_remain_parseable_for_review() -> anyhow::Result<()> {
    let supersede = parse_okf_proposal_markdown(
        Path::new("/bundle"),
        Path::new("/bundle/mem_supersede.md"),
        &proposal_markdown(
            "supersede",
            "semantic",
            "proposed",
            "repo-safe",
            "supersedes: []",
            "",
        ),
    )?
    .expect("legacy supersede packet should remain reviewable");
    assert_eq!(supersede.proposal.action, OkfProposalAction::Supersede);
    assert!(supersede.supersedes.is_empty());
    assert!(supersede.proposal.reason.is_none());

    let tombstone = parse_okf_proposal_markdown(
        Path::new("/bundle"),
        Path::new("/bundle/mem_tombstone.md"),
        &proposal_markdown(
            "tombstone",
            "semantic",
            "proposed",
            "repo-safe",
            "supersedes: []",
            "",
        ),
    )?
    .expect("legacy tombstone packet should remain reviewable");
    assert_eq!(tombstone.proposal.action, OkfProposalAction::Tombstone);
    assert!(tombstone.proposal.target.is_none());
    assert!(tombstone.proposal.reason.is_none());
    Ok(())
}

#[test]
fn proposal_aliases_allow_equal_or_legacy_values_and_reject_conflicts() -> anyhow::Result<()> {
    let legacy_target = proposal_markdown(
        "tombstone",
        "semantic",
        "proposed",
        "repo-safe",
        "supersedes: []",
        "  target_id: semantic/legacy-target\n",
    );
    let parsed = parse_okf_proposal_markdown(
        Path::new("/bundle"),
        Path::new("/bundle/legacy-target.md"),
        &legacy_target,
    )?
    .expect("target_id-only packet should remain compatible");
    assert_eq!(
        parsed.proposal.target.as_deref(),
        Some("semantic/legacy-target")
    );

    let equal_targets = legacy_target.replace(
        "  target_id: semantic/legacy-target\n",
        "  target: semantic/legacy-target\n  target_id: \" semantic/legacy-target \"\n",
    );
    parse_okf_proposal_markdown(
        Path::new("/bundle"),
        Path::new("/bundle/equal-targets.md"),
        &equal_targets,
    )?
    .expect("equal target aliases should parse");

    let conflicting_targets = equal_targets.replace(
        "target_id: \" semantic/legacy-target \"",
        "target_id: semantic/other-target",
    );
    let error = parse_okf_proposal_markdown(
        Path::new("/bundle"),
        Path::new("/bundle/conflicting-targets.md"),
        &conflicting_targets,
    )
    .expect_err("conflicting target aliases must fail");
    assert!(
        error
            .to_string()
            .contains("proposal.target and proposal.target_id must match")
    );
    assert!(!error.to_string().contains("other-target"));

    for (markdown, expected) in [
        (
            proposal_markdown(
                "create",
                "semantic",
                "proposed",
                "repo-safe",
                "supersedes: []",
                "",
            )
            .replace(
                "scope:\n  kind: repo\n",
                "scope_kind: project\nscope:\n  kind: repo\n",
            ),
            "scope_kind and scope.kind must match",
        ),
        (
            proposal_markdown(
                "create",
                "semantic",
                "proposed",
                "repo-safe",
                "supersedes: []",
                "",
            )
            .replace(
                "scope:\n  kind: repo\n",
                "scope_id: top-level\nscope:\n  kind: repo\n  id: nested\n",
            ),
            "scope_id and scope.id must match",
        ),
    ] {
        let error = parse_okf_proposal_markdown(
            Path::new("/bundle"),
            Path::new("/bundle/conflicting-scope.md"),
            &markdown,
        )
        .expect_err("conflicting scope aliases must fail");
        assert!(error.to_string().contains(expected), "{error:#}");
    }
    Ok(())
}

#[test]
fn canonical_aliases_are_normalized_and_conflicts_are_rejected() -> anyhow::Result<()> {
    let markdown = r#"---
type: fact
lane: semantic
title: Alias fixture
scope: " repo "
scope_kind: repo
scope_id: " team-a "
visibility: repo
status: active
confidence: 1.0
source: " human "
source_kind: human
created: 2026-07-01T00:00:00Z
created_at: " 2026-07-01T00:00:00Z "
timestamp: 2026-07-01T00:00:00Z
updated: 2026-07-02T00:00:00Z
updated_at: " 2026-07-02T00:00:00Z "
supersedes: old-record
supersedes_id: " old-record "
expires: 2099-01-01T00:00:00Z
expires_at: " 2099-01-01T00:00:00Z "
---

# Alias fixture

Alias fixture body.
"#;
    let parsed = parse_okf_record_markdown(
        Path::new("/bundle"),
        Path::new("/bundle/alias-fixture.md"),
        markdown,
    )?
    .expect("equal aliases should parse");
    assert_eq!(parsed.draft.scope_id.as_deref(), Some("team-a"));
    assert_eq!(parsed.draft.source_kind.as_deref(), Some("human"));

    for (needle, replacement, expected) in [
        (
            "scope_kind: repo",
            "scope_kind: project",
            "scope_kind and scope must match",
        ),
        (
            "source_kind: human",
            "source_kind: agent",
            "source and source_kind must match",
        ),
        (
            "created_at: \" 2026-07-01T00:00:00Z \"",
            "created_at: 2026-07-03T00:00:00Z",
            "created and created_at must match",
        ),
        (
            "timestamp: 2026-07-01T00:00:00Z",
            "timestamp: 2026-07-03T00:00:00Z",
            "created and timestamp must match",
        ),
        (
            "updated_at: \" 2026-07-02T00:00:00Z \"",
            "updated_at: 2026-07-03T00:00:00Z",
            "updated and updated_at must match",
        ),
        (
            "supersedes_id: \" old-record \"",
            "supersedes_id: other-record",
            "supersedes and supersedes_id must match",
        ),
        (
            "expires_at: \" 2099-01-01T00:00:00Z \"",
            "expires_at: 2098-01-01T00:00:00Z",
            "expires and expires_at must match",
        ),
    ] {
        let error = parse_okf_record_markdown(
            Path::new("/bundle"),
            Path::new("/bundle/alias-fixture.md"),
            &markdown.replace(needle, replacement),
        )
        .expect_err("conflicting aliases must fail");
        assert!(error.to_string().contains(expected), "{error:#}");
    }
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
        error.contains("canonical memory record already exists"),
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
        error.contains("failed to inspect canonical memory record"),
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
        sensitivity: memzoi_core::OkfProposalSensitivity::RepoSafe,
        content_class: memzoi_core::RepositoryContentClass::GeneralRepoKnowledge,
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
    let project_root = temp.path().join("project");
    fs::create_dir_all(&project_root)?;
    let paths = MemoryPaths::with_runtime_home(
        project_root.canonicalize()?,
        temp.path().join("runtime-home"),
    );
    MemoryService::initialize_paths(paths.clone(), InitRequest { force: false })?;
    MemoryService::open_paths(paths)
}

fn examples_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples")
}

fn proposal_markdown(
    action: &str,
    lane: &str,
    status: &str,
    sensitivity: &str,
    supersedes_yaml: &str,
    target_yaml: &str,
) -> String {
    format!(
        r#"---
id: mem_test_proposal
kind: proposal
version: okf/v0.1
profile: memzoi/v0
type: decision
lane: {lane}
title: Test proposal
description: Test proposal description.
status: {status}
proposal:
  action: {action}
  proposed_by: agent
  proposed_at: 2026-07-06T00:00:00Z
{target_yaml}scope:
  kind: repo
  paths:
    - crates/**
tags:
  - testing
timestamp: 2026-07-06T00:00:00Z
created_by: agent
sources:
  - path: crates/memzoi-core/src/okf.rs
{supersedes_yaml}
sensitivity: {sensitivity}
---

# Test proposal

This proposal body is intentionally non-empty.
"#
    )
}
