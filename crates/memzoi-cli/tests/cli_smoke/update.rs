use super::*;

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
