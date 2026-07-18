use super::*;

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
        Some(1),
        "{validated}"
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
        proposal_markdown_with("mystery", "create", "supersedes: []", "")
            .replace("id: mem_test_valid", "id: mem_test_invalid_lane"),
    );
    write_pending_proposal_file(
        repo.path(),
        "invalid-action.md",
        proposal_markdown_with("semantic", "update", "supersedes: []", "")
            .replace("id: mem_test_valid", "id: mem_test_invalid_action"),
    );
    write_pending_proposal_file(
        repo.path(),
        "missing-supersedes.md",
        proposal_markdown_with("semantic", "supersede", "supersedes: []", "")
            .replace("id: mem_test_valid", "id: mem_test_missing_supersedes"),
    );
    write_pending_proposal_file(
        repo.path(),
        "missing-target.md",
        proposal_markdown_with("semantic", "tombstone", "supersedes: []", "")
            .replace("id: mem_test_valid", "id: mem_test_missing_target"),
    );

    let validated =
        run_json_command_failure(repo.path(), &["proposal-files", "validate", "--json"]);
    assert_eq!(validated.get("valid").and_then(Value::as_bool), Some(false));
    assert_eq!(
        validated.get("valid_count").and_then(Value::as_u64),
        Some(1),
        "mixed validation result: {validated}"
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
    assert_eq!(proposals_from_json(&listed).len(), 3);
    assert_eq!(
        listed.get("errors").and_then(Value::as_array).map(Vec::len),
        Some(2)
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

#[cfg(unix)]
#[test]
fn proposal_file_commands_refuse_symlinked_inventory_root_without_reading_outside() {
    let repo = initialized_temp_repo();
    let outside = tempfile::tempdir().expect("outside proposal root");
    let sentinel = "OUTSIDE-PROPOSAL-SENTINEL";
    fs::write(
        outside.path().join("outside.md"),
        valid_proposal_markdown()
            .replace("title: Valid proposal", &format!("title: {sentinel}"))
            .replace("# Valid proposal", &format!("# {sentinel}")),
    )
    .expect("write outside proposal");
    let proposals = repo.path().join(".memzoi/proposals");
    fs::create_dir_all(&proposals).expect("create proposal root");
    std::os::unix::fs::symlink(outside.path(), proposals.join("pending"))
        .expect("symlink pending root");

    let listed = run_json_command_failure(repo.path(), &["proposal-files", "list", "--json"]);
    let rendered = serde_json::to_string(&listed).expect("serialize unsafe-root result");
    assert!(
        !rendered.contains(sentinel),
        "outside packet leaked: {rendered}"
    );
    assert_eq!(listed["proposals"].as_array().map(Vec::len), Some(0));
    assert!(
        rendered.contains("ancestor must be a real directory"),
        "unsafe-root result should be actionable: {rendered}"
    );

    let shown =
        run_command_failure_stderr(repo.path(), &["proposal-files", "show", "mem_test_valid"]);
    assert!(
        !shown.contains(sentinel),
        "show leaked outside packet: {shown}"
    );
}
