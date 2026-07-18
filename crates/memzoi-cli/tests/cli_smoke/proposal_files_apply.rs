use super::*;

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
    use std::process::Command as StdCommand;

    let repo = tempfile::tempdir().expect("temp repo");
    for args in [
        &["init", "--quiet"][..],
        &["config", "user.email", "test@example.com"][..],
        &["config", "user.name", "Memzoi Test"][..],
    ] {
        assert!(
            StdCommand::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .expect("prepare rejection range repository")
                .success()
        );
    }
    let mut init = memzoi();
    init.args(["init", "--json"])
        .current_dir(repo.path())
        .assert()
        .success();
    write_pending_proposal_file(repo.path(), "valid-proposal.md", valid_proposal_markdown());
    assert!(
        StdCommand::new("git")
            .args(["add", "-A"])
            .current_dir(repo.path())
            .status()
            .expect("stage pending proposal")
            .success()
    );
    assert!(
        StdCommand::new("git")
            .args(["commit", "--quiet", "-m", "pending proposal"])
            .current_dir(repo.path())
            .status()
            .expect("commit pending proposal")
            .success()
    );

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
    assert!(
        StdCommand::new("git")
            .args(["add", "-A"])
            .current_dir(repo.path())
            .status()
            .expect("stage rejected receipt")
            .success()
    );
    assert!(
        StdCommand::new("git")
            .args(["commit", "--quiet", "-m", "rejected receipt"])
            .current_dir(repo.path())
            .status()
            .expect("commit rejected receipt")
            .success()
    );
    let mut scan = memzoi();
    scan.current_dir(repo.path())
        .args(["safety", "scan", "--range", "HEAD^...HEAD", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"allowed\": true"));

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
fn redacted_prohibited_class_rejection_receipt_passes_range_scan() {
    use std::process::Command as StdCommand;

    let repo = tempfile::tempdir().expect("temp repo");
    for args in [
        &["init", "--quiet"][..],
        &["config", "user.email", "test@example.com"][..],
        &["config", "user.name", "Memzoi Test"][..],
    ] {
        assert!(
            StdCommand::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .expect("prepare redacted receipt repository")
                .success()
        );
    }
    let mut init = memzoi();
    init.args(["init", "--json"])
        .current_dir(repo.path())
        .assert()
        .success();
    let prohibited = proposal_markdown_with_options(
        "semantic",
        "create",
        "proposed",
        "supersedes: []",
        "",
        "repo-safe",
    )
    .replace(
        "content_class: general_repo_knowledge",
        "content_class: raw_transcript",
    );
    write_pending_proposal_file(repo.path(), "prohibited-proposal.md", prohibited);
    for args in [
        &["add", "-A"][..],
        &["commit", "--quiet", "-m", "prohibited pending proposal"][..],
    ] {
        assert!(
            StdCommand::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .expect("commit prohibited pending proposal")
                .success()
        );
    }

    let apply_error =
        run_command_failure_stderr(repo.path(), &["proposal-files", "apply", "mem_test_valid"]);
    assert!(
        apply_error.contains("repository write blocked") && apply_error.contains("raw_transcript"),
        "prohibited source classification was lost behind its safe receipt: {apply_error}"
    );

    run_json_command(
        repo.path(),
        &[
            "proposal-files",
            "reject",
            "mem_test_valid",
            "--reason",
            "Rejected at the repository trust boundary.",
            "--json",
        ],
    );
    for args in [
        &["add", "-A"][..],
        &["commit", "--quiet", "-m", "redacted rejection receipt"][..],
    ] {
        assert!(
            StdCommand::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .expect("commit redacted rejection receipt")
                .success()
        );
    }

    let mut scan = memzoi();
    scan.current_dir(repo.path())
        .args(["safety", "scan", "--range", "HEAD^...HEAD", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"allowed\": true"));
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
fn legacy_file_proposal_shapes_remain_showable_and_rejectable() {
    let repo = initialized_temp_repo();
    let legacy = proposal_markdown_with_options(
        "semantic",
        "supersede",
        "proposed",
        "supersedes:\n  - legacy-target-one\n  - legacy-target-two",
        "",
        "repo-safe",
    )
    .replace(
        "  reason: Review packet context should not become canonical frontmatter.\n",
        "",
    );
    write_pending_proposal_file(repo.path(), "legacy-shape.md", legacy);

    let shown = run_json_command(
        repo.path(),
        &["proposal-files", "show", "mem_test_valid", "--json"],
    );
    assert_json_string_field(&shown, &["action"], "supersede");
    assert_eq!(
        shown["supersedes"].as_array().map(Vec::len),
        Some(2),
        "{shown}"
    );

    let rejected = run_json_command(
        repo.path(),
        &[
            "proposal-files",
            "reject",
            "mem_test_valid",
            "--reason",
            "Legacy packet is reviewable but not applyable.",
            "--json",
        ],
    );
    assert_json_string_field(&rejected, &["outcome"], "rejected");
}

#[test]
fn legacy_file_proposal_without_content_class_cannot_apply() {
    let repo = initialized_temp_repo();
    let legacy = proposal_markdown_with_options(
        "semantic",
        "create",
        "proposed",
        "supersedes: []",
        "",
        "repo-safe",
    )
    .replace("content_class: general_repo_knowledge\n", "");
    write_pending_proposal_file(repo.path(), "legacy-unclassified.md", legacy);

    let error =
        run_command_failure_stderr(repo.path(), &["proposal-files", "apply", "mem_test_valid"]);
    assert!(
        error.contains("unknown_content_class") || error.contains("repository write blocked"),
        "unexpected unclassified proposal error: {error}"
    );
    assert!(
        test_paths(repo.path()).records_dir().read_dir().is_err()
            || test_paths(repo.path())
                .records_dir()
                .read_dir()
                .expect("read records directory")
                .next()
                .is_none()
    );
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
