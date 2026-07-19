use super::*;

fn repository_runtime_snapshot(paths: &MemoryPaths) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut snapshot = BTreeMap::new();
    snapshot_tree(
        &paths.repository_runtime_dir,
        Path::new("repository-runtime"),
        &mut snapshot,
    );
    snapshot
}

fn git_fixture_output(directory: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = std::process::Command::new("git");
    command.args(args).current_dir(directory);
    for key in [
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_OBJECT_DIRECTORY",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_GRAFT_FILE",
        "GIT_INDEX_FILE",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_REPLACE_REF_BASE",
        "GIT_PREFIX",
        "GIT_SHALLOW_FILE",
        "GIT_COMMON_DIR",
        "GIT_QUARANTINE_PATH",
    ] {
        command.env_remove(key);
    }
    let output = command.output().expect("run Git fixture command");
    assert!(
        output.status.success(),
        "Git fixture command {args:?} failed with {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn maintenance_help_exposes_plan_only_repository_surface() {
    let mut root = memzoi();
    root.args(["maintenance", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("plan").and(predicate::str::contains("\n  apply ").not()));

    let mut plan = memzoi();
    plan.args(["maintenance", "plan", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("--record-id <ID>")
                .and(predicate::str::contains("--evaluated-at <RFC3339>"))
                .and(predicate::str::contains("--output <PATH>"))
                .and(predicate::str::contains("--json"))
                .and(predicate::str::contains("--include-local").not())
                .and(predicate::str::contains("--include-session").not()),
        );
}

#[test]
fn maintenance_plan_is_deterministic_content_free_and_zero_write() {
    let repo = initialized_temp_repo();
    write_canonical_record_fixture(
        repo.path(),
        "cli-maintenance-a",
        "repo",
        None,
        "active",
        "2026-07-01T00:00:00Z",
        "2026-07-01T00:00:00Z",
    );
    write_canonical_record_fixture(
        repo.path(),
        "cli-maintenance-b",
        "repo",
        None,
        "active",
        "2026-07-01T00:00:00Z",
        "2026-07-01T00:00:00Z",
    );
    run_git_fixture(repo.path(), &["add", ".memzoi"]);

    let paths = test_paths(repo.path());
    let managed_before = managed_state_snapshot(&paths);
    let runtime_before = repository_runtime_snapshot(&paths);
    let git_status_before = git_fixture_output(
        repo.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let git_index = repo.path().join(".git/index");
    let git_index_before = fs::read(&git_index).expect("read staged Git index");

    let args = [
        "maintenance",
        "plan",
        "--evaluated-at",
        "2026-07-18T12:00:00Z",
        "--json",
    ];
    let first = run_command_stdout(repo.path(), &args);
    let repeated = run_command_stdout(repo.path(), &args);
    assert_eq!(
        first, repeated,
        "maintenance plan bytes must replay exactly"
    );
    let plan: Value = serde_json::from_str(&first).expect("parse maintenance plan JSON");
    assert_json_string_field(&plan, &["schema"], "memzoi/maintenance-plan");
    assert_json_string_field(&plan["policy"], &["contract_version"], "maintenance-plan/2");
    assert_json_string_field(&plan["scope"], &["kind"], "repository");
    assert_json_string_field(
        &plan["records"][0]["version"],
        &["kind"],
        "canonical_repository",
    );
    assert!(plan["records"][0].get("source_path").is_none());
    assert!(plan["records"][0].get("revision").is_none());
    assert!(
        plan["plan_id"]
            .as_str()
            .is_some_and(|plan_id| !plan_id.is_empty()),
        "plan should have an identity: {plan}"
    );
    assert_eq!(plan["summary"]["records"], 2);
    assert_eq!(plan["summary"]["exact_duplicates"], 1);
    assert_json_string_field(&plan["authority"], &["mode"], "report_only");
    let action_groups = plan["action_groups"]
        .as_array()
        .unwrap_or_else(|| panic!("maintenance plan should contain action groups: {plan}"));
    assert_eq!(
        action_groups
            .iter()
            .map(|group| json_string(group, "kind"))
            .collect::<Vec<_>>(),
        vec![
            "repository_materialization",
            "private_derived_state",
            "owner_authorized_private_mutation",
        ]
    );
    assert!(
        action_groups[1]["actions"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert!(
        action_groups[2]["actions"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );

    let selected = run_json_command(
        repo.path(),
        &[
            "maintenance",
            "plan",
            "--record-id",
            "cli-maintenance-a",
            "--evaluated-at",
            "2026-07-18T12:00:00Z",
            "--json",
        ],
    );
    assert_eq!(
        selected["request"]["record_ids"],
        serde_json::json!(["cli-maintenance-a"]),
        "the immutable artifact must bind explicit target selection"
    );

    let human = run_command_stdout(
        repo.path(),
        &[
            "maintenance",
            "plan",
            "--evaluated-at",
            "2026-07-18T12:00:00Z",
        ],
    );
    assert!(human.starts_with("maintenance-plan\n"), "{human}");
    assert!(
        human.contains(plan["plan_id"].as_str().expect("plan id string")),
        "{human}"
    );
    assert!(human.contains("exact_duplicates\t1"), "{human}");
    assert!(!human.contains("Legacy auth evidence"), "{human}");
    assert!(!human.contains("Original legacy auth evidence"), "{human}");

    let artifact_root = tempfile::tempdir().expect("external artifact directory");
    let artifact = artifact_root.path().join("maintenance-plan.json");
    let emitted = run_command_stdout(
        repo.path(),
        &[
            "maintenance",
            "plan",
            "--evaluated-at",
            "2026-07-18T12:00:00Z",
            "--output",
            artifact.to_str().expect("artifact path utf-8"),
            "--json",
        ],
    );
    assert_eq!(emitted, first);
    assert_eq!(
        fs::read_to_string(&artifact).expect("read maintenance artifact"),
        first
    );

    assert_eq!(
        managed_state_snapshot(&paths),
        managed_before,
        "maintenance planning must not mutate canonical or worktree state"
    );
    assert_eq!(
        repository_runtime_snapshot(&paths),
        runtime_before,
        "maintenance planning must not mutate shared DB, WAL, events, indexes, or overlays"
    );
    assert_eq!(
        fs::read(&git_index).expect("reread staged Git index"),
        git_index_before,
        "maintenance planning must not mutate the Git index"
    );
    assert_eq!(
        git_fixture_output(
            repo.path(),
            &["status", "--porcelain=v1", "--untracked-files=all"],
        ),
        git_status_before,
        "maintenance planning must not change Git status"
    );
}

#[test]
fn maintenance_output_is_no_clobber_and_outside_worktree_and_runtime() {
    let repo = initialized_temp_repo();
    write_canonical_record_fixture(
        repo.path(),
        "cli-maintenance-output",
        "repo",
        None,
        "active",
        "2026-07-01T00:00:00Z",
        "2026-07-01T00:00:00Z",
    );
    let paths = test_paths(repo.path());
    let artifact_root = tempfile::tempdir().expect("external artifact directory");
    let artifact = artifact_root.path().join("maintenance-plan.json");
    let common = [
        "maintenance",
        "plan",
        "--evaluated-at",
        "2026-07-18T12:00:00Z",
        "--output",
    ];

    let mut allowed = common.to_vec();
    allowed.push(artifact.to_str().expect("artifact path utf-8"));
    run_command_stdout(repo.path(), &allowed);
    let saved = fs::read(&artifact).expect("read saved maintenance plan");
    let error = run_command_failure_stderr(repo.path(), &allowed);
    assert!(error.contains("already exists"), "{error}");
    assert_eq!(fs::read(&artifact).expect("reread artifact"), saved);

    for (forbidden, expected) in [
        (repo.path().join("maintenance-plan.json"), "Git worktree"),
        (
            paths.repository_runtime_dir.join("maintenance-plan.json"),
            "Memzoi-managed runtime state",
        ),
        (
            test_memzoi_home().join("maintenance-plan.json"),
            "Memzoi-managed runtime state",
        ),
    ] {
        let mut args = common.to_vec();
        args.push(forbidden.to_str().expect("forbidden path utf-8"));
        let error = run_command_failure_stderr(repo.path(), &args);
        assert!(error.contains(expected), "{error}");
        assert!(!forbidden.exists());
    }

    let missing_parent = artifact_root.path().join("missing/maintenance-plan.json");
    let mut missing_args = common.to_vec();
    missing_args.push(missing_parent.to_str().expect("missing path utf-8"));
    let error = run_command_failure_stderr(repo.path(), &missing_args);
    assert!(error.contains("destination parent"), "{error}");
    assert!(!artifact_root.path().join("missing").exists());
}

#[cfg(unix)]
#[test]
fn maintenance_output_rejects_symlink_destination_and_parent() {
    use std::os::unix::fs::symlink;

    let repo = initialized_temp_repo();
    let artifact_root = tempfile::tempdir().expect("external artifact directory");
    let target = artifact_root.path().join("target.json");
    fs::write(&target, b"do not replace").expect("write symlink target");
    let destination = artifact_root.path().join("maintenance-plan.json");
    symlink(&target, &destination).expect("create destination symlink");

    let error = run_command_failure_stderr(
        repo.path(),
        &[
            "maintenance",
            "plan",
            "--evaluated-at",
            "2026-07-18T12:00:00Z",
            "--output",
            destination.to_str().expect("symlink path utf-8"),
        ],
    );
    assert!(error.contains("must not be a symlink"), "{error}");
    assert_eq!(
        fs::read(&target).expect("read symlink target"),
        b"do not replace"
    );

    let real_parent = artifact_root.path().join("real-parent");
    fs::create_dir(&real_parent).expect("create real parent");
    let symlink_parent = artifact_root.path().join("symlink-parent");
    symlink(&real_parent, &symlink_parent).expect("create parent symlink");
    let through_parent = symlink_parent.join("maintenance-plan.json");
    let error = run_command_failure_stderr(
        repo.path(),
        &[
            "maintenance",
            "plan",
            "--evaluated-at",
            "2026-07-18T12:00:00Z",
            "--output",
            through_parent.to_str().expect("symlink parent path utf-8"),
        ],
    );
    assert!(
        error.contains("parent must be a real existing directory"),
        "{error}"
    );
    assert!(!real_parent.join("maintenance-plan.json").exists());
}
