use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    thread,
};

use assert_cmd::Command;
use memzoi_core::{MemoryDestination, MemoryPaths, TWO_PLANE_MEMORY_POLICY};
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
            "--sensitivity",
            "repo-safe",
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

fn assert_two_plane_policy(output: &str) {
    assert!(
        output.contains("Memzoi's canonical two-plane memory policy:"),
        "missing canonical policy heading: {output}"
    );
    let repo_plane = MemoryDestination::Repo
        .policy()
        .plane
        .map(|plane| plane.as_str())
        .unwrap_or("none");
    let runtime_plane = MemoryDestination::Local
        .policy()
        .plane
        .map(|plane| plane.as_str())
        .unwrap_or("none");
    assert!(
        output.contains(&format!(
            "Git-plane repo memory (`{repo_plane}`) is reviewed, durable project truth in `{}`.",
            TWO_PLANE_MEMORY_POLICY.canonical_records_glob
        )),
        "missing canonical Git-plane path: {output}"
    );
    assert!(
        output.contains(&format!(
            "Runtime-plane local/session memory (`{runtime_plane}`) is local continuity under `{}` and is not canonical shared repo truth.",
            TWO_PLANE_MEMORY_POLICY.runtime_project_root_template
        )),
        "missing canonical runtime-plane path: {output}"
    );

    for destination in MemoryDestination::ALL {
        let policy = destination.policy();
        let row = format!(
            "- `{}`: plane `{}`, route `{}`, review `{}`.",
            destination.as_str(),
            policy.plane.map(|plane| plane.as_str()).unwrap_or("none"),
            policy.write_route.as_str(),
            policy.review.as_str(),
        );
        assert!(
            output.contains(&row),
            "missing destination policy row: {row}\n{output}"
        );
    }

    for exclusion in TWO_PLANE_MEMORY_POLICY.repo_exclusions {
        let wording = exclusion.as_str().replace('_', " ");
        assert!(
            output.contains(&wording),
            "missing canonical repo exclusion {wording:?}: {output}"
        );
    }
    assert!(
        output.contains("raw chat transcripts"),
        "canonical repo exclusions must use raw chat transcript wording: {output}"
    );
    assert!(
        output.contains(
            "- `memzoi propose` and MCP proposals are reviewable operational state, not canonical records."
        ),
        "missing proposal operational-state boundary: {output}"
    );
}

#[test]
fn integrate_list_shows_supported_profiles() {
    let repo = initialized_temp_repo();
    let output = run_command_stdout(repo.path(), &["integrate", "list"]);

    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        vec![
            "codex\tCodex agent instructions\tinstruction-file",
            "claude\tClaude agent instructions\tinstruction-file",
            "mcp\tMCP setup and usage guidance\tsetup-guidance",
        ]
    );

    let listed = run_json_command(repo.path(), &["integrate", "list", "--json"]);
    let profiles = listed["profiles"]
        .as_array()
        .expect("profiles should be an array");
    let profile_names = profiles
        .iter()
        .map(|profile| profile["profile"].as_str().expect("profile name"))
        .collect::<Vec<_>>();
    assert_eq!(profile_names, ["codex", "claude", "mcp"]);
    assert_eq!(
        profiles.len(),
        3,
        "profile list should remain closed: {listed}"
    );
    let claude = profiles
        .iter()
        .find(|profile| profile["profile"] == "claude")
        .expect("list should include claude");
    assert!(
        claude.get("default_file").is_none(),
        "list should not expose a single misleading default_file: {listed}"
    );
    assert_json_array_contains(claude, "default_files", "AGENTS.md");
    assert_json_array_contains(claude, "default_files", "CLAUDE.md");
    assert!(
        claude["selection"]
            .as_str()
            .expect("selection should be a string")
            .contains("AGENTS.md Memzoi block"),
        "claude selection should describe conditional resolution: {listed}"
    );
    assert!(
        profiles.iter().any(|profile| profile["profile"] == "mcp"),
        "list should include mcp: {listed}"
    );
}

#[test]
fn integrate_prompt_prints_memzoi_protocol() {
    let repo = initialized_temp_repo();
    let mut cmd = memzoi();

    cmd.args(["integrate", "prompt", "--profile", "codex"])
        .current_dir(repo.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Before non-trivial work")
                .and(predicate::str::contains("memzoi context --task"))
                .and(predicate::str::contains("memzoi handoff --task"))
                .and(predicate::str::contains("Git-plane repo memory"))
                .and(predicate::str::contains(
                    "Runtime-plane local/session memory",
                ))
                .and(predicate::str::contains(".memzoi/records/*.md"))
                .and(predicate::str::contains("--include-local"))
                .and(predicate::str::contains("memzoi precheck --command"))
                .and(predicate::str::contains("memzoi propose"))
                .and(predicate::str::contains("Do not commit secrets")),
        );
}

#[test]
fn integrate_prompt_profiles_include_canonical_two_plane_policy_and_guidance() {
    let repo = initialized_temp_repo();

    for (profile, identity, search_guidance, context_guidance, precheck_guidance) in [
        (
            "codex",
            "You are working in a repo that uses Memzoi.",
            "Search Memzoi memory before broad scans.",
            "memzoi context --task",
            "memzoi precheck",
        ),
        (
            "claude",
            "You are Claude working in a repo that uses Memzoi.",
            "Search Memzoi memory before broad scans.",
            "memzoi context --task",
            "memzoi precheck",
        ),
        (
            "mcp",
            "Memzoi MCP setup and usage guidance for this repo.",
            "Search Memzoi memory before broad repo scans.",
            "Build context packs for the current task.",
            "Run precheck tools",
        ),
    ] {
        let args = ["integrate", "prompt", "--profile", profile];
        let output = run_command_stdout(repo.path(), &args);
        let repeat = run_command_stdout(repo.path(), &args);
        assert_eq!(output, repeat, "{profile} prompt must be deterministic");
        assert!(
            output.contains(identity),
            "missing {profile} identity: {output}"
        );
        assert_two_plane_policy(&output);
        assert!(
            output.contains(search_guidance),
            "{profile} must search memory before broad scans: {output}"
        );
        assert!(
            output.contains(context_guidance),
            "{profile} must include context guidance: {output}"
        );
        assert!(
            output.contains(precheck_guidance),
            "{profile} must include precheck guidance: {output}"
        );
        assert!(
            output.contains("memzoi propose") || output.contains("proposal requests"),
            "{profile} must include proposal guidance: {output}"
        );
        assert!(
            output.contains(
                "Canonical repo writes require an explicit CLI apply route: DB proposals use `memzoi apply <proposal-id>` or `memzoi proposals apply --all-approved` after approval, or the one-shot `memzoi propose --apply` route; file-backed proposal packets require review followed by `memzoi proposal-files apply <proposal-id>`. DB proposal state and packet review alone are not canonical."
            ),
            "{profile} must name explicit DB and file-backed CLI apply routes: {output}"
        );
        if profile == "mcp" {
            assert!(
                output.contains("DB proposal state and packet review alone are not canonical."),
                "{profile} must distinguish non-canonical DB/MCP state from canonical records: {output}"
            );
            assert!(
                output.contains("Apply proposals or write canonical repo records."),
                "{profile} must forbid MCP from applying or writing canonical records: {output}"
            );
        } else {
            assert!(
                output.contains(
                    "The policy block defines the canonical route; DB-local and MCP proposal state are not canonical before an explicit CLI apply."
                ),
                "{profile} must distinguish pre-apply DB/MCP state from canonical records: {output}"
            );
        }

        assert!(
            output.contains("Do not commit secrets"),
            "{profile} must include safety exclusions: {output}"
        );

        if profile == "mcp" {
            assert!(output.contains("memzoi mcp config --project-root ."));
            assert!(output.contains("MCP clients must not:"));
            assert!(output.contains("Apply proposals or write canonical repo records."));
            assert!(output.contains(
                "Claim that MCP can apply canonical records; canonical writes require an explicit CLI apply route described in the policy block."
            ));
            assert!(
                !output.lines().any(|line| line
                    .trim_start()
                    .starts_with("MCP can apply canonical records")),
                "MCP must not promise canonical apply: {output}"
            );
        }
    }
}

#[test]
fn import_human_mode_keeps_tabular_review_without_json_envelope() {
    let repo = initialized_temp_repo();
    let manifest_path = repo.path().join("human-import.yml");
    fs::write(
        &manifest_path,
        r#"version: memzoi/import-v1
sources:
  - path: imports/human-source.yml
candidates:
  - destination: repo
    reason: human review output
    type: decision
    lane: semantic
    title: Human review candidate
    body: Preserve this candidate body in human output.
    sensitivity: repo-safe
    scope:
      kind: repo
    tags: [review]
"#,
    )
    .expect("write human import manifest");

    let planned_json = run_json_command(
        repo.path(),
        &[
            "import",
            "plan",
            "--from-file",
            manifest_path.to_str().expect("manifest path utf-8"),
            "--json",
        ],
    );
    let plan_id = json_string(&planned_json, "plan_id").to_owned();

    let human_plan = run_command_stdout(
        repo.path(),
        &[
            "import",
            "plan",
            "--from-file",
            manifest_path.to_str().expect("manifest path utf-8"),
        ],
    );
    assert!(human_plan.contains(&format!("plan\t{plan_id}")));
    assert!(human_plan.contains("summary\t"));
    assert!(human_plan.contains("candidate\t0\tHuman review candidate\trepo\t"));
    assert!(human_plan.contains("body\tPreserve this candidate body in human output."));
    assert!(
        !human_plan
            .lines()
            .any(|line| line.trim_start().starts_with('{')),
        "human import plan must not append a pretty JSON object: {human_plan}"
    );
    assert!(
        !human_plan.contains("\"schema\"") && !human_plan.contains("\"mode\""),
        "human import plan must not append a JSON schema envelope: {human_plan}"
    );

    let human_apply = run_command_stdout(
        repo.path(),
        &[
            "import",
            "apply",
            "--from-file",
            manifest_path.to_str().expect("manifest path utf-8"),
            "--plan-id",
            plan_id.as_str(),
        ],
    );
    assert!(human_apply.contains(&format!("applied\t{plan_id}")));
    assert!(human_apply.contains("summary\t"));
    assert!(human_apply.contains("candidate\t0\tHuman review candidate\trepo\t"));
    assert!(human_apply.contains("body\tPreserve this candidate body in human output."));
    assert!(
        !human_apply
            .lines()
            .any(|line| line.trim_start().starts_with('{')),
        "human import apply must not append a pretty JSON object: {human_apply}"
    );
    assert!(
        !human_apply.contains("\"schema\"") && !human_apply.contains("\"mode\""),
        "human import apply must not append a JSON schema envelope: {human_apply}"
    );
}

#[test]
fn integrate_instructions_creates_and_updates_marked_block() {
    let repo = initialized_temp_repo();
    let instructions = repo.path().join("AGENTS.md");

    let created = run_json_command(
        repo.path(),
        &[
            "integrate",
            "instructions",
            "--profile",
            "codex",
            "--file",
            instructions.to_str().expect("utf-8 path"),
            "--json",
        ],
    );
    assert_json_string_field(&created, &["profile"], "codex");
    assert_json_string_field(&created, &["status"], "created");
    assert_json_string_field(&created, &["marker"], "memzoi");
    assert_json_string_field(&created, &["reason"], "explicit_file");
    let first = fs::read_to_string(&instructions).expect("read created instructions");
    assert!(first.contains("<!-- memzoi:start -->"));
    assert!(first.contains("memzoi context --task"));
    assert!(first.contains("memzoi handoff --task"));
    assert!(first.contains("memzoi propose --type"));
    assert!(first.contains("<!-- memzoi:end -->"));
    assert_two_plane_policy(&first);
    assert_eq!(first.matches("<!-- memzoi:end -->").count(), 1);

    fs::write(
        &instructions,
        first.replace("memzoi context --task", "stale-memory context --task"),
    )
    .expect("stale instructions");

    let updated_json = run_json_command(
        repo.path(),
        &[
            "integrate",
            "instructions",
            "--profile",
            "codex",
            "--file",
            instructions.to_str().expect("utf-8 path"),
            "--json",
        ],
    );
    assert_json_string_field(&updated_json, &["status"], "updated");
    let updated = fs::read_to_string(&instructions).expect("read updated instructions");
    assert_eq!(updated.matches("<!-- memzoi:start -->").count(), 1);
    assert!(!updated.contains("stale-memory context --task"));
    assert!(updated.contains("memzoi context --task"));
    assert!(updated.contains("memzoi handoff --task"));
    assert_two_plane_policy(&updated);
    assert_eq!(updated.matches("<!-- memzoi:end -->").count(), 1);

    let repeat_json = run_json_command(
        repo.path(),
        &[
            "integrate",
            "instructions",
            "--profile",
            "codex",
            "--file",
            instructions.to_str().expect("utf-8 path"),
            "--json",
        ],
    );
    assert_json_string_field(&repeat_json, &["status"], "updated");
    let repeated = fs::read_to_string(&instructions).expect("read repeated instructions");
    assert_eq!(
        repeated, updated,
        "marked instruction block should be idempotent"
    );
}

#[test]
fn integrate_instructions_profile_defaults_report_resolved_file_and_reason() {
    let repo = initialized_temp_repo();
    let agents = repo.path().join("AGENTS.md");
    let claude_file = repo.path().join("CLAUDE.md");

    let codex = run_json_command(
        repo.path(),
        &["integrate", "instructions", "--profile", "codex", "--json"],
    );
    assert_json_path(&codex, "file", &agents);
    assert_json_string_field(&codex, &["profile"], "codex");
    assert_json_string_field(&codex, &["status"], "created");
    assert_json_string_field(&codex, &["reason"], "default_profile_file");

    let claude = run_json_command(
        repo.path(),
        &["integrate", "instructions", "--profile", "claude", "--json"],
    );
    assert_json_path(&claude, "file", &agents);
    assert_json_string_field(&claude, &["profile"], "claude");
    assert_json_string_field(&claude, &["status"], "updated");
    assert_json_string_field(&claude, &["reason"], "existing_memzoi_block");
    let updated_agents = fs::read_to_string(&agents).expect("read AGENTS.md");
    assert!(updated_agents.contains("You are Claude"));
    assert!(
        !claude_file.exists(),
        "Claude should reuse the existing AGENTS Memzoi block"
    );
}

#[test]
fn integrate_instructions_claude_prefers_existing_agents_memzoi_block_over_claude_file() {
    let repo = initialized_temp_repo();
    let agents = repo.path().join("AGENTS.md");
    let claude_file = repo.path().join("CLAUDE.md");

    run_json_command(
        repo.path(),
        &["integrate", "instructions", "--profile", "codex", "--json"],
    );
    fs::write(&claude_file, "# Claude instructions\n").expect("write CLAUDE.md");

    let claude = run_json_command(
        repo.path(),
        &["integrate", "instructions", "--profile", "claude", "--json"],
    );

    assert_json_path(&claude, "file", &agents);
    assert_json_string_field(&claude, &["reason"], "existing_memzoi_block");
    let updated_agents = fs::read_to_string(&agents).expect("read AGENTS.md");
    assert!(updated_agents.contains("You are Claude"));
    let unchanged_claude = fs::read_to_string(&claude_file).expect("read CLAUDE.md");
    assert_eq!(unchanged_claude, "# Claude instructions\n");
}

#[test]
fn integrate_instructions_claude_ignores_reversed_agents_markers() {
    let repo = initialized_temp_repo();
    let agents = repo.path().join("AGENTS.md");
    fs::write(
        &agents,
        "# Agent instructions\n\n<!-- memzoi:end -->\nstale\n<!-- memzoi:start -->\n",
    )
    .expect("write reversed markers");

    let claude = run_json_command(
        repo.path(),
        &["integrate", "instructions", "--profile", "claude", "--json"],
    );

    assert_json_path(&claude, "file", &repo.path().join("CLAUDE.md"));
    assert_json_string_field(&claude, &["reason"], "default_profile_file");
    let unchanged_agents = fs::read_to_string(&agents).expect("read AGENTS.md");
    assert!(unchanged_agents.contains("<!-- memzoi:end -->"));
    let claude_file = fs::read_to_string(repo.path().join("CLAUDE.md")).expect("read CLAUDE.md");
    assert!(claude_file.contains("You are Claude"));
}

#[test]
fn integrate_instructions_fails_without_overwriting_unreadable_existing_file() {
    let repo = initialized_temp_repo();
    let instructions = repo.path().join("AGENTS.md");
    let invalid_utf8 = vec![0xff, 0xfe, 0xfd];
    fs::write(&instructions, &invalid_utf8).expect("write invalid utf-8 instructions");

    let stderr = run_command_failure_stderr(
        repo.path(),
        &[
            "integrate",
            "instructions",
            "--profile",
            "codex",
            "--file",
            instructions.to_str().expect("utf-8 path"),
        ],
    );

    assert!(stderr.contains("failed to read"));
    assert_eq!(
        fs::read(&instructions).expect("read instructions after failed update"),
        invalid_utf8
    );
}

#[test]
fn integrate_instructions_claude_falls_back_when_agents_block_check_is_unreadable() {
    let repo = initialized_temp_repo();
    let agents = repo.path().join("AGENTS.md");
    let invalid_utf8 = vec![0xff, 0xfe, 0xfd];
    fs::write(&agents, &invalid_utf8).expect("write invalid utf-8 AGENTS.md");

    let claude = run_json_command(
        repo.path(),
        &["integrate", "instructions", "--profile", "claude", "--json"],
    );

    assert_json_path(&claude, "file", &repo.path().join("CLAUDE.md"));
    assert_json_string_field(&claude, &["reason"], "default_profile_file");
    assert_eq!(
        fs::read(&agents).expect("read AGENTS.md after fallback"),
        invalid_utf8
    );
    let claude_file = fs::read_to_string(repo.path().join("CLAUDE.md")).expect("read CLAUDE.md");
    assert!(claude_file.contains("You are Claude"));
}

#[test]
fn integrate_instructions_defaults_resolve_from_project_root() {
    let repo = initialized_temp_repo();
    let nested = repo.path().join("nested").join("deeper");
    fs::create_dir_all(&nested).expect("create nested directory");

    let mut cmd = memzoi();
    let assert = cmd
        .args(["integrate", "instructions", "--profile", "codex", "--json"])
        .current_dir(&nested)
        .assert()
        .success();
    let written = json_from_stdout(&assert.get_output().stdout);

    assert_json_path(&written, "file", &repo.path().join("AGENTS.md"));
    assert!(
        !nested.join("AGENTS.md").exists(),
        "default instruction file should not be created in the current subdirectory"
    );
}

#[test]
fn integrate_instructions_mcp_skips_unreadable_agents_for_readable_claude() {
    let repo = initialized_temp_repo();
    let agents = repo.path().join("AGENTS.md");
    let claude = repo.path().join("CLAUDE.md");
    let invalid_utf8 = vec![0xff, 0xfe, 0xfd];
    fs::write(&agents, &invalid_utf8).expect("write invalid utf-8 AGENTS.md");
    fs::write(&claude, "# Claude instructions\n").expect("write CLAUDE.md");

    let mcp = run_json_command(
        repo.path(),
        &["integrate", "instructions", "--profile", "mcp", "--json"],
    );

    assert_json_path(&mcp, "file", &claude);
    assert_json_string_field(&mcp, &["reason"], "existing_instruction_file");
    assert_eq!(
        fs::read(&agents).expect("read AGENTS.md after MCP fallback"),
        invalid_utf8
    );
    let updated_claude = fs::read_to_string(&claude).expect("read updated CLAUDE.md");
    assert!(updated_claude.contains("Memzoi MCP setup and usage guidance"));
}

#[test]
fn integrate_rejects_unknown_profile() {
    let repo = initialized_temp_repo();
    let stderr =
        run_command_failure_stderr(repo.path(), &["integrate", "prompt", "--profile", "cursor"]);

    assert!(stderr.contains("invalid value"));
    assert!(stderr.contains("cursor"));
}

#[test]
fn integrate_requires_explicit_profile_for_prompt_and_instructions() {
    let repo = initialized_temp_repo();

    let prompt_stderr = run_command_failure_stderr(repo.path(), &["integrate", "prompt"]);
    assert!(prompt_stderr.contains("required"));
    assert!(prompt_stderr.contains("--profile"));

    let instructions_stderr =
        run_command_failure_stderr(repo.path(), &["integrate", "instructions", "--json"]);
    assert!(instructions_stderr.contains("required"));
    assert!(instructions_stderr.contains("--profile"));
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
            "--sensitivity",
            "repo-safe",
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
            "--sensitivity",
            "repo-safe",
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
            "--sensitivity",
            "repo-safe",
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
            "--sensitivity",
            "repo-safe",
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
            "--sensitivity",
            "repo-safe",
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
fn propose_omitted_sensitivity_is_explicit_unknown_and_cannot_apply() {
    let repo = initialized_temp_repo();
    let sentinel = "private-body-sentinel-must-not-appear-in-errors";
    let proposal = run_json_command(
        repo.path(),
        &[
            "propose",
            "--apply",
            "--type",
            "fact",
            "--title",
            "Unclassified proposal",
            "--body",
            sentinel,
            "--json",
        ],
    );

    assert_eq!(json_string(&proposal, "status"), "pending");
    assert_eq!(json_string(&proposal, "sensitivity"), "unknown");
    assert_eq!(proposal["applied"], false);
    assert_eq!(proposal["record_id"], Value::Null);
    assert!(
        proposal["validation"]["issues"]
            .as_array()
            .is_some_and(|issues| issues
                .iter()
                .any(|issue| { issue["code"] == "repo_sensitivity_required" }))
    );

    let proposal_id = json_string(&proposal, "proposal_id");
    run_json_command(
        repo.path(),
        &[
            "approve",
            proposal_id,
            "--actor",
            "reviewer:human",
            "--json",
        ],
    );
    let blocked = run_json_command_failure(repo.path(), &["apply", proposal_id, "--json"]);
    assert_eq!(blocked["ok"], false);
    assert_json_string_field(&blocked["error"], &["code"], "repo_sensitivity_required");
    assert_json_string_field(&blocked["error"], &["operation"], "apply");
    assert_json_string_field(&blocked["error"], &["sensitivity"], "unknown");
    assert!(
        !serde_json::to_string(&blocked)
            .expect("serialize structured sensitivity error")
            .contains(sentinel)
    );
    assert!(
        fs::read_dir(test_paths(repo.path()).records_dir())
            .expect("records directory")
            .next()
            .is_none(),
        "unclassified proposal must not create canonical files"
    );
}

#[test]
fn supersede_json_blocks_every_non_repo_safe_sensitivity_without_echoing_body() {
    for sensitivity in [
        "local-only",
        "sensitive",
        "secret",
        "raw-transcript",
        "private-personal-data",
        "temporary-state",
        "unknown",
    ] {
        let repo = initialized_temp_repo();
        let applied = run_json_command(
            repo.path(),
            &[
                "propose",
                "--apply",
                "--type",
                "fact",
                "--sensitivity",
                "repo-safe",
                "--title",
                "Supersede sensitivity target",
                "--body",
                "Original canonical body remains active.",
                "--json",
            ],
        );
        let record_id = json_string(&applied, "record_id").to_owned();
        let sentinel = format!("SUPERSEDE-{sensitivity}-BODY-SENTINEL");
        let blocked = run_json_command_failure(
            repo.path(),
            &[
                "supersede",
                &record_id,
                "--type",
                "fact",
                "--scope-kind",
                "repo",
                "--visibility",
                "repo",
                "--sensitivity",
                sensitivity,
                "--title",
                "Blocked replacement",
                "--body",
                &sentinel,
                "--json",
            ],
        );
        assert_eq!(blocked["ok"], false, "{blocked}");
        assert_json_string_field(&blocked["error"], &["code"], "repo_sensitivity_required");
        assert_json_string_field(&blocked["error"], &["operation"], "supersede");
        assert_json_string_field(&blocked["error"], &["sensitivity"], sensitivity);
        assert!(
            !serde_json::to_string(&blocked)
                .expect("serialize supersede error")
                .contains(&sentinel)
        );
        assert_eq!(
            fs::read_dir(test_paths(repo.path()).records_dir())
                .expect("read canonical records")
                .count(),
            1,
            "blocked supersede must not create a replacement"
        );
    }
}

#[test]
fn cli_proposal_evidence_survives_apply_rebuild_and_recall_separately_from_lineage() {
    let repo = initialized_temp_repo();
    let applied = run_json_command(
        repo.path(),
        &[
            "propose",
            "--apply",
            "--type",
            "decision",
            "--sensitivity",
            "repo-safe",
            "--source-kind",
            "issue",
            "--source-ref",
            "issue://42#trust-contract",
            "--title",
            "CLI evidence survives rebuild",
            "--body",
            "Zircon provenance remains attached to original evidence.",
            "--json",
        ],
    );
    let proposal_id = json_string(&applied, "proposal_id").to_owned();
    let record_id = json_string(&applied, "record_id").to_owned();

    run_json_command(repo.path(), &["rebuild", "--json"]);
    let recalled = run_json_command(repo.path(), &["search", "zircon provenance", "--json"]);
    let result = &recalled["records"][0];
    assert_json_string_field(&result["record"], &["id"], &record_id);
    assert_json_string_field(&result["record"], &["source_kind"], "issue");
    assert_json_string_field(
        &result["record"],
        &["source_ref"],
        "issue://42#trust-contract",
    );
    assert_json_string_field(&result["record"], &["proposal_id"], &proposal_id);
    assert_json_string_field(&result["citations"][0], &["source_kind"], "issue");
    assert_json_string_field(
        &result["citations"][0],
        &["source_ref"],
        "issue://42#trust-contract",
    );
    assert!(result["citations"][0].get("proposal_id").is_none());
}

#[test]
fn cli_source_reference_does_not_fabricate_an_evidence_kind() {
    let repo = initialized_temp_repo();
    let applied = run_json_command(
        repo.path(),
        &[
            "propose",
            "--apply",
            "--type",
            "fact",
            "--sensitivity",
            "repo-safe",
            "--source-ref",
            "issue://42#reference-only",
            "--title",
            "Reference-only CLI evidence",
            "--body",
            "Topaz reference-only provenance remains independently nullable.",
            "--json",
        ],
    );
    let proposal_id = json_string(&applied, "proposal_id").to_owned();

    run_json_command(repo.path(), &["rebuild", "--json"]);
    let recalled = run_json_command(repo.path(), &["search", "topaz reference-only", "--json"]);
    let result = &recalled["records"][0];
    assert_eq!(result["record"]["source_kind"], Value::Null);
    assert_json_string_field(
        &result["record"],
        &["source_ref"],
        "issue://42#reference-only",
    );
    assert_json_string_field(&result["record"], &["proposal_id"], &proposal_id);
    assert_eq!(result["citations"][0]["source_kind"], Value::Null);
    assert_json_string_field(
        &result["citations"][0],
        &["source_ref"],
        "issue://42#reference-only",
    );
}

#[test]
fn cli_multiline_evidence_round_trips_without_index_drift() {
    let repo = initialized_temp_repo();
    let source_ref = "issue://42\nfragment";
    run_json_command(
        repo.path(),
        &[
            "propose",
            "--apply",
            "--type",
            "fact",
            "--sensitivity",
            "repo-safe",
            "--source-ref",
            source_ref,
            "--title",
            "Multiline evidence encoding",
            "--body",
            "Jasper multiline evidence must not create canonical/index drift.",
            "--json",
        ],
    );

    let doctor = run_json_command(repo.path(), &["doctor", "--json"]);
    assert_check_status(&doctor, "repo_index", "ok");
    run_json_command(repo.path(), &["rebuild", "--json"]);
    let recalled = run_json_command(repo.path(), &["search", "jasper multiline", "--json"]);
    assert_eq!(recalled["records"][0]["record"]["source_ref"], source_ref);
    assert_eq!(
        recalled["records"][0]["citations"][0]["source_ref"],
        source_ref
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
            "--sensitivity",
            "repo-safe",
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
            "--sensitivity",
            "repo-safe",
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
            "--sensitivity",
            "repo-safe",
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
            "--sensitivity",
            "repo-safe",
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
            "--sensitivity",
            "repo-safe",
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
            "--sensitivity",
            "repo-safe",
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
fn proposal_files_apply_repo_safe_create_resolves_packet_and_updates_runtime_index() {
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
    assert_json_string_field(&applied, &["status"], "applied");
    assert_json_string_field(&applied, &["outcome"], "applied");
    assert_eq!(applied["runtime_index_updated"], true);
    assert_eq!(applied["already_resolved"], false);
    assert_json_string_field(
        &applied,
        &["resolved_path"],
        ".memzoi/proposals/resolved/applied/valid-proposal.md",
    );

    let record_path = repo.path().join(json_string(&applied, "record_path"));
    let rendered = fs::read_to_string(&record_path).expect("read canonical record");
    assert!(rendered.contains("type: decision\n"));
    assert!(rendered.contains("lane: semantic\n"));
    assert!(rendered.contains("status: active\n"));
    assert!(rendered.contains("visibility: repo\n"));
    assert!(rendered.contains("confidence: 1\n"));
    assert!(rendered.contains("source: path\n"));
    assert!(rendered.contains("source_ref: src/lib.rs\n"));
    assert!(rendered.contains("proposal_id: mem_test_valid\n"));
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
        !repo
            .path()
            .join(".memzoi/proposals/pending/valid-proposal.md")
            .exists(),
        "apply should remove the resolved packet from pending"
    );
    let resolved_path = repo
        .path()
        .join(".memzoi/proposals/resolved/applied/valid-proposal.md");
    let resolved = fs::read_to_string(&resolved_path).expect("read resolved proposal");
    assert!(resolved.contains("status: applied\n"), "{resolved}");
    assert!(resolved.contains("outcome: applied\n"), "{resolved}");
    assert!(resolved.contains("resolved_by: cli\n"), "{resolved}");
    assert!(
        resolved.contains("record_id: valid-proposal\n"),
        "{resolved}"
    );
    let conn = Connection::open(test_paths(repo.path()).db_path).expect("open runtime db");
    let runtime_records: i64 = conn
        .query_row("SELECT COUNT(*) FROM memory_record", [], |row| row.get(0))
        .expect("count runtime records");
    assert_eq!(
        runtime_records, 1,
        "apply should update derived SQLite state"
    );

    let current_doctor = run_json_command(repo.path(), &["doctor", "--json"]);
    assert_check_status(&current_doctor, "repo_index", "ok");
    assert_check_status(&current_doctor, "proposal_files", "ok");
    let search = run_json_command(repo.path(), &["search", "proposal body", "--json"]);
    assert_eq!(
        search["records"].as_array().map(Vec::len),
        Some(1),
        "runtime index should immediately recall the applied proposal: {search}"
    );
    let recalled = &search["records"][0]["record"];
    assert_json_string_field(recalled, &["source_kind"], "path");
    assert_json_string_field(recalled, &["source_ref"], "src/lib.rs");
    assert_json_string_field(recalled, &["proposal_id"], "mem_test_valid");

    run_json_command(repo.path(), &["rebuild", "--json"]);
    let rebuilt_search = run_json_command(repo.path(), &["search", "proposal body", "--json"]);
    let rebuilt = &rebuilt_search["records"][0]["record"];
    assert_json_string_field(rebuilt, &["source_kind"], "path");
    assert_json_string_field(rebuilt, &["source_ref"], "src/lib.rs");
    assert_json_string_field(rebuilt, &["proposal_id"], "mem_test_valid");

    let pending = run_json_command(repo.path(), &["proposal-files", "list", "--json"]);
    assert!(proposals_from_json(&pending).is_empty(), "{pending}");
    let shown = run_json_command(
        repo.path(),
        &["proposal-files", "show", "mem_test_valid", "--json"],
    );
    assert_json_string_field(&shown, &["status"], "applied");
    assert_json_string_field(&shown["resolution"], &["outcome"], "applied");

    let resolved_before = fs::read(&resolved_path).expect("read resolved packet before rerun");
    let record_before = fs::read(&record_path).expect("read record before rerun");
    let repeated = run_json_command(
        repo.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    assert_eq!(repeated["already_resolved"], true);
    assert_json_string_field(&repeated, &["outcome"], "applied");
    assert_eq!(
        fs::read(&resolved_path).expect("read resolved rerun"),
        resolved_before
    );
    assert_eq!(
        fs::read(&record_path).expect("read record rerun"),
        record_before
    );
}

#[test]
fn proposal_files_reject_archives_reason_without_creating_canonical_memory() {
    let repo = initialized_temp_repo();
    write_pending_proposal_file(repo.path(), "valid-proposal.md", valid_proposal_markdown());

    let rejected = run_json_command(
        repo.path(),
        &[
            "proposal-files",
            "reject",
            "mem_test_valid",
            "--reason",
            "Reviewer found the evidence too weak.",
            "--actor",
            "reviewer:human",
            "--json",
        ],
    );
    assert_json_string_field(&rejected, &["status"], "rejected");
    assert_json_string_field(&rejected, &["outcome"], "rejected");
    assert_eq!(rejected["already_resolved"], false);
    assert_eq!(rejected["runtime_index_updated"], false);
    assert!(rejected["record_id"].is_null(), "{rejected}");
    assert_json_string_field(
        &rejected["resolution"],
        &["reason"],
        "Reviewer found the evidence too weak.",
    );
    assert_json_string_field(&rejected["resolution"], &["resolved_by"], "reviewer:human");

    assert_eq!(
        fs::read_dir(test_paths(repo.path()).records_dir())
            .expect("read records directory")
            .count(),
        0
    );
    assert!(
        !repo
            .path()
            .join(".memzoi/proposals/pending/valid-proposal.md")
            .exists()
    );
    let resolved_path = repo
        .path()
        .join(".memzoi/proposals/resolved/rejected/valid-proposal.md");
    let resolved_before = fs::read(&resolved_path).expect("read rejected packet");
    let rendered = String::from_utf8(resolved_before.clone()).expect("resolved UTF-8");
    assert!(rendered.contains("status: rejected\n"), "{rendered}");
    assert!(
        rendered.contains("reason: Reviewer found the evidence too weak."),
        "{rendered}"
    );

    let repeated = run_json_command(
        repo.path(),
        &[
            "proposal-files",
            "reject",
            "mem_test_valid",
            "--reason",
            "A different repeated reason is not written.",
            "--json",
        ],
    );
    assert_eq!(repeated["already_resolved"], true);
    assert_json_string_field(
        &repeated["resolution"],
        &["reason"],
        "Reviewer found the evidence too weak.",
    );
    assert_eq!(
        fs::read(&resolved_path).expect("read repeated rejection"),
        resolved_before
    );

    let conflict =
        run_command_failure_stderr(repo.path(), &["proposal-files", "apply", "mem_test_valid"]);
    assert!(
        conflict.contains("already resolved as rejected"),
        "{conflict}"
    );
    let search = run_json_command(repo.path(), &["search", "proposal body", "--json"]);
    assert!(search["records"].as_array().is_some_and(Vec::is_empty));
}

#[test]
fn proposal_file_validation_and_rejection_never_echo_non_repo_safe_content() {
    for (sensitivity, sentinel) in [
        ("secret", "SECRET-CONTENT-SENTINEL"),
        ("raw-transcript", "RAW-TRANSCRIPT-CONTENT-SENTINEL"),
        (
            "private-personal-data",
            "PRIVATE-PERSONAL-DATA-CONTENT-SENTINEL",
        ),
        ("unknown", "UNKNOWN-CONTENT-SENTINEL"),
    ] {
        let repo = initialized_temp_repo();
        let mut proposal = proposal_markdown_with_options(
            "semantic",
            "create",
            "proposed",
            "supersedes: []",
            "",
            sensitivity,
        )
        .replace(
            "title: Valid proposal",
            &format!("title: \"{sentinel} title\""),
        )
        .replace(
            "description: Valid proposal description.",
            &format!("description: \"{sentinel} description\""),
        )
        .replace("# Valid proposal", &format!("# {sentinel} title"))
        .replace("This proposal body is valid.", &format!("{sentinel} body"))
        .replace(
            "  - path: src/lib.rs",
            &format!("  - path: \"src/{sentinel}.rs\""),
        );
        if sensitivity == "unknown" {
            proposal = proposal.replace("sensitivity: unknown\n", "");
        }
        write_pending_proposal_file(repo.path(), "valid-proposal.md", proposal);

        let validation =
            run_json_command_failure(repo.path(), &["proposal-files", "validate", "--json"]);
        let validation_stdout =
            serde_json::to_string(&validation).expect("serialize validation output");
        let validation_stderr =
            run_command_failure_stderr(repo.path(), &["proposal-files", "validate", "--json"]);
        assert!(
            !validation_stdout.contains(sentinel),
            "validation stdout leaked {sensitivity} content: {validation_stdout}"
        );
        assert!(
            !validation_stderr.contains(sentinel),
            "validation stderr leaked {sensitivity} content: {validation_stderr}"
        );
        assert!(
            validation_stdout.contains(&format!("sensitivity {sensitivity}")),
            "validation should report the actionable classification: {validation_stdout}"
        );

        let rejected = run_json_command(
            repo.path(),
            &[
                "proposal-files",
                "reject",
                "mem_test_valid",
                "--reason",
                "Rejected at the repository trust boundary.",
                "--actor",
                "reviewer:human",
                "--json",
            ],
        );
        let rejected_stdout = serde_json::to_string(&rejected).expect("serialize rejection output");
        assert!(
            !rejected_stdout.contains(sentinel),
            "rejection stdout leaked {sensitivity} content: {rejected_stdout}"
        );
        assert!(
            json_string(&rejected, "proposal_id").starts_with("redacted-identity-"),
            "{rejected}"
        );
        assert!(
            json_string(&rejected, "file_id").starts_with("redacted-identity-"),
            "{rejected}"
        );
        assert_json_string_field(&rejected, &["outcome"], "rejected");
        assert_json_string_field(&rejected, &["sensitivity"], sensitivity);
        assert_json_string_field(&rejected["resolution"], &["resolved_by"], "reviewer:human");
        assert_json_string_field(
            &rejected["resolution"],
            &["reason"],
            "Rejected at the repository trust boundary.",
        );

        let resolved_path = repo.path().join(json_string(&rejected, "resolved_path"));
        let resolved = fs::read_to_string(&resolved_path).expect("read redacted rejection receipt");
        assert!(
            !resolved.contains(sentinel),
            "resolved receipt leaked {sensitivity} content: {resolved}"
        );
        assert!(resolved.contains("Content hash (BLAKE3):"), "{resolved}");
        assert!(resolved.contains("status: rejected\n"), "{resolved}");
        assert!(resolved.contains("outcome: rejected\n"), "{resolved}");
        assert!(
            resolved.contains("resolved_by: reviewer:human\n"),
            "{resolved}"
        );

        let repeated = run_json_command(
            repo.path(),
            &[
                "proposal-files",
                "reject",
                "mem_test_valid",
                "--reason",
                "A repeated request must remain redacted.",
                "--json",
            ],
        );
        let repeated_stdout = serde_json::to_string(&repeated).expect("serialize repeated output");
        assert!(!repeated_stdout.contains(sentinel));
        assert_eq!(repeated["already_resolved"], true);
    }
}

#[test]
fn non_repo_safe_rejection_hashes_packet_id_and_file_id_but_replays_raw_aliases() {
    let repo = initialized_temp_repo();
    let raw_id = "secret-proposal-identity-sentinel";
    let raw_file_id = "secret-file-identity-sentinel";
    let body_sentinel = "SECRET-IDENTITY-BODY-SENTINEL";
    let proposal = proposal_markdown_with_options(
        "semantic",
        "create",
        "proposed",
        "supersedes: []",
        "",
        "secret",
    )
    .replace("id: mem_test_valid", &format!("id: {raw_id}"))
    .replace("This proposal body is valid.", body_sentinel);
    write_pending_proposal_file(repo.path(), &format!("{raw_file_id}.md"), proposal.clone());

    let validation =
        run_json_command_failure(repo.path(), &["proposal-files", "validate", "--json"]);
    let validation_json = serde_json::to_string(&validation).expect("serialize validation");
    for forbidden in [raw_id, raw_file_id, body_sentinel] {
        assert!(!validation_json.contains(forbidden), "{validation_json}");
    }

    let rejected = run_json_command(
        repo.path(),
        &[
            "proposal-files",
            "reject",
            raw_id,
            "--reason",
            "Rejected without retaining raw packet identity.",
            "--json",
        ],
    );
    let rejected_json = serde_json::to_string(&rejected).expect("serialize rejection");
    for forbidden in [raw_id, raw_file_id, body_sentinel] {
        assert!(!rejected_json.contains(forbidden), "{rejected_json}");
    }
    let receipt_id = json_string(&rejected, "proposal_id");
    let receipt_file_id = json_string(&rejected, "file_id");
    assert!(receipt_id.starts_with("redacted-identity-"), "{rejected}");
    assert!(
        receipt_file_id.starts_with("redacted-identity-"),
        "{rejected}"
    );
    let resolved_path = repo.path().join(json_string(&rejected, "resolved_path"));
    assert!(resolved_path.is_file(), "{}", resolved_path.display());
    let receipt = fs::read_to_string(&resolved_path).expect("read rejection receipt");
    for forbidden in [raw_id, raw_file_id, body_sentinel] {
        assert!(!receipt.contains(forbidden), "{receipt}");
    }

    for raw_alias in [raw_id, raw_file_id] {
        let replayed = run_json_command(
            repo.path(),
            &[
                "proposal-files",
                "reject",
                raw_alias,
                "--reason",
                "Replay must use the existing hash-only receipt.",
                "--json",
            ],
        );
        assert_eq!(replayed["already_resolved"], true, "{replayed}");
        assert_json_string_field(&replayed, &["proposal_id"], receipt_id);
        assert_json_string_field(&replayed, &["file_id"], receipt_file_id);
    }

    write_pending_proposal_file(repo.path(), &format!("{raw_file_id}.md"), proposal);
    let collision = run_command_failure_stderr(
        repo.path(),
        &[
            "proposal-files",
            "reject",
            raw_id,
            "--reason",
            "A stale raw packet must not bypass the hash-only resolution.",
        ],
    );
    for forbidden in [raw_id, raw_file_id, body_sentinel] {
        assert!(!collision.contains(forbidden), "{collision}");
    }
    assert!(
        collision.contains("reintroduces resolved identity")
            && collision.contains("already rejected"),
        "{collision}"
    );
}

#[test]
fn malformed_known_non_safe_packet_validates_and_rejects_without_echoing_fields() {
    let repo = initialized_temp_repo();
    let raw_id = "secret-malformed-id-sentinel";
    let raw_file_id = "secret-malformed-file-sentinel";
    let malformed_type = "secret-malformed-type-sentinel";
    let proposal = proposal_markdown_with_options(
        "semantic",
        "create",
        "proposed",
        "supersedes: []",
        "",
        "secret",
    )
    .replace("id: mem_test_valid", &format!("id: {raw_id}"))
    .replace("type: decision", &format!("type: {malformed_type}"));
    write_pending_proposal_file(repo.path(), &format!("{raw_file_id}.md"), proposal);

    let validation =
        run_json_command_failure(repo.path(), &["proposal-files", "validate", "--json"]);
    let rendered = serde_json::to_string(&validation).expect("serialize validation");
    for forbidden in [raw_id, raw_file_id, malformed_type] {
        assert!(!rendered.contains(forbidden), "{rendered}");
    }
    assert!(rendered.contains("sensitivity secret"), "{rendered}");

    let rejected = run_json_command(
        repo.path(),
        &[
            "proposal-files",
            "reject",
            raw_id,
            "--reason",
            "Malformed secret packet rejected at preflight.",
            "--json",
        ],
    );
    assert_json_string_field(&rejected, &["outcome"], "rejected");
    assert!(json_string(&rejected, "proposal_id").starts_with("redacted-identity-"));
    let rendered = serde_json::to_string(&rejected).expect("serialize rejection");
    for forbidden in [raw_id, raw_file_id, malformed_type] {
        assert!(!rendered.contains(forbidden), "{rendered}");
    }

    let replayed = run_json_command(
        repo.path(),
        &[
            "proposal-files",
            "reject",
            raw_file_id,
            "--reason",
            "Replay malformed packet rejection.",
            "--json",
        ],
    );
    assert_eq!(replayed["already_resolved"], true, "{replayed}");
}

#[test]
fn non_repo_safe_rejection_receipt_hashes_target_lineage_and_proposal_authorship() {
    let repo = initialized_temp_repo();
    let proposal = proposal_markdown_with_options(
        "semantic",
        "tombstone",
        "proposed",
        "supersedes: []",
        "  target: TARGET-LINEAGE-SENTINEL\n",
        "secret",
    )
    .replace(
        "reason: Review packet context should not become canonical frontmatter.",
        "reason: ORIGINAL-PROPOSAL-REASON-SENTINEL",
    )
    .replace(
        "proposed_by: agent",
        "proposed_by: ORIGINAL-PROPOSER-SENTINEL",
    )
    .replace("created_by: agent", "created_by: ORIGINAL-CREATOR-SENTINEL");
    write_pending_proposal_file(repo.path(), "valid-proposal.md", proposal);

    let rejected = run_json_command(
        repo.path(),
        &[
            "proposal-files",
            "reject",
            "mem_test_valid",
            "--reason",
            "Rejected at the repository trust boundary.",
            "--actor",
            "reviewer:human",
            "--json",
        ],
    );
    let output = serde_json::to_string(&rejected).expect("serialize rejection output");
    assert!(!output.contains("SENTINEL"), "{output}");
    assert_json_string_field(&rejected, &["action"], "create");
    assert!(rejected["target_id"].is_null(), "{rejected}");

    let resolved_path = repo.path().join(json_string(&rejected, "resolved_path"));
    let resolved = fs::read_to_string(&resolved_path).expect("read redacted rejection receipt");
    assert!(!resolved.contains("SENTINEL"), "{resolved}");
    assert!(resolved.contains("action: create\n"), "{resolved}");
    assert!(resolved.contains("supersedes: []\n"), "{resolved}");
    assert!(resolved.contains("Content hash (BLAKE3):"), "{resolved}");

    let repeated = run_json_command(
        repo.path(),
        &[
            "proposal-files",
            "reject",
            "mem_test_valid",
            "--reason",
            "A repeated request must remain redacted.",
            "--json",
        ],
    );
    assert_eq!(repeated["already_resolved"], true);
    assert!(
        !serde_json::to_string(&repeated)
            .expect("serialize repeated rejection")
            .contains("SENTINEL")
    );
}

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
        error.contains("duplicate pending proposal identity") && error.contains("mem_test_valid"),
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
        r#"version: memzoi/import-v1
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
    assert_json_string_field(&planned, &["schema"], "memzoi/import-plan-v1");
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
    assert_json_string_field(&applied, &["schema"], "memzoi/import-plan-v1");
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
        r#"version: memzoi/import-v1
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
        r#"version: memzoi/import-v1
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

#[test]
fn proposal_files_supersede_preserves_target_evidence_and_creates_active_lineage() {
    let repo = initialized_temp_repo();
    write_canonical_record_fixture(
        repo.path(),
        "legacy-auth-guidance",
        "repo",
        None,
        "active",
        "2026-07-01T00:00:00Z",
        "2026-07-01T00:00:00Z",
    );
    let proposal = proposal_markdown_with(
        "semantic",
        "supersede",
        "supersedes:\n  - legacy-auth-guidance",
        "",
    )
    .replace("title: Valid proposal", "title: Replacement auth guidance")
    .replace("# Valid proposal", "# Replacement auth guidance")
    .replace(
        "This proposal body is valid.",
        "Replacement guidance requires server-side session validation.",
    );
    write_pending_proposal_file(repo.path(), "valid-proposal.md", proposal);

    let applied = run_json_command(
        repo.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    assert_json_string_field(&applied, &["action"], "supersede");
    assert_json_string_field(&applied, &["outcome"], "applied");
    assert_json_string_field(&applied, &["record_id"], "replacement-auth-guidance");
    assert_json_string_field(&applied, &["record_status"], "active");
    assert_json_string_field(&applied, &["target_id"], "legacy-auth-guidance");
    assert_eq!(applied["runtime_index_updated"], true);

    let old_path = repo.path().join(".memzoi/records/legacy-auth-guidance.md");
    let old = fs::read_to_string(&old_path).expect("read superseded target");
    assert!(old.contains("status: superseded\n"), "{old}");
    assert!(
        old.contains("source_ref: \"evidence://legacy-auth\"\n"),
        "{old}"
    );
    assert!(old.contains("tags:\n  - legacy-evidence\n"), "{old}");
    assert!(old.contains("applies_to:\n  - \"src/auth/**\"\n"), "{old}");
    assert!(
        old.contains("Original legacy auth evidence remains reviewable."),
        "{old}"
    );

    let replacement_path = repo
        .path()
        .join(".memzoi/records/replacement-auth-guidance.md");
    let replacement = fs::read_to_string(&replacement_path).expect("read replacement");
    assert!(replacement.contains("status: active\n"), "{replacement}");
    assert!(
        replacement.contains("supersedes: legacy-auth-guidance\n"),
        "{replacement}"
    );
    assert!(
        replacement.contains("source: path\n")
            && replacement.contains("source_ref: src/lib.rs\n")
            && replacement.contains("proposal_id: mem_test_valid\n"),
        "{replacement}"
    );

    let resolved_path = repo
        .path()
        .join(".memzoi/proposals/resolved/applied/valid-proposal.md");
    let resolved = fs::read_to_string(&resolved_path).expect("read resolved supersede packet");
    assert!(resolved.contains("action: supersede\n"), "{resolved}");
    assert!(
        resolved.contains("target_id: legacy-auth-guidance\n"),
        "{resolved}"
    );
    assert!(
        resolved.contains("reason: Review packet context should not become canonical frontmatter."),
        "{resolved}"
    );
    assert!(resolved.contains("path: src/lib.rs\n"), "{resolved}");

    let search = run_json_command(repo.path(), &["search", "server-side session", "--json"]);
    assert_eq!(
        record_ids_from_json(&search),
        vec!["replacement-auth-guidance"]
    );
    let old_search = run_json_command(repo.path(), &["search", "Original legacy", "--json"]);
    assert!(record_ids_from_json(&old_search).is_empty(), "{old_search}");
    let doctor = run_json_command(repo.path(), &["doctor", "--json"]);
    assert_check_status(&doctor, "repo_index", "ok");

    let repeated = run_json_command(
        repo.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    assert_eq!(repeated["already_resolved"], true);
    assert_json_string_field(&repeated, &["target_id"], "legacy-auth-guidance");
}

#[test]
fn proposal_files_tombstone_preserves_canonical_evidence_and_removes_target_from_recall() {
    let repo = initialized_temp_repo();
    write_canonical_record_fixture(
        repo.path(),
        "obsolete-client-auth",
        "repo",
        None,
        "active",
        "2026-07-01T00:00:00Z",
        "2026-07-01T00:00:00Z",
    );
    write_pending_proposal_file(
        repo.path(),
        "valid-proposal.md",
        proposal_markdown_with(
            "semantic",
            "tombstone",
            "supersedes: []",
            "  target: obsolete-client-auth\n",
        ),
    );

    let applied = run_json_command(
        repo.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    assert_json_string_field(&applied, &["action"], "tombstone");
    assert_json_string_field(&applied, &["record_id"], "obsolete-client-auth");
    assert_json_string_field(&applied, &["record_status"], "tombstoned");
    assert_json_string_field(&applied, &["target_id"], "obsolete-client-auth");

    let target_path = repo.path().join(".memzoi/records/obsolete-client-auth.md");
    let target = fs::read_to_string(&target_path).expect("read tombstoned target");
    assert!(target.contains("status: tombstoned\n"), "{target}");
    assert!(
        target.contains("source_ref: \"evidence://legacy-auth\"\n"),
        "{target}"
    );
    assert!(
        target.contains("Original legacy auth evidence remains reviewable."),
        "{target}"
    );
    let resolved = fs::read_to_string(
        repo.path()
            .join(".memzoi/proposals/resolved/applied/valid-proposal.md"),
    )
    .expect("read resolved tombstone packet");
    assert!(resolved.contains("action: tombstone\n"), "{resolved}");
    assert!(
        resolved.contains("target_id: obsolete-client-auth\n"),
        "{resolved}"
    );
    assert!(
        resolved.contains("resolution:\n  outcome: applied\n"),
        "{resolved}"
    );

    let search = run_json_command(repo.path(), &["search", "legacy auth evidence", "--json"]);
    assert!(record_ids_from_json(&search).is_empty(), "{search}");
    let doctor = run_json_command(repo.path(), &["doctor", "--json"]);
    assert_check_status(&doctor, "repo_index", "ok");

    let repeated = run_json_command(
        repo.path(),
        &["proposal-files", "apply", "mem_test_valid", "--json"],
    );
    assert_eq!(repeated["already_resolved"], true);
    assert_json_string_field(&repeated, &["record_id"], "obsolete-client-auth");
}

#[test]
fn proposal_files_validate_rejects_invalid_target_shapes_and_states_before_mutation() {
    for (case, target, proposal, expected) in [
        (
            "missing",
            None,
            proposal_markdown_with(
                "semantic",
                "supersede",
                "supersedes:\n  - missing-target",
                "",
            ),
            "target does not exist",
        ),
        (
            "inactive",
            Some(("repo", None, "tombstoned", "2026-07-01T00:00:00Z")),
            proposal_markdown_with(
                "semantic",
                "tombstone",
                "supersedes: []",
                "  target: lifecycle-target\n",
            ),
            "is inactive",
        ),
        (
            "cross-scope",
            Some((
                "project",
                Some("other-project"),
                "active",
                "2026-07-01T00:00:00Z",
            )),
            proposal_markdown_with(
                "semantic",
                "supersede",
                "supersedes:\n  - lifecycle-target",
                "",
            ),
            "cross-scope",
        ),
        (
            "stale",
            Some(("repo", None, "active", "2026-07-07T00:00:00Z")),
            proposal_markdown_with(
                "semantic",
                "tombstone",
                "supersedes: []",
                "  target: lifecycle-target\n",
            ),
            "is stale",
        ),
    ] {
        let repo = initialized_temp_repo();
        if let Some((scope, scope_id, status, updated)) = target {
            write_canonical_record_fixture(
                repo.path(),
                "lifecycle-target",
                scope,
                scope_id,
                status,
                "2026-07-01T00:00:00Z",
                updated,
            );
        }
        write_pending_proposal_file(repo.path(), "valid-proposal.md", proposal);
        let pending_path = repo
            .path()
            .join(".memzoi/proposals/pending/valid-proposal.md");
        let pending_before = fs::read(&pending_path).expect("read pending fixture");
        let target_before = target.map(|_| {
            fs::read(repo.path().join(".memzoi/records/lifecycle-target.md"))
                .expect("read target fixture")
        });

        let validated =
            run_json_command_failure(repo.path(), &["proposal-files", "validate", "--json"]);
        let errors = serde_json::to_string(&validated["errors"]).expect("serialize errors");
        assert!(errors.contains(expected), "{case}: {validated}");
        assert_eq!(
            fs::read(&pending_path).expect("read pending after validate"),
            pending_before
        );
        if let Some(target_before) = target_before {
            assert_eq!(
                fs::read(repo.path().join(".memzoi/records/lifecycle-target.md"))
                    .expect("read target after validate"),
                target_before,
                "{case} validation mutated canonical target"
            );
        }
    }

    let multiple = initialized_temp_repo();
    write_pending_proposal_file(
        multiple.path(),
        "valid-proposal.md",
        proposal_markdown_with(
            "semantic",
            "supersede",
            "supersedes:\n  - first-target\n  - second-target",
            "",
        ),
    );
    let invalid =
        run_json_command_failure(multiple.path(), &["proposal-files", "validate", "--json"]);
    assert!(
        serde_json::to_string(&invalid["errors"])
            .expect("serialize errors")
            .contains("exactly one supersedes target"),
        "{invalid}"
    );

    let missing_reason = initialized_temp_repo();
    write_pending_proposal_file(
        missing_reason.path(),
        "valid-proposal.md",
        proposal_markdown_with(
            "semantic",
            "supersede",
            "supersedes:\n  - lifecycle-target",
            "",
        )
        .replace(
            "  reason: Review packet context should not become canonical frontmatter.\n",
            "",
        ),
    );
    let invalid = run_json_command_failure(
        missing_reason.path(),
        &["proposal-files", "validate", "--json"],
    );
    assert!(
        serde_json::to_string(&invalid["errors"])
            .expect("serialize errors")
            .contains("must include proposal.reason"),
        "{invalid}"
    );
}

#[cfg(unix)]
#[test]
fn proposal_file_target_cannot_escape_records_root_through_symlinked_directory() {
    let repo = initialized_temp_repo();
    let outside = tempfile::tempdir().expect("outside records directory");
    let outside_record = outside.path().join("victim.md");
    fs::write(
        &outside_record,
        r#"---
type: decision
lane: semantic
title: Outside victim
description: Outside canonical-looking bytes must never be mutated.
timestamp: 2026-07-01T00:00:00Z
updated: 2026-07-01T00:00:00Z
status: active
scope: repo
visibility: repo
confidence: 1
source: human
source_ref: evidence://outside
---

# Outside victim

Outside canonical-looking bytes must never be mutated.
"#,
    )
    .expect("write outside record");
    let outside_before = fs::read(&outside_record).expect("read outside bytes");
    let records_root = test_paths(repo.path()).records_dir();
    std::os::unix::fs::symlink(outside.path(), records_root.join("linked"))
        .expect("create symlinked records directory");

    write_pending_proposal_file(
        repo.path(),
        "valid-proposal.md",
        proposal_markdown_with(
            "semantic",
            "tombstone",
            "supersedes: []",
            "  target: linked/victim\n",
        ),
    );
    let error =
        run_command_failure_stderr(repo.path(), &["proposal-files", "apply", "mem_test_valid"]);
    assert!(
        error.contains("target does not exist") || error.contains("unsafe"),
        "unexpected symlink target error: {error}"
    );
    assert_eq!(
        fs::read(&outside_record).expect("read outside bytes after apply"),
        outside_before,
        "file-backed lifecycle apply escaped the canonical records root"
    );
}

#[test]
fn failed_file_supersede_leaves_target_index_and_pending_packet_unchanged() {
    let repo = initialized_temp_repo();
    write_canonical_record_fixture(
        repo.path(),
        "atomic-target",
        "repo",
        None,
        "active",
        "2026-07-01T00:00:00Z",
        "2026-07-01T00:00:00Z",
    );
    write_pending_proposal_file(
        repo.path(),
        "valid-proposal.md",
        proposal_markdown_with(
            "semantic",
            "supersede",
            "supersedes:\n  - atomic-target",
            "",
        )
        .replace("title: Valid proposal", "title: Atomic replacement")
        .replace("# Valid proposal", "# Atomic replacement"),
    );
    let applied_root = repo.path().join(".memzoi/proposals/resolved/applied");
    fs::create_dir_all(applied_root.parent().expect("resolved parent"))
        .expect("create resolved parent");
    fs::write(&applied_root, "block resolved directory creation").expect("create resolved blocker");

    let target_path = repo.path().join(".memzoi/records/atomic-target.md");
    let pending_path = repo
        .path()
        .join(".memzoi/proposals/pending/valid-proposal.md");
    let target_before = fs::read(&target_path).expect("read target before failure");
    let pending_before = fs::read(&pending_path).expect("read pending before failure");
    let indexed_before: (String, i64) = Connection::open(test_paths(repo.path()).db_path)
        .expect("open index before failure")
        .query_row(
            "SELECT status, COUNT(*) FROM memory_record WHERE id = 'atomic-target'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read index before failure");

    let error =
        run_command_failure_stderr(repo.path(), &["proposal-files", "apply", "mem_test_valid"]);
    assert!(
        error.contains("failed to inspect resolved proposal packet"),
        "{error}"
    );
    assert_eq!(
        fs::read(&target_path).expect("read target after failure"),
        target_before
    );
    assert_eq!(
        fs::read(&pending_path).expect("read pending after failure"),
        pending_before
    );
    assert!(
        !repo
            .path()
            .join(".memzoi/records/atomic-replacement.md")
            .exists()
    );
    let indexed_after: (String, i64) = Connection::open(test_paths(repo.path()).db_path)
        .expect("open index after failure")
        .query_row(
            "SELECT status, COUNT(*) FROM memory_record WHERE id = 'atomic-target'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read index after failure");
    assert_eq!(indexed_after, indexed_before);
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
            "--sensitivity",
            "repo-safe",
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
fn expiry_command_shows_records_excluded_from_normal_search_and_explains_why() {
    let repo = initialized_temp_repo();
    let record_id = create_applied_memory(
        repo.path(),
        "warning",
        "repo",
        "Expired CLI diagnostic memory",
        "The expirydiagnostic token should be hidden from normal search.",
    );
    let conn = Connection::open(memory_db_path(repo.path())).expect("open runtime database");
    conn.execute(
        "UPDATE memory_record SET expires_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
        [record_id.as_str()],
    )
    .expect("set expired diagnostic fixture");
    drop(conn);

    let search = run_json_command(repo.path(), &["search", "expirydiagnostic", "--json"]);
    assert!(record_ids_from_json(&search).is_empty());

    let diagnostic = run_json_command(repo.path(), &["expiry", record_id.as_str(), "--json"]);
    assert_eq!(diagnostic["record"]["id"], record_id);
    assert_eq!(diagnostic["record"]["status"], "active");
    assert_eq!(diagnostic["expired"], true);
    assert_eq!(diagnostic["excluded_from_normal_reads"], true);
    assert!(
        diagnostic["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("at or after expiry")),
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
fn session_end_routes_structured_candidates_without_canonical_repo_writes() {
    let repo = initialized_temp_repo();
    let repo = repo.path();
    let input_path = repo.join("session-end.yml");
    fs::write(
        &input_path,
        r#"task: "Implement session-end routing"
candidates:
  - destination: repo
    type: decision
    lane: semantic
    title: Session-end repo zircon decision
    body: Repo session-end zircon durable decision should become a proposal.
    sensitivity: repo-safe
    reason: Learned while implementing tests.
    scope:
      kind: repo
      paths:
        - src/session/**
    tags:
      - session-end
      - zircon
  - destination: local
    type: preference
    lane: semantic
    title: Session-end local zircon preference
    body: Local session-end zircon preference stays private.
  - destination: session
    type: episode
    lane: session
    title: Session-end checkpoint zircon task
    body: Session-end zircon checkpoint stays runtime-only.
  - destination: discard
    type: fact
    lane: semantic
    title: Discard zircon note
    body: This candidate should not be written.
  - destination: needs_review
    type: warning
    lane: semantic
    title: Needs review zircon warning
    body: This candidate needs human review before writing.
"#,
    )
    .expect("write session-end input");

    let promoted = run_json_command(
        repo,
        &[
            "session-end",
            "--from-file",
            input_path.to_str().expect("session-end path utf-8"),
            "--json",
        ],
    );
    assert_json_string_field(&promoted, &["task"], "Implement session-end routing");
    assert_json_string_field(&promoted["source"], &["kind"], "file");
    let candidates = promoted["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("session-end JSON should include candidates: {promoted}"));
    assert_eq!(candidates.len(), 5, "unexpected candidates: {promoted}");

    assert_json_string_field(&candidates[0], &["destination"], "repo");
    assert_json_string_field(&candidates[0], &["status"], "written");
    assert_json_string_field(&candidates[0]["write"], &["kind"], "proposal_file");
    assert_json_string_field(
        &candidates[0]["write"],
        &["proposal_id"],
        "mem_session_session-end-repo-zircon-decision",
    );
    let proposal_path = repo.join(json_string(&candidates[0]["write"], "path"));
    assert!(proposal_path.is_file(), "missing proposal file: {promoted}");
    let rendered = fs::read_to_string(&proposal_path).expect("read session-end proposal");
    assert!(
        !rendered.contains("destination:"),
        "destination must stay routing metadata, not proposal frontmatter: {rendered}"
    );
    assert!(
        !test_paths(repo)
            .records_dir()
            .join("session-end-repo-zircon-decision.md")
            .exists(),
        "session-end repo candidates must not write canonical repo records"
    );
    let validated = run_json_command(repo, &["proposal-files", "validate", "--json"]);
    assert_eq!(
        validated.get("valid").and_then(Value::as_bool),
        Some(true),
        "generated session-end proposal should validate: {validated}"
    );
    let shown = run_json_command(
        repo,
        &[
            "proposal-files",
            "show",
            "mem_session_session-end-repo-zircon-decision",
            "--json",
        ],
    );
    assert_json_string_field(
        &shown["proposal"],
        &["reason"],
        "Learned while implementing tests.",
    );

    assert_json_string_field(&candidates[1], &["destination"], "local");
    assert_json_string_field(&candidates[1]["write"], &["kind"], "runtime_record");
    assert_json_string_field(
        &candidates[1]["write"],
        &["record_id"],
        "local-session-end-local-zircon-preference",
    );
    let local = run_json_command(repo, &["local", "list", "--json"]);
    assert_eq!(
        record_ids_from_json(&local),
        vec!["local-session-end-local-zircon-preference"],
        "local session-end candidate should create local runtime memory only: {local}"
    );

    assert_json_string_field(&candidates[2], &["destination"], "session");
    assert_json_string_field(
        &candidates[2]["write"],
        &["record_id"],
        "session-session-end-checkpoint-zircon-task",
    );
    let checkpoints = run_json_command(repo, &["checkpoint", "list", "--json"]);
    assert_eq!(
        record_ids_from_json(&checkpoints),
        vec!["session-session-end-checkpoint-zircon-task"],
        "session session-end candidate should create checkpoint runtime memory only: {checkpoints}"
    );

    assert_json_string_field(&candidates[3], &["destination"], "discard");
    assert_json_string_field(&candidates[3], &["status"], "skipped");
    assert!(
        candidates[3].get("write").is_some_and(Value::is_null),
        "discard candidate should not write: {promoted}"
    );
    assert_json_string_field(&candidates[4], &["destination"], "needs_review");
    assert_json_string_field(&candidates[4], &["status"], "blocked");
    assert!(
        candidates[4].get("write").is_some_and(Value::is_null),
        "needs_review candidate should not write: {promoted}"
    );

    let global_search = run_json_command(repo, &["search", "zircon", "--json"]);
    assert!(
        record_ids_from_json(&global_search).is_empty(),
        "global search should stay repo-only and not include runtime session-end writes: {global_search}"
    );
    let context = run_json_command(repo, &["context", "--task", "session-end zircon", "--json"]);
    assert_json_does_not_reference_records(
        &context,
        &[
            "local-session-end-local-zircon-preference".to_owned(),
            "session-session-end-checkpoint-zircon-task".to_owned(),
        ],
    );
    let export = run_json_command(repo, &["export", "okf", "--json"]);
    assert!(
        written_paths_from_json(&export).is_empty(),
        "session-end runtime memory should not be exported as repo memory: {export}"
    );
}

#[test]
fn session_end_validates_whole_batch_before_writing_anything() {
    let repo = initialized_temp_repo();
    let repo = repo.path();
    let input_path = repo.join("bad-session-end.yml");
    fs::write(
        &input_path,
        r#"task: "Validate first"
candidates:
  - destination: local
    type: preference
    lane: semantic
    title: Should not write local first
    body: This local candidate should not be written when the batch is invalid.
  - destination: repo
    type: decision
    lane: semantic
    title: OMITTED-SESSION-TITLE-SENTINEL
    body: OMITTED-SESSION-BODY-SENTINEL
    reason: OMITTED-SESSION-REASON-SENTINEL
    scope:
      kind: repo
      paths: [src/OMITTED-SESSION-PATH-SENTINEL/**]
"#,
    )
    .expect("write invalid session-end input");

    let result = run_json_command(
        repo,
        &[
            "session-end",
            "--from-file",
            input_path.to_str().expect("session-end path utf-8"),
            "--json",
        ],
    );
    let candidates = result["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("blocked session-end result needs candidates: {result}"));
    assert_eq!(candidates.len(), 2);
    assert_json_string_field(&candidates[0], &["status"], "blocked");
    assert_json_string_field(&candidates[0], &["sensitivity"], "unknown");
    assert_json_string_field(&candidates[1], &["status"], "blocked");
    assert_json_string_field(&candidates[1], &["sensitivity"], "unknown");
    assert_json_string_field(
        &candidates[1],
        &["title"],
        "Redacted non-repo-safe candidate",
    );
    assert!(
        candidates[1]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("got unknown")),
        "blocked outcome should explain the explicit unknown classification: {result}"
    );
    let rendered = serde_json::to_string(&result).expect("serialize blocked result");
    assert!(!rendered.contains("SENTINEL"), "{rendered}");
    let local = run_json_command(repo, &["local", "list", "--json"]);
    assert!(
        record_ids_from_json(&local).is_empty(),
        "blocked session-end batch must not create earlier local rows: {local}"
    );
    let pending = repo.join(".memzoi").join("proposals").join("pending");
    assert!(
        !pending.exists()
            || fs::read_dir(&pending)
                .expect("read pending dir")
                .next()
                .is_none(),
        "blocked session-end batch must not write proposal files"
    );
}

#[test]
fn session_end_blocks_every_non_repo_safe_repo_candidate() {
    for sensitivity in [
        "local-only",
        "sensitive",
        "secret",
        "raw-transcript",
        "private-personal-data",
        "temporary-state",
        "unknown",
    ] {
        let repo = initialized_temp_repo();
        let input_path = repo.path().join("blocked-session-end.yml");
        let sentinel = format!("SESSION-END-{sensitivity}-BODY-SENTINEL");
        fs::write(
            &input_path,
            format!(
                "task: Block unsafe session-end candidate\ncandidates:\n  - destination: repo\n    type: fact\n    lane: semantic\n    title: Blocked session-end candidate\n    body: {sentinel}\n    sensitivity: {sensitivity}\n"
            ),
        )
        .expect("write blocked session-end input");

        let result = run_json_command(
            repo.path(),
            &[
                "session-end",
                "--from-file",
                input_path.to_str().expect("session-end path utf-8"),
                "--json",
            ],
        );
        let candidate = &result["candidates"][0];
        assert_json_string_field(candidate, &["status"], "blocked");
        assert_json_string_field(candidate, &["sensitivity"], sensitivity);
        assert!(candidate["write"].is_null(), "{result}");
        assert!(
            !serde_json::to_string(&result)
                .expect("serialize blocked session-end result")
                .contains(&sentinel)
        );
        let pending = test_paths(repo.path()).proposals_dir().join("pending");
        assert!(
            !pending.exists()
                || fs::read_dir(pending)
                    .expect("read pending proposals")
                    .next()
                    .is_none()
        );
    }
}

#[test]
fn session_end_write_failure_does_not_leave_runtime_rows() {
    let repo = initialized_temp_repo();
    let repo = repo.path();
    let proposals_dir = repo.join(".memzoi").join("proposals");
    fs::create_dir_all(&proposals_dir).expect("create proposals dir");
    fs::write(proposals_dir.join("pending"), "not a directory")
        .expect("block pending proposal directory creation");
    let input_path = repo.join("write-failure-session-end.yml");
    fs::write(
        &input_path,
        r#"task: "Write failure rollback"
candidates:
  - destination: local
    type: preference
    lane: semantic
    title: Runtime row must not survive write failure
    body: This local row should not survive if the repo proposal cannot be written.
  - destination: repo
    type: decision
    lane: semantic
    title: Proposal cannot be written
    body: This repo proposal cannot be written because pending is a file.
    sensitivity: repo-safe
"#,
    )
    .expect("write session-end input");

    let stderr = run_command_failure_stderr(
        repo,
        &[
            "session-end",
            "--from-file",
            input_path.to_str().expect("session-end path utf-8"),
        ],
    );
    assert!(
        stderr.contains("failed to create proposal directory")
            || stderr.contains("failed to create session-end proposal")
            || stderr.contains("failed to inspect pending proposal"),
        "session-end should fail clearly on proposal file write errors: {stderr}"
    );
    let local = run_json_command(repo, &["local", "list", "--json"]);
    assert!(
        record_ids_from_json(&local).is_empty(),
        "failed session-end writes must not leave earlier local rows behind: {local}"
    );
}

#[test]
fn session_end_uses_deterministic_proposal_id_suffixes() {
    let repo = initialized_temp_repo();
    let repo = repo.path();
    let input_path = repo.join("duplicates-session-end.yml");
    fs::write(
        &input_path,
        r#"task: "Duplicate repo candidates"
candidates:
  - destination: repo
    type: decision
    lane: semantic
    title: Duplicate session-end zircon
    body: First duplicate body.
    sensitivity: repo-safe
  - destination: repo
    type: decision
    lane: semantic
    title: Duplicate session-end zircon
    body: Second duplicate body.
    sensitivity: repo-safe
"#,
    )
    .expect("write duplicate session-end input");

    let promoted = run_json_command(
        repo,
        &[
            "session-end",
            "--from-file",
            input_path.to_str().expect("session-end path utf-8"),
            "--json",
        ],
    );
    let candidates = promoted["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("session-end JSON should include candidates: {promoted}"));
    assert_json_string_field(
        &candidates[0]["write"],
        &["proposal_id"],
        "mem_session_duplicate-session-end-zircon",
    );
    assert_json_string_field(
        &candidates[1]["write"],
        &["proposal_id"],
        "mem_session_duplicate-session-end-zircon-2",
    );
    assert!(
        repo.join(".memzoi/proposals/pending/mem_session_duplicate-session-end-zircon.md")
            .is_file()
    );
    assert!(
        repo.join(".memzoi/proposals/pending/mem_session_duplicate-session-end-zircon-2.md")
            .is_file()
    );
}

#[test]
fn session_end_from_checkpoint_reads_only_explicit_checkpoint_body() {
    let repo = initialized_temp_repo();
    let repo = repo.path();
    let checkpoint_body = r#"task: "Checkpoint promotion"
candidates:
  - destination: repo
    type: decision
    lane: semantic
    title: Checkpoint session-end zircon decision
    body: Checkpoint body is the explicit structured source.
    sensitivity: repo-safe
"#;
    let checkpoint = run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Structured checkpoint",
            "--note",
            checkpoint_body,
            "--json",
        ],
    );
    let checkpoint_id = json_string(&checkpoint, "record_id").to_owned();

    let promoted = run_json_command(
        repo,
        &[
            "session-end",
            "--from-checkpoint",
            checkpoint_id.as_str(),
            "--json",
        ],
    );
    assert_json_string_field(&promoted["source"], &["kind"], "checkpoint");
    assert_json_string_field(&promoted["source"], &["record_id"], &checkpoint_id);
    let candidates = promoted["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("session-end JSON should include candidates: {promoted}"));
    assert_json_string_field(
        &candidates[0]["write"],
        &["proposal_id"],
        "mem_session_checkpoint-session-end-zircon-decision",
    );

    let prose_checkpoint = run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Prose checkpoint",
            "--note",
            "This is unstructured prose, not a session-end document.",
            "--json",
        ],
    );
    let prose_checkpoint_id = json_string(&prose_checkpoint, "record_id").to_owned();
    let stderr = run_command_failure_stderr(
        repo,
        &[
            "session-end",
            "--from-checkpoint",
            prose_checkpoint_id.as_str(),
        ],
    );
    assert!(
        stderr.contains("failed to parse session-end structured input"),
        "free-text checkpoints should not be extracted implicitly: {stderr}"
    );
}

#[test]
fn session_end_accepts_markdown_frontmatter_with_crlf_newlines() {
    let repo = initialized_temp_repo();
    let repo = repo.path();
    let input_path = repo.join("session-end-crlf.md");
    fs::write(
        &input_path,
        concat!(
            "---\r\n",
            "task: \"CRLF frontmatter\"\r\n",
            "candidates:\r\n",
            "  - destination: repo\r\n",
            "    type: decision\r\n",
            "    lane: semantic\r\n",
            "    title: CRLF session-end zircon decision\r\n",
            "    body: CRLF Markdown frontmatter should parse as structured input.\r\n",
            "    sensitivity: repo-safe\r\n",
            "---\r\n",
            "\r\n",
            "This Markdown body is not used for extraction.\r\n",
        ),
    )
    .expect("write CRLF session-end markdown input");

    let promoted = run_json_command(
        repo,
        &[
            "session-end",
            "--from-file",
            input_path.to_str().expect("CRLF input path utf-8"),
            "--json",
        ],
    );
    assert_json_string_field(&promoted, &["task"], "CRLF frontmatter");
    let candidates = promoted["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("session-end JSON should include candidates: {promoted}"));
    assert_json_string_field(
        &candidates[0]["write"],
        &["proposal_id"],
        "mem_session_crlf-session-end-zircon-decision",
    );
    let validated = run_json_command(repo, &["proposal-files", "validate", "--json"]);
    assert_eq!(
        validated.get("valid").and_then(Value::as_bool),
        Some(true),
        "CRLF session-end proposal should validate: {validated}"
    );
}

#[test]
fn session_end_rejects_invalid_structured_inputs() {
    let repo = initialized_temp_repo();
    let repo = repo.path();
    for (name, contents, expected) in [
        ("empty.yml", "", "session-end input cannot be empty"),
        (
            "prose.md",
            "This is just prose.",
            "failed to parse session-end structured input",
        ),
        (
            "missing-candidates.yml",
            "task: Missing candidates\n",
            "missing field `candidates`",
        ),
        (
            "empty-title.yml",
            r#"task: "Empty title"
candidates:
  - destination: repo
    type: decision
    lane: semantic
    title: " "
    body: Body exists.
    sensitivity: repo-safe
"#,
            "title is required",
        ),
        (
            "invalid-destination.yml",
            r#"task: "Invalid destination"
candidates:
  - destination: cloud
    type: decision
    lane: semantic
    title: Cloud candidate
    body: Body exists.
"#,
            "unknown variant `cloud`",
        ),
    ] {
        let path = repo.join(name);
        fs::write(&path, contents).expect("write invalid session-end input");
        let stderr = run_command_failure_stderr(
            repo,
            &[
                "session-end",
                "--from-file",
                path.to_str().expect("invalid input path utf-8"),
            ],
        );
        assert!(
            stderr.contains(expected),
            "expected {name} to fail with {expected:?}, got {stderr}"
        );
    }
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

    assert_eq!(pack["budget"]["requested"].as_u64(), Some(60));
    assert_eq!(pack["budget"]["effective"].as_u64(), Some(60));
    assert_eq!(
        pack["budget"]["estimate_unit"].as_str(),
        Some("approx_words")
    );
    assert!(
        pack["budget"]["estimated_used"]
            .as_u64()
            .is_some_and(|used| used > 0),
        "context JSON should expose estimated budget use: {pack}"
    );
    let included = pack["included"]
        .as_array()
        .unwrap_or_else(|| panic!("context JSON should expose included metadata: {pack}"));
    assert!(
        included.iter().any(|item| {
            item.get("record_id").and_then(Value::as_str) == Some(matching.as_str())
                && item.get("type").and_then(Value::as_str) == Some("procedure")
                && item.get("provenance").and_then(Value::as_str) == Some("git")
                && item.get("destination").and_then(Value::as_str) == Some("repo")
        }),
        "context JSON should expose included record provenance metadata: {pack}"
    );
    assert!(
        pack["next_queries"].as_array().is_some_and(Vec::is_empty),
        "context JSON should include an empty next_queries array for now: {pack}"
    );

    let citation = citation_for_record(&pack, &matching)
        .unwrap_or_else(|| panic!("context JSON should cite {matching}: {pack}"));
    assert_json_string_field(citation, &["record_id", "id"], &matching);
    assert_json_string_field(citation, &["type", "memory_type"], "procedure");
    assert_json_string_field(citation, &["scope", "scope_kind"], "repo");
    assert_json_string_field(citation, &["destination"], "repo");
    assert_json_string_field(citation, &["visibility"], "repo");
    assert_eq!(citation["source_kind"], Value::Null);
    assert_json_string_field(citation, &["source_ref"], "issue://cli-context#procedure");
    let first_record = pack["records"]
        .as_array()
        .and_then(|records| records.first())
        .unwrap_or_else(|| panic!("context JSON should include selected records: {pack}"));
    assert!(
        first_record.get("ranking").is_some(),
        "context JSON should expose ranking metadata per selected record: {pack}"
    );
    assert_eq!(
        pack["policy"]["requested_destinations"],
        serde_json::json!(["repo"])
    );
    assert_eq!(
        pack["budget"]["selected_records"].as_u64(),
        Some(ids.len() as u64)
    );
}

#[test]
fn context_include_local_and_session_flags_are_explicit_opt_ins() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let repo_record = create_applied_memory(
        repo,
        "decision",
        "repo",
        "Layered omega repo decision",
        "Layered omega context should include repo memory by default.",
    );
    let local = run_json_command(
        repo,
        &[
            "local",
            "add",
            "--type",
            "preference",
            "--title",
            "Layered omega local preference",
            "--body",
            "Layered omega context should include local memory only with explicit opt-in.",
            "--json",
        ],
    );
    let local_id = json_string(&local, "record_id").to_owned();
    let checkpoint = run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Layered omega session checkpoint",
            "--note",
            "Layered omega context should include session memory only with explicit opt-in.",
            "--json",
        ],
    );
    let checkpoint_id = json_string(&checkpoint, "record_id").to_owned();

    let default_pack = run_json_command(
        repo,
        &["context", "--task", "layered omega context", "--json"],
    );
    let default_ids = record_ids_from_json(&default_pack);
    assert_eq!(
        default_ids,
        vec![repo_record.as_str()],
        "context should be repo-only by default: {default_pack}"
    );
    assert_json_does_not_reference_records(
        &default_pack,
        &[local_id.clone(), checkpoint_id.clone()],
    );
    assert_eq!(
        default_pack["policy"]["requested_destinations"],
        serde_json::json!(["repo"])
    );

    let layered_pack = run_json_command(
        repo,
        &[
            "context",
            "--task",
            "layered omega context",
            "--include-local",
            "--include-session",
            "--json",
        ],
    );
    let layered_ids = record_ids_from_json(&layered_pack);
    assert!(layered_ids.contains(&repo_record.as_str()));
    assert!(layered_ids.contains(&local_id.as_str()));
    assert!(layered_ids.contains(&checkpoint_id.as_str()));
    assert_eq!(
        layered_pack["policy"]["requested_destinations"],
        serde_json::json!(["repo", "local", "session"])
    );
    let prompt = prompt_text(&layered_pack)
        .unwrap_or_else(|| panic!("context JSON should include prompt text: {layered_pack}"));
    assert!(
        prompt.contains("destination=local") && prompt.contains("destination=session"),
        "prompt should label non-repo memory provenance: {prompt:?}"
    );
    let local_citation = citation_for_record(&layered_pack, &local_id)
        .unwrap_or_else(|| panic!("layered context should cite local memory: {layered_pack}"));
    assert_json_string_field(local_citation, &["destination"], "local");
    assert_json_string_field(local_citation, &["visibility"], "private");
    assert_json_string_field(local_citation, &["source_kind"], "memzoi-local");
}

#[test]
fn context_json_excludes_runtime_memory_without_leaking_content_or_counts() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let repo_record = create_applied_memory(
        repo,
        "decision",
        "repo",
        "Runtime zircon repo decision",
        "Repo runtime zircon memory may appear in global context.",
    );
    run_json_command(
        repo,
        &[
            "local",
            "add",
            "--type",
            "fact",
            "--title",
            "Runtime zircon local private title",
            "--body",
            "Runtime zircon local private body must not leak.",
            "--json",
        ],
    );
    run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Runtime zircon session private title",
            "--note",
            "Runtime zircon session private body must not leak.",
            "--json",
        ],
    );

    let pack = run_json_command(
        repo,
        &[
            "context",
            "--task",
            "runtime zircon",
            "--token-budget",
            "120",
            "--json",
        ],
    );
    assert_eq!(
        record_ids_from_json(&pack),
        vec![repo_record.as_str()],
        "global context records should remain repo-only: {pack}"
    );
    let rendered = serde_json::to_string(&pack).expect("serialize context JSON");
    assert!(
        !rendered.contains("local private")
            && !rendered.contains("session private")
            && !rendered.contains("must not leak"),
        "context JSON should not leak local/session titles or bodies: {pack}"
    );

    let warnings = pack["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("context JSON should include warnings: {pack}"));
    assert!(
        warnings.is_empty(),
        "context JSON must not count or expose local/session memory unless explicitly opted in: {pack}"
    );
}

#[test]
fn handoff_json_wraps_context_and_reports_proposal_inbox() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let record = create_applied_memory(
        repo,
        "decision",
        "repo",
        "Handoff delta repo decision",
        "Handoff delta context should be wrapped under the handoff JSON context field.",
    );
    let proposal = run_json_command(
        repo,
        &[
            "propose",
            "--type",
            "fact",
            "--title",
            "Handoff delta pending proposal",
            "--body",
            "Handoff delta proposal inbox count should come from the DB inbox.",
            "--manual",
            "--json",
        ],
    );
    assert_eq!(proposal_status(&proposal), Some("pending"));

    let handoff = run_json_command(
        repo,
        &[
            "handoff",
            "--task",
            "handoff delta context",
            "--token-budget",
            "100",
            "--json",
        ],
    );

    assert_eq!(json_string(&handoff, "task"), "handoff delta context");
    assert_eq!(handoff["proposal_inbox"]["source"], "db");
    assert_eq!(handoff["proposal_inbox"]["open_total"].as_u64(), Some(1));
    assert_eq!(handoff["proposal_inbox"]["pending"].as_u64(), Some(1));
    assert_eq!(
        record_ids_from_json(&handoff["context"]),
        vec![record.as_str()],
        "handoff should wrap selected context records under context: {handoff}"
    );
    assert!(
        handoff["context"]["included"].as_array().is_some(),
        "handoff context should expose included metadata: {handoff}"
    );
    assert!(
        handoff["context"]["omitted"].as_array().is_some(),
        "handoff context should expose omitted metadata: {handoff}"
    );
    assert_eq!(
        handoff["context"]["policy"]["requested_destinations"],
        serde_json::json!(["repo"])
    );
}

#[test]
fn handoff_path_only_uses_stable_task_fallback() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let matching = create_applied_memory(
        repo,
        "warning",
        "repo",
        "Handoff path-only warning",
        "Path-only handoff should include this path-scoped memory.",
    );
    attach_memory_path(repo, &matching, "crates/memzoi-core/src/handoff.rs");

    let handoff = run_json_command(
        repo,
        &[
            "handoff",
            "--path",
            "crates/memzoi-core/src/handoff.rs",
            "--token-budget",
            "90",
            "--json",
        ],
    );

    assert_eq!(
        json_string(&handoff, "task"),
        "Handoff for path crates/memzoi-core/src/handoff.rs"
    );
    assert_eq!(
        handoff["context"]["task"].as_str(),
        Some("Handoff for path crates/memzoi-core/src/handoff.rs")
    );
    assert_eq!(
        record_ids_from_json(&handoff["context"]),
        vec![matching.as_str()],
        "path-only handoff should include path-scoped records: {handoff}"
    );
}

#[test]
fn handoff_runtime_memory_requires_explicit_opt_in() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let repo_record = create_applied_memory(
        repo,
        "decision",
        "repo",
        "Layered handoff repo decision",
        "Layered handoff should include repo memory by default.",
    );
    let local = run_json_command(
        repo,
        &[
            "local",
            "add",
            "--type",
            "preference",
            "--title",
            "Layered handoff local preference",
            "--body",
            "Layered handoff should include local memory only with explicit opt-in.",
            "--json",
        ],
    );
    let local_id = json_string(&local, "record_id").to_owned();
    let checkpoint = run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Layered handoff session checkpoint",
            "--note",
            "Layered handoff should include session memory only with explicit opt-in.",
            "--json",
        ],
    );
    let checkpoint_id = json_string(&checkpoint, "record_id").to_owned();

    let default_handoff =
        run_json_command(repo, &["handoff", "--task", "layered handoff", "--json"]);
    assert_eq!(
        record_ids_from_json(&default_handoff["context"]),
        vec![repo_record.as_str()],
        "handoff should be repo-only by default: {default_handoff}"
    );
    assert_json_does_not_reference_records(
        &default_handoff,
        &[local_id.clone(), checkpoint_id.clone()],
    );

    let layered_handoff = run_json_command(
        repo,
        &[
            "handoff",
            "--task",
            "layered handoff",
            "--include-local",
            "--include-session",
            "--json",
        ],
    );
    let layered_ids = record_ids_from_json(&layered_handoff["context"]);
    assert!(layered_ids.contains(&repo_record.as_str()));
    assert!(layered_ids.contains(&local_id.as_str()));
    assert!(layered_ids.contains(&checkpoint_id.as_str()));
    assert_eq!(
        layered_handoff["context"]["policy"]["requested_destinations"],
        serde_json::json!(["repo", "local", "session"])
    );
}

#[test]
fn handoff_text_labels_proposal_inbox_and_stays_repo_only_by_default() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    create_applied_memory(
        repo,
        "procedure",
        "repo",
        "Text handoff repo procedure",
        "Text handoff should render this repo memory.",
    );
    run_json_command(
        repo,
        &[
            "local",
            "add",
            "--type",
            "fact",
            "--title",
            "Text handoff local private title",
            "--body",
            "Text handoff local private body must not leak.",
            "--json",
        ],
    );

    let stdout = run_command_stdout(repo, &["handoff", "--task", "text handoff"]);
    assert!(stdout.contains("# Memzoi Handoff"), "{stdout}");
    assert!(
        stdout.contains("Proposal inbox: 0 open DB proposals"),
        "{stdout}"
    );
    assert!(stdout.contains("Text handoff repo procedure"), "{stdout}");
    assert!(
        !stdout.contains("Text handoff local private"),
        "default text handoff should not leak local memory: {stdout}"
    );
}

#[test]
fn handoff_requires_task_or_path_at_cli_boundary() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let stderr = run_command_failure_stderr(repo, &["handoff"]);
    assert!(
        stderr.contains("handoff requires --task or --path"),
        "handoff should explain missing required task/path input: {stderr}"
    );
}

#[test]
fn precheck_json_warns_for_path_only_governance_and_cites_memory() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let risk = create_applied_memory(
        repo,
        "risk",
        "repo",
        "Preserve settlement invariants",
        "Changing the rounding order previously broke tax calculation.",
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
        warning
            .to_string()
            .contains("Preserve settlement invariants")
            || warning.to_string().contains("rounding order"),
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
            "--sensitivity",
            "repo-safe",
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
            "--sensitivity",
            "repo-safe",
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
fn events_export_jsonl_emits_compact_standalone_event_objects() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    create_applied_memory(
        repo,
        "decision",
        "repo",
        "JSONL event export decision",
        "JSONL event export must emit one compact event object per line.",
    );

    let stdout = run_command_stdout(repo, &["events", "export", "--jsonl"]);

    assert!(
        !stdout.is_empty(),
        "JSONL export stdout should not be empty"
    );
    assert!(
        stdout.ends_with('\n'),
        "JSONL export should terminate the final row with a newline: {stdout:?}"
    );
    assert!(
        !stdout.contains("\n  \""),
        "JSONL export should be compact, not pretty/multiline JSON: {stdout}"
    );

    let lines = stdout.lines().collect::<Vec<_>>();
    assert!(
        !lines.is_empty(),
        "JSONL export should emit at least one row"
    );

    let mut saw_memory_proposed = false;
    for (index, line) in lines.iter().copied().enumerate() {
        assert_eq!(
            line,
            line.trim(),
            "JSONL row {} should not have leading/trailing whitespace: {line:?}",
            index + 1
        );
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "JSONL row {} should be a standalone object line: {line}",
            index + 1
        );

        let value = serde_json::from_str::<Value>(line).unwrap_or_else(|error| {
            panic!("JSONL row {} should parse: {error}: {line}", index + 1)
        });
        let object = value
            .as_object()
            .unwrap_or_else(|| panic!("JSONL row {} should parse to an object: {line}", index + 1));

        for field in ["id", "event_type", "actor", "created_at"] {
            assert!(
                object.get(field).and_then(Value::as_str).is_some(),
                "JSONL row {} should include string field {field:?}: {line}",
                index + 1
            );
        }
        assert!(
            object.contains_key("payload"),
            "JSONL row {} should include a payload field: {line}",
            index + 1
        );

        saw_memory_proposed |=
            object.get("event_type").and_then(Value::as_str) == Some("memory.proposed");
    }

    assert!(
        saw_memory_proposed,
        "JSONL export should include the memory.proposed event: {stdout}"
    );
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

fn run_command_stdout(repo: &Path, args: &[&str]) -> String {
    let mut cmd = memzoi();
    let assert = cmd.args(args).current_dir(repo).assert().success();
    std::str::from_utf8(&assert.get_output().stdout)
        .expect("stdout is utf-8")
        .to_owned()
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

fn write_canonical_record_fixture(
    repo: &Path,
    id: &str,
    scope: &str,
    scope_id: Option<&str>,
    status: &str,
    created: &str,
    updated: &str,
) {
    let records = test_paths(repo).records_dir();
    fs::create_dir_all(&records).expect("create records fixture directory");
    let scope_id = scope_id
        .map(|scope_id| format!("scope_id: {scope_id}\n"))
        .unwrap_or_default();
    fs::write(
        records.join(format!("{id}.md")),
        format!(
            r#"---
type: decision
lane: semantic
title: Legacy auth evidence
description: Original legacy auth evidence remains reviewable.
timestamp: {created}
updated: {updated}
status: {status}
scope: {scope}
{scope_id}visibility: repo
confidence: 1
source: reviewed-source
source_ref: evidence://legacy-auth
tags:
  - legacy-evidence
applies_to:
  - src/auth/**
---

# Legacy auth evidence

Original legacy auth evidence remains reviewable.
"#
        ),
    )
    .expect("write canonical record fixture");
    run_json_command(repo, &["rebuild", "--json"]);
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
            "--sensitivity",
            "repo-safe",
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
