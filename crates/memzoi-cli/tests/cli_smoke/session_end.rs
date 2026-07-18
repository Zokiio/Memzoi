use super::*;

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
    content_class: general_repo_knowledge
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
        r#"task: "BLOCKED-SESSION-END-TASK-SENTINEL"
candidates:
  - destination: local
    type: preference
    lane: semantic
    title: BLOCKED-LOCAL-TITLE-SENTINEL
    body: BLOCKED-LOCAL-BODY-SENTINEL
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
fn session_end_task_credentials_are_blocked_and_redacted_in_cli_json() {
    let repo = initialized_temp_repo();
    let input_path = repo.path().join("credential-task-session-end.yml");
    let task_sentinel = "ghp_SESSION_END_TASK_SENTINEL_0123456789abcdefghijklmnop";
    let input = format!(
        "task: {task_sentinel}\ncandidates:\n  - destination: repo\n    type: fact\n    lane: semantic\n    title: Harmless repository candidate\n    body: General repository knowledge.\n    sensitivity: repo-safe\n    content_class: general_repo_knowledge\n"
    );
    let mut input_file = fs::File::options()
        .write(true)
        .create_new(true)
        .open(&input_path)
        .expect("create credential task session-end input");
    let mut input_bytes = input.as_bytes();
    std::io::copy(&mut input_bytes, &mut input_file)
        .expect("write credential task session-end input");
    drop(input_file);

    let result = run_json_command(
        repo.path(),
        &[
            "session-end",
            "--from-file",
            input_path.to_str().expect("session-end path utf-8"),
            "--json",
        ],
    );
    assert_json_string_field(&result, &["task"], "Redacted blocked session-end task");
    assert_json_string_field(&result["candidates"][0], &["status"], "blocked");
    let rendered = serde_json::to_string(&result).expect("serialize blocked session-end result");
    assert!(!rendered.contains(task_sentinel), "{rendered}");
}

#[test]
fn session_end_omitted_content_class_fails_closed() {
    let repo = initialized_temp_repo();
    let input_path = repo.path().join("unclassified-session-end.yml");
    fs::write(
        &input_path,
        "task: Unclassified session end\ncandidates:\n  - destination: repo\n    type: fact\n    lane: semantic\n    title: Unclassified candidate\n    body: Lexically harmless repository knowledge.\n    sensitivity: repo-safe\n",
    )
    .expect("write unclassified session-end input");

    let result = run_json_command(
        repo.path(),
        &[
            "session-end",
            "--from-file",
            input_path.to_str().expect("session-end path utf-8"),
            "--json",
        ],
    );
    assert_json_string_field(&result["candidates"][0], &["status"], "blocked");
    assert!(result["candidates"][0]["write"].is_null(), "{result}");
    let pending = test_paths(repo.path()).proposals_dir().join("pending");
    assert!(
        !pending.exists()
            || fs::read_dir(pending)
                .expect("read pending proposals")
                .next()
                .is_none()
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
    content_class: general_repo_knowledge
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
            || stderr.contains("failed to inspect pending proposal")
            || stderr.contains("pending proposal root ancestor must be a real directory"),
        "session-end should fail clearly on proposal file write errors: {stderr}"
    );
    assert!(
        !stderr.contains("created session-end proposal batch does not match"),
        "a create failure must not run rollback for an empty created set: {stderr}"
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
    content_class: general_repo_knowledge
  - destination: repo
    type: decision
    lane: semantic
    title: Duplicate session-end zircon
    body: Second duplicate body.
    sensitivity: repo-safe
    content_class: general_repo_knowledge
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
    content_class: general_repo_knowledge
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
            "--operation-id",
            "session-end-source-checkpoint",
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
            "--operation-id",
            "promote-source-checkpoint",
            "--expected-version",
            json_string(&checkpoint, "record_version"),
            "--json",
        ],
    );
    assert_json_string_field(&promoted["source"], &["kind"], "checkpoint");
    assert_json_string_field(&promoted["source"], &["record_id"], &checkpoint_id);
    assert_json_string_field(
        &promoted["checkpoint_closure"],
        &["checkpoint_id"],
        &checkpoint_id,
    );
    assert_eq!(promoted["checkpoint_closure"]["applied"], true);
    let candidates = promoted["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("session-end JSON should include candidates: {promoted}"));
    assert_json_string_field(
        &candidates[0]["write"],
        &["proposal_id"],
        "mem_session_checkpoint-session-end-zircon-decision",
    );
    let listed_after_promotion = run_json_command(repo, &["checkpoint", "list", "--json"]);
    assert!(
        !record_ids_from_json(&listed_after_promotion).contains(&checkpoint_id.as_str()),
        "successful session-end must atomically close its source checkpoint: {listed_after_promotion}"
    );

    let replayed = run_json_command(
        repo,
        &[
            "session-end",
            "--from-checkpoint",
            checkpoint_id.as_str(),
            "--operation-id",
            "promote-source-checkpoint",
            "--expected-version",
            json_string(&checkpoint, "record_version"),
            "--json",
        ],
    );
    assert_eq!(replayed["checkpoint_closure"]["replayed"], true);
    assert_json_string_field(
        &replayed["candidates"][0]["write"],
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
            "--operation-id",
            "prose-checkpoint",
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
            "    content_class: general_repo_knowledge\r\n",
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
