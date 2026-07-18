use super::*;

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
        r#"schema: memzoi/import
origin_key: test-import:integrate-human
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
    content_class: general_repo_knowledge
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
