use super::*;

#[test]
fn init_json_creates_memory_bundle_and_second_init_fails_without_force() {
    let repo = tempfile::tempdir().expect("temp repo");
    run_git_fixture(repo.path(), &["init", "-q"]);

    let mut cmd = memzoi();
    let assert = cmd
        .args(["init", "--json"])
        .current_dir(repo.path())
        .assert()
        .success();
    let stdout = std::str::from_utf8(&assert.get_output().stdout).expect("stdout is utf-8");
    let json: Value = serde_json::from_str(stdout).expect("stdout is JSON");

    let paths = test_paths(repo.path());
    let memory_dir = repo.path().join(".memzoi");
    let records_dir = memory_dir.join("records");
    let config_path = paths.config_path.clone();
    let db_path = paths.db_path.clone();
    let exports_dir = paths.exports_dir.clone();

    assert_json_path(&json, "project_root", repo.path());
    assert_json_path(&json, "memory_dir", &memory_dir);
    assert_json_path(&json, "records_dir", &records_dir);
    assert_json_path(&json, "runtime_dir", &paths.runtime_dir);
    assert_json_path(&json, "config_path", &config_path);
    assert_json_path(&json, "db_path", &db_path);
    assert_json_path(&json, "exports_dir", &exports_dir);

    assert!(
        records_dir.is_dir(),
        "missing records dir at {}",
        records_dir.display()
    );
    assert!(
        config_path.is_file(),
        "missing config at {}",
        config_path.display()
    );
    assert!(
        db_path.is_file(),
        "missing database at {}",
        db_path.display()
    );
    assert!(
        exports_dir.is_dir(),
        "missing exports dir at {}",
        exports_dir.display()
    );

    let conn = Connection::open(&db_path).expect("open initialized database");
    let has_memory_record_table: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'memory_record')",
            [],
            |row| row.get(0),
        )
        .expect("query initialized schema");
    assert!(
        has_memory_record_table,
        "database was created without the current schema"
    );

    let mut second = memzoi();
    second
        .arg("init")
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists; pass --force"));

    fs::write(&config_path, "changed = true\n").expect("mutate config before force init");
    let mut forced = memzoi();
    forced
        .args(["init", "--force"])
        .current_dir(repo.path())
        .assert()
        .success();

    let rewritten_config = fs::read_to_string(&config_path).expect("read forced config");
    assert!(!rewritten_config.contains("version"));
    assert!(rewritten_config.contains("scope_kind = \"repo\""));
    assert!(!rewritten_config.contains("changed = true"));
}

#[test]
fn linked_worktrees_share_durable_runtime_and_isolate_repo_indexes() {
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let temp = tempfile::tempdir().expect("worktree fixture root");
    let main = temp.path().join("main");
    let linked = temp.path().join("linked");
    let home = temp.path().join("runtime");
    fs::create_dir_all(&main).expect("create main worktree");
    run_git_fixture(&main, &["init", "-q"]);
    run_git_fixture(&main, &["config", "user.email", "fixture@example.test"]);
    run_git_fixture(&main, &["config", "user.name", "Fixture"]);
    fs::write(main.join("README.md"), "fixture\n").expect("write fixture README");
    run_git_fixture(&main, &["add", "README.md"]);
    run_git_fixture(&main, &["commit", "-qm", "base"]);

    run_json_command_with_home(&main, &["init", "--json"], &home);
    fs::write(main.join(".memzoi/index.md"), "# Memory\n").expect("write tracked memory marker");
    fs::write(main.join(".memzoi/records/.gitkeep"), "").expect("write tracked records marker");
    run_git_fixture(&main, &["add", ".memzoi"]);
    run_git_fixture(&main, &["commit", "-qm", "initialize memory"]);

    let local = run_json_command_with_home(
        &main,
        &[
            "local",
            "add",
            "--type",
            "preference",
            "--title",
            "Shared worktree preference",
            "--body",
            "Linked worktrees must share this local runtime memory.",
            "--json",
        ],
        &home,
    );
    let local_id = json_string(&local, "record_id").to_owned();
    let main_proposal = run_json_command_with_home(
        &main,
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
            "--title",
            "Shared worktree proposal",
            "--body",
            "Database proposals must be visible from every linked worktree.",
            "--json",
        ],
        &home,
    );
    let main_proposal_id = json_string(&main_proposal, "proposal_id").to_owned();

    run_git_fixture(
        &main,
        &[
            "worktree",
            "add",
            "-qb",
            "linked",
            linked.to_str().expect("linked path UTF-8"),
        ],
    );

    let linked_local = run_json_command_with_home(&linked, &["local", "list", "--json"], &home);
    assert!(record_ids_from_json(&linked_local).contains(&local_id.as_str()));
    let linked_context = run_json_command_with_home(
        &linked,
        &[
            "context",
            "--task",
            "shared worktree preference",
            "--include-local",
            "--json",
        ],
        &home,
    );
    assert!(record_ids_from_json(&linked_context).contains(&local_id.as_str()));
    run_json_command_with_home(
        &linked,
        &["precheck", "--path", "README.md", "--json"],
        &home,
    );
    let linked_proposals = run_json_command_with_home(
        &linked,
        &["proposals", "list", "--status", "open", "--json"],
        &home,
    );
    assert!(
        proposals_from_json(&linked_proposals)
            .iter()
            .any(|proposal| json_string(proposal, "id") == main_proposal_id)
    );

    let linked_proposal = run_json_command_with_home(
        &linked,
        &[
            "propose",
            "--manual",
            "--type",
            "fact",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--sensitivity",
            "repo-safe",
            "--title",
            "Linked proposal visibility",
            "--body",
            "The main worktree must see proposals created in a linked worktree.",
            "--json",
        ],
        &home,
    );
    let linked_proposal_id = json_string(&linked_proposal, "proposal_id").to_owned();
    let main_proposals = run_json_command_with_home(
        &main,
        &["proposals", "list", "--status", "open", "--json"],
        &home,
    );
    assert!(
        proposals_from_json(&main_proposals)
            .iter()
            .any(|proposal| json_string(proposal, "id") == linked_proposal_id)
    );

    run_json_command_with_home(
        &main,
        &[
            "propose",
            "--apply",
            "--type",
            "fact",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--sensitivity",
            "repo-safe",
            "--content-class",
            "general_repo_knowledge",
            "--title",
            "Main branch only zircon",
            "--body",
            "This zircon record exists only in the main worktree.",
            "--json",
        ],
        &home,
    );
    let main_search = run_json_command_with_home(&main, &["search", "zircon", "--json"], &home);
    assert!(!record_ids_from_json(&main_search).is_empty());
    let linked_search = run_json_command_with_home(&linked, &["search", "zircon", "--json"], &home);
    assert!(
        record_ids_from_json(&linked_search).is_empty(),
        "linked worktree index must not contain main-only canonical records: {linked_search}"
    );
}
