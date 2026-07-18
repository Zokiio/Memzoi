use super::*;

#[test]
fn help_advertises_the_init_subcommand() {
    let mut cmd = memzoi();

    cmd.arg("--help").assert().success().stdout(
        predicate::str::contains("Local-first memory governance")
            .and(predicate::str::contains("Usage: memzoi <COMMAND>"))
            .and(predicate::str::contains("init"))
            .and(predicate::str::contains("update")),
    );
}

#[test]
fn init_help_advertises_init_options() {
    let mut cmd = memzoi();

    cmd.args(["init", "--help"]).assert().success().stdout(
        predicate::str::contains("Initialize repo .memzoi memory and local runtime state")
            .and(predicate::str::contains("Usage: memzoi init [OPTIONS]"))
            .and(predicate::str::contains("--force"))
            .and(predicate::str::contains("--json")),
    );
}

#[test]
fn update_help_advertises_update_options() {
    let mut cmd = memzoi();

    cmd.args(["update", "--help"]).assert().success().stdout(
        predicate::str::contains("Check for or apply a Memzoi release update")
            .and(predicate::str::contains("Usage: memzoi update [OPTIONS]"))
            .and(predicate::str::contains("--check"))
            .and(predicate::str::contains("--ref"))
            .and(predicate::str::contains("--json")),
    );
}

fn safety_record_markdown(body: &str, content_class: Option<&str>) -> String {
    let content_class = content_class
        .map(|value| format!("content_class: {value}\n"))
        .unwrap_or_default();
    format!(
        "---\nid: safety-fixture\nkind: memory\nprofile: memzoi\nretention: {{}}\norigin:\n  origin_key: test-safety:safety-fixture\n  route: repository_materialization\ntype: fact\nlane: semantic\ntitle: Safety fixture\ntimestamp: 2026-07-14T00:00:00Z\nupdated: 2026-07-14T00:00:00Z\nstatus: active\nscope: repo\nvisibility: repo\n{content_class}confidence: 1\nsource: test\nsource_ref: fixture://safety\n---\n\n# Safety fixture\n\n{body}\n"
    )
}

#[test]
fn safety_file_scan_uses_stable_exit_codes_and_redacted_json() {
    let temp = tempfile::tempdir().expect("temp repo");
    let records = temp.path().join(".memzoi/records");
    fs::create_dir_all(&records).expect("records directory");
    let safe = records.join("safe.md");
    fs::write(
        &safe,
        safety_record_markdown(
            "General repository knowledge.",
            Some("general_repo_knowledge"),
        ),
    )
    .expect("safe record");

    let mut safe_command = memzoi();
    safe_command
        .current_dir(temp.path())
        .args([
            "safety",
            "scan",
            "--file",
            ".memzoi/records/safe.md",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"allowed\": true"));

    let sentinel = "ghp_SECRET_SENTINEL_0123456789abcdefghijklmnop";
    let blocked = records.join("blocked.md");
    fs::write(
        &blocked,
        safety_record_markdown(sentinel, Some("general_repo_knowledge")),
    )
    .expect("blocked record");
    let mut blocked_command = memzoi();
    blocked_command
        .current_dir(temp.path())
        .args([
            "safety",
            "scan",
            "--file",
            ".memzoi/records/blocked.md",
            "--json",
        ])
        .assert()
        .code(2)
        .stdout(
            predicate::str::contains("\"allowed\": false")
                .and(predicate::str::contains("credential_token"))
                .and(predicate::str::contains(sentinel).not()),
        );
}

#[test]
fn safety_file_scan_rejects_parent_components_before_reading() {
    let temp = tempfile::tempdir().expect("temp repo");
    let sentinel = "PARENT-COMPONENT-TARGET-SENTINEL";
    write_pending_proposal_file(
        temp.path(),
        "parent-target.md",
        safety_record_markdown(sentinel, Some("general_repo_knowledge")),
    );
    fs::create_dir_all(temp.path().join(".memzoi/records")).expect("records directory");

    let mut command = memzoi();
    command
        .current_dir(temp.path())
        .args([
            "safety",
            "scan",
            "--file",
            ".memzoi/records/../../.memzoi/proposals/pending/parent-target.md",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("unsafe path component")
                .and(predicate::str::contains(sentinel).not()),
        )
        .stdout(predicate::str::contains(sentinel).not());
}

#[cfg(unix)]
#[test]
fn safety_file_scan_rejects_symlinked_ancestors_before_reading_outside() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temp repo");
    let outside = tempfile::tempdir().expect("outside target");
    let sentinel = "OUTSIDE-SYMLINK-TARGET-SENTINEL";
    write_pending_proposal_file(
        outside.path(),
        "outside.md",
        safety_record_markdown(sentinel, Some("general_repo_knowledge")),
    );
    let records = temp.path().join(".memzoi/records");
    fs::create_dir_all(&records).expect("records directory");
    symlink(
        outside.path().join(".memzoi/proposals/pending"),
        records.join("linked"),
    )
    .expect("symlink managed ancestor outside");

    let mut command = memzoi();
    command
        .current_dir(temp.path())
        .args([
            "safety",
            "scan",
            "--file",
            ".memzoi/records/linked/outside.md",
            "--json",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(sentinel).not())
        .stdout(
            predicate::str::contains("\"allowed\": false")
                .and(predicate::str::contains("unsafe_output_path"))
                .and(predicate::str::contains("<redacted-path:"))
                .and(predicate::str::contains(sentinel).not()),
        );
}

#[test]
fn safety_scans_block_oversized_worktree_staged_and_range_blobs() {
    use std::process::Command as StdCommand;

    let temp = tempfile::tempdir().expect("temp repo");
    for args in [
        &["init", "--quiet"][..],
        &["config", "user.email", "test@example.com"][..],
        &["config", "user.name", "Memzoi Test"][..],
        &["commit", "--allow-empty", "--quiet", "-m", "base"][..],
    ] {
        assert!(
            StdCommand::new("git")
                .args(args)
                .current_dir(temp.path())
                .status()
                .expect("run git setup")
                .success()
        );
    }
    let relative = ".memzoi/proposals/pending/oversized.md";
    write_pending_proposal_file(temp.path(), "oversized.md", "x".repeat(512 * 1024 + 1));

    let mut file = memzoi();
    file.current_dir(temp.path())
        .args(["safety", "scan", "--file", relative, "--json"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("candidate_too_large"));

    assert!(
        StdCommand::new("git")
            .args(["add", relative])
            .current_dir(temp.path())
            .status()
            .expect("stage oversized fixture")
            .success()
    );
    let mut staged = memzoi();
    staged
        .current_dir(temp.path())
        .args(["safety", "scan", "--staged", "--json"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("candidate_too_large"));

    assert!(
        StdCommand::new("git")
            .args(["commit", "--quiet", "-m", "oversized fixture"])
            .current_dir(temp.path())
            .status()
            .expect("commit oversized fixture")
            .success()
    );
    let mut range = memzoi();
    range
        .current_dir(temp.path())
        .args(["safety", "scan", "--range", "HEAD^...HEAD", "--json"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("candidate_too_large"));
}

#[test]
fn safety_scan_redacts_a_blocked_repository_path() {
    let temp = tempfile::tempdir().expect("temp repo");
    let records = temp.path().join(".memzoi/records");
    fs::create_dir_all(&records).expect("records directory");
    let sentinel = "ghp_PathSecretSentinel0123456789abcdef";
    let relative = format!(".memzoi/records/{sentinel}.md");
    fs::write(
        temp.path().join(&relative),
        safety_record_markdown("Safe body.", Some("general_repo_knowledge")),
    )
    .expect("path fixture");

    let mut command = memzoi();
    command
        .current_dir(temp.path())
        .args(["safety", "scan", "--file", &relative, "--json"])
        .assert()
        .code(2)
        .stdout(
            predicate::str::contains("credential_token")
                .and(predicate::str::contains("<redacted-path:"))
                .and(predicate::str::contains(sentinel).not()),
        );
}

#[cfg(unix)]
#[test]
fn safety_scan_redacts_a_real_utf8_path_equal_to_the_non_utf8_sentinel() {
    let temp = tempfile::tempdir().expect("temp repo");
    let relative = ".memzoi/memory/<non-utf8-git-path>";
    let path = temp.path().join(relative);
    fs::create_dir_all(path.parent().expect("memory parent")).expect("memory directory");
    fs::write(
        &path,
        safety_record_markdown(
            "ghp_UTF8_PATH_SENTINEL_0123456789abcdefghijklmnop",
            Some("general_repo_knowledge"),
        ),
    )
    .expect("UTF-8 sentinel-looking path fixture");

    let mut command = memzoi();
    command
        .current_dir(temp.path())
        .args(["safety", "scan", "--file", relative, "--json"])
        .assert()
        .code(2)
        .stdout(
            predicate::str::contains("\"allowed\": false")
                .and(predicate::str::contains("<redacted-path:"))
                .and(predicate::str::contains("<non-utf8-git-path>").not()),
        );
}

#[test]
fn staged_and_range_scans_block_contextually_prohibited_records() {
    use std::process::Command as StdCommand;

    let temp = tempfile::tempdir().expect("temp repo");
    StdCommand::new("git")
        .args(["init", "--quiet"])
        .current_dir(temp.path())
        .status()
        .expect("initialize git repository");
    StdCommand::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp.path())
        .status()
        .expect("configure git email");
    StdCommand::new("git")
        .args(["config", "user.name", "Memzoi Test"])
        .current_dir(temp.path())
        .status()
        .expect("configure git name");
    StdCommand::new("git")
        .args(["commit", "--allow-empty", "--quiet", "-m", "base"])
        .current_dir(temp.path())
        .status()
        .expect("create base commit");

    let path_sentinel = "private-context-filename-sentinel";
    let relative = format!(".memzoi/records/{path_sentinel}.md");
    let path = temp.path().join(&relative);
    fs::create_dir_all(path.parent().expect("record parent")).expect("records directory");
    fs::write(
        &path,
        safety_record_markdown("Lexically harmless transcript.", Some("raw_transcript")),
    )
    .expect("raw transcript record");
    StdCommand::new("git")
        .args(["add", &relative])
        .current_dir(temp.path())
        .status()
        .expect("stage contextual fixture");

    let mut staged = memzoi();
    staged
        .current_dir(temp.path())
        .args(["safety", "scan", "--staged", "--json"])
        .assert()
        .code(2)
        .stdout(
            predicate::str::contains("raw_transcript")
                .and(predicate::str::contains("<redacted-path:"))
                .and(predicate::str::contains(path_sentinel).not()),
        );

    StdCommand::new("git")
        .args(["commit", "--quiet", "-m", "contextual fixture"])
        .current_dir(temp.path())
        .status()
        .expect("commit contextual fixture");
    let mut range = memzoi();
    range
        .current_dir(temp.path())
        .args(["safety", "scan", "--range", "HEAD^...HEAD", "--json"])
        .assert()
        .code(2)
        .stdout(
            predicate::str::contains("raw_transcript")
                .and(predicate::str::contains("<redacted-path:"))
                .and(predicate::str::contains(path_sentinel).not()),
        );
}

#[cfg(unix)]
#[test]
fn staged_and_range_scans_block_managed_git_type_changes() {
    use std::{os::unix::fs::symlink, process::Command as StdCommand};

    for entry_kind in ["symlink", "gitlink"] {
        let temp = tempfile::tempdir().expect("temp repo");
        for args in [
            &["init", "--quiet"][..],
            &["config", "user.email", "test@example.com"][..],
            &["config", "user.name", "Memzoi Test"][..],
        ] {
            assert!(
                StdCommand::new("git")
                    .args(args)
                    .current_dir(temp.path())
                    .status()
                    .expect("run git setup")
                    .success()
            );
        }
        let relative = ".memzoi/records/type-change.md";
        let path = temp.path().join(relative);
        fs::create_dir_all(path.parent().expect("record parent")).expect("records directory");
        fs::write(
            &path,
            safety_record_markdown("Safe baseline.", Some("general_repo_knowledge")),
        )
        .expect("baseline record");
        assert!(
            StdCommand::new("git")
                .args(["add", relative])
                .current_dir(temp.path())
                .status()
                .expect("stage baseline")
                .success()
        );
        assert!(
            StdCommand::new("git")
                .args(["commit", "--quiet", "-m", "baseline"])
                .current_dir(temp.path())
                .status()
                .expect("commit baseline")
                .success()
        );

        if entry_kind == "symlink" {
            fs::remove_file(&path).expect("remove regular record");
            symlink("outside-target", &path).expect("replace record with symlink");
            assert!(
                StdCommand::new("git")
                    .args(["add", relative])
                    .current_dir(temp.path())
                    .status()
                    .expect("stage symlink")
                    .success()
            );
        } else {
            let head = StdCommand::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(temp.path())
                .output()
                .expect("read baseline commit");
            assert!(head.status.success());
            let oid = String::from_utf8(head.stdout).expect("commit ID is UTF-8");
            assert!(
                StdCommand::new("git")
                    .args([
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("160000,{},{}", oid.trim(), relative),
                    ])
                    .current_dir(temp.path())
                    .status()
                    .expect("stage gitlink")
                    .success()
            );
        }

        let mut staged = memzoi();
        staged
            .current_dir(temp.path())
            .args(["safety", "scan", "--staged", "--json"])
            .assert()
            .code(2)
            .stdout(predicate::str::contains("\"allowed\": false"));

        assert!(
            StdCommand::new("git")
                .args(["commit", "--quiet", "-m", entry_kind])
                .current_dir(temp.path())
                .status()
                .expect("commit type change")
                .success()
        );
        let mut range = memzoi();
        range
            .current_dir(temp.path())
            .args(["safety", "scan", "--range", "HEAD^...HEAD", "--json"])
            .assert()
            .code(2)
            .stdout(predicate::str::contains("\"allowed\": false"));
    }
}

#[test]
fn range_scan_rejects_unsafe_revision_tokens_before_invoking_git() {
    for range in ["-p...HEAD", "HEAD...-p", "HEAD ^...HEAD", "HEAD...HEAD\n"] {
        let mut command = memzoi();
        command
            .args(["safety", "scan", &format!("--range={range}"), "--json"])
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "--range contains an unsafe Git revision token",
            ));
    }
}

#[test]
fn repository_content_class_cli_defaults_fail_closed() {
    let mut propose = memzoi();
    propose
        .args(["propose", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[default: unknown]"));

    let mut supersede = memzoi();
    supersede
        .args(["supersede", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[default: unknown]"));
}

#[cfg(unix)]
#[test]
fn staged_safety_scan_blocks_non_utf8_git_paths_with_exit_two() {
    use std::io::Write;
    use std::process::{Command as StdCommand, Stdio};

    let temp = tempfile::tempdir().expect("temp repo");
    StdCommand::new("git")
        .args(["init", "--quiet"])
        .current_dir(temp.path())
        .status()
        .expect("initialize git repository");
    let records = temp.path().join(".memzoi/records");
    fs::create_dir_all(&records).expect("records directory");
    let object = StdCommand::new("git")
        .args(["hash-object", "-w", "--stdin"])
        .current_dir(temp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .expect("hash-object stdin")
                .write_all(b"General repository knowledge.\n")?;
            child.wait_with_output()
        })
        .expect("write Git blob");
    assert!(object.status.success());
    let object_id = String::from_utf8(object.stdout).expect("object ID is UTF-8");
    let mut unrelated_entry = format!("100644 {}\tassets/invalid-", object_id.trim()).into_bytes();
    unrelated_entry.push(0xff);
    unrelated_entry.extend_from_slice(b".bin\0");
    let mut update_unrelated = StdCommand::new("git")
        .args(["update-index", "-z", "--index-info"])
        .current_dir(temp.path())
        .stdin(Stdio::piped())
        .spawn()
        .expect("start unrelated update-index");
    update_unrelated
        .stdin
        .take()
        .expect("unrelated update-index stdin")
        .write_all(&unrelated_entry)
        .expect("write unrelated raw index entry");
    assert!(
        update_unrelated
            .wait()
            .expect("stage unrelated raw index entry")
            .success()
    );

    let mut unrelated_scan = memzoi();
    unrelated_scan
        .current_dir(temp.path())
        .args(["safety", "scan", "--staged", "--json"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"allowed\": true")
                .and(predicate::str::contains("<non-utf8-git-path>").not()),
        );

    let mut index_entry =
        format!("100644 {}\t.memzoi/records/invalid-", object_id.trim()).into_bytes();
    index_entry.push(0xff);
    index_entry.extend_from_slice(b".md\0");
    let mut update_index = StdCommand::new("git")
        .args(["update-index", "-z", "--index-info"])
        .current_dir(temp.path())
        .stdin(Stdio::piped())
        .spawn()
        .expect("start update-index");
    update_index
        .stdin
        .take()
        .expect("update-index stdin")
        .write_all(&index_entry)
        .expect("write raw index entry");
    assert!(
        update_index
            .wait()
            .expect("stage raw index entry")
            .success()
    );

    let mut command = memzoi();
    command
        .current_dir(temp.path())
        .args(["safety", "scan", "--staged", "--json"])
        .assert()
        .code(2)
        .stdout(
            predicate::str::contains("\"allowed\": false")
                .and(predicate::str::contains("invalid_encoding"))
                .and(predicate::str::contains(
                    "\"path\": \".memzoi/memory/<non-utf8-git-path>\"",
                ))
                .and(predicate::str::contains("invalid-").not()),
        );
}
