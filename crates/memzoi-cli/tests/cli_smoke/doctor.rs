use super::*;

#[test]
fn doctor_json_reports_missing_bundle_before_init() {
    let repo = tempfile::tempdir().expect("temp repo");
    run_git_fixture(repo.path(), &["init", "-q"]);

    let doctor = run_json_command(repo.path(), &["doctor", "--json"]);

    assert_eq!(json_string(&doctor, "status"), "warning");
    assert_json_path(&doctor, "project_root", repo.path());
    assert_json_array_contains(&doctor, "next_steps", "memzoi init");
    assert_check_status(&doctor, "config", "warning");
    assert_check_status(&doctor, "database", "warning");
}

#[test]
fn normal_commands_require_initialized_bundle_without_creating_one() {
    let repo = tempfile::tempdir().expect("temp repo");
    run_git_fixture(repo.path(), &["init", "-q"]);

    let mut cmd = memzoi();
    cmd.args(["search", "quickstart", "--json"])
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("memzoi init"));

    assert!(
        !repo.path().join(".memzoi").exists(),
        "search should not create a bundle before explicit init"
    );
}

#[test]
fn doctor_warns_when_exports_dir_is_missing_even_if_bundle_dir_exists() {
    let repo = tempfile::tempdir().expect("temp repo");
    run_git_fixture(repo.path(), &["init", "-q"]);
    let paths = test_paths(repo.path());
    fs::create_dir_all(paths.records_dir()).expect("create records dir");
    fs::create_dir_all(&paths.runtime_dir).expect("create runtime dir");
    fs::write(&paths.config_path, "scope_kind = \"repo\"\n").expect("write config");

    let doctor = run_json_command(repo.path(), &["doctor", "--json"]);

    assert_check_status(&doctor, "config", "ok");
    assert_check_status(&doctor, "exports", "warning");
}

#[test]
fn doctor_json_reports_ready_after_init_and_warns_when_mcp_binary_is_missing() {
    let repo = initialized_temp_repo();
    let mut cmd = memzoi();
    let git_bin_dir = git_fixture_executable()
        .parent()
        .expect("Git fixture executable has a parent directory")
        .to_path_buf();

    let assert = cmd
        .args(["doctor", "--json"])
        .current_dir(repo.path())
        .env("PATH", git_bin_dir)
        .assert()
        .success();
    let stdout = std::str::from_utf8(&assert.get_output().stdout).expect("stdout is utf-8");
    let doctor: Value = serde_json::from_str(stdout).expect("stdout is JSON");

    assert_eq!(json_string(&doctor, "status"), "warning");
    assert_json_path(&doctor, "project_root", repo.path());
    assert_check_status(&doctor, "config", "ok");
    assert_check_status(&doctor, "shared_database", "ok");
    assert_check_status(&doctor, "shared_schema", "ok");
    assert_check_status(&doctor, "database", "ok");
    assert_check_status(&doctor, "schema", "ok");
    assert_check_status(&doctor, "exports", "ok");
    assert_check_status(&doctor, "proposals", "ok");
    assert_check_status(&doctor, "mcp", "warning");
    assert_json_array_contains(&doctor, "next_steps", "memzoi mcp config --project-root .");
}

#[test]
fn doctor_distinguishes_broken_shared_schema_from_ready_worktree_index() {
    let repo = initialized_temp_repo();
    let shared_db_path = shared_memory_db_path(repo.path());
    let conn = Connection::open(&shared_db_path).expect("open shared database fixture");
    conn.execute("DROP TABLE memory_record", [])
        .expect("remove shared memory table");
    drop(conn);

    let doctor = run_json_command(repo.path(), &["doctor", "--json"]);

    assert_check_status(&doctor, "shared_database", "ok");
    assert_check_status(&doctor, "shared_schema", "warning");
    assert_check_status(&doctor, "schema", "ok");
    let conn = Connection::open(&shared_db_path).expect("reopen shared database read-only check");
    let memory_table_exists: bool = conn
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'memory_record'
             )",
            [],
            |row| row.get(0),
        )
        .expect("inspect shared schema after doctor");
    assert!(
        !memory_table_exists,
        "doctor must not repair a broken shared schema before reporting it"
    );
}

#[test]
fn doctor_warns_about_hidden_lifecycle_artifacts_without_exposing_their_identity() {
    let repo = initialized_temp_repo();
    let pending = repo.path().join(".memzoi/proposals/pending");
    fs::create_dir_all(&pending).expect("create pending root");
    let sentinel = "RAW-SECRET-IDENTITY-SENTINEL";
    fs::write(
        pending.join(format!(".{sentinel}.nonce.pending.tmp")),
        "unresolved transaction bytes",
    )
    .expect("write transaction artifact");

    let doctor = run_json_command(repo.path(), &["doctor", "--json"]);
    assert_check_status(&doctor, "lifecycle_transactions", "warning");
    let rendered = serde_json::to_string(&doctor).expect("serialize doctor output");
    assert!(rendered.contains("1 hidden lifecycle transaction artifact"));
    assert!(
        !rendered.contains(sentinel),
        "doctor leaked the transaction identity: {rendered}"
    );
}

#[test]
fn doctor_json_warns_about_open_proposals_and_prints_next_steps() {
    let repo = initialized_temp_repo();
    run_json_command(
        repo.path(),
        &[
            "propose",
            "--manual",
            "--type",
            "decision",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--sensitivity",
            "repo-safe",
            "--content-class",
            "general_repo_knowledge",
            "--title",
            "Doctor should surface pending proposals",
            "--body",
            "Doctor must report pending proposal inbox work before destructive maintenance.",
            "--actor",
            "agent:cli-smoke",
            "--json",
        ],
    );

    let doctor = run_json_command(repo.path(), &["doctor", "--json"]);

    assert_check_status(&doctor, "proposals", "warning");
    assert_json_array_contains_substring(&doctor, "next_steps", "memzoi proposals");
}

#[test]
fn quickstart_without_apply_prints_manual_commands() {
    let repo = initialized_temp_repo();
    let mut cmd = memzoi();

    cmd.arg("quickstart")
        .current_dir(repo.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("memzoi quickstart --apply-sample")
                .and(predicate::str::contains("memzoi search quickstart"))
                .and(predicate::str::contains(
                    "memzoi mcp config --project-root .",
                )),
        );
}

#[test]
fn quickstart_apply_sample_creates_searchable_memory_and_export() {
    let repo = initialized_temp_repo();

    let quickstart = run_json_command(repo.path(), &["quickstart", "--apply-sample", "--json"]);

    assert_eq!(
        quickstart.get("created").and_then(Value::as_bool),
        Some(true)
    );
    assert!(json_string(&quickstart, "proposal_id").starts_with("prop_"));
    assert_eq!(
        json_string(&quickstart, "record_id"),
        "use-memzoi-quickstart-sample"
    );
    assert_eq!(
        quickstart.get("search_count").and_then(Value::as_u64),
        Some(1)
    );
    assert_json_array_contains(
        &quickstart,
        "next_steps",
        "memzoi mcp config --project-root .",
    );
    assert_export_paths_exist(&written_paths_from_json(&quickstart));

    let search = run_json_command(repo.path(), &["search", "quickstart", "--json"]);
    let ids = record_ids_from_json(&search);
    assert!(ids.contains(&json_string(&quickstart, "record_id")));
}

#[test]
fn quickstart_apply_sample_is_idempotent() {
    let repo = initialized_temp_repo();

    let first = run_json_command(repo.path(), &["quickstart", "--apply-sample", "--json"]);
    let second = run_json_command(repo.path(), &["quickstart", "--apply-sample", "--json"]);

    assert_eq!(second.get("created").and_then(Value::as_bool), Some(false));
    assert_eq!(
        json_string(&second, "record_id"),
        json_string(&first, "record_id")
    );
    assert_eq!(second.get("proposal_id"), Some(&Value::Null));
    assert_eq!(second.get("search_count").and_then(Value::as_u64), Some(1));
}

#[test]
fn mcp_config_json_uses_absolute_project_root() {
    let repo = initialized_temp_repo();

    let config = run_json_command(repo.path(), &["mcp", "config", "--project-root", "."]);
    let server = &config["mcpServers"]["memzoi"];

    assert_eq!(server["command"], "memzoi-mcp");
    assert_eq!(
        server["args"][0].as_str(),
        Some("--project-root"),
        "MCP config should pass project-root explicitly: {config}"
    );
    assert_eq!(
        server["args"][1].as_str(),
        Some(repo.path().canonicalize().unwrap().to_str().unwrap()),
        "MCP config should use an absolute project root: {config}"
    );
}
