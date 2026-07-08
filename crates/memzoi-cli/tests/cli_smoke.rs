use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    thread,
};

use assert_cmd::Command;
use memzoi_core::MemoryPaths;
use predicates::prelude::*;
use rusqlite::Connection;
use semver::Version;
use serde_json::Value;

fn memzoi() -> Command {
    memzoi_with_home(&test_memzoi_home())
}

fn memzoi_with_home(memzoi_home: &Path) -> Command {
    let mut command = Command::cargo_bin("memzoi").expect("memzoi binary");
    command.env("MEMZOI_HOME", memzoi_home);
    command
}

fn test_memzoi_home() -> PathBuf {
    std::env::temp_dir()
        .canonicalize()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join(format!("memzoi-cli-tests-{}", std::process::id()))
}

fn test_paths(repo: &Path) -> MemoryPaths {
    MemoryPaths::with_runtime_home(
        repo.canonicalize().expect("repo path should exist"),
        test_memzoi_home(),
    )
}

#[test]
fn help_advertises_the_init_subcommand() {
    let mut cmd = memzoi();

    cmd.arg("--help").assert().success().stdout(
        predicate::str::contains("Local-first memory governance")
            .and(predicate::str::contains("Usage: memzoi <COMMAND>"))
            .and(predicate::str::contains("init"))
            .and(predicate::str::contains("update")),
    );
}

#[test]
fn init_help_advertises_init_options() {
    let mut cmd = memzoi();

    cmd.args(["init", "--help"]).assert().success().stdout(
        predicate::str::contains("Initialize repo .memzoi memory and local runtime state")
            .and(predicate::str::contains("Usage: memzoi init [OPTIONS]"))
            .and(predicate::str::contains("--force"))
            .and(predicate::str::contains("--json")),
    );
}

#[test]
fn update_help_advertises_update_options() {
    let mut cmd = memzoi();

    cmd.args(["update", "--help"]).assert().success().stdout(
        predicate::str::contains("Check for or apply a Memzoi release update")
            .and(predicate::str::contains("Usage: memzoi update [OPTIONS]"))
            .and(predicate::str::contains("--check"))
            .and(predicate::str::contains("--ref"))
            .and(predicate::str::contains("--json")),
    );
}

#[test]
fn update_check_json_reports_available_even_when_apply_is_unsupported() {
    let repo = tempfile::tempdir().expect("temp repo");
    let target_ref = next_patch_release_ref();
    let api_base = spawn_latest_release_api(target_ref.as_str());
    let mut cmd = memzoi();

    let assert = cmd
        .args(["update", "--check", "--json"])
        .current_dir(repo.path())
        .env("MEMZOI_RELEASE_API_BASE", api_base)
        .assert()
        .success();
    let update = json_from_stdout(&assert.get_output().stdout);

    assert_eq!(json_string(&update, "status"), "update_available");
    assert_eq!(
        update.get("check_only").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(update.get("updated").and_then(Value::as_bool), Some(false));
    assert_eq!(
        update.get("apply_supported").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(json_string(&update, "target_ref"), target_ref);
    assert_eq!(
        update
            .get("manual_command")
            .and_then(Value::as_str)
            .expect("manual command"),
        "git pull && make install"
    );
}

#[test]
fn update_apply_json_reports_unsupported_for_source_builds_without_network() {
    let repo = tempfile::tempdir().expect("temp repo");
    let mut cmd = memzoi();

    let assert = cmd
        .args(["update", "--json"])
        .current_dir(repo.path())
        .env("MEMZOI_RELEASE_API_BASE", "http://127.0.0.1:1")
        .assert()
        .failure();
    let update = json_from_stdout(&assert.get_output().stdout);

    assert_eq!(json_string(&update, "status"), "unsupported");
    assert_eq!(
        update.get("apply_supported").and_then(Value::as_bool),
        Some(false)
    );
    assert!(update.get("target_ref").is_some_and(Value::is_null));
    assert_json_string_contains(&update, "message", "source checkout");
    assert_eq!(
        update
            .get("manual_command")
            .and_then(Value::as_str)
            .expect("manual command"),
        "git pull && make install"
    );
}

#[test]
fn update_apply_human_failure_reports_message_once() {
    let repo = tempfile::tempdir().expect("temp repo");
    let mut cmd = memzoi();

    let assert = cmd
        .args(["update"])
        .current_dir(repo.path())
        .env("MEMZOI_RELEASE_API_BASE", "http://127.0.0.1:1")
        .assert()
        .failure();
    let stdout = std::str::from_utf8(&assert.get_output().stdout).expect("stdout is utf-8");
    let stderr = std::str::from_utf8(&assert.get_output().stderr).expect("stderr is utf-8");

    assert_eq!(stdout, "");
    assert_eq!(stderr.matches("source checkout").count(), 1);
    assert!(stderr.contains("Use: git pull && make install"));
}

#[test]
fn update_invalid_ref_fails_before_network() {
    let repo = tempfile::tempdir().expect("temp repo");
    let mut cmd = memzoi();

    let assert = cmd
        .args(["update", "--check", "--ref", "main", "--json"])
        .current_dir(repo.path())
        .env("MEMZOI_RELEASE_API_BASE", "http://127.0.0.1:1")
        .assert()
        .failure();
    let update = json_from_stdout(&assert.get_output().stdout);

    assert_eq!(json_string(&update, "status"), "invalid_ref");
    assert!(update.get("target_ref").is_some_and(Value::is_null));
    assert_json_string_contains(&update, "message", "branch refs");
}

#[test]
fn doctor_json_reports_missing_bundle_before_init() {
    let repo = tempfile::tempdir().expect("temp repo");
    fs::create_dir(repo.path().join(".git")).expect("create git marker");

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
    fs::create_dir(repo.path().join(".git")).expect("create git marker");

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
    fs::create_dir(repo.path().join(".git")).expect("create git marker");
    let paths = test_paths(repo.path());
    fs::create_dir_all(paths.records_dir()).expect("create records dir");
    fs::create_dir_all(&paths.runtime_dir).expect("create runtime dir");
    fs::write(&paths.config_path, "version = 1\n").expect("write config");

    let doctor = run_json_command(repo.path(), &["doctor", "--json"]);

    assert_check_status(&doctor, "config", "ok");
    assert_check_status(&doctor, "exports", "warning");
}

#[test]
fn doctor_json_reports_ready_after_init_and_warns_when_mcp_binary_is_missing() {
    let repo = initialized_temp_repo();
    let mut cmd = memzoi();

    let assert = cmd
        .args(["doctor", "--json"])
        .current_dir(repo.path())
        .env("PATH", "")
        .assert()
        .success();
    let stdout = std::str::from_utf8(&assert.get_output().stdout).expect("stdout is utf-8");
    let doctor: Value = serde_json::from_str(stdout).expect("stdout is JSON");

    assert_eq!(json_string(&doctor, "status"), "warning");
    assert_json_path(&doctor, "project_root", repo.path());
    assert_check_status(&doctor, "config", "ok");
    assert_check_status(&doctor, "database", "ok");
    assert_check_status(&doctor, "schema", "ok");
    assert_check_status(&doctor, "exports", "ok");
    assert_check_status(&doctor, "proposals", "ok");
    assert_check_status(&doctor, "mcp", "warning");
    assert_json_array_contains(&doctor, "next_steps", "memzoi mcp config --project-root .");
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

#[test]
fn integrate_prompt_prints_memzoi_protocol() {
    let repo = initialized_temp_repo();
    let mut cmd = memzoi();

    cmd.args(["integrate", "prompt"])
        .current_dir(repo.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Before non-trivial work")
                .and(predicate::str::contains("memzoi context --task"))
                .and(predicate::str::contains("memzoi precheck --command"))
                .and(predicate::str::contains("memzoi propose"))
                .and(predicate::str::contains("Do not store secrets")),
        );
}

#[test]
fn integrate_instructions_creates_and_updates_marked_block() {
    let repo = initialized_temp_repo();
    let instructions = repo.path().join("AGENTS.md");

    run_json_command(
        repo.path(),
        &[
            "integrate",
            "instructions",
            "--file",
            instructions.to_str().expect("utf-8 path"),
            "--json",
        ],
    );
    let first = fs::read_to_string(&instructions).expect("read created instructions");
    assert!(first.contains("<!-- memzoi:start -->"));
    assert!(first.contains("memzoi context --task"));
    assert!(first.contains("memzoi propose --type"));
    assert!(first.contains("<!-- memzoi:end -->"));

    fs::write(
        &instructions,
        first.replace("memzoi context --task", "stale-memory context --task"),
    )
    .expect("stale instructions");

    run_json_command(
        repo.path(),
        &[
            "integrate",
            "instructions",
            "--file",
            instructions.to_str().expect("utf-8 path"),
            "--json",
        ],
    );
    let updated = fs::read_to_string(&instructions).expect("read updated instructions");
    assert_eq!(updated.matches("<!-- memzoi:start -->").count(), 1);
    assert!(!updated.contains("stale-memory context --task"));
    assert!(updated.contains("memzoi context --task"));
}

#[test]
fn init_json_creates_memory_bundle_and_second_init_fails_without_force() {
    let repo = tempfile::tempdir().expect("temp repo");
    fs::create_dir(repo.path().join(".git")).expect("create git marker");

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
        "database was created without the v0 schema"
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
    assert!(rewritten_config.contains("version = 1"));
    assert!(rewritten_config.contains("scope_kind = \"repo\""));
    assert!(!rewritten_config.contains("changed = true"));
}

#[test]
fn proposal_commands_json_drive_approve_apply_supersede_and_tombstone_workflow() {
    let repo = initialized_temp_repo();

    let proposal = run_json_command(
        repo.path(),
        &[
            "propose",
            "--manual",
            "--type",
            "fact",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--title",
            "CLI proposals produce JSON",
            "--body",
            "The CLI proposal workflow should be scriptable from JSON output.",
            "--actor",
            "agent:red-tests",
            "--json",
        ],
    );
    let proposal_id = json_string(&proposal, "proposal_id").to_owned();
    assert_eq!(json_string(&proposal, "status"), "pending");

    let approved = run_json_command(
        repo.path(),
        &[
            "approve",
            proposal_id.as_str(),
            "--actor",
            "reviewer:human",
            "--json",
        ],
    );
    assert_eq!(json_string(&approved, "proposal_id"), proposal_id.as_str());
    assert_eq!(json_string(&approved, "status"), "approved");

    let applied = run_json_command(
        repo.path(),
        &[
            "apply",
            proposal_id.as_str(),
            "--actor",
            "agent:applier",
            "--json",
        ],
    );
    let record_id = json_string(&applied, "record_id").to_owned();
    assert_eq!(json_string(&applied, "proposal_id"), proposal_id.as_str());
    assert_eq!(json_string(&applied, "record_status"), "active");
    let record_file = test_paths(repo.path())
        .records_dir()
        .join(format!("{record_id}.md"));
    let record_markdown = fs::read_to_string(&record_file).unwrap_or_else(|error| {
        panic!(
            "applied record should write {}: {error}",
            record_file.display()
        )
    });
    assert!(
        record_markdown.contains("type: fact"),
        "record file should be OKF-shaped: {record_markdown}"
    );
    assert!(
        record_markdown.contains("# CLI proposals produce JSON"),
        "record file should be reviewable markdown: {record_markdown}"
    );

    let superseded = run_json_command(
        repo.path(),
        &[
            "supersede",
            record_id.as_str(),
            "--type",
            "fact",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--title",
            "CLI supersede writes replacement",
            "--body",
            "The supersede command should keep a replacement active and mark the old record \
             superseded.",
            "--actor",
            "agent:red-tests",
            "--json",
        ],
    );
    let replacement_id = json_string(&superseded, "record_id").to_owned();
    assert_eq!(
        json_string(&superseded, "superseded_record_id"),
        record_id.as_str()
    );
    assert_eq!(
        json_string(&superseded, "superseded_record_status"),
        "superseded"
    );
    assert_eq!(json_string(&superseded, "record_status"), "active");

    let tombstoned = run_json_command(
        repo.path(),
        &[
            "tombstone",
            replacement_id.as_str(),
            "--reason",
            "The memory is obsolete.",
            "--actor",
            "agent:red-tests",
            "--json",
        ],
    );
    assert_eq!(
        json_string(&tombstoned, "record_id"),
        replacement_id.as_str()
    );
    assert_eq!(json_string(&tombstoned, "record_status"), "tombstoned");
}

#[test]
fn propose_default_approves_manual_keeps_pending_and_apply_creates_active_record() {
    let repo = initialized_temp_repo();

    let approved = run_json_command(
        repo.path(),
        &[
            "propose",
            "--type",
            "fact",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--title",
            "Default proposal is approved",
            "--body",
            "A proposal without manual policy should be ready to apply.",
            "--actor",
            "agent:cli-smoke",
            "--json",
        ],
    );
    assert_eq!(json_string(&approved, "status"), "approved");

    let manual = run_json_command(
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
            "--title",
            "Manual proposal stays pending",
            "--body",
            "Manual policy must require human approval before application.",
            "--actor",
            "agent:cli-smoke",
            "--json",
        ],
    );
    assert_eq!(json_string(&manual, "status"), "pending");

    let applied = run_json_command(
        repo.path(),
        &[
            "propose",
            "--apply",
            "--type",
            "procedure",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--title",
            "Apply approved proposal immediately",
            "--body",
            "The --apply flag should create an active record for an approved proposal.",
            "--actor",
            "agent:cli-smoke",
            "--json",
        ],
    );
    assert_eq!(json_string(&applied, "record_status"), "active");
    assert_eq!(applied.get("applied").and_then(Value::as_bool), Some(true));

    let record_id = json_string(&applied, "record_id");
    let record_file = test_paths(repo.path())
        .records_dir()
        .join(format!("{record_id}.md"));
    let record_markdown = fs::read_to_string(&record_file).unwrap_or_else(|error| {
        panic!(
            "--apply should write the active record file {}: {error}",
            record_file.display()
        )
    });
    assert!(
        record_markdown.contains("# Apply approved proposal immediately"),
        "applied record should be reviewable markdown: {record_markdown}"
    );
}

#[test]
fn propose_apply_implies_auto_approval_when_repo_policy_is_manual() {
    let repo = initialized_temp_repo();
    fs::write(
        repo.path().join(".memzoi/config.toml"),
        "[workflow]\nproposal_approval = \"manual\"\n",
    )
    .expect("write repo approval policy");

    let applied = run_json_command(
        repo.path(),
        &[
            "propose",
            "--apply",
            "--type",
            "fact",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--title",
            "Apply overrides manual policy",
            "--body",
            "The CLI --apply flag creates, approves, and applies even under repo manual policy.",
            "--actor",
            "agent:cli-smoke",
            "--json",
        ],
    );

    assert_eq!(json_string(&applied, "status"), "applied");
    assert_eq!(json_string(&applied, "record_status"), "active");
    assert_eq!(applied.get("applied").and_then(Value::as_bool), Some(true));
    let record_id = json_string(&applied, "record_id");
    assert!(
        test_paths(repo.path())
            .records_dir()
            .join(format!("{record_id}.md"))
            .is_file(),
        "--apply under manual repo policy should write the active canonical record"
    );
}

#[test]
fn propose_policy_flags_reject_conflicting_combinations() {
    let repo = initialized_temp_repo();

    let mut manual_auto = memzoi();
    manual_auto
        .args([
            "propose",
            "--manual",
            "--auto-approve",
            "--type",
            "fact",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--title",
            "Conflicting policies",
            "--body",
            "Manual and auto approval cannot both own the same proposal.",
            "--json",
        ])
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--manual").and(predicate::str::contains("--auto-approve")),
        );

    let mut manual_apply = memzoi();
    manual_apply
        .args([
            "propose",
            "--manual",
            "--apply",
            "--type",
            "fact",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--title",
            "Manual apply conflict",
            "--body",
            "Pending proposals must not be applied in the same command.",
            "--json",
        ])
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("--manual").and(predicate::str::contains("--apply")));
}

#[test]
fn proposals_list_show_and_bulk_apply_report_proposal_state() {
    let repo = initialized_temp_repo();

    let pending = run_json_command(
        repo.path(),
        &[
            "propose",
            "--manual",
            "--type",
            "warning",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--title",
            "Human reviews risky memory",
            "--body",
            "Manual proposals should appear in the proposal inbox until reviewed.",
            "--actor",
            "agent:cli-smoke",
            "--json",
        ],
    );
    let pending_id = json_string(&pending, "proposal_id").to_owned();

    let approved_one = run_json_command(
        repo.path(),
        &[
            "propose",
            "--type",
            "fact",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--title",
            "Bulk apply first approved proposal",
            "--body",
            "Bulk apply should apply every approved proposal in the inbox.",
            "--actor",
            "agent:cli-smoke",
            "--json",
        ],
    );
    let approved_one_id = json_string(&approved_one, "proposal_id").to_owned();

    let approved_two = run_json_command(
        repo.path(),
        &[
            "propose",
            "--type",
            "decision",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--title",
            "Bulk apply second approved proposal",
            "--body",
            "Bulk apply should leave pending proposals untouched.",
            "--actor",
            "agent:cli-smoke",
            "--json",
        ],
    );
    let approved_two_id = json_string(&approved_two, "proposal_id").to_owned();

    let pending_list = run_json_command(
        repo.path(),
        &["proposals", "list", "--status", "pending", "--json"],
    );
    let pending_proposals = proposals_from_json(&pending_list);
    assert!(
        pending_proposals
            .iter()
            .any(|proposal| proposal_id_from_value(proposal) == pending_id
                && proposal_status(proposal) == Some("pending")),
        "pending list should include the manual proposal: {pending_list}"
    );
    assert!(
        !pending_proposals
            .iter()
            .any(|proposal| proposal_id_from_value(proposal) == approved_one_id),
        "pending list should exclude approved proposals: {pending_list}"
    );

    let mut human_list = memzoi();
    human_list
        .args(["proposals", "list", "--status", "pending"])
        .current_dir(repo.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains(pending_id.as_str())
                .and(predicate::str::contains("pending"))
                .and(predicate::str::contains("Human reviews risky memory")),
        );

    let shown = run_json_command(
        repo.path(),
        &["proposals", "show", pending_id.as_str(), "--json"],
    );
    assert_eq!(proposal_id_from_value(&shown), pending_id);
    assert_eq!(proposal_status(&shown), Some("pending"));
    assert_eq!(
        proposal_title(&shown),
        Some("Human reviews risky memory"),
        "show JSON should include the proposal payload/title: {shown}"
    );

    let bulk_applied = run_json_command(
        repo.path(),
        &[
            "proposals",
            "apply",
            "--all-approved",
            "--actor",
            "agent:bulk-applier",
            "--json",
        ],
    );
    let applied = applied_proposals_from_json(&bulk_applied);
    assert!(
        applied
            .iter()
            .any(|proposal| proposal_id_from_value(proposal) == approved_one_id),
        "bulk apply should apply first approved proposal: {bulk_applied}"
    );
    assert!(
        applied
            .iter()
            .any(|proposal| proposal_id_from_value(proposal) == approved_two_id),
        "bulk apply should apply second approved proposal: {bulk_applied}"
    );
    assert!(
        !applied
            .iter()
            .any(|proposal| proposal_id_from_value(proposal) == pending_id),
        "bulk apply should not apply pending proposals: {bulk_applied}"
    );

    let applied_list = run_json_command(
        repo.path(),
        &["proposals", "list", "--status", "applied", "--json"],
    );
    let applied_proposals = proposals_from_json(&applied_list);
    assert!(
        applied_proposals.iter().any(|proposal| {
            proposal_id_from_value(proposal) == approved_one_id
                && proposal_status(proposal) == Some("applied")
        }),
        "first approved proposal should be listed as applied after bulk apply: {applied_list}"
    );
    assert!(
        applied_proposals.iter().any(|proposal| {
            proposal_id_from_value(proposal) == approved_two_id
                && proposal_status(proposal) == Some("applied")
        }),
        "second approved proposal should be listed as applied after bulk apply: {applied_list}"
    );

    let still_pending = run_json_command(
        repo.path(),
        &["proposals", "list", "--status", "pending", "--json"],
    );
    assert!(
        proposals_from_json(&still_pending)
            .iter()
            .any(|proposal| proposal_id_from_value(proposal) == pending_id),
        "pending proposal should remain pending after bulk apply: {still_pending}"
    );
}

#[test]
fn proposal_files_list_show_and_validate_valid_pending_files() {
    let repo = initialized_temp_repo();
    write_pending_proposal_file(repo.path(), "valid-proposal.md", valid_proposal_markdown());

    let listed = run_json_command(repo.path(), &["proposal-files", "list", "--json"]);
    let listed_proposals = proposals_from_json(&listed);
    assert_eq!(listed_proposals.len(), 1, "expected one proposal: {listed}");
    let listed_proposal = &listed_proposals[0];
    assert_eq!(proposal_id_from_value(listed_proposal), "mem_test_valid");
    assert_json_string_field(listed_proposal, &["action"], "create");
    assert_json_string_field(listed_proposal, &["lane"], "semantic");
    assert_json_string_field(listed_proposal, &["type"], "decision");
    assert_json_string_field(listed_proposal, &["sensitivity"], "repo-safe");
    assert_json_string_field(listed_proposal, &["title"], "Valid proposal");
    assert!(
        listed
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "valid list should not include errors: {listed}"
    );

    let shown = run_json_command(
        repo.path(),
        &["proposal-files", "show", "mem_test_valid", "--json"],
    );
    assert_eq!(proposal_id_from_value(&shown), "mem_test_valid");
    assert_json_string_field(&shown, &["body"], "This proposal body is valid.");
    assert_json_string_field(&shown["proposal"], &["action"], "create");

    let shown_by_file_id = run_json_command(
        repo.path(),
        &["proposal-files", "show", "valid-proposal", "--json"],
    );
    assert_eq!(proposal_id_from_value(&shown_by_file_id), "mem_test_valid");

    let validated = run_json_command(repo.path(), &["proposal-files", "validate", "--json"]);
    assert_eq!(validated.get("valid").and_then(Value::as_bool), Some(true));
    assert_eq!(
        validated.get("valid_count").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        validated.get("invalid_count").and_then(Value::as_u64),
        Some(0)
    );
}

#[test]
fn proposal_files_missing_pending_directory_reports_empty_success() {
    let repo = initialized_temp_repo();

    let listed = run_json_command(repo.path(), &["proposal-files", "list", "--json"]);
    assert!(
        proposals_from_json(&listed).is_empty(),
        "missing pending dir should list no proposals: {listed}"
    );
    assert!(
        listed
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "missing pending dir should not report errors: {listed}"
    );

    let validated = run_json_command(repo.path(), &["proposal-files", "validate", "--json"]);
    assert_eq!(validated.get("valid").and_then(Value::as_bool), Some(true));
    assert_eq!(
        validated.get("valid_count").and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        validated.get("invalid_count").and_then(Value::as_u64),
        Some(0)
    );
}

#[test]
fn proposal_files_validate_reports_invalid_files_and_list_refuses_mixed_state() {
    let repo = initialized_temp_repo();
    write_pending_proposal_file(repo.path(), "valid-proposal.md", valid_proposal_markdown());
    write_pending_proposal_file(
        repo.path(),
        "invalid-lane.md",
        proposal_markdown_with("mystery", "create", "supersedes: []", ""),
    );
    write_pending_proposal_file(
        repo.path(),
        "invalid-action.md",
        proposal_markdown_with("semantic", "update", "supersedes: []", ""),
    );
    write_pending_proposal_file(
        repo.path(),
        "missing-supersedes.md",
        proposal_markdown_with("semantic", "supersede", "supersedes: []", ""),
    );
    write_pending_proposal_file(
        repo.path(),
        "missing-target.md",
        proposal_markdown_with("semantic", "tombstone", "supersedes: []", ""),
    );

    let validated =
        run_json_command_failure(repo.path(), &["proposal-files", "validate", "--json"]);
    assert_eq!(validated.get("valid").and_then(Value::as_bool), Some(false));
    assert_eq!(
        validated.get("valid_count").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        validated.get("invalid_count").and_then(Value::as_u64),
        Some(4)
    );
    let rendered_errors = serde_json::to_string(validated.get("errors").expect("errors"))
        .expect("serialize validation errors");
    for expected in [
        "unknown memory lane",
        "unknown OKF proposal action",
        "supersede proposals",
        "proposal.target",
    ] {
        assert!(
            rendered_errors.contains(expected),
            "validate should report {expected:?}: {validated}"
        );
    }

    let listed = run_json_command_failure(repo.path(), &["proposal-files", "list", "--json"]);
    assert_eq!(proposals_from_json(&listed).len(), 1);
    assert_eq!(
        listed.get("errors").and_then(Value::as_array).map(Vec::len),
        Some(4)
    );
}

#[cfg(unix)]
#[test]
fn proposal_files_skip_symlinks_under_pending_directory() {
    let repo = initialized_temp_repo();
    let pending = repo
        .path()
        .join(".memzoi")
        .join("proposals")
        .join("pending");
    fs::create_dir_all(&pending).expect("create pending proposals dir");

    let outside = repo.path().join("outside-proposals");
    fs::create_dir_all(&outside).expect("create outside proposals dir");
    fs::write(outside.join("external.md"), valid_proposal_markdown())
        .expect("write external proposal fixture");
    std::os::unix::fs::symlink(&outside, pending.join("linked-outside"))
        .expect("create symlinked proposal dir");

    let validated = run_json_command(repo.path(), &["proposal-files", "validate", "--json"]);
    assert_eq!(validated.get("valid").and_then(Value::as_bool), Some(true));
    assert_eq!(
        validated.get("valid_count").and_then(Value::as_u64),
        Some(0),
        "symlinked proposal files should be skipped: {validated}"
    );
    assert_eq!(
        validated.get("invalid_count").and_then(Value::as_u64),
        Some(0),
        "skipped symlinks should not become validation errors: {validated}"
    );
}

#[test]
fn proposal_files_apply_repo_safe_create_writes_compact_record_without_runtime_db_mutation() {
    let repo = initialized_temp_repo();
    write_pending_proposal_file(repo.path(), "valid-proposal.md", valid_proposal_markdown());

    let applied = run_json_command(
        repo.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    assert_json_string_field(&applied, &["proposal_id"], "mem_test_valid");
    assert_json_string_field(&applied, &["file_id"], "valid-proposal");
    assert_json_string_field(&applied, &["record_id"], "valid-proposal");
    assert_json_string_field(
        &applied,
        &["record_path"],
        ".memzoi/records/valid-proposal.md",
    );
    assert_json_string_field(&applied, &["action"], "create");
    assert_json_string_field(&applied, &["sensitivity"], "repo-safe");
    assert_json_string_field(&applied, &["title"], "Valid proposal");

    let record_path = repo.path().join(json_string(&applied, "record_path"));
    let rendered = fs::read_to_string(&record_path).expect("read canonical record");
    assert!(rendered.contains("type: decision\n"));
    assert!(rendered.contains("lane: semantic\n"));
    assert!(rendered.contains("status: active\n"));
    assert!(rendered.contains("visibility: repo\n"));
    assert!(rendered.contains("confidence: 1\n"));
    assert!(rendered.contains("source: memzoi-proposal-file\n"));
    assert!(rendered.contains("source_ref: mem_test_valid\n"));
    assert!(rendered.contains("# Valid proposal\n\nThis proposal body is valid."));
    for forbidden in [
        "kind:",
        "version:",
        "profile:",
        "proposal:",
        "proposed_by:",
        "proposed_at:",
        "reason:",
        "sensitivity:",
    ] {
        assert!(
            !rendered.contains(forbidden),
            "canonical record should omit review-only field {forbidden:?}: {rendered}"
        );
    }

    assert!(
        repo.path()
            .join(".memzoi/proposals/pending/valid-proposal.md")
            .is_file(),
        "apply should leave the pending proposal file in place"
    );
    let conn = Connection::open(test_paths(repo.path()).db_path).expect("open runtime db");
    let runtime_records: i64 = conn
        .query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))
        .expect("count runtime records");
    assert_eq!(runtime_records, 0, "Git-plane apply must not write SQLite");
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
        stderr.contains("failed to create memory record"),
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

#[test]
fn proposal_files_apply_rejects_unsupported_actions() {
    for (action, supersedes_yaml, target_yaml) in [
        ("supersede", "supersedes:\n  - existing-memory", ""),
        ("tombstone", "supersedes: []", "  target: existing-memory\n"),
    ] {
        let repo = initialized_temp_repo();
        write_pending_proposal_file(
            repo.path(),
            "valid-proposal.md",
            proposal_markdown_with_options(
                "semantic",
                action,
                "proposed",
                supersedes_yaml,
                target_yaml,
                "repo-safe",
            ),
        );

        let stderr =
            run_command_failure_stderr(repo.path(), &["proposal-files", "apply", "mem_test_valid"]);
        assert!(
            stderr.contains(&format!("action {action} is not supported")),
            "expected unsupported action rejection for {action}, got {stderr}"
        );
    }
}

#[test]
fn proposal_files_apply_fails_cleanly_for_invalid_or_missing_proposals() {
    let missing_dir = initialized_temp_repo();
    let stderr = run_command_failure_stderr(
        missing_dir.path(),
        &["proposal-files", "apply", "mem_test_valid"],
    );
    assert!(
        stderr.contains("proposal file not found: mem_test_valid"),
        "missing pending dir should fail cleanly, got {stderr}"
    );

    let missing_id = initialized_temp_repo();
    write_pending_proposal_file(
        missing_id.path(),
        "valid-proposal.md",
        valid_proposal_markdown(),
    );
    let stderr = run_command_failure_stderr(
        missing_id.path(),
        &["proposal-files", "apply", "missing-proposal"],
    );
    assert!(
        stderr.contains("proposal file not found: missing-proposal"),
        "missing id should fail cleanly, got {stderr}"
    );

    let invalid_status = initialized_temp_repo();
    write_pending_proposal_file(
        invalid_status.path(),
        "valid-proposal.md",
        proposal_markdown_with_options(
            "semantic",
            "create",
            "approved",
            "supersedes: []",
            "",
            "repo-safe",
        ),
    );
    let invalid = run_json_command_failure(
        invalid_status.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    assert_eq!(invalid.get("valid").and_then(Value::as_bool), Some(false));
    let errors = serde_json::to_string(invalid.get("errors").expect("errors"))
        .expect("serialize invalid status errors");
    assert!(
        errors.contains("unknown OKF proposal status"),
        "non-proposed status should fail cleanly through schema validation: {invalid}"
    );

    let invalid_pending = initialized_temp_repo();
    write_pending_proposal_file(
        invalid_pending.path(),
        "valid-proposal.md",
        valid_proposal_markdown(),
    );
    write_pending_proposal_file(
        invalid_pending.path(),
        "invalid-lane.md",
        proposal_markdown_with("mystery", "create", "supersedes: []", ""),
    );
    let invalid = run_json_command_failure(
        invalid_pending.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    assert_eq!(invalid.get("valid").and_then(Value::as_bool), Some(false));
    assert!(
        !invalid_pending
            .path()
            .join(".memzoi/records/valid-proposal.md")
            .exists(),
        "apply should not write while any pending proposal file is invalid"
    );
}

#[test]
fn reject_json_prevents_apply_from_creating_active_record() {
    let repo = initialized_temp_repo();

    let proposal = run_json_command(
        repo.path(),
        &[
            "propose",
            "--manual",
            "--type",
            "warning",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--title",
            "Rejected memories do not apply",
            "--body",
            "A rejected proposal must not create an active memory record.",
            "--actor",
            "agent:red-tests",
            "--json",
        ],
    );
    let proposal_id = json_string(&proposal, "proposal_id").to_owned();

    let rejected = run_json_command(
        repo.path(),
        &[
            "reject",
            proposal_id.as_str(),
            "--reason",
            "The evidence is insufficient.",
            "--actor",
            "reviewer:human",
            "--json",
        ],
    );
    assert_eq!(json_string(&rejected, "proposal_id"), proposal_id.as_str());
    assert_eq!(json_string(&rejected, "status"), "rejected");

    let mut apply = memzoi();
    apply
        .args([
            "apply",
            proposal_id.as_str(),
            "--actor",
            "agent:applier",
            "--json",
        ])
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("rejected proposal"));
}

#[test]
fn search_json_filters_scope_type_path_limit_and_excludes_inactive_records() {
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

    let wrong_scope = create_applied_memory(
        repo,
        "decision",
        "team",
        "Zircon CLI search team decision",
        "This decision matches the text and path but should be excluded by --scope repo.",
    );
    attach_memory_path(repo, &wrong_scope, "crates/search/src/lib.rs");

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
            "--scope",
            "repo",
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
        "search JSON should return only the active record that survives scope/type/path filters and limit: {search}"
    );
    assert_json_does_not_reference_records(
        &search,
        &[wrong_type, wrong_scope, wrong_path, tombstoned],
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
    conn.execute(
        "UPDATE memory_record SET status = 'tombstoned' WHERE id = ?1",
        [&inactive_id],
    )
    .expect("mark local row inactive");
    drop(conn);

    fs::create_dir_all(test_paths(repo).records_dir()).expect("create records dir");
    fs::write(
        test_paths(repo)
            .records_dir()
            .join("repo-zircon-decision.md"),
        r#"---
type: decision
title: Repo zircon decision
description: Canonical repo memory imported during rebuild.
timestamp: 2026-07-08T00:00:00Z
status: active
visibility: repo
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

    let conn = Connection::open(memory_db_path(repo)).expect("open runtime db");
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

#[test]
fn rebuild_refuses_to_delete_unreadable_runtime_db() {
    let isolated_home = tempfile::tempdir().expect("isolated memzoi home");
    let repo = initialized_temp_repo_with_home(isolated_home.path());
    let repo = repo.path();
    let db_path = MemoryPaths::with_runtime_home(
        repo.canonicalize().expect("canonical repo path"),
        isolated_home.path().to_path_buf(),
    )
    .db_path;
    let original_bytes = b"not a sqlite database with local runtime memory";
    fs::write(&db_path, original_bytes).expect("corrupt runtime db");

    let stderr =
        run_command_failure_stderr_with_home(repo, &["rebuild", "--json"], isolated_home.path());
    assert!(
        stderr.contains("local/session runtime memory could not be"),
        "rebuild should explain that local runtime memory could not be preserved: {stderr}"
    );
    assert_eq!(
        fs::read(&db_path).expect("runtime db should remain after failed rebuild"),
        original_bytes,
        "failed rebuild must not delete an unreadable runtime db"
    );
}

#[test]
fn context_json_returns_prompt_ready_pack_records_and_citations() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let matching = create_applied_memory(
        repo,
        "procedure",
        "repo",
        "Zircon CLI context procedure",
        "Path-bound zircon context should be included in prompt-ready output for context.rs.",
    );
    attach_memory_path(repo, &matching, "crates/memzoi-core/src/context.rs");
    update_record_source_ref(repo, &matching, "issue://cli-context#procedure");

    let unrelated_path = create_applied_memory(
        repo,
        "procedure",
        "repo",
        "Zircon CLI unrelated path",
        "This memory matches zircon text but belongs to a different source path.",
    );
    attach_memory_path(repo, &unrelated_path, "crates/memzoi-cli/src/main.rs");
    update_record_source_ref(repo, &unrelated_path, "issue://cli-context#global");

    let tombstoned = create_applied_memory(
        repo,
        "procedure",
        "repo",
        "Zircon CLI context tombstoned",
        "Inactive context memory must not be rendered into the prompt.",
    );
    attach_memory_path(repo, &tombstoned, "crates/memzoi-core/src/context.rs");
    update_record_source_ref(repo, &tombstoned, "issue://cli-context#old");
    run_json_command(
        repo,
        &[
            "tombstone",
            tombstoned.as_str(),
            "--reason",
            "obsolete context",
            "--json",
        ],
    );

    let pack = run_json_command(
        repo,
        &[
            "context",
            "--task",
            "Need zircon context procedure while editing context.rs",
            "--path",
            "crates/memzoi-core/src/context.rs",
            "--token-budget",
            "60",
            "--json",
        ],
    );

    let ids = record_ids_from_json(&pack);
    assert!(
        ids.contains(&matching.as_str()),
        "context JSON should include the path-relevant active memory record: {pack}"
    );
    assert!(
        !ids.contains(&tombstoned.as_str()),
        "context JSON should suppress inactive records: {pack}"
    );
    assert_eq!(
        ids.first().copied(),
        Some(matching.as_str()),
        "path-relevant memory should rank first in context records when --path is supplied: {pack}"
    );

    let prompt = prompt_text(&pack)
        .unwrap_or_else(|| panic!("context JSON should include prompt-ready text: {pack}"));
    assert!(
        prompt.contains("Path-bound zircon context")
            || prompt.contains("Zircon CLI context procedure"),
        "prompt-ready text should include the relevant active memory: {prompt:?}"
    );
    assert!(
        !prompt.contains("Inactive context memory"),
        "prompt-ready text should not include tombstoned memory: {prompt:?}"
    );
    assert!(
        prompt.split_whitespace().count() <= 80,
        "context --token-budget should cap prompt-ready output approximately: {prompt:?}"
    );

    let citation = citation_for_record(&pack, &matching)
        .unwrap_or_else(|| panic!("context JSON should cite {matching}: {pack}"));
    assert_json_string_field(citation, &["record_id", "id"], &matching);
    assert_json_string_field(citation, &["type", "memory_type"], "procedure");
    assert_json_string_field(citation, &["scope", "scope_kind"], "repo");
    assert_json_string_field(citation, &["source_ref"], "issue://cli-context#procedure");
}

#[test]
fn precheck_json_warns_for_risky_path_and_cites_memory() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let risk = create_applied_memory(
        repo,
        "risk",
        "repo",
        "Billing file is fragile",
        "Editing invoice rounding in billing previously broke tax calculation.",
    );
    attach_memory_path(repo, &risk, "apps/api/src/billing/invoice.rs");
    update_record_source_ref(repo, &risk, "issue://billing-risk#invoice");

    let unrelated = create_applied_memory(
        repo,
        "warning",
        "repo",
        "Auth command warning",
        "Do not run auth migrations while smoke tests are active.",
    );
    attach_memory_path(repo, &unrelated, "apps/api/src/auth/mod.rs");

    let precheck = run_json_command(
        repo,
        &[
            "precheck",
            "--path",
            "apps/api/src/billing/invoice.rs",
            "--action",
            "change invoice rounding",
            "--json",
        ],
    );

    let warnings = precheck
        .get("warnings")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("precheck JSON should expose warnings array: {precheck}"));
    assert_eq!(
        warnings.len(),
        1,
        "precheck should only warn for matching risky path: {precheck}"
    );
    let warning = &warnings[0];
    assert_json_string_field(warning, &["record_id"], &risk);
    assert_json_string_field(warning, &["severity"], "high");
    assert!(
        warning.to_string().contains("Billing file is fragile")
            || warning.to_string().contains("invoice rounding"),
        "warning should explain the matching memory: {warning}"
    );
    assert_json_does_not_reference_records(&precheck, &[unrelated]);
}

#[test]
fn precheck_json_warns_for_risky_command_without_path() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let warning = create_applied_memory(
        repo,
        "warning",
        "repo",
        "npm install warning",
        "Running npm install mutates lockfiles; use the package manager already configured by the repo.",
    );
    attach_memory_path(repo, &warning, "package.json");

    let precheck = run_json_command(repo, &["precheck", "--command", "npm install", "--json"]);

    let warnings = precheck
        .get("warnings")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("precheck JSON should expose warnings array: {precheck}"));
    assert_eq!(
        warnings.len(),
        1,
        "command-only precheck should warn: {precheck}"
    );
    assert_json_string_field(&warnings[0], &["record_id"], &warning);
    assert_json_string_field(&warnings[0], &["severity"], "warning");
}

#[test]
fn rebuild_json_restores_search_context_and_precheck_from_canonical_records() {
    let repo = initialized_temp_repo();
    let repo = repo.path();
    let records_dir = test_paths(repo).records_dir().join("core");
    fs::create_dir_all(&records_dir).expect("create canonical records directory");
    fs::write(
        records_dir.join("canonical-rebuild-decision.md"),
        r#"---
type: decision
title: Rebuild sentinel routing decision
description: Restores context packs from canonical records.
timestamp: 2026-07-05T00:00:00Z
status: active
visibility: repo
confidence: 0.93
source: test
source_ref: test://rebuild-decision
applies_to:
  - crates/memzoi-core/src/context.rs
---

# Rebuild sentinel routing decision

Use the rebuild sentinel routing recall token when restoring context packs from canonical records.
"#,
    )
    .expect("write canonical decision record");
    fs::write(
        records_dir.join("canonical-rebuild-risk.md"),
        r#"---
type: risk
title: Rebuild sentinel precheck risk
description: Restores precheck warnings from canonical records.
timestamp: 2026-07-05T00:00:00Z
status: active
visibility: repo
confidence: 0.97
source: test
source_ref: test://rebuild-risk
applies_to:
  - crates/memzoi-core/src/precheck.rs
---

# Rebuild sentinel precheck risk

Changing rebuild sentinel precheck command handling previously hid destructive command warnings.
"#,
    )
    .expect("write canonical risk record");

    let rebuild = run_json_command(repo, &["rebuild", "--json"]);
    assert_json_array_contains(&rebuild, "record_ids", "core/canonical-rebuild-decision");
    assert_json_array_contains(&rebuild, "record_ids", "core/canonical-rebuild-risk");

    let search = run_json_command(
        repo,
        &[
            "search",
            "rebuild sentinel routing",
            "--scope-kind",
            "repo",
            "--type",
            "decision",
            "--path",
            "crates/memzoi-core/src",
            "--json",
        ],
    );
    assert_eq!(
        record_ids_from_json(&search),
        vec!["core/canonical-rebuild-decision"],
        "rebuilt DB should make canonical decision searchable: {search}"
    );

    let pack = run_json_command(
        repo,
        &[
            "context",
            "--task",
            "Need rebuild sentinel routing for context packs",
            "--path",
            "crates/memzoi-core/src/context.rs",
            "--json",
        ],
    );
    let context_ids = record_ids_from_json(&pack);
    assert!(
        context_ids.contains(&"core/canonical-rebuild-decision"),
        "rebuilt DB should make canonical decision available to context packs: {pack}"
    );

    let precheck = run_json_command(
        repo,
        &[
            "precheck",
            "--path",
            "crates/memzoi-core/src/precheck.rs",
            "--action",
            "change rebuild sentinel precheck command handling",
            "--json",
        ],
    );
    let warnings = precheck
        .get("warnings")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("precheck JSON should expose warnings array: {precheck}"));
    assert!(
        warnings.iter().any(|warning| {
            warning.get("record_id").and_then(Value::as_str) == Some("core/canonical-rebuild-risk")
        }),
        "rebuilt DB should make canonical risk available to precheck: {precheck}"
    );
}

#[test]
fn rebuild_refuses_to_discard_open_proposals_with_ids_statuses_and_next_steps() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let pending = run_json_command(
        repo,
        &[
            "propose",
            "--manual",
            "--type",
            "decision",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--title",
            "Pending rebuild protection",
            "--body",
            "Rebuild should not silently discard pending proposals.",
            "--actor",
            "agent:red-tests",
            "--json",
        ],
    );
    let pending_id = json_string(&pending, "proposal_id").to_owned();

    let approved = run_json_command(
        repo,
        &[
            "propose",
            "--type",
            "fact",
            "--scope-kind",
            "repo",
            "--visibility",
            "repo",
            "--title",
            "Approved rebuild protection",
            "--body",
            "Rebuild should not silently discard approved proposals waiting to apply.",
            "--actor",
            "agent:red-tests",
            "--json",
        ],
    );
    let approved_id = json_string(&approved, "proposal_id").to_owned();

    let mut rebuild = memzoi();
    rebuild
        .args(["rebuild", "--json"])
        .current_dir(repo)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains(pending_id.as_str())
                .and(predicate::str::contains("pending"))
                .and(predicate::str::contains(approved_id.as_str()))
                .and(predicate::str::contains("approved"))
                .and(predicate::str::contains(
                    "memzoi proposals list --status open",
                ))
                .and(predicate::str::contains(
                    "memzoi proposals apply --all-approved",
                )),
        );
}

#[test]
fn export_okf_json_writes_active_repo_records_and_filters_inactive_private_personal() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let active_decision = create_applied_memory(
        repo,
        "decision",
        "repo",
        "OKF active repo decision",
        "OKF export must include this active repo decision.",
    );
    let active_fact = create_applied_memory(
        repo,
        "fact",
        "repo",
        "OKF active repo fact",
        "OKF export must include this active repo fact.",
    );
    let inactive = create_applied_memory(
        repo,
        "warning",
        "repo",
        "OKF inactive warning",
        "OKF export must exclude this tombstoned warning.",
    );
    run_json_command(
        repo,
        &[
            "tombstone",
            inactive.as_str(),
            "--reason",
            "inactive export fixture",
            "--json",
        ],
    );
    let private = create_applied_memory_with_visibility(
        repo,
        "procedure",
        "repo",
        "private",
        "OKF private procedure",
        "OKF export must exclude this private procedure.",
    );
    let personal = create_applied_memory_with_visibility(
        repo,
        "decision",
        "personal",
        "repo",
        "OKF personal decision",
        "OKF export must exclude this personal-scope decision.",
    );

    let export = run_json_command(repo, &["export", "okf", "--json"]);
    let written_paths = written_paths_from_json(&export);

    assert_eq!(
        written_paths.len(),
        2,
        "OKF export should write one markdown file per active non-private repo record: {export}"
    );
    assert_export_paths_exist(&written_paths);
    let exported = read_exported_contents(&written_paths);
    assert!(
        exported.contains("OKF active repo decision")
            && exported.contains("OKF export must include this active repo decision.")
            && exported.contains(&active_decision),
        "OKF export should render the active repo decision record: {exported}"
    );
    assert!(
        exported.contains("OKF active repo fact")
            && exported.contains("OKF export must include this active repo fact.")
            && exported.contains(&active_fact),
        "OKF export should render the active repo fact record: {exported}"
    );
    for excluded in [inactive, private, personal] {
        assert!(
            !exported.contains(&excluded),
            "OKF export should not render excluded record id {excluded}: {exported}"
        );
    }
    for excluded_text in [
        "OKF inactive warning",
        "OKF private procedure",
        "OKF personal decision",
    ] {
        assert!(
            !exported.contains(excluded_text),
            "OKF export should not render excluded record text {excluded_text:?}: {exported}"
        );
    }
}

#[test]
fn export_instruction_markdown_json_writes_agent_files_and_filters_non_projectable_records() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let active_procedure = create_applied_memory(
        repo,
        "procedure",
        "repo",
        "Projection active repo procedure",
        "Instruction projections must include this active repo procedure.",
    );
    let active_risk = create_applied_memory(
        repo,
        "risk",
        "repo",
        "Projection active repo risk",
        "Instruction projections must include this active repo risk.",
    );
    let active_fact = create_applied_memory(
        repo,
        "fact",
        "repo",
        "Projection active repo fact",
        "Instruction projections must exclude facts.",
    );
    let inactive = create_applied_memory(
        repo,
        "warning",
        "repo",
        "Projection inactive warning",
        "Instruction projections must exclude this tombstoned warning.",
    );
    run_json_command(
        repo,
        &[
            "tombstone",
            inactive.as_str(),
            "--reason",
            "inactive projection fixture",
            "--json",
        ],
    );
    let private = create_applied_memory_with_visibility(
        repo,
        "decision",
        "repo",
        "private",
        "Projection private decision",
        "Instruction projections must exclude this private decision.",
    );
    let personal = create_applied_memory_with_visibility(
        repo,
        "procedure",
        "personal",
        "repo",
        "Projection personal procedure",
        "Instruction projections must exclude this personal-scope procedure.",
    );

    for (subcommand, expected_file, projected_agent) in [
        ("agents-md", "AGENTS.memory.md", "AGENTS.md"),
        ("claude-md", "CLAUDE.memory.md", "CLAUDE.md"),
    ] {
        let export = run_json_command(repo, &["export", subcommand, "--json"]);
        let written_paths = written_paths_from_json(&export);
        let expected_path = test_paths(repo)
            .exports_dir
            .join(expected_file)
            .canonicalize()
            .unwrap_or_else(|error| panic!("canonicalize expected export path: {error}"));

        assert_eq!(
            written_paths,
            vec![expected_path],
            "{subcommand} should report its default export path in written_paths: {export}"
        );
        assert_export_paths_exist(&written_paths);
        let exported = read_exported_contents(&written_paths);
        assert!(
            exported.contains(projected_agent),
            "{subcommand} should render the projection for {projected_agent}: {exported}"
        );
        assert!(
            exported.contains("Projection active repo procedure")
                && exported
                    .contains("Instruction projections must include this active repo procedure.")
                && exported.contains(&active_procedure),
            "{subcommand} should render the active repo procedure: {exported}"
        );
        assert!(
            exported.contains("Projection active repo risk")
                && exported.contains("Instruction projections must include this active repo risk.")
                && exported.contains(&active_risk),
            "{subcommand} should render the active repo risk: {exported}"
        );
        for excluded in [
            active_fact.as_str(),
            inactive.as_str(),
            private.as_str(),
            personal.as_str(),
        ] {
            assert!(
                !exported.contains(excluded),
                "{subcommand} should not render excluded record id {excluded}: {exported}"
            );
        }
        for excluded_text in [
            "Projection active repo fact",
            "Projection inactive warning",
            "Projection private decision",
            "Projection personal procedure",
        ] {
            assert!(
                !exported.contains(excluded_text),
                "{subcommand} should not render excluded record text {excluded_text:?}: {exported}"
            );
        }
    }
}

fn initialized_temp_repo() -> tempfile::TempDir {
    initialized_temp_repo_with_home(&test_memzoi_home())
}

fn initialized_temp_repo_with_home(memzoi_home: &Path) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("temp repo");
    fs::create_dir(repo.path().join(".git")).expect("create git marker");

    let mut init = memzoi_with_home(memzoi_home);
    init.args(["init", "--json"])
        .current_dir(repo.path())
        .assert()
        .success();

    repo
}

fn run_json_command(repo: &Path, args: &[&str]) -> Value {
    let mut cmd = memzoi();
    let assert = cmd.args(args).current_dir(repo).assert().success();
    json_from_stdout(&assert.get_output().stdout)
}

fn run_json_command_failure(repo: &Path, args: &[&str]) -> Value {
    let mut cmd = memzoi();
    let assert = cmd.args(args).current_dir(repo).assert().failure();
    json_from_stdout(&assert.get_output().stdout)
}

fn run_command_failure_stderr(repo: &Path, args: &[&str]) -> String {
    run_command_failure_stderr_with_home(repo, args, &test_memzoi_home())
}

fn run_command_failure_stderr_with_home(repo: &Path, args: &[&str], memzoi_home: &Path) -> String {
    let mut cmd = memzoi_with_home(memzoi_home);
    let assert = cmd.args(args).current_dir(repo).assert().failure();
    std::str::from_utf8(&assert.get_output().stderr)
        .expect("stderr is utf-8")
        .to_owned()
}

fn write_pending_proposal_file(repo: &Path, name: &str, contents: String) {
    let pending = repo.join(".memzoi").join("proposals").join("pending");
    fs::create_dir_all(&pending).expect("create pending proposals dir");
    fs::write(pending.join(name), contents).expect("write pending proposal fixture");
}

fn valid_proposal_markdown() -> String {
    proposal_markdown_with("semantic", "create", "supersedes: []", "")
}

fn proposal_markdown_with_title(title: &str) -> String {
    format!(
        r#"---
id: mem_test_unicode
kind: proposal
version: okf/v0.1
profile: memzoi/v0
type: decision
lane: semantic
title: "{title}"
description: Valid proposal description.
status: proposed
proposal:
  action: create
  proposed_by: agent
  proposed_at: 2026-07-06T00:00:00Z
scope:
  kind: repo
  paths:
    - src/**
tags:
  - testing
timestamp: 2026-07-06T00:00:00Z
created_by: agent
sources:
  - path: src/lib.rs
supersedes: []
sensitivity: repo-safe
---

# {title}

This proposal body is valid.
"#
    )
}

fn proposal_markdown_with(
    lane: &str,
    action: &str,
    supersedes_yaml: &str,
    target_yaml: &str,
) -> String {
    proposal_markdown_with_options(
        lane,
        action,
        "proposed",
        supersedes_yaml,
        target_yaml,
        "repo-safe",
    )
}

fn proposal_markdown_with_options(
    lane: &str,
    action: &str,
    status: &str,
    supersedes_yaml: &str,
    target_yaml: &str,
    sensitivity: &str,
) -> String {
    format!(
        r#"---
id: mem_test_valid
kind: proposal
version: okf/v0.1
profile: memzoi/v0
type: decision
lane: {lane}
title: Valid proposal
description: Valid proposal description.
status: {status}
proposal:
  action: {action}
  proposed_by: agent
  proposed_at: 2026-07-06T00:00:00Z
  reason: Review packet context should not become canonical frontmatter.
  confidence: medium
{target_yaml}scope:
  kind: repo
  paths:
    - src/**
tags:
  - testing
timestamp: 2026-07-06T00:00:00Z
created_by: agent
sources:
  - path: src/lib.rs
{supersedes_yaml}
sensitivity: {sensitivity}
---

# Valid proposal

This proposal body is valid.
"#
    )
}

fn json_from_stdout(stdout: &[u8]) -> Value {
    let stdout = std::str::from_utf8(stdout).expect("stdout is utf-8");
    serde_json::from_str(stdout)
        .unwrap_or_else(|error| panic!("stdout should be JSON: {error}; stdout was {stdout:?}",))
}

fn next_patch_release_ref() -> String {
    let current = Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver");
    format!("v{}.{}.{}", current.major, current.minor, current.patch + 1)
}

fn spawn_latest_release_api(tag: &str) -> String {
    let tag = tag.to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local release API");
    let address = listener.local_addr().expect("local release API address");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept release API request");
        let mut request = [0; 2048];
        let read = stream.read(&mut request).expect("read release API request");
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(
            request.starts_with("GET /latest "),
            "unexpected release API request: {request}"
        );
        let body = format!(r#"{{"tag_name":"{tag}"}}"#);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write release API response");
    });

    format!("http://{address}")
}

fn create_applied_memory(
    repo: &Path,
    memory_type: &str,
    scope_kind: &str,
    title: &str,
    body: &str,
) -> String {
    create_applied_memory_with_visibility(repo, memory_type, scope_kind, "repo", title, body)
}

fn create_applied_memory_with_visibility(
    repo: &Path,
    memory_type: &str,
    scope_kind: &str,
    visibility: &str,
    title: &str,
    body: &str,
) -> String {
    let applied = run_json_command(
        repo,
        &[
            "propose",
            "--apply",
            "--type",
            memory_type,
            "--scope-kind",
            scope_kind,
            "--visibility",
            visibility,
            "--title",
            title,
            "--body",
            body,
            "--actor",
            "agent:cli-search-context-tests",
            "--json",
        ],
    );

    assert_eq!(json_string(&applied, "record_status"), "active");
    assert_eq!(applied.get("applied").and_then(Value::as_bool), Some(true));
    json_string(&applied, "record_id").to_owned()
}

fn attach_memory_path(repo: &Path, record_id: &str, path: &str) {
    let conn = Connection::open(memory_db_path(repo)).expect("open memory db for path fixture");
    conn.execute(
        "INSERT INTO memory_path(id, record_id, path, line_start, line_end)
         VALUES (?1, ?2, ?3, 1, 12)",
        rusqlite::params![format!("path-{record_id}"), record_id, path],
    )
    .expect("attach path metadata");
}

fn update_record_source_ref(repo: &Path, record_id: &str, source_ref: &str) {
    let conn = Connection::open(memory_db_path(repo)).expect("open memory db for source fixture");
    conn.execute(
        "UPDATE memory_record SET source_ref = ?1 WHERE id = ?2",
        rusqlite::params![source_ref, record_id],
    )
    .expect("update source_ref fixture");
}

fn memory_db_path(repo: &Path) -> std::path::PathBuf {
    test_paths(repo).db_path
}

fn record_ids_from_json(json: &Value) -> Vec<&str> {
    json.get("records")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("JSON should include records array: {json}"))
        .iter()
        .map(record_id_from_value)
        .collect()
}

fn written_paths_from_json(json: &Value) -> Vec<PathBuf> {
    json.get("written_paths")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("JSON should include written_paths array: {json}"))
        .iter()
        .map(|path| {
            let path = PathBuf::from(
                path.as_str()
                    .unwrap_or_else(|| panic!("export path should be a string: {path}")),
            );
            path.canonicalize().unwrap_or_else(|error| {
                panic!("canonicalize export path {}: {error}", path.display())
            })
        })
        .collect()
}

fn assert_export_paths_exist(paths: &[PathBuf]) {
    for path in paths {
        assert!(path.is_file(), "missing export file at {}", path.display());
    }
}

fn read_exported_contents(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| {
            fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("read export {}: {error}", path.display()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn record_id_from_value(value: &Value) -> &str {
    value
        .get("record")
        .and_then(|record| record.get("id"))
        .or_else(|| value.get("record_id"))
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("record entry should expose a record id: {value}"))
}

fn assert_json_does_not_reference_records(json: &Value, excluded_record_ids: &[String]) {
    let rendered = serde_json::to_string(json).expect("serialize JSON for exclusion assertion");
    for record_id in excluded_record_ids {
        assert!(
            !rendered.contains(record_id),
            "JSON should not reference excluded record {record_id}: {json}"
        );
    }
}

fn citation_for_record<'a>(json: &'a Value, record_id: &str) -> Option<&'a Value> {
    json.get("citations")
        .and_then(Value::as_array)?
        .iter()
        .find(|citation| {
            citation
                .get("record_id")
                .or_else(|| citation.get("id"))
                .and_then(Value::as_str)
                == Some(record_id)
        })
}

fn assert_json_string_field(value: &Value, keys: &[&str], expected: &str) {
    let actual = keys
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str));
    assert_eq!(
        actual,
        Some(expected),
        "expected one of {keys:?} to equal {expected:?} in {value}"
    );
}

fn prompt_text(json: &Value) -> Option<&str> {
    ["prompt", "prompt_text", "context", "text", "rendered"]
        .into_iter()
        .find_map(|key| json.get(key).and_then(Value::as_str))
}

fn proposals_from_json(json: &Value) -> &[Value] {
    json.get("proposals")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("JSON should include proposals array: {json}"))
}

fn applied_proposals_from_json(json: &Value) -> &[Value] {
    json.get("applied_proposals")
        .or_else(|| json.get("applied"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("JSON should include applied proposals array: {json}"))
}

fn proposal_id_from_value(value: &Value) -> &str {
    value
        .get("proposal")
        .and_then(|proposal| proposal.get("id").or_else(|| proposal.get("proposal_id")))
        .or_else(|| value.get("proposal_id"))
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("proposal entry should expose a proposal id: {value}"))
}

fn proposal_status(value: &Value) -> Option<&str> {
    value
        .get("proposal")
        .and_then(|proposal| proposal.get("status"))
        .or_else(|| value.get("status"))
        .and_then(Value::as_str)
}

fn proposal_title(value: &Value) -> Option<&str> {
    value
        .get("proposal")
        .and_then(|proposal| {
            proposal.get("title").or_else(|| {
                proposal
                    .get("payload")
                    .and_then(|payload| payload.get("title"))
            })
        })
        .or_else(|| value.get("title"))
        .or_else(|| {
            value
                .get("payload")
                .and_then(|payload| payload.get("title"))
        })
        .and_then(Value::as_str)
}

fn json_string<'a>(json: &'a Value, key: &str) -> &'a str {
    json.get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string field {key} in {json}"))
}

fn assert_json_string_contains(json: &Value, key: &str, expected: &str) {
    let actual = json_string(json, key);
    assert!(
        actual.contains(expected),
        "expected {key} to contain {expected:?} in {json}"
    );
}

fn assert_json_path(json: &Value, key: &str, expected_path: &Path) {
    let expected = expected_path
        .canonicalize()
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", expected_path.display()));
    let expected = expected.to_string_lossy();

    assert_eq!(
        json.get(key).and_then(Value::as_str),
        Some(expected.as_ref()),
        "unexpected JSON path for {key}"
    );
}

fn assert_json_array_contains(json: &Value, key: &str, expected: &str) {
    let values = json
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing array field {key} in {json}"));
    assert!(
        values.iter().any(|value| value.as_str() == Some(expected)),
        "expected {key} to contain {expected:?} in {json}"
    );
}

fn assert_json_array_contains_substring(json: &Value, key: &str, expected: &str) {
    let values = json
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing array field {key} in {json}"));
    assert!(
        values
            .iter()
            .filter_map(Value::as_str)
            .any(|value| value.contains(expected)),
        "expected {key} to contain a value with {expected:?} in {json}"
    );
}

fn assert_check_status(json: &Value, check_name: &str, expected_status: &str) {
    let checks = json
        .get("checks")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing checks array in {json}"));
    let check = checks
        .iter()
        .find(|check| check.get("name").and_then(Value::as_str) == Some(check_name))
        .unwrap_or_else(|| panic!("missing check {check_name:?} in {json}"));
    assert_eq!(
        check.get("status").and_then(Value::as_str),
        Some(expected_status),
        "unexpected status for check {check_name:?}: {json}"
    );
}
