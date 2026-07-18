use super::*;

#[test]
fn checkpoint_lifecycle_commands_replay_before_versions_and_preserve_successor_lineage() {
    let repo = initialized_temp_repo();
    let repo = repo.path();

    let created = run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Lifecycle checkpoint",
            "--note",
            "Explicit checkpoint state",
            "--operation-id",
            "create-lifecycle-checkpoint",
            "--json",
        ],
    );
    let checkpoint_id = json_string(&created, "record_id").to_owned();
    let initial_version = json_string(&created, "record_version").to_owned();

    let continued = run_json_command(
        repo,
        &[
            "checkpoint",
            "continue",
            &checkpoint_id,
            "--operation-id",
            "continue-lifecycle-checkpoint",
            "--expected-version",
            &initial_version,
            "--json",
        ],
    );
    assert_eq!(continued["applied"], true);
    assert_eq!(continued["replayed"], false);
    let continued_version = json_string(&continued, "record_version").to_owned();
    assert_ne!(continued_version, initial_version);

    let replayed_continue = run_json_command(
        repo,
        &[
            "checkpoint",
            "continue",
            &checkpoint_id,
            "--operation-id",
            "continue-lifecycle-checkpoint",
            "--expected-version",
            &initial_version,
            "--json",
        ],
    );
    assert_eq!(replayed_continue["replayed"], true);
    assert_eq!(
        json_string(&replayed_continue, "record_version"),
        continued_version
    );

    let mismatch = run_command_failure_stderr(
        repo,
        &[
            "checkpoint",
            "continue",
            &checkpoint_id,
            "--operation-id",
            "continue-lifecycle-checkpoint",
            "--expected-version",
            &continued_version,
            "--json",
        ],
    );
    assert!(
        mismatch.contains("origin_reuse_mismatch"),
        "changed inputs must not reuse an operation identity: {mismatch}"
    );

    let closed = run_json_command(
        repo,
        &[
            "checkpoint",
            "close",
            &checkpoint_id,
            "--operation-id",
            "close-lifecycle-checkpoint",
            "--expected-version",
            &continued_version,
            "--json",
        ],
    );
    let closed_version = json_string(&closed, "record_version").to_owned();
    assert!(closed["retention"]["closed_at"].is_string());

    let replayed_close = run_json_command(
        repo,
        &[
            "checkpoint",
            "close",
            &checkpoint_id,
            "--operation-id",
            "close-lifecycle-checkpoint",
            "--expected-version",
            &continued_version,
            "--json",
        ],
    );
    assert_eq!(replayed_close["replayed"], true);
    assert_eq!(
        json_string(&replayed_close, "record_version"),
        closed_version
    );

    let successor = run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Lifecycle successor",
            "--note",
            "Continued work after terminal handoff",
            "--successor-of",
            &checkpoint_id,
            "--operation-id",
            "create-lifecycle-successor",
            "--expected-version",
            &closed_version,
            "--json",
        ],
    );
    assert_json_string_field(&successor["lineage"], &["kind"], "session_successor");
    assert_json_string_field(&successor["lineage"], &["predecessor_id"], &checkpoint_id);

    let reopen = run_command_failure_stderr(
        repo,
        &[
            "checkpoint",
            "continue",
            &checkpoint_id,
            "--operation-id",
            "attempt-reopen",
            "--expected-version",
            &closed_version,
            "--json",
        ],
    );
    assert!(
        reopen.contains("cannot be reopened"),
        "closed checkpoint must remain terminal: {reopen}"
    );
}

#[test]
fn machine_checkpoint_commands_require_caller_controlled_ids_and_versions() {
    let repo = initialized_temp_repo();
    let repo = repo.path();
    let missing_operation = run_command_failure_stderr(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Machine checkpoint",
            "--note",
            "Machine request",
            "--json",
        ],
    );
    assert!(missing_operation.contains("--operation-id is required with --json"));

    let created = run_json_command(
        repo,
        &[
            "checkpoint",
            "add",
            "--task",
            "Machine checkpoint",
            "--note",
            "Machine request",
            "--operation-id",
            "machine-create",
            "--json",
        ],
    );
    let missing_version = run_command_failure_stderr(
        repo,
        &[
            "checkpoint",
            "close",
            json_string(&created, "record_id"),
            "--operation-id",
            "machine-close",
            "--json",
        ],
    );
    assert!(missing_version.contains("--expected-version is required with --json"));
}
