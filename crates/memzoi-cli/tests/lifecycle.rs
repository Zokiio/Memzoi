use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use assert_cmd::Command;
use memzoi_core::{
    MemoryPaths, PRIVATE_LIFECYCLE_GRANT_SCHEMA, PRIVATE_LIFECYCLE_MAX_ARTIFACT_BYTES,
    PRIVATE_LIFECYCLE_REQUEST_SCHEMA, PRIVATE_LIFECYCLE_RESULT_SCHEMA, PrivateLifecycleAction,
    PrivateLifecycleRequest, PrivateLifecycleSource,
};
use rusqlite::{Connection, types::Value as SqlValue};
use serde_json::Value;

struct LifecycleFixture {
    repo: tempfile::TempDir,
    home: tempfile::TempDir,
    _artifacts: tempfile::TempDir,
    artifacts_root: PathBuf,
}

impl LifecycleFixture {
    fn new() -> Self {
        let repo = tempfile::tempdir().expect("temporary Git repository");
        let home = tempfile::tempdir().expect("temporary Memzoi home");
        let artifacts = tempfile::tempdir().expect("temporary authority artifact directory");
        let artifacts_root = fs::canonicalize(artifacts.path())
            .expect("canonical temporary authority artifact directory");
        run_git(repo.path(), &["init", "-q"]);

        let fixture = Self {
            repo,
            home,
            _artifacts: artifacts,
            artifacts_root,
        };
        fixture.run_json(&["init", "--json"]);
        fixture
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("memzoi").expect("memzoi binary");
        command.env("MEMZOI_HOME", self.home.path());
        command.current_dir(self.repo.path());
        command
    }

    fn run_json(&self, args: &[&str]) -> Value {
        let mut command = self.command();
        let assert = command.args(args).assert().success();
        serde_json::from_slice(&assert.get_output().stdout).unwrap_or_else(|error| {
            panic!(
                "command output should be JSON: {error}; stdout was {:?}",
                String::from_utf8_lossy(&assert.get_output().stdout)
            )
        })
    }

    fn failure_stderr(&self, args: &[&str]) -> String {
        let mut command = self.command();
        let assert = command.args(args).assert().failure();
        String::from_utf8(assert.get_output().stderr.clone()).expect("stderr is UTF-8")
    }

    fn success_stdout(&self, args: &[&str]) -> String {
        let mut command = self.command();
        let assert = command.args(args).assert().success();
        String::from_utf8(assert.get_output().stdout.clone()).expect("stdout is UTF-8")
    }

    fn add_private_record(&self, title: &str, body: &str) -> String {
        let result = self.run_json(&[
            "local",
            "add",
            "--type",
            "preference",
            "--title",
            title,
            "--body",
            body,
            "--actor",
            "owner:lifecycle-cli-test",
            "--json",
        ]);
        json_string(&result, "record_id").to_owned()
    }

    fn inspect_record(&self, record_id: &str) -> Value {
        self.run_json(&["lifecycle", "inspect", "record", record_id, "--json"])
    }

    fn inspect_grant(&self, grant_id: &str) -> Value {
        self.run_json(&["lifecycle", "inspect", "grant", grant_id, "--json"])
    }

    fn paths(&self) -> MemoryPaths {
        MemoryPaths::with_runtime_home(
            self.repo.path().to_path_buf(),
            self.home.path().to_path_buf(),
        )
    }

    fn write_request(
        &self,
        stem: &str,
        operation_id: &str,
        action: PrivateLifecycleAction,
    ) -> (PrivateLifecycleRequest, PathBuf) {
        let request = PrivateLifecycleRequest::with_computed_id(
            operation_id,
            PrivateLifecycleSource::Direct,
            vec![action],
        )
        .expect("valid private lifecycle request fixture");
        let path = self.artifacts_root.join(format!("{stem}.json"));
        let mut bytes = serde_json::to_vec_pretty(&request).expect("serialize lifecycle request");
        bytes.push(b'\n');
        fs::write(&path, bytes).expect("write lifecycle request fixture");
        (request, path)
    }

    fn authorize(&self, request_path: &Path) -> Value {
        self.run_json(&[
            "lifecycle",
            "authorize",
            "--request-file",
            path_text(request_path),
            "--json",
        ])
    }

    fn apply(&self, request_path: &Path, grant_id: &str) -> Value {
        self.run_json(&[
            "lifecycle",
            "apply",
            "--request-file",
            path_text(request_path),
            "--grant-id",
            grant_id,
            "--json",
        ])
    }
}

#[test]
fn lifecycle_help_exposes_the_exact_owner_authority_flow() {
    let fixture = LifecycleFixture::new();

    let mut lifecycle = fixture.command();
    let lifecycle = lifecycle.args(["lifecycle", "--help"]).assert().success();
    let lifecycle = String::from_utf8_lossy(&lifecycle.get_output().stdout);
    for command in [
        "maintenance",
        "plan",
        "authorize",
        "revoke",
        "inspect",
        "apply",
    ] {
        assert!(
            lifecycle.contains(command),
            "lifecycle help should expose {command}: {lifecycle}"
        );
    }

    let maintenance = fixture.success_stdout(&["lifecycle", "maintenance", "--help"]);
    for command in ["enable", "disable", "inspect", "reconcile"] {
        assert!(
            maintenance.contains(command),
            "lifecycle maintenance help should expose {command}: {maintenance}"
        );
    }

    let mut plan = fixture.command();
    let plan = plan
        .args(["lifecycle", "plan", "--help"])
        .assert()
        .success();
    let plan = String::from_utf8_lossy(&plan.get_output().stdout);
    assert!(
        plan.contains("Unix-only"),
        "plan help must disclose the private-plan output platform boundary: {plan}"
    );

    let mut authorize = fixture.command();
    let authorize = authorize
        .args(["lifecycle", "authorize", "--help"])
        .assert()
        .success();
    let authorize = String::from_utf8_lossy(&authorize.get_output().stdout);
    for option in ["--request-file", "--plan-file", "--expires-at", "--json"] {
        assert!(
            authorize.contains(option),
            "authorize help should expose {option}: {authorize}"
        );
    }
    assert!(
        !authorize.contains("--authorized-at"),
        "the public CLI must not accept a caller-controlled authorization time: {authorize}"
    );
    assert!(
        authorize.contains("secure regular-file reads currently require Unix"),
        "authorize help must disclose its artifact platform boundary: {authorize}"
    );

    let mut apply = fixture.command();
    let apply = apply
        .args(["lifecycle", "apply", "--help"])
        .assert()
        .success();
    let apply = String::from_utf8_lossy(&apply.get_output().stdout);
    for option in ["--request-file", "--grant-id", "--plan-file", "--json"] {
        assert!(
            apply.contains(option),
            "apply help should expose {option}: {apply}"
        );
    }
    assert!(
        apply.contains("secure regular-file reads currently require Unix"),
        "apply help must disclose its artifact platform boundary: {apply}"
    );
}

#[test]
fn standing_private_maintenance_cli_suppresses_and_atomically_disables() {
    let fixture = LifecycleFixture::new();
    let first = fixture.add_private_record("Private conflict", "Authentication is required.");
    fixture.add_private_record("Private conflict", "Authentication is not required.");

    let enabled = fixture.run_json(&["lifecycle", "maintenance", "enable", "--json"]);
    assert_eq!(enabled["outcome"], "enabled");
    assert_eq!(enabled["projection"]["state"], "current");
    assert_eq!(enabled["projection"]["member_count"], 2);
    assert_eq!(enabled["projection"]["edge_count"], 1);

    let record = fixture.inspect_record(&first);
    assert_eq!(record["base_eligibility"]["is_current"], true);
    assert_eq!(
        record["effective_automatic_recall_eligibility"]["is_current"],
        false
    );
    assert_eq!(record["conflicts"].as_array().map(Vec::len), Some(1));

    let reconciled = fixture.run_json(&["lifecycle", "maintenance", "reconcile", "--json"]);
    assert_eq!(reconciled["projection"]["state"], "current");
    assert_eq!(reconciled["projection"]["edge_count"], 1);

    let disabled = fixture.run_json(&["lifecycle", "maintenance", "disable", "--json"]);
    assert_eq!(disabled["projection"]["state"], "disabled");
    let record = fixture.inspect_record(&first);
    assert_eq!(
        record["effective_automatic_recall_eligibility"]["is_current"],
        true
    );
}

#[test]
fn private_plan_is_read_only_and_contains_no_private_title_or_body() {
    let fixture = LifecycleFixture::new();
    let title = "PRIVATE-LIFECYCLE-PLAN-TITLE-SENTINEL";
    let body = "PRIVATE-LIFECYCLE-PLAN-BODY-SENTINEL";
    let record_id = fixture.add_private_record(title, body);
    let before = fixture.inspect_record(&record_id);

    let first = fixture.run_json(&[
        "lifecycle",
        "plan",
        "--record-id",
        &record_id,
        "--evaluated-at",
        "2026-07-19T12:00:00Z",
        "--json",
    ]);
    let second = fixture.run_json(&[
        "lifecycle",
        "plan",
        "--record-id",
        &record_id,
        "--evaluated-at",
        "2026-07-19T12:00:00Z",
        "--json",
    ]);

    assert_eq!(
        first, second,
        "fixed-time private planning should be deterministic"
    );
    assert_eq!(first["schema"], "memzoi/maintenance-plan");
    assert!(first["policy"].get("contract_version").is_none());
    assert_eq!(first["scope"]["kind"], "private_runtime");
    let rendered = serde_json::to_string(&first).expect("serialize private plan assertion");
    for forbidden in [title, body, "\"title\"", "\"body\""] {
        assert!(
            !rendered.contains(forbidden),
            "private plan must not expose {forbidden:?}: {rendered}"
        );
    }
    assert_eq!(
        fixture.inspect_record(&record_id),
        before,
        "planning must not mutate private records or lifecycle state"
    );
}

#[test]
fn plan_and_inspect_leave_every_runtime_artifact_byte_for_byte_unchanged() {
    let fixture = LifecycleFixture::new();
    let record_id = fixture.add_private_record(
        "Read-only lifecycle boundary",
        "plan and inspect cannot write recovery, mirror, record, or authority state",
    );
    let version = json_string(&fixture.inspect_record(&record_id), "version").to_owned();
    let (_, request_path) = fixture.write_request(
        "readonly-inspection",
        "cli-readonly-inspection",
        PrivateLifecycleAction::Pin {
            record_id: record_id.clone(),
            expected_version: version,
        },
    );
    let grant = fixture.authorize(&request_path);
    let grant_id = json_string(&grant, "grant_id").to_owned();
    let recovery_sentinel = fixture
        .paths()
        .repository_runtime_dir
        .join("shared-sync.json");
    fs::write(&recovery_sentinel, b"DO-NOT-RECOVER-ON-READ\n")
        .expect("write read-only recovery sentinel");
    let before_recovery_rejection = snapshot_tree(fixture.home.path());
    let error = fixture.failure_stderr(&[
        "lifecycle",
        "plan",
        "--record-id",
        &record_id,
        "--evaluated-at",
        "2026-07-19T12:00:00Z",
        "--json",
    ]);
    assert!(
        error.contains("requires shared runtime recovery"),
        "read-only planning must fail safely instead of recovering: {error}"
    );
    assert_eq!(
        snapshot_tree(fixture.home.path()),
        before_recovery_rejection,
        "read-only recovery rejection must itself perform zero writes"
    );
    fs::remove_file(&recovery_sentinel).expect("clear synthetic recovery sentinel");

    let before = snapshot_tree(fixture.home.path());

    fixture.run_json(&[
        "lifecycle",
        "plan",
        "--record-id",
        &record_id,
        "--evaluated-at",
        "2026-07-19T12:00:00Z",
        "--json",
    ]);
    fixture.inspect_record(&record_id);
    fixture.inspect_grant(&grant_id);

    assert_eq!(
        snapshot_tree(fixture.home.path()),
        before,
        "read-only lifecycle commands must not recover, refresh, initialize, or persist SQLite state"
    );
}

#[test]
fn private_plan_output_is_no_clobber_and_cannot_enter_worktree_or_runtime_state() {
    let fixture = LifecycleFixture::new();
    let record_id = fixture.add_private_record(
        "Private output boundary",
        "private plan artifacts stay outside versioned and managed runtime paths",
    );

    let external = fixture.artifacts_root.join("private-plan.json");
    let emitted = fixture.run_json(&[
        "lifecycle",
        "plan",
        "--record-id",
        &record_id,
        "--evaluated-at",
        "2026-07-19T12:00:00Z",
        "--output",
        path_text(&external),
        "--json",
    ]);
    let installed: Value = serde_json::from_slice(
        &fs::read(&external).expect("read atomically installed private plan"),
    )
    .expect("installed private plan is JSON");
    assert_eq!(installed, emitted);

    let occupied = fixture.artifacts_root.join("occupied-plan.json");
    let sentinel = b"OWNER-CONTROLLED-NO-CLOBBER-SENTINEL\n";
    fs::write(&occupied, sentinel).expect("write no-clobber destination fixture");
    let error = fixture.failure_stderr(&[
        "lifecycle",
        "plan",
        "--record-id",
        &record_id,
        "--output",
        path_text(&occupied),
        "--json",
    ]);
    assert!(
        error.contains("destination already exists"),
        "existing output must fail closed: {error}"
    );
    assert_eq!(
        fs::read(&occupied).expect("read occupied destination after failure"),
        sentinel,
        "no-clobber output must preserve owner-controlled bytes"
    );

    let worktree_target = fixture.repo.path().join("private-lifecycle-plan.json");
    let error = fixture.failure_stderr(&[
        "lifecycle",
        "plan",
        "--record-id",
        &record_id,
        "--output",
        path_text(&worktree_target),
        "--json",
    ]);
    assert!(
        error.contains("inside the Git worktree"),
        "private output must reject the Git worktree: {error}"
    );
    assert!(!worktree_target.exists());

    let runtime_target = fixture
        .paths()
        .repository_runtime_dir
        .join("private-lifecycle-plan.json");
    let error = fixture.failure_stderr(&[
        "lifecycle",
        "plan",
        "--record-id",
        &record_id,
        "--output",
        path_text(&runtime_target),
        "--json",
    ]);
    assert!(
        error.contains("Memzoi-managed runtime state"),
        "private output must reject managed runtime state: {error}"
    );
    assert!(!runtime_target.exists());
}

#[test]
fn authorize_apply_quarantine_and_exact_replay_follow_the_one_shot_contract() {
    let fixture = LifecycleFixture::new();
    let title = "Quarantine history remains inspectable";
    let body = "quarantine-search-sentinel remains preserved but leaves ordinary reads";
    let record_id = fixture.add_private_record(title, body);
    let version = json_string(&fixture.inspect_record(&record_id), "version").to_owned();
    let (request, request_path) = fixture.write_request(
        "quarantine",
        "cli-quarantine-once",
        PrivateLifecycleAction::Quarantine {
            record_id: record_id.clone(),
            expected_version: version,
            reason_code: "owner_requested_review".to_owned(),
        },
    );
    assert_eq!(request.schema, PRIVATE_LIFECYCLE_REQUEST_SCHEMA);

    let grant = fixture.authorize(&request_path);
    assert_eq!(grant["schema"], PRIVATE_LIFECYCLE_GRANT_SCHEMA);
    assert_eq!(grant["request_id"], request.request_id);
    assert_eq!(grant["state"], "active");
    let grant_id = json_string(&grant, "grant_id");
    let identical_authorization = fixture.authorize(&request_path);
    assert_eq!(
        identical_authorization["grant_id"], grant_id,
        "identical active authority should reuse the stored grant"
    );
    assert_eq!(fixture.inspect_grant(grant_id)["state"], "active");
    assert_eq!(
        fixture.inspect_record(&record_id)["state"]["quarantined"],
        false
    );

    let applied = fixture.apply(&request_path, grant_id);
    assert_eq!(applied["schema"], PRIVATE_LIFECYCLE_RESULT_SCHEMA);
    assert_eq!(applied["request_id"], request.request_id);
    assert_eq!(applied["replayed"], false);
    let application_id = json_string(&applied, "application_id").to_owned();

    let inspection = fixture.inspect_record(&record_id);
    assert_eq!(inspection["state"]["quarantined"], true);
    assert_eq!(inspection["record"]["title"], title);
    assert_eq!(inspection["record"]["body"], body);
    let ordinary = fixture.run_json(&["local", "search", "quarantine-search-sentinel", "--json"]);
    assert_eq!(
        ordinary["records"],
        Value::Array(Vec::new()),
        "ordinary local search must not serve quarantined content: {ordinary}"
    );

    let replay = fixture.apply(&request_path, grant_id);
    assert_eq!(replay["application_id"], application_id);
    assert_eq!(replay["replayed"], true);
    assert_eq!(fixture.inspect_grant(grant_id)["state"], "consumed");

    let revoke = fixture.run_json(&["lifecycle", "revoke", "--grant-id", grant_id, "--json"]);
    assert_eq!(revoke["outcome"], "already_consumed");
}

#[test]
fn event_export_never_exposes_quarantined_or_superseded_private_content() {
    let fixture = LifecycleFixture::new();
    let quarantined_title = "PRIVATE-EVENT-QUARANTINE-TITLE-SENTINEL";
    let quarantined_body = "PRIVATE-EVENT-QUARANTINE-BODY-SENTINEL";
    let quarantined_id = fixture.add_private_record(quarantined_title, quarantined_body);
    let quarantined_version =
        json_string(&fixture.inspect_record(&quarantined_id), "version").to_owned();
    let (_, quarantine_path) = fixture.write_request(
        "event-export-quarantine",
        "cli-event-export-quarantine",
        PrivateLifecycleAction::Quarantine {
            record_id: quarantined_id.clone(),
            expected_version: quarantined_version,
            reason_code: "event_export_boundary".to_owned(),
        },
    );
    let quarantine_grant = fixture.authorize(&quarantine_path);
    fixture.apply(&quarantine_path, json_string(&quarantine_grant, "grant_id"));

    let superseded_title = "PRIVATE-EVENT-SUPERSEDED-TITLE-SENTINEL";
    let superseded_body = "PRIVATE-EVENT-SUPERSEDED-BODY-SENTINEL";
    let successor_title = "PRIVATE-EVENT-SUCCESSOR-TITLE-SENTINEL";
    let successor_body = "PRIVATE-EVENT-SUCCESSOR-BODY-SENTINEL";
    let superseded_id = fixture.add_private_record(superseded_title, superseded_body);
    let successor_id = fixture.add_private_record(successor_title, successor_body);
    let (_, supersede_path) = fixture.write_request(
        "event-export-supersede",
        "cli-event-export-supersede",
        PrivateLifecycleAction::Supersede {
            record_id: superseded_id.clone(),
            expected_version: json_string(&fixture.inspect_record(&superseded_id), "version")
                .to_owned(),
            successor_record_id: successor_id.clone(),
            expected_successor_version: json_string(
                &fixture.inspect_record(&successor_id),
                "version",
            )
            .to_owned(),
            reason_code: "owner_selected_successor".to_owned(),
        },
    );
    let supersede_grant = fixture.authorize(&supersede_path);
    fixture.apply(&supersede_path, json_string(&supersede_grant, "grant_id"));

    let search_query = "PRIVATE-EVENT-SEARCH-QUERY-SENTINEL";
    let context_task = "PRIVATE-EVENT-CONTEXT-TASK-SENTINEL";
    let handoff_task = "PRIVATE-EVENT-HANDOFF-TASK-SENTINEL";
    let precheck_command = "PRIVATE-EVENT-PRECHECK-COMMAND-SENTINEL";
    let read_only_private_id = fixture.add_private_record(
        search_query,
        "This private record ID must not escape through raw read telemetry.",
    );
    fixture.run_json(&["local", "search", search_query, "--json"]);
    fixture.run_json(&[
        "context",
        "--task",
        context_task,
        "--include-local",
        "--json",
    ]);
    fixture.run_json(&[
        "handoff",
        "--task",
        handoff_task,
        "--include-local",
        "--json",
    ]);
    fixture.run_json(&["precheck", "--command", precheck_command, "--json"]);

    let private_proposal_title = "PRIVATE-EVENT-PROPOSAL-TITLE-SENTINEL";
    let private_rejection_reason = "PRIVATE-EVENT-REJECTION-REASON-SENTINEL";
    let private_tombstone_reason = "PRIVATE-EVENT-TOMBSTONE-REASON-SENTINEL";
    let private_proposal = fixture.run_json(&[
        "propose",
        "--manual",
        "--type",
        "fact",
        "--title",
        private_proposal_title,
        "--body",
        "Unclassified proposal telemetry must remain local.",
        "--json",
    ]);
    fixture.run_json(&[
        "reject",
        json_string(&private_proposal, "proposal_id"),
        "--reason",
        private_rejection_reason,
        "--json",
    ]);
    let repository_record = fixture.run_json(&[
        "propose",
        "--apply",
        "--type",
        "fact",
        "--sensitivity",
        "repo-safe",
        "--content-class",
        "general_repo_knowledge",
        "--title",
        "Repository event-export control",
        "--body",
        "This repository-safe record exercises tombstone telemetry classification.",
        "--json",
    ]);
    fixture.run_json(&[
        "tombstone",
        json_string(&repository_record, "record_id"),
        "--reason",
        private_tombstone_reason,
        "--json",
    ]);

    let paths = fixture.paths();
    let private_read_event_count = [&paths.shared_db_path, &paths.index_db_path]
        .into_iter()
        .map(|path| {
            let conn = Connection::open(path).expect("open lifecycle event database");
            conn.query_row(
                "SELECT COUNT(*) FROM event_log
                 WHERE data_class = 'private'
                   AND event_type IN (
                     'memory.searched',
                     'memory.context_pack_built',
                     'memory.precheck_ran'
                   )",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count private read events")
        })
        .sum::<i64>();
    assert!(
        private_read_event_count >= 4,
        "raw read telemetry should be retained locally with an explicit private data class"
    );
    let private_caller_text_event_count = [&paths.shared_db_path, &paths.index_db_path]
        .into_iter()
        .map(|path| {
            let conn = Connection::open(path).expect("open proposal event database");
            conn.query_row(
                "SELECT COUNT(*) FROM event_log
                 WHERE data_class = 'private'
                   AND (
                     payload_json LIKE '%' || ?1 || '%'
                     OR payload_json LIKE '%' || ?2 || '%'
                     OR payload_json LIKE '%' || ?3 || '%'
                   )",
                [
                    private_proposal_title,
                    private_rejection_reason,
                    private_tombstone_reason,
                ],
                |row| row.get::<_, i64>(0),
            )
            .expect("count private caller-text events")
        })
        .sum::<i64>();
    assert!(
        private_caller_text_event_count >= 3,
        "unclassified proposal, rejection, and tombstone text must be explicitly private"
    );

    let exported = fixture.success_stdout(&["events", "export", "--jsonl"]);
    for forbidden in [
        quarantined_title,
        quarantined_body,
        superseded_title,
        superseded_body,
        successor_title,
        successor_body,
        search_query,
        context_task,
        handoff_task,
        precheck_command,
        private_proposal_title,
        private_rejection_reason,
        private_tombstone_reason,
        &read_only_private_id,
    ] {
        assert!(
            !exported.contains(forbidden),
            "event export leaked private lifecycle history {forbidden:?}: {exported}"
        );
    }
    assert!(
        !exported.contains("memory.local_created"),
        "private creation events must not cross the repository event-export boundary: {exported}"
    );
    for private_read_event in [
        "memory.searched",
        "memory.context_pack_built",
        "memory.precheck_ran",
    ] {
        assert!(
            !exported.contains(private_read_event),
            "raw read event {private_read_event} crossed the repository export boundary: {exported}"
        );
    }
    assert!(
        exported.contains("memory.private_lifecycle_applied"),
        "content-free lifecycle audit events should remain exportable: {exported}"
    );
}

#[test]
fn shared_lifecycle_authority_remains_available_without_the_disposable_index() {
    let fixture = LifecycleFixture::new();
    let record_id = fixture.add_private_record(
        "Missing mirror grant sentinel",
        "Grant authority must not depend on a disposable worktree index.",
    );
    let version = json_string(&fixture.inspect_record(&record_id), "version").to_owned();
    let (_, request_path) = fixture.write_request(
        "missing-mirror-revoke",
        "cli-missing-mirror-revoke",
        PrivateLifecycleAction::Pin {
            record_id: record_id.clone(),
            expected_version: version,
        },
    );
    let grant = fixture.authorize(&request_path);
    let grant_id = json_string(&grant, "grant_id").to_owned();
    let paths = fixture.paths();
    fs::remove_file(&paths.index_db_path).expect("remove disposable worktree index");

    let plan = fixture.run_json(&["lifecycle", "plan", "--record-id", &record_id, "--json"]);
    assert_eq!(plan["scope"]["kind"], "private_runtime");
    assert_eq!(
        fixture.inspect_record(&record_id)["record"]["id"],
        record_id
    );
    assert_eq!(fixture.inspect_grant(&grant_id)["state"], "active");

    let identical = fixture.authorize(&request_path);
    assert_eq!(identical["grant_id"], grant_id);
    let revoked = fixture.run_json(&["lifecycle", "revoke", "--grant-id", &grant_id, "--json"]);
    assert_eq!(revoked["outcome"], "revoked");
    assert_eq!(fixture.inspect_grant(&grant_id)["state"], "revoked");
    assert!(
        !paths.index_db_path.exists(),
        "shared-only lifecycle commands must not recreate the disposable index"
    );

    let apply_error = fixture.failure_stderr(&[
        "lifecycle",
        "apply",
        "--request-file",
        path_text(&request_path),
        "--grant-id",
        &grant_id,
        "--json",
    ]);
    assert!(
        apply_error.contains("current SQLite database does not exist"),
        "apply should retain its fail-closed mirror dependency: {apply_error}"
    );
    assert_eq!(fixture.inspect_grant(&grant_id)["state"], "revoked");
}

#[test]
fn checkpoint_close_rejects_quarantine_before_json_or_text_mutation() {
    let fixture = LifecycleFixture::new();
    let checkpoint = fixture.run_json(&[
        "checkpoint",
        "add",
        "--task",
        "Quarantined checkpoint close",
        "--note",
        "A quarantined checkpoint cannot be closed outside lifecycle apply.",
        "--operation-id",
        "create-quarantined-close-checkpoint",
        "--json",
    ]);
    let checkpoint_id = json_string(&checkpoint, "record_id").to_owned();
    let (_, quarantine_path) = fixture.write_request(
        "quarantined-checkpoint-close",
        "cli-quarantine-checkpoint-before-close",
        PrivateLifecycleAction::Quarantine {
            record_id: checkpoint_id.clone(),
            expected_version: json_string(&checkpoint, "record_version").to_owned(),
            reason_code: "owner_quarantine".to_owned(),
        },
    );
    let grant = fixture.authorize(&quarantine_path);
    fixture.apply(&quarantine_path, json_string(&grant, "grant_id"));
    let quarantined_version =
        json_string(&fixture.inspect_record(&checkpoint_id), "version").to_owned();
    let before = authority_storage_snapshot(&fixture.paths());

    let json_error = fixture.failure_stderr(&[
        "checkpoint",
        "close",
        &checkpoint_id,
        "--operation-id",
        "blocked-quarantined-close-json",
        "--expected-version",
        &quarantined_version,
        "--json",
    ]);
    assert!(json_error.contains("quarantined"), "{json_error}");
    assert_eq!(
        authority_storage_snapshot(&fixture.paths()),
        before,
        "JSON checkpoint close mutated quarantined state before failing"
    );

    let text_error = fixture.failure_stderr(&[
        "checkpoint",
        "close",
        &checkpoint_id,
        "--operation-id",
        "blocked-quarantined-close-text",
        "--expected-version",
        &quarantined_version,
    ]);
    assert!(text_error.contains("quarantined"), "{text_error}");
    assert_eq!(
        authority_storage_snapshot(&fixture.paths()),
        before,
        "text checkpoint close mutated quarantined state before failing"
    );
}

#[test]
fn authorize_changes_only_one_shared_grant_row() {
    let fixture = LifecycleFixture::new();
    let record_id = fixture.add_private_record(
        "Authorize mutation boundary",
        "grant creation cannot change lifecycle records or downstream state",
    );
    let version = json_string(&fixture.inspect_record(&record_id), "version").to_owned();
    let (_, request_path) = fixture.write_request(
        "authorize-only",
        "cli-authorize-only",
        PrivateLifecycleAction::Pin {
            record_id,
            expected_version: version,
        },
    );
    let paths = fixture.paths();
    let recovery_sentinel = paths.repository_runtime_dir.join("shared-sync.json");
    fs::write(&recovery_sentinel, b"DO-NOT-RECOVER-DURING-AUTHORIZE\n")
        .expect("write authorize recovery sentinel");
    let before = authority_storage_snapshot(&paths);

    let recovery_error = fixture.failure_stderr(&[
        "lifecycle",
        "authorize",
        "--request-file",
        path_text(&request_path),
        "--json",
    ]);
    assert!(
        recovery_error.contains("requires shared runtime recovery"),
        "authorize must fail closed on a pending authoritative snapshot: {recovery_error}"
    );
    assert_eq!(
        authority_storage_snapshot(&paths),
        before,
        "authorize may neither recover a pending journal nor create authority from a stale snapshot"
    );
    assert_eq!(
        fs::read(&recovery_sentinel).expect("read authorize recovery sentinel"),
        b"DO-NOT-RECOVER-DURING-AUTHORIZE\n",
        "authorize may not run unrelated shared-sync recovery"
    );
    fs::remove_file(&recovery_sentinel).expect("clear authorize recovery sentinel");

    let grant = fixture.authorize(&request_path);
    assert_eq!(grant["state"], "active");
    let after_first = authority_storage_snapshot(&paths);

    assert_eq!(
        after_first.shared_non_grant, before.shared_non_grant,
        "authorize may not mutate records, lifecycle state, relations, events, receipts, or generations"
    );
    assert_eq!(
        after_first.index_all, before.index_all,
        "grants are shared-authority-only and authorize may not mutate the worktree mirror"
    );
    assert_eq!(
        after_first.shared_grants.len(),
        before.shared_grants.len() + 1,
        "authorization must create exactly one grant row"
    );
    let identical = fixture.authorize(&request_path);
    assert_eq!(identical["grant_id"], grant["grant_id"]);
    assert_eq!(
        authority_storage_snapshot(&paths),
        after_first,
        "identical active authorization must reuse the authoritative grant without another write"
    );
}

#[test]
fn old_sqlite_schema_is_rejected_before_authority_writes() {
    let fixture = LifecycleFixture::new();
    let record_id = fixture.add_private_record(
        "Current-only schema boundary",
        "pre-1.0 databases must be manually removed or regenerated",
    );
    let version = json_string(&fixture.inspect_record(&record_id), "version").to_owned();
    let (_, request_path) = fixture.write_request(
        "old-schema",
        "cli-old-schema",
        PrivateLifecycleAction::Pin {
            record_id,
            expected_version: version,
        },
    );
    let paths = fixture.paths();
    {
        let connection =
            Connection::open(&paths.shared_db_path).expect("open shared old-schema fixture");
        connection
            .pragma_update(None, "user_version", 1_i64)
            .expect("downgrade fixture schema marker");
    }
    let before = snapshot_tree(fixture.home.path());

    let error = fixture.failure_stderr(&[
        "lifecycle",
        "authorize",
        "--request-file",
        path_text(&request_path),
        "--json",
    ]);

    assert!(
        error.contains("unsupported SQLite schema"),
        "old databases must fail with the current-schema-only contract: {error}"
    );
    assert_eq!(
        snapshot_tree(fixture.home.path()),
        before,
        "rejecting an old schema must not initialize, migrate, recover, or write authority state"
    );
}

#[test]
fn revoked_authority_is_a_typed_no_op_and_cannot_mutate_a_record() {
    let fixture = LifecycleFixture::new();
    let record_id = fixture.add_private_record(
        "Revoked grant fixture",
        "revoked grant cannot mutate this private record",
    );
    let version = json_string(&fixture.inspect_record(&record_id), "version").to_owned();
    let (_, request_path) = fixture.write_request(
        "revoked-pin",
        "cli-revoked-pin",
        PrivateLifecycleAction::Pin {
            record_id: record_id.clone(),
            expected_version: version,
        },
    );
    let grant = fixture.authorize(&request_path);
    let grant_id = json_string(&grant, "grant_id");

    let first = fixture.run_json(&["lifecycle", "revoke", "--grant-id", grant_id, "--json"]);
    assert_eq!(first["outcome"], "revoked");
    let second = fixture.run_json(&["lifecycle", "revoke", "--grant-id", grant_id, "--json"]);
    assert_eq!(second["outcome"], "already_revoked");

    let before = fixture.inspect_record(&record_id);
    let error = fixture.failure_stderr(&[
        "lifecycle",
        "apply",
        "--request-file",
        path_text(&request_path),
        "--grant-id",
        grant_id,
        "--json",
    ]);
    assert!(
        error.contains("owner_action_grant_not_active"),
        "revoked apply should fail with a typed authority error: {error}"
    );
    assert_eq!(fixture.inspect_record(&record_id), before);
    assert_eq!(fixture.inspect_grant(grant_id)["state"], "revoked");
}

#[test]
fn a_stale_request_performs_zero_writes_and_leaves_its_grant_active() {
    let fixture = LifecycleFixture::new();
    let record_id = fixture.add_private_record(
        "Stale authority fixture",
        "a competing lifecycle action rotates the exact private version",
    );
    let version = json_string(&fixture.inspect_record(&record_id), "version").to_owned();

    let (_, stale_path) = fixture.write_request(
        "stale-quarantine",
        "cli-stale-quarantine",
        PrivateLifecycleAction::Quarantine {
            record_id: record_id.clone(),
            expected_version: version.clone(),
            reason_code: "stale_authority_test".to_owned(),
        },
    );
    let stale_grant = fixture.authorize(&stale_path);
    let stale_grant_id = json_string(&stale_grant, "grant_id");

    let (_, competing_path) = fixture.write_request(
        "competing-pin",
        "cli-competing-pin",
        PrivateLifecycleAction::Pin {
            record_id: record_id.clone(),
            expected_version: version,
        },
    );
    let competing_grant = fixture.authorize(&competing_path);
    fixture.apply(&competing_path, json_string(&competing_grant, "grant_id"));

    let before_failed_apply = fixture.inspect_record(&record_id);
    assert_eq!(before_failed_apply["state"]["pinned"], true);
    assert_eq!(before_failed_apply["state"]["quarantined"], false);
    let error = fixture.failure_stderr(&[
        "lifecycle",
        "apply",
        "--request-file",
        path_text(&stale_path),
        "--grant-id",
        stale_grant_id,
        "--json",
    ]);
    assert!(
        error.contains("changed") || error.contains("stale") || error.contains("version mismatch"),
        "stale exact-version authority should fail closed: {error}"
    );
    assert_eq!(
        fixture.inspect_record(&record_id),
        before_failed_apply,
        "stale apply must not partially mutate lifecycle state"
    );
    assert_eq!(
        fixture.inspect_grant(stale_grant_id)["state"],
        "active",
        "failed stale apply must not consume its grant"
    );
}

#[test]
fn authorize_rejects_non_regular_request_artifacts_before_lifecycle_writes() {
    let fixture = LifecycleFixture::new();
    let record_id = fixture.add_private_record(
        "Regular artifact fixture",
        "authority inputs must be regular non-symlink files",
    );
    let before = fixture.inspect_record(&record_id);

    let error = fixture.failure_stderr(&[
        "lifecycle",
        "authorize",
        "--request-file",
        path_text(&fixture.artifacts_root),
        "--json",
    ]);

    assert!(
        error.contains("regular, non-symlink file"),
        "directory authority input should fail strict artifact admission: {error}"
    );
    assert_eq!(fixture.inspect_record(&record_id), before);
}

#[test]
fn strict_authority_artifacts_are_rejected_before_the_lifecycle_bundle_is_opened() {
    let fixture = LifecycleFixture::new();
    let (_, valid_request_path) = fixture.write_request(
        "valid-artifact-target",
        "cli-artifact-boundary",
        PrivateLifecycleAction::Pin {
            record_id: "local-opaque-artifact-fixture".to_owned(),
            expected_version: "0123456789abcdef0123456789abcdef".to_owned(),
        },
    );
    let valid_request =
        fs::read_to_string(&valid_request_path).expect("read valid request fixture");
    let private_plan = fixture.run_json(&[
        "lifecycle",
        "plan",
        "--evaluated-at",
        "2026-07-19T12:00:00Z",
        "--json",
    ]);
    let paths = fixture.paths();
    let hidden_config = paths.repository_runtime_dir.join("config.hidden-by-test");
    fs::rename(&paths.config_path, &hidden_config).expect("hide lifecycle config fixture");

    let invalid = fixture.artifacts_root.join("malformed.json");
    fs::write(&invalid, b"{not-json\n").expect("write malformed lifecycle request");
    assert_failure_preserves_runtime(
        &fixture,
        &[
            "lifecycle",
            "authorize",
            "--request-file",
            path_text(&invalid),
            "--json",
        ],
        &["strict JSON"],
    );
    assert_failure_preserves_runtime(
        &fixture,
        &[
            "lifecycle",
            "apply",
            "--request-file",
            path_text(&invalid),
            "--grant-id",
            "grant-must-not-be-looked-up",
            "--json",
        ],
        &["strict JSON"],
    );

    let v1_request = fixture.artifacts_root.join("v1-request.json");
    fs::write(
        &v1_request,
        valid_request.replacen(
            PRIVATE_LIFECYCLE_REQUEST_SCHEMA,
            "memzoi/private-lifecycle-request/v1",
            1,
        ),
    )
    .expect("write v1 lifecycle request fixture");
    assert_failure_preserves_runtime(
        &fixture,
        &[
            "lifecycle",
            "authorize",
            "--request-file",
            path_text(&v1_request),
            "--json",
        ],
        &["unsupported private lifecycle request schema"],
    );

    let duplicate_key = fixture.artifacts_root.join("duplicate-key.json");
    fs::write(
        &duplicate_key,
        format!(
            "{{\"schema\":\"{PRIVATE_LIFECYCLE_REQUEST_SCHEMA}\",{}",
            &valid_request[1..]
        ),
    )
    .expect("write duplicate-key lifecycle request fixture");
    assert_failure_preserves_runtime(
        &fixture,
        &[
            "lifecycle",
            "authorize",
            "--request-file",
            path_text(&duplicate_key),
            "--json",
        ],
        &["duplicate mapping key", "schema"],
    );

    let oversized = fixture.artifacts_root.join("oversized.json");
    fs::write(
        &oversized,
        vec![b' '; PRIVATE_LIFECYCLE_MAX_ARTIFACT_BYTES + 1],
    )
    .expect("write oversized lifecycle request fixture");
    assert_failure_preserves_runtime(
        &fixture,
        &[
            "lifecycle",
            "authorize",
            "--request-file",
            path_text(&oversized),
            "--json",
        ],
        &["exceeds the 2 MiB limit"],
    );

    let mut removed_contract_plan = private_plan;
    removed_contract_plan["policy"]["contract_version"] =
        Value::String("maintenance-plan/2".to_owned());
    let removed_contract_plan_path = fixture.artifacts_root.join("removed-contract-plan.json");
    fs::write(
        &removed_contract_plan_path,
        serde_json::to_vec_pretty(&removed_contract_plan)
            .expect("serialize removed contract plan fixture"),
    )
    .expect("write removed contract maintenance plan fixture");
    assert_failure_preserves_runtime(
        &fixture,
        &[
            "lifecycle",
            "authorize",
            "--request-file",
            path_text(&valid_request_path),
            "--plan-file",
            path_text(&removed_contract_plan_path),
            "--json",
        ],
        &["invalid memzoi/maintenance-plan artifact"],
    );
    assert_failure_preserves_runtime(
        &fixture,
        &[
            "lifecycle",
            "apply",
            "--request-file",
            path_text(&valid_request_path),
            "--grant-id",
            "grant-must-not-be-looked-up",
            "--plan-file",
            path_text(&removed_contract_plan_path),
            "--json",
        ],
        &["invalid memzoi/maintenance-plan artifact"],
    );

    #[cfg(unix)]
    {
        let symlink = fixture.artifacts_root.join("request-symlink.json");
        std::os::unix::fs::symlink(&valid_request_path, &symlink)
            .expect("create request symlink fixture");
        assert_failure_preserves_runtime(
            &fixture,
            &[
                "lifecycle",
                "authorize",
                "--request-file",
                path_text(&symlink),
                "--json",
            ],
            &["without following symlinks"],
        );

        let fifo = fixture.artifacts_root.join("request.fifo");
        let status = StdCommand::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("run mkfifo for lifecycle fixture");
        assert!(status.success(), "mkfifo must create the lifecycle fixture");
        assert_failure_preserves_runtime(
            &fixture,
            &[
                "lifecycle",
                "authorize",
                "--request-file",
                path_text(&fifo),
                "--json",
            ],
            &["regular, non-symlink file"],
        );
    }

    assert!(
        hidden_config.is_file() && !paths.config_path.exists(),
        "rejected input must not initialize or recover lifecycle runtime state"
    );
}

fn json_string<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected JSON string field {key:?}: {value}"))
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("fixture path is UTF-8")
}

fn assert_failure_preserves_runtime(
    fixture: &LifecycleFixture,
    args: &[&str],
    expected_error_fragments: &[&str],
) {
    let before = snapshot_tree(fixture.home.path());
    let error = fixture.failure_stderr(args);
    assert!(
        expected_error_fragments
            .iter()
            .all(|fragment| error.contains(fragment)),
        "artifact rejection should contain {expected_error_fragments:?}: {error}"
    );
    assert_eq!(
        snapshot_tree(fixture.home.path()),
        before,
        "rejected authority artifact must not open-recover or mutate runtime state"
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotEntry {
    Directory,
    File { len: usize, digest: String },
    Symlink(PathBuf),
    Other,
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, SnapshotEntry> {
    fn visit(root: &Path, path: &Path, snapshot: &mut BTreeMap<PathBuf, SnapshotEntry>) {
        let metadata = fs::symlink_metadata(path)
            .unwrap_or_else(|error| panic!("inspect snapshot path {}: {error}", path.display()));
        let relative = path
            .strip_prefix(root)
            .expect("snapshot entry is beneath root")
            .to_path_buf();
        if metadata.file_type().is_symlink() {
            snapshot.insert(
                relative,
                SnapshotEntry::Symlink(
                    fs::read_link(path).expect("read lifecycle runtime snapshot symlink"),
                ),
            );
        } else if metadata.is_dir() {
            snapshot.insert(relative, SnapshotEntry::Directory);
            for entry in fs::read_dir(path).unwrap_or_else(|error| {
                panic!("read snapshot directory {}: {error}", path.display())
            }) {
                visit(
                    root,
                    &entry.expect("read lifecycle runtime snapshot entry").path(),
                    snapshot,
                );
            }
        } else if metadata.is_file() {
            let bytes = fs::read(path)
                .unwrap_or_else(|error| panic!("read snapshot file {}: {error}", path.display()));
            snapshot.insert(
                relative,
                SnapshotEntry::File {
                    len: bytes.len(),
                    digest: blake3::hash(&bytes).to_hex().to_string(),
                },
            );
        } else {
            snapshot.insert(relative, SnapshotEntry::Other);
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

#[derive(Debug, Clone, PartialEq)]
struct AuthorityStorageSnapshot {
    shared_non_grant: BTreeMap<String, Vec<Vec<SqlValue>>>,
    shared_grants: Vec<Vec<SqlValue>>,
    index_all: BTreeMap<String, Vec<Vec<SqlValue>>>,
}

fn authority_storage_snapshot(paths: &MemoryPaths) -> AuthorityStorageSnapshot {
    const NON_GRANT_TABLES: &[&str] = &[
        "event_log",
        "memory_record",
        "origin_outcome",
        "scope_binding",
        "memory_path",
        "proposal",
        "memory_tag",
        "memory_capture",
        "runtime_mirror_state",
        "private_lifecycle_generation",
        "private_lifecycle_state",
        "private_lifecycle_relation",
        "private_lifecycle_application",
        "private_maintenance_grant",
        "private_maintenance_projection",
        "private_conflict_set",
        "private_conflict_member",
        "private_conflict_edge",
        "read_audit",
    ];

    let shared = Connection::open(&paths.shared_db_path).expect("open shared authority snapshot");
    let index = Connection::open(&paths.index_db_path).expect("open worktree mirror snapshot");
    let shared_non_grant = NON_GRANT_TABLES
        .iter()
        .map(|table| ((*table).to_owned(), table_rows(&shared, table)))
        .collect();
    let shared_grants = table_rows(&shared, "owner_action_grant");
    let index_all = NON_GRANT_TABLES
        .iter()
        .chain(std::iter::once(&"owner_action_grant"))
        .map(|table| ((*table).to_owned(), table_rows(&index, table)))
        .collect();
    AuthorityStorageSnapshot {
        shared_non_grant,
        shared_grants,
        index_all,
    }
}

fn table_rows(connection: &Connection, table: &str) -> Vec<Vec<SqlValue>> {
    let sql = format!("SELECT * FROM {table} ORDER BY rowid");
    let mut statement = connection
        .prepare(&sql)
        .unwrap_or_else(|error| panic!("prepare snapshot table {table}: {error}"));
    let column_count = statement.column_count();
    statement
        .query_map([], |row| {
            (0..column_count)
                .map(|column| row.get(column))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_else(|error| panic!("query snapshot table {table}: {error}"))
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap_or_else(|error| panic!("read snapshot table {table}: {error}"))
}

fn run_git(repo: &Path, args: &[&str]) {
    let mut command = StdCommand::new("git");
    command.args(args).current_dir(repo);
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
        "GIT_QUARANTINE_PATH",
    ] {
        command.env_remove(key);
    }
    let output = command.output().expect("run Git fixture command");
    assert!(
        output.status.success(),
        "Git fixture command {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
