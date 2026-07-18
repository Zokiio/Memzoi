use super::*;

#[test]
fn repo_safe_secret_packet_is_hash_only_in_inventory_and_rejection_receipt() {
    let repo = initialized_temp_repo();
    let sentinel = "ghp_REPO_SAFE_SECRET_SENTINEL_0123456789abcdefghijklmnop";
    let markdown = valid_proposal_markdown().replace("This proposal body is valid.", sentinel);
    write_pending_proposal_file(repo.path(), "valid-proposal.md", markdown);

    let listed = run_json_command(repo.path(), &["proposal-files", "list", "--json"]);
    let rendered = serde_json::to_string(&listed).expect("serialize redacted inventory");
    assert!(!rendered.contains(sentinel), "{rendered}");
    let proposal = listed["proposals"]
        .as_array()
        .and_then(|proposals| proposals.first())
        .unwrap_or_else(|| panic!("redacted pending proposal missing: {listed}"));
    assert_json_string_field(proposal, &["title"], "Redacted non-repo-safe proposal");
    let redacted_id = proposal_id_from_value(proposal).to_owned();

    let rejected = run_json_command(
        repo.path(),
        &[
            "proposal-files",
            "reject",
            &redacted_id,
            "--reason",
            "unsafe content removed",
            "--json",
        ],
    );
    let rendered = serde_json::to_string(&rejected).expect("serialize redacted rejection");
    assert!(!rendered.contains(sentinel), "{rendered}");
    assert!(
        !repo
            .path()
            .join(".memzoi/proposals/pending/valid-proposal.md")
            .exists()
    );
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
            "not a current assertion",
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
content_class: general_repo_knowledge
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
        proposal_markdown_with("mystery", "create", "supersedes: []", "")
            .replace("id: mem_test_valid", "id: mem_test_invalid_lane"),
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
