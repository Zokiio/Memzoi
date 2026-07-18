use super::*;

#[test]
fn reintroduced_pending_packet_cannot_reverse_a_resolved_outcome() {
    let rejected_repo = initialized_temp_repo();
    let proposal = valid_proposal_markdown();
    write_pending_proposal_file(rejected_repo.path(), "valid-proposal.md", proposal.clone());
    run_json_command(
        rejected_repo.path(),
        &[
            "proposal-files",
            "reject",
            "mem_test_valid",
            "--reason",
            "Reviewed rejection remains final.",
            "--json",
        ],
    );
    write_pending_proposal_file(rejected_repo.path(), "valid-proposal.md", proposal.clone());
    let validation = run_json_command_failure(
        rejected_repo.path(),
        &["proposal-files", "validate", "--json"],
    );
    assert_eq!(validation["valid"], false, "{validation}");
    assert!(
        serde_json::to_string(&validation["errors"])
            .expect("serialize terminal identity errors")
            .contains("reintroduces resolved identity"),
        "{validation}"
    );
    let doctor = run_json_command(rejected_repo.path(), &["doctor", "--json"]);
    let proposal_check = doctor["checks"]
        .as_array()
        .and_then(|checks| {
            checks
                .iter()
                .find(|check| check["name"] == "proposal_files")
        })
        .expect("doctor proposal-files check");
    assert_eq!(proposal_check["status"], "warning", "{doctor}");
    assert!(
        proposal_check["message"]
            .as_str()
            .is_some_and(|message| message.contains("reintroduces resolved identity")),
        "{doctor}"
    );
    let rejected_then_apply = run_command_failure_stderr(
        rejected_repo.path(),
        &["proposal-files", "apply", "mem_test_valid"],
    );
    assert!(
        rejected_then_apply.contains("reintroduces resolved identity")
            && rejected_then_apply.contains("already rejected"),
        "unexpected contradictory apply error: {rejected_then_apply}"
    );
    assert!(
        !rejected_repo
            .path()
            .join(".memzoi/records/valid-proposal.md")
            .exists()
    );

    let applied_repo = initialized_temp_repo();
    write_pending_proposal_file(applied_repo.path(), "valid-proposal.md", proposal.clone());
    run_json_command(
        applied_repo.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    let record_path = applied_repo
        .path()
        .join(".memzoi/records/valid-proposal.md");
    let record_before = fs::read(&record_path).expect("read applied record");
    write_pending_proposal_file(applied_repo.path(), "valid-proposal.md", proposal);
    let applied_then_reject = run_command_failure_stderr(
        applied_repo.path(),
        &[
            "proposal-files",
            "reject",
            "mem_test_valid",
            "--reason",
            "A stale branch must not reverse apply.",
        ],
    );
    assert!(
        applied_then_reject.contains("reintroduces resolved identity")
            && applied_then_reject.contains("already applied"),
        "unexpected contradictory reject error: {applied_then_reject}"
    );
    assert_eq!(
        fs::read(&record_path).expect("read record after refused rejection"),
        record_before
    );
    assert!(
        !applied_repo
            .path()
            .join(".memzoi/proposals/resolved/rejected/valid-proposal.md")
            .exists()
    );
}

#[test]
fn proposal_file_apply_detects_duplicate_pending_identity_even_by_unique_file_slug() {
    let repo = initialized_temp_repo();
    write_pending_proposal_file(repo.path(), "valid-proposal.md", valid_proposal_markdown());
    write_pending_proposal_file(repo.path(), "duplicate-slug.md", valid_proposal_markdown());

    let error =
        run_command_failure_stderr(repo.path(), &["proposal-files", "apply", "valid-proposal"]);
    assert!(
        error.contains("duplicate pending proposal identity token")
            && !error.contains("mem_test_valid"),
        "unexpected duplicate identity error: {error}"
    );
    assert!(
        !repo
            .path()
            .join(".memzoi/records/valid-proposal.md")
            .exists()
    );
}

#[test]
fn applied_replay_repairs_missing_and_stale_derived_rows_from_canonical_truth() {
    let repo = initialized_temp_repo();
    write_pending_proposal_file(repo.path(), "valid-proposal.md", valid_proposal_markdown());
    run_json_command(
        repo.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );

    {
        let conn = Connection::open(test_paths(repo.path()).db_path).expect("open runtime db");
        conn.execute("DELETE FROM memory_record WHERE id = 'valid-proposal'", [])
            .expect("delete derived row");
    }
    let repaired_missing = run_json_command(
        repo.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    assert_eq!(repaired_missing["already_resolved"], true);
    assert_eq!(repaired_missing["runtime_index_updated"], true);
    let search = run_json_command(repo.path(), &["search", "proposal body", "--json"]);
    assert_eq!(record_ids_from_json(&search), vec!["valid-proposal"]);

    {
        let conn = Connection::open(test_paths(repo.path()).db_path).expect("open runtime db");
        conn.execute(
            "UPDATE memory_record SET body = 'stale derived bytes' WHERE id = 'valid-proposal'",
            [],
        )
        .expect("stale derived row");
    }
    let repaired_stale = run_json_command(
        repo.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    assert_eq!(repaired_stale["runtime_index_updated"], true);
    let already_current = run_json_command(
        repo.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    assert_eq!(already_current["already_resolved"], true);
    assert_eq!(already_current["runtime_index_updated"], false);
}

#[test]
fn applied_replay_repairs_fts_only_drift_and_doctor_reports_it() {
    let repo = initialized_temp_repo();
    write_pending_proposal_file(repo.path(), "valid-proposal.md", valid_proposal_markdown());
    run_json_command(
        repo.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );

    {
        let conn = Connection::open(test_paths(repo.path()).db_path).expect("open runtime db");
        conn.execute(
            "INSERT INTO memory_fts(memory_fts, rowid, title, body)
             SELECT 'delete', rowid, title, body
             FROM memory_record
             WHERE id = 'valid-proposal'",
            [],
        )
        .expect("delete only the full-text index entry");
    }
    let missing_search =
        run_command_failure_stderr(repo.path(), &["search", "proposal body", "--json"]);
    assert!(
        missing_search.contains("fts_out_of_sync=true"),
        "search must refuse an out-of-sync derived index: {missing_search}"
    );
    let stale_doctor = run_json_command(repo.path(), &["doctor", "--json"]);
    assert_check_status(&stale_doctor, "repo_index", "warning");
    assert!(
        serde_json::to_string(&stale_doctor)
            .expect("serialize doctor")
            .contains("fts_out_of_sync=true"),
        "{stale_doctor}"
    );

    let repaired = run_json_command(
        repo.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    assert_eq!(repaired["already_resolved"], true);
    assert_eq!(repaired["runtime_index_updated"], true);
    let repaired_search = run_json_command(repo.path(), &["search", "proposal body", "--json"]);
    assert_eq!(
        record_ids_from_json(&repaired_search),
        vec!["valid-proposal"]
    );
    let healthy_doctor = run_json_command(repo.path(), &["doctor", "--json"]);
    assert_check_status(&healthy_doctor, "repo_index", "ok");
}

#[test]
fn applied_replay_refuses_missing_or_changed_canonical_truth() {
    let missing = initialized_temp_repo();
    write_pending_proposal_file(
        missing.path(),
        "valid-proposal.md",
        valid_proposal_markdown(),
    );
    run_json_command(
        missing.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    fs::remove_file(missing.path().join(".memzoi/records/valid-proposal.md"))
        .expect("remove canonical record");
    let missing_error = run_command_failure_stderr(
        missing.path(),
        &["proposal-files", "apply", "mem_test_valid"],
    );
    assert!(
        missing_error.contains("canonical drift") && missing_error.contains("valid-proposal"),
        "unexpected missing canonical error: {missing_error}"
    );

    let changed = initialized_temp_repo();
    write_pending_proposal_file(
        changed.path(),
        "valid-proposal.md",
        valid_proposal_markdown(),
    );
    run_json_command(
        changed.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    let record_path = changed.path().join(".memzoi/records/valid-proposal.md");
    let changed_bytes = fs::read_to_string(&record_path)
        .expect("read canonical record")
        .replace(
            "This proposal body is valid.",
            "Human-edited canonical bytes after resolution.",
        );
    fs::write(&record_path, &changed_bytes).expect("change canonical bytes");
    let changed_error = run_command_failure_stderr(
        changed.path(),
        &["proposal-files", "apply", "mem_test_valid"],
    );
    assert!(
        changed_error.contains("canonical byte drift"),
        "unexpected changed canonical error: {changed_error}"
    );
    assert_eq!(
        fs::read_to_string(&record_path).expect("read changed canonical after replay"),
        changed_bytes,
        "replay must not overwrite changed canonical truth"
    );
}

#[test]
fn import_plan_and_apply_mixed_destinations_preserve_boundaries() {
    let repo = initialized_temp_repo();
    let repo_path = repo.path();
    let manifest_path = repo_path.join("mixed-import.yml");
    fs::write(
        &manifest_path,
        r#"schema: memzoi/import
origin_key: test-import:proposal-recovery-safe
sources:
  - path: imports/safe-source.yml
candidates:
  - destination: repo
    reason: durable project convention
    type: decision
    lane: semantic
    title: Repository convention
    body: The repository uses explicit review before durable memory changes.
    sensitivity: repo-safe
    content_class: general_repo_knowledge
    scope:
      kind: repo
    tags: [workflow]
  - destination: local
    reason: private developer preference
    type: fact
    title: Local preference
    body: Keep this preference in local runtime memory only.
    sensitivity: local-only
  - destination: session
    reason: current handoff continuity
    type: episode
    lane: session
    title: Session continuity
    body: Resume the import review in the next session.
    sensitivity: local-only
  - destination: discard
    reason: stale transient note
    type: fact
    title: Transient note
    body: This note is no longer useful.
    sensitivity: unknown
  - destination: needs_review
    reason: ambiguous privacy boundary
    type: fact
    title: Ambiguous note
    body: Decide whether this content is safe to retain.
    sensitivity: unknown
"#,
    )
    .expect("write mixed import manifest");

    let paths = test_paths(repo_path);
    let directory_entries = |directory: &Path| -> Vec<String> {
        if !directory.is_dir() {
            return Vec::new();
        }
        let mut entries = fs::read_dir(directory)
            .expect("read directory")
            .map(|entry| {
                entry
                    .expect("read directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        entries.sort();
        entries
    };
    let records_before = directory_entries(&paths.records_dir());
    let proposals_before = directory_entries(&paths.proposals_dir());
    let database_before = fs::read(&paths.db_path).expect("read initialized runtime db");

    let planned = run_json_command(
        repo_path,
        &[
            "import",
            "plan",
            "--from-file",
            manifest_path.to_str().expect("manifest path utf-8"),
            "--json",
        ],
    );
    assert_json_string_field(&planned, &["mode"], "plan");
    assert_json_string_field(&planned, &["schema"], "memzoi/import-plan");
    assert_json_string_field(&planned, &["source_file"], "mixed-import.yml");
    let plan_id = json_string(&planned, "plan_id").to_owned();
    assert!(
        !plan_id.is_empty(),
        "plan should expose a plan id: {planned}"
    );
    assert_eq!(
        planned["summary"],
        serde_json::json!({
            "total": 5,
            "create_proposals": 1,
            "local_writes": 1,
            "session_writes": 1,
            "duplicates": 0,
            "discarded": 1,
            "needs_review": 1,
            "replays": 0,
        }),
        "plan should summarize every destination: {planned}"
    );
    let planned_candidates = planned["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("plan should include candidates: {planned}"));
    assert_eq!(planned_candidates.len(), 5);
    for (candidate, (destination, action)) in planned_candidates.iter().zip([
        ("repo", "create_proposal"),
        ("local", "create_runtime"),
        ("session", "create_runtime"),
        ("discard", "no_write"),
        ("needs_review", "blocked"),
    ]) {
        assert_json_string_field(&candidate["classification"], &["destination"], destination);
        assert_json_string_field(&candidate["action"], &["kind"], action);
    }
    assert!(
        planned.get("writes").is_none(),
        "plan mode must not report writes: {planned}"
    );
    assert_eq!(
        directory_entries(&paths.records_dir()),
        records_before,
        "plan must not create canonical records"
    );
    assert_eq!(
        directory_entries(&paths.proposals_dir()),
        proposals_before,
        "plan must not create proposal files"
    );
    assert_eq!(
        fs::read(&paths.db_path).expect("read runtime db after plan"),
        database_before,
        "plan must not mutate runtime state"
    );

    let repo_proposal_id = json_string(&planned_candidates[0]["action"], "proposal_id").to_owned();
    let applied = run_json_command(
        repo_path,
        &[
            "import",
            "apply",
            "--from-file",
            manifest_path.to_str().expect("manifest path utf-8"),
            "--plan-id",
            plan_id.as_str(),
            "--json",
        ],
    );
    assert_json_string_field(&applied, &["mode"], "apply");
    assert_json_string_field(&applied, &["expected_plan_id"], &plan_id);
    assert_json_string_field(&applied, &["schema"], "memzoi/import-plan");
    assert_json_string_field(&applied, &["source_file"], "mixed-import.yml");
    let writes = applied["writes"]
        .as_array()
        .unwrap_or_else(|| panic!("apply should include writes: {applied}"));
    assert_eq!(
        writes.len(),
        3,
        "repo/local/session should write: {applied}"
    );
    assert_json_string_field(&writes[0], &["kind"], "proposal_file");
    assert_eq!(writes[0]["index"], 0);
    assert_json_string_field(&writes[0], &["proposal_id"], &repo_proposal_id);
    assert_json_string_field(
        &writes[0],
        &["path"],
        &format!(".memzoi/proposals/pending/{repo_proposal_id}.md"),
    );
    assert_json_string_field(&writes[1], &["kind"], "runtime_record");
    assert_eq!(writes[1]["index"], 1);
    assert_json_string_field(&writes[1], &["destination"], "local");
    assert_json_string_field(&writes[2], &["kind"], "runtime_record");
    assert_eq!(writes[2]["index"], 2);
    assert_json_string_field(&writes[2], &["destination"], "session");
    let applied_candidates = applied["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("apply should include candidates: {applied}"));
    assert_eq!(applied_candidates.len(), 5);
    for (candidate, (destination, action)) in applied_candidates.iter().zip([
        ("repo", "create_proposal"),
        ("local", "create_runtime"),
        ("session", "create_runtime"),
        ("discard", "no_write"),
        ("needs_review", "blocked"),
    ]) {
        assert_json_string_field(&candidate["classification"], &["destination"], destination);
        assert_json_string_field(&candidate["action"], &["kind"], action);
    }
    assert_json_string_field(
        &applied_candidates[1]["action"],
        &["route"],
        "runtime_local",
    );
    assert_json_string_field(
        &applied_candidates[2]["action"],
        &["route"],
        "runtime_session",
    );
    assert_eq!(
        directory_entries(&paths.records_dir()),
        records_before,
        "import apply must not create canonical records"
    );
    let database_after_apply = fs::read(&paths.db_path).expect("read runtime db after apply");
    assert_ne!(
        database_after_apply, database_before,
        "import apply should write local/session runtime memory"
    );
    let pending_path = paths
        .proposals_dir()
        .join("pending")
        .join(format!("{repo_proposal_id}.md"));
    assert!(
        pending_path.is_file(),
        "repo candidate should create pending OKF proposal"
    );
    let rendered_proposal = fs::read_to_string(&pending_path).expect("read pending proposal");
    assert!(
        rendered_proposal
            .contains("The repository uses explicit review before durable memory changes."),
        "pending proposal should contain only repo candidate content: {rendered_proposal}"
    );
    for private_content in [
        "Keep this preference in local runtime memory only.",
        "Resume the import review in the next session.",
        "Decide whether this content is safe to retain.",
    ] {
        assert!(
            !rendered_proposal.contains(private_content),
            "pending proposal must not contain non-repo content {private_content:?}"
        );
    }

    let local = run_json_command(repo_path, &["local", "list", "--json"]);
    let local_records = local["records"]
        .as_array()
        .unwrap_or_else(|| panic!("local list should include records: {local}"));
    assert_eq!(
        local_records.len(),
        1,
        "local import should write once: {local}"
    );
    assert_json_string_field(&local_records[0], &["destination"], "local");
    assert_json_string_field(
        &local_records[0],
        &["body"],
        "Keep this preference in local runtime memory only.",
    );
    let session = run_json_command(repo_path, &["checkpoint", "list", "--json"]);
    let session_records = session["records"]
        .as_array()
        .unwrap_or_else(|| panic!("checkpoint list should include records: {session}"));
    assert_eq!(
        session_records.len(),
        1,
        "session import should write once: {session}"
    );
    assert_json_string_field(&session_records[0], &["destination"], "session");
    assert_json_string_field(
        &session_records[0],
        &["body"],
        "Resume the import review in the next session.",
    );

    let validated = run_json_command(repo_path, &["proposal-files", "validate", "--json"]);
    assert_eq!(
        validated["valid"], true,
        "generated proposal should validate: {validated}"
    );
    assert_eq!(validated["valid_count"], 1);
    assert_eq!(validated["invalid_count"], 0);
    let shown = run_json_command(
        repo_path,
        &[
            "proposal-files",
            "show",
            repo_proposal_id.as_str(),
            "--json",
        ],
    );
    assert_eq!(proposal_id_from_value(&shown), repo_proposal_id);
    assert_json_string_field(&shown, &["action"], "create");
    assert_json_string_field(&shown, &["status"], "proposed");
    assert_json_string_field(&shown, &["sensitivity"], "repo-safe");
    assert_json_string_field(
        &shown["proposal"],
        &["reason"],
        "durable project convention",
    );
    assert_json_string_field(
        &shown,
        &["body"],
        "The repository uses explicit review before durable memory changes.",
    );
    assert_json_string_field(&shown["sources"][0], &["path"], "imports/safe-source.yml");

    let pending_after_apply = directory_entries(&paths.proposals_dir());
    assert_eq!(pending_after_apply, vec!["pending".to_owned()]);
    let proposal_files_before_stale = directory_entries(&paths.proposals_dir().join("pending"));
    let stale_id = format!("{plan_id}-stale");
    let stale_error = run_command_failure_stderr(
        repo_path,
        &[
            "import",
            "apply",
            "--from-file",
            manifest_path.to_str().expect("manifest path utf-8"),
            "--plan-id",
            stale_id.as_str(),
            "--json",
        ],
    );
    assert!(
        stale_error.contains("stale import plan"),
        "stale plan failure should identify the plan mismatch: {stale_error}"
    );
    assert_eq!(
        directory_entries(&paths.proposals_dir().join("pending")),
        proposal_files_before_stale,
        "stale plan must not create a second proposal"
    );
    assert_eq!(
        directory_entries(&paths.records_dir()),
        records_before,
        "stale plan must not create canonical records"
    );
    assert_eq!(
        fs::read(&paths.db_path).expect("read runtime db after stale plan"),
        database_after_apply,
        "stale plan must not mutate runtime state"
    );
}

#[test]
fn import_cli_redacts_blocked_repo_candidate_from_plan_apply_and_proposal_files() {
    let repo = initialized_temp_repo();
    let manifest_path = repo.path().join("blocked-import.yml");
    fs::write(
        &manifest_path,
        r#"schema: memzoi/import
origin_key: test-import:proposal-recovery-sentinel
sources:
  - path: imports/IMPORT-SOURCE-SENTINEL.yml
candidates:
  - destination: repo
    reason: IMPORT-REASON-SENTINEL
    type: fact
    title: IMPORT-TITLE-SENTINEL
    body: IMPORT-BODY-SENTINEL
    scope:
      kind: repo
      paths: [src/IMPORT-PATH-SENTINEL/**]
  - destination: repo
    reason: durable project fact
    type: fact
    title: Safe imported fact
    body: This repo-safe candidate may become a reviewed proposal.
    sensitivity: repo-safe
    content_class: general_repo_knowledge
"#,
    )
    .expect("write blocked import manifest");

    let mut plan_command = memzoi();
    let plan_assert = plan_command
        .args([
            "import",
            "plan",
            "--from-file",
            manifest_path.to_str().expect("manifest path utf-8"),
            "--json",
        ])
        .current_dir(repo.path())
        .assert()
        .success();
    let plan_stdout =
        std::str::from_utf8(&plan_assert.get_output().stdout).expect("plan stdout utf-8");
    let plan_stderr =
        std::str::from_utf8(&plan_assert.get_output().stderr).expect("plan stderr utf-8");
    assert!(!plan_stdout.contains("SENTINEL"), "{plan_stdout}");
    assert!(!plan_stderr.contains("SENTINEL"), "{plan_stderr}");
    let plan: Value = serde_json::from_str(plan_stdout).expect("plan stdout JSON");
    assert_eq!(plan["sources"], serde_json::json!([]));
    assert_json_string_field(&plan["candidates"][0], &["sensitivity"], "unknown");
    assert_json_string_field(
        &plan["candidates"][0],
        &["title"],
        "Redacted non-repo-safe import candidate",
    );
    assert_json_string_field(&plan["candidates"][0]["action"], &["kind"], "blocked");
    assert_json_string_field(&plan["candidates"][1]["action"], &["kind"], "blocked");
    assert!(
        plan["candidates"][1]["action"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("split the manifest")),
        "{plan}"
    );
    let plan_id = json_string(&plan, "plan_id").to_owned();

    let mut apply_command = memzoi();
    let apply_assert = apply_command
        .args([
            "import",
            "apply",
            "--from-file",
            manifest_path.to_str().expect("manifest path utf-8"),
            "--plan-id",
            &plan_id,
            "--json",
        ])
        .current_dir(repo.path())
        .assert()
        .success();
    let apply_stdout =
        std::str::from_utf8(&apply_assert.get_output().stdout).expect("apply stdout utf-8");
    let apply_stderr =
        std::str::from_utf8(&apply_assert.get_output().stderr).expect("apply stderr utf-8");
    assert!(!apply_stdout.contains("SENTINEL"), "{apply_stdout}");
    assert!(!apply_stderr.contains("SENTINEL"), "{apply_stderr}");
    let applied: Value = serde_json::from_str(apply_stdout).expect("apply stdout JSON");
    assert_eq!(applied["writes"].as_array().map(Vec::len), Some(0));

    let pending = test_paths(repo.path()).proposals_dir().join("pending");
    let proposal_paths = if pending.is_dir() {
        fs::read_dir(&pending)
            .expect("read pending proposals")
            .map(|entry| entry.expect("read proposal entry").path())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    assert!(proposal_paths.is_empty(), "{proposal_paths:?}");
    assert!(test_paths(repo.path()).records_dir().is_dir());
    assert_eq!(
        fs::read_dir(test_paths(repo.path()).records_dir())
            .expect("read canonical records")
            .count(),
        0
    );
}

#[test]
fn import_plan_and_apply_hide_external_manifest_paths() {
    let repo = initialized_temp_repo();
    let external_dir = tempfile::tempdir().expect("external manifest dir");
    let manifest_path = external_dir.path().join("external-import.yml");
    fs::write(
        &manifest_path,
        r#"schema: memzoi/import
origin_key: test-import:proposal-recovery-external
sources:
  - path: imports/external-source.yml
candidates:
  - destination: discard
    reason: external manifest privacy check
    type: fact
    title: External manifest candidate
    body: This candidate must not expose its manifest path.
    sensitivity: unknown
"#,
    )
    .expect("write external import manifest");

    let planned = run_json_command(
        repo.path(),
        &[
            "import",
            "plan",
            "--from-file",
            manifest_path.to_str().expect("manifest path utf-8"),
            "--json",
        ],
    );
    assert!(
        planned.get("source_file").is_none_or(Value::is_null),
        "external plan must omit or null source_file rather than expose an absolute path: {planned}"
    );
    let plan_id = json_string(&planned, "plan_id").to_owned();

    let applied = run_json_command(
        repo.path(),
        &[
            "import",
            "apply",
            "--from-file",
            manifest_path.to_str().expect("manifest path utf-8"),
            "--plan-id",
            plan_id.as_str(),
            "--json",
        ],
    );
    assert!(
        applied.get("source_file").is_none_or(Value::is_null),
        "external apply must omit or null source_file rather than expose an absolute path: {applied}"
    );
}

#[test]
fn proposal_files_apply_uses_file_id_fallback_for_titles_without_ascii_slug() {
    let repo = initialized_temp_repo();
    write_pending_proposal_file(
        repo.path(),
        "unicode-proposal.md",
        proposal_markdown_with_title("記憶"),
    );

    let applied = run_json_command(
        repo.path(),
        &["proposal-files", "apply", "mem_test_unicode", "--json"],
    );
    assert_json_string_field(&applied, &["record_id"], "unicode-proposal");
    assert_json_string_field(
        &applied,
        &["record_path"],
        ".memzoi/records/unicode-proposal.md",
    );
    assert!(
        repo.path()
            .join(".memzoi/records/unicode-proposal.md")
            .is_file(),
        "non-ASCII titles should use deterministic file-id fallback"
    );
}

#[test]
fn proposal_files_apply_refuses_existing_canonical_record() {
    let repo = initialized_temp_repo();
    write_pending_proposal_file(repo.path(), "valid-proposal.md", valid_proposal_markdown());
    let record_path = repo.path().join(".memzoi/records/valid-proposal.md");
    fs::create_dir_all(record_path.parent().expect("record parent")).expect("create records dir");
    fs::write(&record_path, "human-authored canonical memory\n").expect("write collision");

    let stderr =
        run_command_failure_stderr(repo.path(), &["proposal-files", "apply", "mem_test_valid"]);
    assert!(
        stderr.contains("canonical memory record already exists"),
        "expected collision error, got {stderr}"
    );
    assert_eq!(
        fs::read_to_string(&record_path).expect("read existing record"),
        "human-authored canonical memory\n"
    );
}

#[test]
fn proposal_files_apply_rejects_non_repo_safe_sensitivity() {
    for (sensitivity, guidance) in [
        (
            "local-only",
            "local-only proposals belong in the future local/runtime memory plane",
        ),
        (
            "sensitive",
            "classify or sanitize sensitive content before applying it to the repo plane",
        ),
        (
            "secret",
            "secret proposals must not become repo-shared memory",
        ),
        (
            "raw-transcript",
            "raw transcripts must not become repo-shared memory",
        ),
        (
            "private-personal-data",
            "private personal data must not become repo-shared memory",
        ),
        (
            "temporary-state",
            "temporary task state belongs in local or session memory",
        ),
        (
            "unknown",
            "classify the proposal sensitivity before applying it to repo records",
        ),
    ] {
        let repo = initialized_temp_repo();
        write_pending_proposal_file(
            repo.path(),
            "valid-proposal.md",
            proposal_markdown_with_options(
                "semantic",
                "create",
                "proposed",
                "supersedes: []",
                "",
                sensitivity,
            ),
        );

        let blocked = run_json_command_failure(
            repo.path(),
            &["proposal-files", "apply", "mem_test_valid", "--json"],
        );
        assert_eq!(blocked["ok"], false);
        assert_json_string_field(&blocked["error"], &["code"], "repo_sensitivity_required");
        assert_json_string_field(&blocked["error"], &["operation"], "proposal_files_apply");
        assert_json_string_field(&blocked["error"], &["sensitivity"], sensitivity);
        let rendered = serde_json::to_string(&blocked).expect("serialize sensitivity error");
        assert!(rendered.contains(guidance), "{rendered}");
        assert!(
            !rendered.contains("This proposal body is valid."),
            "{rendered}"
        );

        let stderr =
            run_command_failure_stderr(repo.path(), &["proposal-files", "apply", "mem_test_valid"]);
        assert!(
            stderr.contains(&format!(
                "sensitivity {sensitivity} cannot be applied into repo records"
            )),
            "expected sensitivity rejection for {sensitivity}, got {stderr}"
        );
        assert!(
            stderr.contains(guidance),
            "expected next-step guidance for {sensitivity}, got {stderr}"
        );
        assert!(
            !repo
                .path()
                .join(".memzoi/records/valid-proposal.md")
                .exists(),
            "blocked sensitivity {sensitivity} should not create a record"
        );
        let conn = Connection::open(test_paths(repo.path()).db_path).expect("open runtime db");
        let runtime_records: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))
            .expect("count runtime records");
        assert_eq!(
            runtime_records, 0,
            "blocked sensitivity {sensitivity} should not write SQLite"
        );
    }
}
