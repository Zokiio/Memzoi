use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    thread,
};

use assert_cmd::Command;
use memzoi_core::{
    MemoryDestination, MemoryPaths, TWO_PLANE_MEMORY_POLICY, parse_okf_record_file,
    render_okf_record_markdown,
};
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

#[path = "cli_smoke/capture.rs"]
mod capture;
#[path = "cli_smoke/checkpoint_lifecycle.rs"]
mod checkpoint_lifecycle;
#[path = "cli_smoke/doctor.rs"]
mod doctor;
#[path = "cli_smoke/eval.rs"]
mod eval;
#[path = "cli_smoke/export.rs"]
mod export;
#[path = "cli_smoke/init.rs"]
mod init;
#[path = "cli_smoke/integrate.rs"]
mod integrate;
#[path = "cli_smoke/proposal_files_apply.rs"]
mod proposal_files_apply;
#[path = "cli_smoke/proposal_files_inventory.rs"]
mod proposal_files_inventory;
#[path = "cli_smoke/proposal_files_lifecycle.rs"]
mod proposal_files_lifecycle;
#[path = "cli_smoke/proposal_files_recovery.rs"]
mod proposal_files_recovery;
#[path = "cli_smoke/proposals.rs"]
mod proposals;
#[path = "cli_smoke/recall.rs"]
mod recall;
#[path = "cli_smoke/recall_workflows.rs"]
mod recall_workflows;
#[path = "cli_smoke/safety.rs"]
mod safety;
#[path = "cli_smoke/session_end.rs"]
mod session_end;
#[path = "cli_smoke/update.rs"]
mod update;

fn initialized_temp_repo() -> tempfile::TempDir {
    initialized_temp_repo_with_home(&test_memzoi_home())
}

fn initialized_temp_repo_with_home(memzoi_home: &Path) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("temp repo");
    run_git_fixture(repo.path(), &["init", "-q"]);

    let mut init = memzoi_with_home(memzoi_home);
    init.args(["init", "--json"])
        .current_dir(repo.path())
        .assert()
        .success();

    repo
}

fn capture_markdown_fixture() -> &'static str {
    "# Capture CLI fixture\n\n## Decision: Keep capture explicit\n\nCapture reads only the Markdown source explicitly named by the caller.\n\n## Procedure: Review before routing\n\nReview every extracted candidate before routing it to governed memory.\n"
}

fn capture_plan_review_fixture(repo: &Path) -> (PathBuf, PathBuf, PathBuf, Value, Value) {
    let source = repo.join("capture-source.md");
    fs::write(&source, capture_markdown_fixture()).expect("write capture source");
    let runtime_dir = test_paths(repo).runtime_dir;
    let plan_path = runtime_dir.join("capture-plan.json");
    let plan = run_json_command(
        repo,
        &[
            "capture",
            "plan",
            "--source",
            "capture-source.md",
            "--output",
            plan_path.to_str().expect("plan path utf-8"),
            "--json",
        ],
    );
    let decisions = plan["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("capture plan should contain candidates: {plan}"))
        .iter()
        .map(|candidate| {
            serde_json::json!({
                "candidate_id": json_string(candidate, "candidate_id"),
                "outcome": "edit",
                "reason_code": "explicit-contextual-classification",
                "memory": candidate["memory"].clone(),
                "requested_destination": "repo",
                "content_class": "general_repo_knowledge",
            })
        })
        .collect::<Vec<_>>();
    let decisions_path = runtime_dir.join("capture-decisions.json");
    fs::write(
        &decisions_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "memzoi/capture-review-input-v2",
            "plan_id": json_string(&plan, "plan_id"),
            "decisions": decisions,
        }))
        .expect("serialize capture decisions"),
    )
    .expect("write capture decisions");
    let review_path = runtime_dir.join("capture-review.json");
    let review = run_json_command(
        repo,
        &[
            "capture",
            "review",
            "--plan-file",
            plan_path.to_str().expect("plan path utf-8"),
            "--decisions-file",
            decisions_path.to_str().expect("decisions path utf-8"),
            "--reviewed-by",
            "maintainer:test",
            "--reviewed-at",
            "2026-07-10T18:00:00Z",
            "--output",
            review_path.to_str().expect("review path utf-8"),
            "--json",
        ],
    );
    (source, plan_path, review_path, plan, review)
}

fn managed_state_snapshot(paths: &MemoryPaths) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut snapshot = BTreeMap::new();
    snapshot_tree(&paths.memory_dir, Path::new("repo"), &mut snapshot);
    snapshot_tree(&paths.runtime_dir, Path::new("runtime"), &mut snapshot);
    snapshot
}

fn snapshot_tree(root: &Path, label: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
    if !root.exists() {
        return;
    }
    snapshot.insert(label.to_path_buf(), b"directory".to_vec());
    snapshot_tree_entries(root, root, label, snapshot);
}

fn snapshot_tree_entries(
    root: &Path,
    directory: &Path,
    label: &Path,
    snapshot: &mut BTreeMap<PathBuf, Vec<u8>>,
) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read managed state {}: {error}", directory.display()))
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap_or_else(|error| panic!("collect managed state {}: {error}", directory.display()));
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("managed state entry should be beneath snapshot root");
        let key = label.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .unwrap_or_else(|error| panic!("inspect managed state {}: {error}", path.display()));
        if metadata.is_dir() {
            snapshot.insert(key, b"directory".to_vec());
            snapshot_tree_entries(root, &path, label, snapshot);
        } else if metadata.is_file() {
            let mut value = b"file\0".to_vec();
            value.extend(
                fs::read(&path).unwrap_or_else(|error| {
                    panic!("read managed state {}: {error}", path.display())
                }),
            );
            snapshot.insert(key, value);
        } else if metadata.file_type().is_symlink() {
            let mut value = b"symlink\0".to_vec();
            value.extend(
                fs::read_link(&path)
                    .unwrap_or_else(|error| {
                        panic!("read managed symlink {}: {error}", path.display())
                    })
                    .as_os_str()
                    .as_encoded_bytes(),
            );
            snapshot.insert(key, value);
        } else {
            snapshot.insert(key, b"special".to_vec());
        }
    }
}

fn run_json_command(repo: &Path, args: &[&str]) -> Value {
    let mut cmd = memzoi();
    let assert = cmd.args(args).current_dir(repo).assert().success();
    json_from_stdout(&assert.get_output().stdout)
}

fn run_json_command_with_home(repo: &Path, args: &[&str], home: &Path) -> Value {
    let mut cmd = memzoi_with_home(home);
    let assert = cmd.args(args).current_dir(repo).assert().success();
    json_from_stdout(&assert.get_output().stdout)
}

fn run_git_fixture(directory: &Path, args: &[&str]) {
    let mut command = std::process::Command::new("git");
    command.args(args).current_dir(directory);
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
        "GIT_COMMON_DIR",
        "GIT_QUARANTINE_PATH",
    ] {
        command.env_remove(key);
    }
    let output = command.output().expect("run Git fixture command");
    assert!(
        output.status.success(),
        "Git fixture command {args:?} failed with {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_fixture_executable() -> PathBuf {
    let executable = format!("git{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::var_os("PATH").expect("PATH is set for Git fixtures");
    std::env::split_paths(&path)
        .map(|directory| directory.join(&executable))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("could not locate {executable} on PATH for Git fixture"))
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

fn checked_recall_corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/v2/corpus.yaml")
}

fn checked_recall_baseline() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/recall/v2/baseline.json")
}

fn checked_capture_corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/capture/v1/corpus.yaml")
}

fn checked_capture_baseline() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evals/capture/v1/baseline.json")
}

fn failing_recall_corpus() -> (tempfile::TempDir, PathBuf) {
    let fixture = tempfile::tempdir().expect("recall fixture root");
    let checked = checked_recall_corpus();
    let checked_root = checked.parent().expect("checked corpus has a parent");
    copy_directory(checked_root, fixture.path());
    let corpus = fixture.path().join("corpus.yaml");
    let yaml = fs::read_to_string(&corpus).expect("read copied v2 corpus");
    let original = "relevant_ids: [lexical-target]";
    assert_eq!(
        yaml.matches(original).count(),
        1,
        "checked corpus should have one lexical expectation to perturb"
    );
    fs::write(
        &corpus,
        yaml.replacen(original, "relevant_ids: [path-target]", 1),
    )
    .expect("write failing v2 recall corpus");
    (fixture, corpus)
}

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create copied fixture directory");
    for entry in fs::read_dir(source).expect("read checked fixture directory") {
        let entry = entry.expect("read checked fixture entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type().expect("read checked fixture type");
        if file_type.is_dir() {
            copy_directory(&source_path, &destination_path);
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path).unwrap_or_else(|error| {
                panic!(
                    "copy checked fixture {} to {}: {error}",
                    source_path.display(),
                    destination_path.display()
                )
            });
        }
    }
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
id: {id}
kind: memory
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: test-canonical:{id}
  route: repository_materialization
type: decision
lane: semantic
title: Legacy auth evidence
description: Original legacy auth evidence remains reviewable.
timestamp: {created}
updated: {updated}
status: {status}
scope: {scope}
{scope_id}visibility: repo
content_class: general_repo_knowledge
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
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: test-proposal:mem_test_unicode
  route: repository_proposal
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
content_class: general_repo_knowledge
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
version: okf/v0.2
profile: memzoi/v1
retention:
  policy_version: memzoi/lane-retention-v1
origin:
  version: memzoi/origin-v1
  origin_key: test-proposal:mem_test_valid
  route: repository_proposal
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
content_class: general_repo_knowledge
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
    let policy_supported =
        matches!(scope_kind, "repo" | "project") && matches!(visibility, "repo" | "public");
    if !policy_supported {
        let local = run_json_command(
            repo,
            &[
                "local",
                "add",
                "--type",
                memory_type,
                "--title",
                title,
                "--body",
                body,
                "--actor",
                "agent:cli-search-context-tests",
                "--json",
            ],
        );
        return json_string(&local, "record_id").to_owned();
    }

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
            "--content-class",
            "general_repo_knowledge",
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
    mutate_canonical_record(repo, record_id, |record| {
        if !record.applies_to.iter().any(|existing| existing == path) {
            record.applies_to.push(path.to_owned());
        }
    });
}

fn update_record_source_ref(repo: &Path, record_id: &str, source_ref: &str) {
    mutate_canonical_record(repo, record_id, |record| {
        record.draft.source_ref = Some(source_ref.to_owned());
    });
}

fn set_record_expiry(repo: &Path, record_id: &str, expires_at: &str) {
    mutate_canonical_record(repo, record_id, |record| {
        record.retention.explicit_expires_at = Some(expires_at.to_owned());
    });
}

fn mutate_canonical_record(
    repo: &Path,
    record_id: &str,
    mutate: impl FnOnce(&mut memzoi_core::OkfRecordFile),
) {
    let paths = test_paths(repo);
    let records = paths.records_dir();
    let path = records.join(format!("{record_id}.md"));
    let mut record = parse_okf_record_file(&records, &path)
        .unwrap_or_else(|error| panic!("parse canonical record {record_id}: {error:#}"))
        .unwrap_or_else(|| panic!("canonical record {record_id} must exist"));
    mutate(&mut record);
    fs::write(
        &path,
        render_okf_record_markdown(&record)
            .unwrap_or_else(|error| panic!("render canonical record {record_id}: {error:#}")),
    )
    .unwrap_or_else(|error| panic!("write canonical record {record_id}: {error}"));
    run_json_command(repo, &["rebuild", "--json"]);
}

fn memory_db_path(repo: &Path) -> std::path::PathBuf {
    test_paths(repo).db_path
}

fn shared_memory_db_path(repo: &Path) -> std::path::PathBuf {
    test_paths(repo).shared_db_path
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
