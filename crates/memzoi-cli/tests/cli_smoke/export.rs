use super::*;

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
