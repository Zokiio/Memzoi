use super::*;

#[test]
fn search_json_filters_type_path_limit_and_excludes_inactive_records() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let matching = create_applied_memory(
        repo,
        "decision",
        "repo",
        "Zircon CLI search decision",
        "The zircon search command must return this path-scoped decision.",
    );
    attach_memory_path(repo, &matching, "crates/search/src/lib.rs");

    let wrong_type = create_applied_memory(
        repo,
        "fact",
        "repo",
        "Zircon CLI search fact",
        "This fact matches the text and path but should be excluded by --type decision.",
    );
    attach_memory_path(repo, &wrong_type, "crates/search/src/lib.rs");

    let wrong_path = create_applied_memory(
        repo,
        "decision",
        "repo",
        "Zircon CLI search wrong path",
        "This decision matches the text but should be excluded by --path crates/search/.",
    );
    attach_memory_path(repo, &wrong_path, "crates/other/src/lib.rs");

    let tombstoned = create_applied_memory(
        repo,
        "decision",
        "repo",
        "Zircon CLI search tombstoned",
        "This inactive decision matches every filter and must still be excluded.",
    );
    attach_memory_path(repo, &tombstoned, "crates/search/src/lib.rs");
    run_json_command(
        repo,
        &[
            "tombstone",
            tombstoned.as_str(),
            "--reason",
            "superseded by active fixture",
            "--json",
        ],
    );

    let search = run_json_command(
        repo,
        &[
            "search",
            "zircon",
            "--type",
            "decision",
            "--path",
            "crates/search/",
            "--limit",
            "1",
            "--json",
        ],
    );

    let ids = record_ids_from_json(&search);
    assert_eq!(
        ids,
        vec![matching.as_str()],
        "search JSON should return only the active record that survives type/path filters and limit: {search}"
    );
    assert_json_does_not_reference_records(&search, &[wrong_type, wrong_path, tombstoned]);
}

#[test]
fn expiry_command_shows_records_excluded_from_normal_search_and_explains_why() {
    let repo = initialized_temp_repo();
    let record_id = create_applied_memory(
        repo.path(),
        "warning",
        "repo",
        "Expired CLI diagnostic memory",
        "The expirydiagnostic token should be hidden from normal search.",
    );
    set_record_expiry(repo.path(), &record_id, "2000-01-01T00:00:00Z");

    let search = run_json_command(repo.path(), &["search", "expirydiagnostic", "--json"]);
    assert!(record_ids_from_json(&search).is_empty());

    let diagnostic = run_json_command(repo.path(), &["expiry", record_id.as_str(), "--json"]);
    assert_eq!(diagnostic["record"]["id"], record_id);
    assert_eq!(diagnostic["record"]["status"], "active");
    assert_eq!(diagnostic["retention"]["state"], "query_only");
    assert_eq!(diagnostic["current_assertion"], false);
    assert_eq!(diagnostic["excluded_from_normal_reads"], true);
    assert!(
        diagnostic["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("explicit_expiry")),
        "diagnostic should explain exclusion: {diagnostic}"
    );
}

#[test]
fn local_commands_create_list_search_and_stay_out_of_repo_outputs() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let added = run_json_command(
        repo,
        &[
            "local",
            "add",
            "--type",
            "preference",
            "--title",
            "Local zircon preference",
            "--body",
            "Remember the private local zircon preference only in runtime memory.",
            "--json",
        ],
    );
    let record_id = json_string(&added, "record_id").to_owned();
    assert_eq!(record_id, "local-local-zircon-preference");
    assert_json_string_field(&added, &["destination"], "local");
    assert_json_string_field(&added, &["visibility"], "private");
    assert_json_string_field(&added, &["status"], "active");
    assert_json_string_field(&added, &["source_kind"], "memzoi-local");

    assert!(
        !test_paths(repo)
            .records_dir()
            .join(format!("{record_id}.md"))
            .exists(),
        "local add must not write canonical repo files"
    );

    let conn = Connection::open(memory_db_path(repo)).expect("open runtime db");
    let destination: String = conn
        .query_row(
            "SELECT destination FROM memory_record WHERE id = ?1",
            [&record_id],
            |row| row.get(0),
        )
        .expect("read local record destination");
    assert_eq!(destination, "local");

    let listed = run_json_command(repo, &["local", "list", "--json"]);
    assert_json_string_field(&listed, &["destination"], "local");
    assert_eq!(record_ids_from_json(&listed), vec![record_id.as_str()]);
    assert_json_string_field(&listed["records"][0], &["destination"], "local");
    assert_json_string_field(&listed["records"][0], &["source_kind"], "memzoi-local");

    let local_search = run_json_command(repo, &["local", "search", "zircon", "--json"]);
    assert_eq!(
        record_ids_from_json(&local_search),
        vec![record_id.as_str()]
    );
    assert_json_string_field(&local_search, &["destination"], "local");
    assert_json_string_field(
        &local_search["records"][0]["record"],
        &["destination"],
        "local",
    );
    assert_json_string_field(
        &local_search["records"][0]["record"],
        &["source_kind"],
        "memzoi-local",
    );

    let global_search = run_json_command(repo, &["search", "zircon", "--json"]);
    assert!(
        record_ids_from_json(&global_search).is_empty(),
        "global search should stay repo-only: {global_search}"
    );

    let export = run_json_command(repo, &["export", "okf", "--json"]);
    assert!(
        written_paths_from_json(&export).is_empty(),
        "local memory should not be exported as repo memory: {export}"
    );

    let inactive = run_json_command(
        repo,
        &[
            "local",
            "add",
            "--type",
            "fact",
            "--title",
            "Inactive local zircon archive",
            "--body",
            "This inactive local zircon row should survive rebuild even though local list hides it.",
            "--json",
        ],
    );
    let inactive_id = json_string(&inactive, "record_id").to_owned();
    let shared_conn =
        Connection::open(shared_memory_db_path(repo)).expect("open shared runtime db");
    shared_conn
        .execute(
            "UPDATE memory_record SET status = 'tombstoned' WHERE id = ?1",
            [&inactive_id],
        )
        .expect("mark local row inactive");
    drop(shared_conn);
    drop(conn);

    fs::create_dir_all(test_paths(repo).records_dir()).expect("create records dir");
    fs::write(
        test_paths(repo)
            .records_dir()
            .join("repo-zircon-decision.md"),
        r#"---
id: repo-zircon-decision
kind: memory
profile: memzoi
retention: {}
origin:
  origin_key: test-record:repo-zircon-decision
  route: repository_materialization
type: decision
lane: semantic
title: Repo zircon decision
description: Canonical repo memory imported during rebuild.
timestamp: 2026-07-08T00:00:00Z
status: active
scope: repo
visibility: repo
content_class: general_repo_knowledge
confidence: 1
source: test
source_ref: test://repo-zircon
---

# Repo zircon decision

Canonical repo zircon memory should rebuild as repo destination.
"#,
    )
    .expect("write canonical repo record");
    let rebuild = run_json_command(repo, &["rebuild", "--json"]);
    assert_json_array_contains(&rebuild, "record_ids", "repo-zircon-decision");

    let local_after_rebuild = run_json_command(repo, &["local", "search", "zircon", "--json"]);
    assert_eq!(
        record_ids_from_json(&local_after_rebuild),
        vec![record_id.as_str()],
        "rebuild should preserve local runtime memory: {local_after_rebuild}"
    );

    let repo_after_rebuild = run_json_command(repo, &["search", "zircon", "--json"]);
    assert_eq!(
        record_ids_from_json(&repo_after_rebuild),
        vec!["repo-zircon-decision"],
        "rebuild should import canonical repo records as repo destination: {repo_after_rebuild}"
    );
    assert_json_string_field(
        &repo_after_rebuild["records"][0]["record"],
        &["destination"],
        "repo",
    );

    let conn = Connection::open(memory_db_path(repo)).expect("open runtime db after rebuild");
    let (inactive_destination, inactive_status): (String, String) = conn
        .query_row(
            "SELECT destination, status FROM memory_record WHERE id = ?1",
            [&inactive_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("inactive local record should survive rebuild");
    assert_eq!(inactive_destination, "local");
    assert_eq!(inactive_status, "tombstoned");
}

#[test]
fn checkpoint_commands_create_list_and_stay_out_of_repo_outputs() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let first = run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Implement checkpoint workflow",
            "--note",
            "  Remember the checkpoint zircon state only as runtime continuity.  \n",
            "--operation-id",
            "checkpoint-first",
            "--json",
        ],
    );
    let first_id = json_string(&first, "record_id").to_owned();
    assert_eq!(first_id, "session-implement-checkpoint-workflow");
    assert_json_string_field(&first, &["type"], "episode");
    assert_json_string_field(&first, &["lane"], "session");
    assert_json_string_field(&first, &["destination"], "session");
    assert_json_string_field(&first, &["scope_kind"], "personal");
    assert_json_string_field(&first, &["visibility"], "private");
    assert_json_string_field(&first, &["status"], "active");
    assert_json_string_field(&first, &["source_kind"], "memzoi-checkpoint");
    assert_json_string_field(
        &first,
        &["body"],
        "Remember the checkpoint zircon state only as runtime continuity.",
    );
    assert!(
        first.get("source_ref").is_some_and(Value::is_null),
        "checkpoint source_ref should stay null: {first}"
    );
    assert!(
        !test_paths(repo)
            .records_dir()
            .join(format!("{first_id}.md"))
            .exists(),
        "checkpoint add must not write canonical repo files"
    );

    let note_path = repo.join("checkpoint-notes.md");
    fs::write(
        &note_path,
        "\n  File checkpoint body stays explicit only.  \n",
    )
    .expect("write checkpoint note file");
    let from_file = run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "File checkpoint workflow",
            "--from-file",
            note_path.to_str().expect("note path utf-8"),
            "--operation-id",
            "checkpoint-from-file",
            "--json",
        ],
    );
    let from_file_id = json_string(&from_file, "record_id").to_owned();
    assert_eq!(from_file_id, "session-file-checkpoint-workflow");
    assert_json_string_field(
        &from_file,
        &["body"],
        "File checkpoint body stays explicit only.",
    );
    assert!(
        from_file.get("source_ref").is_some_and(Value::is_null),
        "checkpoint source_ref should not store the local source file path: {from_file}"
    );
    assert!(
        !serde_json::to_string(&from_file)
            .expect("serialize from-file checkpoint")
            .contains(note_path.to_str().expect("note path utf-8")),
        "from-file JSON should not include the local source path: {from_file}"
    );

    let duplicate = run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Implement checkpoint workflow",
            "--note",
            "Second checkpoint for the same task.",
            "--operation-id",
            "checkpoint-duplicate-title",
            "--json",
        ],
    );
    let duplicate_id = json_string(&duplicate, "record_id").to_owned();
    assert_eq!(duplicate_id, "session-implement-checkpoint-workflow-2");

    let local = run_json_command(
        repo,
        &[
            "local",
            "add",
            "--type",
            "preference",
            "--title",
            "Checkpoint local zircon preference",
            "--body",
            "Local memory should survive rebuild beside session checkpoints.",
            "--json",
        ],
    );
    let local_id = json_string(&local, "record_id").to_owned();

    let conn = Connection::open(shared_memory_db_path(repo)).expect("open shared runtime db");
    for (record_id, created_at) in [
        (first_id.as_str(), "2026-07-08T00:00:00Z"),
        (duplicate_id.as_str(), "2026-07-08T00:01:00Z"),
        (from_file_id.as_str(), "2026-07-08T00:02:00Z"),
    ] {
        conn.execute(
            "UPDATE memory_record SET created_at = ?1, updated_at = ?1 WHERE id = ?2",
            rusqlite::params![created_at, record_id],
        )
        .expect("pin checkpoint ordering fixture");
    }
    drop(conn);

    let listed = run_json_command(repo, &["checkpoint", "list", "--json"]);
    assert_json_string_field(&listed, &["destination"], "session");
    assert_eq!(
        record_ids_from_json(&listed),
        vec![
            from_file_id.as_str(),
            duplicate_id.as_str(),
            first_id.as_str()
        ],
        "checkpoint list should return newest checkpoints first: {listed}"
    );

    let global_search = run_json_command(repo, &["search", "zircon", "--json"]);
    assert!(
        record_ids_from_json(&global_search).is_empty(),
        "global search should stay repo-only and exclude runtime checkpoints/local memory: {global_search}"
    );

    let context = run_json_command(repo, &["context", "--task", "checkpoint zircon", "--json"]);
    assert_json_does_not_reference_records(
        &context,
        &[first_id.clone(), duplicate_id.clone(), from_file_id.clone()],
    );

    let export = run_json_command(repo, &["export", "okf", "--json"]);
    assert!(
        written_paths_from_json(&export).is_empty(),
        "runtime checkpoints should not be exported as repo memory: {export}"
    );

    let rebuild = run_json_command(repo, &["rebuild", "--json"]);
    assert!(
        rebuild
            .get("record_ids")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "empty canonical repo should rebuild without importing runtime checkpoints: {rebuild}"
    );

    let checkpoints_after_rebuild = run_json_command(repo, &["checkpoint", "list", "--json"]);
    assert_eq!(
        record_ids_from_json(&checkpoints_after_rebuild),
        vec![
            from_file_id.as_str(),
            duplicate_id.as_str(),
            first_id.as_str()
        ],
        "rebuild should preserve session checkpoint runtime rows: {checkpoints_after_rebuild}"
    );
    let local_after_rebuild = run_json_command(repo, &["local", "list", "--json"]);
    assert_eq!(
        record_ids_from_json(&local_after_rebuild),
        vec![local_id.as_str()],
        "rebuild should preserve local runtime rows beside checkpoints: {local_after_rebuild}"
    );
}

#[test]
fn checkpoint_add_rejects_invalid_explicit_inputs() {
    let repo = initialized_temp_repo();
    let repo = repo.path();
    let empty_note_path = repo.join("empty-checkpoint-note.md");
    fs::write(&empty_note_path, " \n ").expect("write empty checkpoint note");

    let missing_note = run_command_failure_stderr(repo, &["checkpoint", "add", "--task", "Task"]);
    assert!(
        missing_note.contains("requires --note or --from-file"),
        "missing checkpoint body should fail clearly: {missing_note}"
    );

    let both_inputs = run_command_failure_stderr(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Task",
            "--note",
            "note",
            "--from-file",
            empty_note_path.to_str().expect("empty note path utf-8"),
        ],
    );
    assert!(
        both_inputs.contains("either --note or --from-file"),
        "checkpoint add should reject ambiguous inputs: {both_inputs}"
    );

    let empty_note = run_command_failure_stderr(
        repo,
        &["checkpoint", "add", "--task", "Task", "--note", "  "],
    );
    assert!(
        empty_note.contains("note is required"),
        "empty checkpoint note should fail validation: {empty_note}"
    );

    let empty_task = run_command_failure_stderr(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "  ",
            "--note",
            "explicit note",
        ],
    );
    assert!(
        empty_task.contains("task is required"),
        "empty checkpoint task should fail validation: {empty_task}"
    );

    let empty_file = run_command_failure_stderr(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Task",
            "--from-file",
            empty_note_path.to_str().expect("empty note path utf-8"),
        ],
    );
    assert!(
        empty_file.contains("note is required"),
        "empty checkpoint file should fail validation: {empty_file}"
    );

    let listed = run_json_command(repo, &["checkpoint", "list", "--json"]);
    assert!(
        record_ids_from_json(&listed).is_empty(),
        "invalid checkpoint inputs should not create runtime records: {listed}"
    );
}
