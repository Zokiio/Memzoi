use std::{
    collections::BTreeSet,
    ffi::OsStr,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitRepositoryIdentity {
    pub worktree_root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

/// The Git facts that determine whether a canonical record is review-visible.
///
/// Both `Tracked` and `UntrackedAndNotIgnored` are eligible for review. Callers
/// must fail closed for every [`GitReviewVisibilityError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitReviewVisibility {
    Tracked,
    UntrackedAndNotIgnored,
    IgnoredUntracked,
}

impl GitReviewVisibility {
    pub(crate) const fn is_review_visible(self) -> bool {
        !matches!(self, Self::IgnoredUntracked)
    }
}

/// The Git command whose result could not establish record review visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitReviewVisibilityCommand {
    Worktree,
    Tracked,
    Ignore,
}

/// Fail-closed errors from [`git_review_visibility`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum GitReviewVisibilityError {
    #[error("Git review visibility requires a real worktree")]
    NotAWorktree,
    #[error("Git review visibility requires a canonical project-relative record path")]
    InvalidRecordPath,
    #[error("failed to execute Git {operation:?} while determining review visibility: {message}")]
    GitExecution {
        operation: GitReviewVisibilityCommand,
        message: String,
    },
    #[error("Git {operation:?} output exceeded the review-visibility safety limit")]
    OutputLimitExceeded {
        operation: GitReviewVisibilityCommand,
    },
    #[error("Git {operation:?} returned unexpected output while determining review visibility")]
    UnexpectedOutput {
        operation: GitReviewVisibilityCommand,
    },
    #[error(
        "Git {operation:?} failed with status {status:?} while determining review visibility: {stderr}"
    )]
    GitCommandFailed {
        operation: GitReviewVisibilityCommand,
        status: Option<i32>,
        stderr: String,
    },
}

const MAX_GIT_REVIEW_VISIBILITY_OUTPUT_BYTES: usize = 8 * 1024;
const MAX_GIT_MAINTENANCE_OUTPUT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitMaintenanceTargetReview {
    pub(crate) tracked_paths: Vec<PathBuf>,
    pub(crate) untracked_paths: Vec<PathBuf>,
}

/// Verifies that only the selected canonical paths are clean enough for a new
/// maintenance projection. This intentionally does not inspect unrelated dirt.
pub(crate) fn git_maintenance_targets_clean(
    project_root: &Path,
    paths: &[PathBuf],
) -> Result<GitMaintenanceTargetReview> {
    if paths.is_empty()
        || paths
            .iter()
            .any(|path| !is_canonical_project_relative_path(path))
    {
        bail!("maintenance Git inspection requires canonical selected paths");
    }
    let mut canonical = paths.to_vec();
    canonical.sort();
    canonical.dedup();
    if canonical.len() != paths.len() {
        bail!("maintenance Git inspection paths must be unique");
    }

    let mut status_args = vec![
        OsStr::new("--no-optional-locks"),
        OsStr::new("-c"),
        OsStr::new("core.fsmonitor=false"),
        OsStr::new("status"),
        OsStr::new("--porcelain=v2"),
        OsStr::new("-z"),
        OsStr::new("--untracked-files=all"),
        OsStr::new("--ignored=matching"),
        OsStr::new("--no-renames"),
        OsStr::new("--"),
    ];
    status_args.extend(canonical.iter().map(|path| path.as_os_str()));
    let status = run_git_maintenance_command(project_root, &status_args)?;
    if !status.status.success() {
        bail!(
            "maintenance Git status failed: {}",
            bounded_stderr(&status.stderr)
        );
    }

    let mut untracked = BTreeSet::new();
    for entry in status
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        match entry.first().copied() {
            Some(b'?') => {
                let path = entry
                    .strip_prefix(b"? ")
                    .context("maintenance Git status returned malformed untracked output")?;
                untracked.insert(matched_maintenance_path(path, &canonical)?);
            }
            Some(b'!') => bail!("maintenance target is ignored by Git"),
            Some(b'u') => bail!("maintenance target has an unresolved Git conflict"),
            Some(b'1' | b'2') => {
                let xy = entry
                    .get(2..4)
                    .context("maintenance Git status returned malformed tracked output")?;
                if xy != b".." {
                    bail!("maintenance target has staged or unstaged Git changes");
                }
            }
            _ => bail!("maintenance Git status returned unexpected output"),
        }
    }

    let mut index_args = vec![
        OsStr::new("--no-optional-locks"),
        OsStr::new("-c"),
        OsStr::new("core.fsmonitor=false"),
        OsStr::new("ls-files"),
        OsStr::new("--stage"),
        OsStr::new("-z"),
        OsStr::new("--"),
    ];
    index_args.extend(canonical.iter().map(|path| path.as_os_str()));
    let index = run_git_maintenance_command(project_root, &index_args)?;
    if !index.status.success() {
        bail!(
            "maintenance Git index inspection failed: {}",
            bounded_stderr(&index.stderr)
        );
    }

    let mut tracked = BTreeSet::new();
    for entry in index
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .context("maintenance Git index inspection returned malformed output")?;
        let (metadata, path_with_tab) = entry.split_at(tab);
        let path = &path_with_tab[1..];
        let fields = metadata.split(|byte| *byte == b' ').collect::<Vec<_>>();
        if fields.len() != 3 || fields[2] != b"0" || fields[1].iter().all(|byte| *byte == b'0') {
            bail!("maintenance target has an unsupported Git index entry");
        }
        let path = matched_maintenance_path(path, &canonical)?;
        if !canonical.binary_search(&path).is_ok() || !tracked.insert(path) {
            bail!("maintenance Git index inspection returned an unexpected path");
        }
    }

    let untracked_or_absent = canonical
        .iter()
        .filter(|path| !tracked.contains(*path))
        .collect::<Vec<_>>();
    if !untracked_or_absent.is_empty() {
        let mut ignore_args = vec![
            OsStr::new("--no-optional-locks"),
            OsStr::new("-c"),
            OsStr::new("core.fsmonitor=false"),
            OsStr::new("check-ignore"),
            OsStr::new("--no-index"),
            OsStr::new("--"),
        ];
        ignore_args.extend(untracked_or_absent.iter().map(|path| path.as_os_str()));
        let ignored = run_git_maintenance_command(project_root, &ignore_args)?;
        match ignored.status.code() {
            Some(0) if !ignored.stdout.is_empty() => {
                for path in ignored
                    .stdout
                    .split(|byte| *byte == b'\n')
                    .filter(|entry| !entry.is_empty())
                {
                    let _ = matched_maintenance_path(path, &canonical)?;
                }
                bail!("maintenance target is ignored by Git");
            }
            Some(1) if ignored.stdout.is_empty() => {}
            _ => bail!(
                "maintenance Git ignore inspection failed: {}",
                bounded_stderr(&ignored.stderr)
            ),
        }
    }

    for path in &canonical {
        let absolute = project_root.join(path);
        if tracked.contains(path) && !absolute.exists() {
            bail!("absent maintenance target already has a Git index entry");
        }
        if !tracked.contains(path) && absolute.exists() && !untracked.contains(path) {
            bail!("maintenance target Git visibility is ambiguous");
        }
    }
    Ok(GitMaintenanceTargetReview {
        tracked_paths: tracked.into_iter().collect(),
        untracked_paths: untracked.into_iter().collect(),
    })
}

fn matched_maintenance_path(bytes: &[u8], expected: &[PathBuf]) -> Result<PathBuf> {
    expected
        .iter()
        .find(|path| path.as_os_str().as_encoded_bytes() == bytes)
        .cloned()
        .context("maintenance Git command returned an unexpected path")
}

fn run_git_maintenance_command(
    project_root: &Path,
    arguments: &[&OsStr],
) -> Result<GitReviewCommandOutput> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(project_root)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_discovery_command(&mut command);
    let mut child = command
        .spawn()
        .context("failed to execute maintenance Git inspection")?;
    let stdout = child
        .stdout
        .take()
        .context("maintenance Git stdout is missing")?;
    let stderr = child
        .stderr
        .take()
        .context("maintenance Git stderr is missing")?;
    let stdout_reader = std::thread::spawn(move || {
        collect_bounded_git_output(stdout, MAX_GIT_MAINTENANCE_OUTPUT_BYTES)
    });
    let stderr_reader = std::thread::spawn(move || {
        collect_bounded_git_output(stderr, MAX_GIT_MAINTENANCE_OUTPUT_BYTES)
    });
    let status = child
        .wait()
        .context("failed to wait for maintenance Git inspection")?;
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("maintenance Git stdout reader panicked"))??;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("maintenance Git stderr reader panicked"))??;
    if stdout_truncated || stderr_truncated {
        bail!("maintenance Git output exceeded its safety limit");
    }
    Ok(GitReviewCommandOutput {
        status,
        stdout,
        stderr,
    })
}

fn bounded_stderr(stderr: &[u8]) -> String {
    let value = String::from_utf8_lossy(stderr).trim().to_owned();
    if value.is_empty() {
        "no error output".to_owned()
    } else {
        value
    }
}

/// Determines whether `record_path` is visible to Git review from `project_root`.
///
/// The path must be a canonical POSIX project-relative path. Tracked paths are
/// eligible before ignore matching, because later ignore rules do not make an
/// already tracked canonical record private.
pub(crate) fn git_review_visibility(
    project_root: &Path,
    record_path: &Path,
) -> std::result::Result<GitReviewVisibility, GitReviewVisibilityError> {
    if !is_canonical_project_relative_path(record_path) {
        return Err(GitReviewVisibilityError::InvalidRecordPath);
    }

    let worktree = run_git_review_command(
        project_root,
        GitReviewVisibilityCommand::Worktree,
        &[OsStr::new("rev-parse"), OsStr::new("--is-inside-work-tree")],
    )?;
    if !worktree.status.success() || worktree.stdout != b"true\n" {
        return Err(GitReviewVisibilityError::NotAWorktree);
    }

    let tracked = run_git_review_command(
        project_root,
        GitReviewVisibilityCommand::Tracked,
        &[
            OsStr::new("ls-files"),
            OsStr::new("--cached"),
            OsStr::new("-z"),
            OsStr::new("--"),
            record_path.as_os_str(),
        ],
    )?;
    if !tracked.status.success() {
        return Err(git_review_command_failed(
            GitReviewVisibilityCommand::Tracked,
            tracked,
        ));
    }
    if tracked.stdout.is_empty() {
        // `ls-files` succeeded but found no index entry. Only then may an
        // ignore rule decide the untracked path's visibility.
    } else if tracked
        .stdout
        .strip_suffix(&[0])
        .is_some_and(|path| path == record_path.as_os_str().as_encoded_bytes())
    {
        return Ok(GitReviewVisibility::Tracked);
    } else {
        return Err(GitReviewVisibilityError::UnexpectedOutput {
            operation: GitReviewVisibilityCommand::Tracked,
        });
    }

    let ignored = run_git_review_command(
        project_root,
        GitReviewVisibilityCommand::Ignore,
        &[
            OsStr::new("check-ignore"),
            OsStr::new("--quiet"),
            OsStr::new("--no-index"),
            OsStr::new("--"),
            record_path.as_os_str(),
        ],
    )?;
    match ignored.status.code() {
        Some(0) => Ok(GitReviewVisibility::IgnoredUntracked),
        Some(1) => Ok(GitReviewVisibility::UntrackedAndNotIgnored),
        _ => Err(git_review_command_failed(
            GitReviewVisibilityCommand::Ignore,
            ignored,
        )),
    }
}

struct GitReviewCommandOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn is_canonical_project_relative_path(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    !bytes.is_empty()
        && !path.is_absolute()
        && !bytes.contains(&b'\\')
        && !bytes
            .split(|byte| *byte == b'/')
            .any(|component| component.is_empty() || component == b"." || component == b"..")
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn run_git_review_command(
    project_root: &Path,
    operation: GitReviewVisibilityCommand,
    arguments: &[&OsStr],
) -> std::result::Result<GitReviewCommandOutput, GitReviewVisibilityError> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(project_root)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_discovery_command(&mut command);

    let mut child = command
        .spawn()
        .map_err(|error| GitReviewVisibilityError::GitExecution {
            operation,
            message: error.to_string(),
        })?;
    let stdout = child
        .stdout
        .take()
        .expect("piped Git review command stdout must be available");
    let stderr = child
        .stderr
        .take()
        .expect("piped Git review command stderr must be available");
    let stdout_reader = std::thread::spawn(move || {
        collect_bounded_git_output(stdout, MAX_GIT_REVIEW_VISIBILITY_OUTPUT_BYTES)
    });
    let stderr_reader = std::thread::spawn(move || {
        collect_bounded_git_output(stderr, MAX_GIT_REVIEW_VISIBILITY_OUTPUT_BYTES)
    });

    let status = child
        .wait()
        .map_err(|error| GitReviewVisibilityError::GitExecution {
            operation,
            message: error.to_string(),
        })?;
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| GitReviewVisibilityError::GitExecution {
            operation,
            message: "stdout reader panicked".to_owned(),
        })?
        .map_err(|error| GitReviewVisibilityError::GitExecution {
            operation,
            message: error.to_string(),
        })?;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| GitReviewVisibilityError::GitExecution {
            operation,
            message: "stderr reader panicked".to_owned(),
        })?
        .map_err(|error| GitReviewVisibilityError::GitExecution {
            operation,
            message: error.to_string(),
        })?;
    if stdout_truncated || stderr_truncated {
        return Err(GitReviewVisibilityError::OutputLimitExceeded { operation });
    }

    Ok(GitReviewCommandOutput {
        status,
        stdout,
        stderr,
    })
}

fn collect_bounded_git_output(
    mut reader: impl Read,
    maximum_bytes: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 1024];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok((output, truncated));
        }
        let remaining = maximum_bytes.saturating_sub(output.len());
        let retained = read.min(remaining);
        output.extend_from_slice(&buffer[..retained]);
        truncated |= retained != read;
    }
}

fn git_review_command_failed(
    operation: GitReviewVisibilityCommand,
    output: GitReviewCommandOutput,
) -> GitReviewVisibilityError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    GitReviewVisibilityError::GitCommandFailed {
        operation,
        status: output.status.code(),
        stderr: if stderr.is_empty() {
            "no error output".to_owned()
        } else {
            stderr
        },
    }
}

pub(crate) fn discover_git_repository(start: &Path) -> Result<Option<GitRepositoryIdentity>> {
    let Some(worktree_root) = git_path(start, "--show-toplevel")? else {
        return Ok(None);
    };
    let git_dir =
        git_path(start, "--git-dir")?.context("Git reported a worktree without a Git directory")?;
    let common_dir = git_path(start, "--git-common-dir")?
        .context("Git reported a worktree without a common directory")?;

    Ok(Some(GitRepositoryIdentity {
        worktree_root: canonicalize_git_path(&worktree_root, start, "worktree root")?,
        git_dir: canonicalize_git_path(&git_dir, start, "Git directory")?,
        common_dir: canonicalize_git_path(&common_dir, start, "Git common directory")?,
    }))
}

fn git_path(start: &Path, argument: &str) -> Result<Option<PathBuf>> {
    let mut command = Command::new("git");
    command.arg("-C").arg(start).args(["rev-parse", argument]);
    configure_discovery_command(&mut command);
    let output = match command.output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to execute Git repository discovery"),
    };
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .context("Git repository discovery returned non-UTF-8 output")?;
    let value = stdout.trim();
    if value.is_empty() || value.lines().count() != 1 {
        bail!("Git repository discovery returned an invalid {argument} path");
    }
    Ok(Some(PathBuf::from(value)))
}

fn configure_discovery_command(command: &mut Command) {
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
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
        "GIT_TRACE",
        "GIT_TRACE2",
        "GIT_TRACE2_EVENT",
        "GIT_TRACE_PACKET",
        "GIT_TRACE_PERFORMANCE",
        "GIT_TRACE_SETUP",
        "GIT_TRACE_SHALLOW",
        "GIT_CURL_VERBOSE",
    ] {
        command.env_remove(key);
    }
}

fn canonicalize_git_path(path: &Path, start: &Path, label: &str) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        start.join(path)
    };
    absolute
        .canonicalize()
        .with_context(|| format!("failed to resolve Git {label} {}", absolute.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn discovery_command_ignores_inherited_repository_context() -> Result<()> {
        let temp = tempfile::TempDir::new()?;
        let expected = temp.path().join("expected");
        let contaminating = temp.path().join("contaminating");
        initialize_git_repository(&expected)?;
        initialize_git_repository(&contaminating)?;

        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&expected)
            .args(["rev-parse", "--show-toplevel"])
            .env("GIT_DIR", contaminating.join(".git"))
            .env("GIT_WORK_TREE", &contaminating)
            .env("GIT_COMMON_DIR", contaminating.join(".git"));
        configure_discovery_command(&mut command);
        let output = command.output()?;

        assert!(output.status.success());
        assert_eq!(
            PathBuf::from(std::str::from_utf8(&output.stdout)?.trim()).canonicalize()?,
            expected.canonicalize()?
        );
        Ok(())
    }

    #[test]
    fn review_visibility_requires_a_real_worktree() {
        let temp = tempfile::TempDir::new().unwrap();

        assert_eq!(
            git_review_visibility(temp.path(), Path::new(".memzoi/records/record.md")),
            Err(GitReviewVisibilityError::NotAWorktree)
        );
    }

    #[test]
    fn review_visibility_keeps_tracked_records_eligible_after_ignore_rule() -> Result<()> {
        let temp = tempfile::TempDir::new()?;
        let record = Path::new(".memzoi/records/tracked.md");
        initialize_git_repository(temp.path())?;
        fs::create_dir_all(temp.path().join(".memzoi/records"))?;
        fs::write(temp.path().join(record), "tracked canonical record")?;
        stage_git_path(temp.path(), record)?;
        fs::write(temp.path().join(".gitignore"), ".memzoi/records/*.md\n")?;

        assert_eq!(
            git_review_visibility(temp.path(), record),
            Ok(GitReviewVisibility::Tracked)
        );
        Ok(())
    }

    #[test]
    fn review_visibility_fails_closed_for_untracked_ignored_records() -> Result<()> {
        let temp = tempfile::TempDir::new()?;
        let record = Path::new(".memzoi/records/ignored.md");
        initialize_git_repository(temp.path())?;
        fs::create_dir_all(temp.path().join(".memzoi/records"))?;
        fs::write(temp.path().join(record), "ignored canonical record")?;
        fs::write(temp.path().join(".gitignore"), ".memzoi/records/*.md\n")?;

        assert_eq!(
            git_review_visibility(temp.path(), record),
            Ok(GitReviewVisibility::IgnoredUntracked)
        );
        Ok(())
    }

    #[test]
    fn review_visibility_accepts_untracked_unignored_records() -> Result<()> {
        let temp = tempfile::TempDir::new()?;
        initialize_git_repository(temp.path())?;

        assert_eq!(
            git_review_visibility(
                temp.path(),
                Path::new(".memzoi/records/untracked-but-reviewable.md")
            ),
            Ok(GitReviewVisibility::UntrackedAndNotIgnored)
        );
        Ok(())
    }

    #[test]
    fn review_visibility_rejects_noncanonical_or_absolute_paths() -> Result<()> {
        let temp = tempfile::TempDir::new()?;
        initialize_git_repository(temp.path())?;
        let absolute = temp.path().join(".memzoi/records/record.md");

        for path in [
            Path::new("../record.md"),
            Path::new(".memzoi//records/record.md"),
            absolute.as_path(),
        ] {
            assert_eq!(
                git_review_visibility(temp.path(), path),
                Err(GitReviewVisibilityError::InvalidRecordPath)
            );
        }
        Ok(())
    }

    #[test]
    fn maintenance_git_check_is_path_scoped_and_read_only() -> Result<()> {
        let temp = tempfile::TempDir::new()?;
        let selected = PathBuf::from(".memzoi/records/selected.md");
        initialize_git_repository(temp.path())?;
        fs::create_dir_all(temp.path().join(".memzoi/records"))?;
        fs::write(temp.path().join(&selected), "selected canonical record")?;
        fs::write(temp.path().join("unrelated.md"), "baseline")?;
        stage_git_path(temp.path(), &selected)?;
        stage_git_path(temp.path(), Path::new("unrelated.md"))?;
        commit_git_repository(temp.path())?;
        fs::write(temp.path().join("unrelated.md"), "unrelated dirty work")?;
        let index_before = fs::read(temp.path().join(".git/index"))?;

        let review = git_maintenance_targets_clean(temp.path(), std::slice::from_ref(&selected))?;

        assert_eq!(review.tracked_paths, vec![selected]);
        assert!(review.untracked_paths.is_empty());
        assert_eq!(fs::read(temp.path().join(".git/index"))?, index_before);
        assert_eq!(
            fs::read_to_string(temp.path().join("unrelated.md"))?,
            "unrelated dirty work"
        );
        Ok(())
    }

    #[test]
    fn maintenance_git_check_blocks_dirty_staged_intent_and_ignored_targets() -> Result<()> {
        let selected = PathBuf::from(".memzoi/records/selected.md");

        let dirty = tempfile::TempDir::new()?;
        initialize_git_repository(dirty.path())?;
        fs::create_dir_all(dirty.path().join(".memzoi/records"))?;
        fs::write(dirty.path().join(&selected), "baseline")?;
        stage_git_path(dirty.path(), &selected)?;
        commit_git_repository(dirty.path())?;
        fs::write(dirty.path().join(&selected), "dirty")?;
        assert!(
            git_maintenance_targets_clean(dirty.path(), std::slice::from_ref(&selected)).is_err()
        );

        let staged = tempfile::TempDir::new()?;
        initialize_git_repository(staged.path())?;
        fs::create_dir_all(staged.path().join(".memzoi/records"))?;
        fs::write(staged.path().join(&selected), "staged")?;
        stage_git_path(staged.path(), &selected)?;
        assert!(
            git_maintenance_targets_clean(staged.path(), std::slice::from_ref(&selected)).is_err()
        );

        let intent = tempfile::TempDir::new()?;
        initialize_git_repository(intent.path())?;
        fs::create_dir_all(intent.path().join(".memzoi/records"))?;
        fs::write(intent.path().join(&selected), "intent to add")?;
        let output = Command::new("git")
            .arg("-C")
            .arg(intent.path())
            .args(["add", "-N", "--"])
            .arg(&selected)
            .output()?;
        assert!(output.status.success());
        assert!(
            git_maintenance_targets_clean(intent.path(), std::slice::from_ref(&selected)).is_err()
        );

        let ignored = tempfile::TempDir::new()?;
        initialize_git_repository(ignored.path())?;
        fs::create_dir_all(ignored.path().join(".memzoi/records"))?;
        fs::write(ignored.path().join(".gitignore"), ".memzoi/records/*.md\n")?;
        fs::write(ignored.path().join(&selected), "ignored")?;
        assert!(git_maintenance_targets_clean(ignored.path(), &[selected]).is_err());

        let absent_ignored = tempfile::TempDir::new()?;
        initialize_git_repository(absent_ignored.path())?;
        fs::write(
            absent_ignored.path().join(".gitignore"),
            ".memzoi/records/*.md\n",
        )?;
        let absent = PathBuf::from(".memzoi/records/absent.md");
        assert!(git_maintenance_targets_clean(absent_ignored.path(), &[absent]).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn maintenance_git_check_disables_configured_fsmonitor() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::TempDir::new()?;
        let selected = PathBuf::from(".memzoi/records/selected.md");
        initialize_git_repository(temp.path())?;
        fs::create_dir_all(temp.path().join(".memzoi/records"))?;
        fs::write(temp.path().join(&selected), "selected canonical record")?;
        stage_git_path(temp.path(), &selected)?;
        commit_git_repository(temp.path())?;

        let sentinel = temp.path().join("fsmonitor-invoked");
        let monitor = temp.path().join("fsmonitor-hook.sh");
        fs::write(
            &monitor,
            format!("#!/bin/sh\ntouch '{}'\nprintf '\\n'\n", sentinel.display()),
        )?;
        let mut permissions = fs::metadata(&monitor)?.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&monitor, permissions)?;
        let configured = Command::new("git")
            .arg("-C")
            .arg(temp.path())
            .args(["config", "core.fsmonitor"])
            .arg(&monitor)
            .status()?;
        assert!(configured.success());

        git_maintenance_targets_clean(temp.path(), &[selected])?;
        assert!(!sentinel.exists(), "Git preflight executed core.fsmonitor");
        Ok(())
    }

    fn stage_git_path(repository: &Path, path: &Path) -> Result<()> {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(repository)
            .args(["add", "--"])
            .arg(path);
        configure_discovery_command(&mut command);
        let output = command.output()?;
        if !output.status.success() {
            bail!(
                "failed to stage Git test path: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    fn initialize_git_repository(path: &Path) -> Result<()> {
        let mut command = Command::new("git");
        command
            .args([OsStr::new("init"), OsStr::new("-q")])
            .arg(path);
        configure_discovery_command(&mut command);
        let output = command.output()?;
        if !output.status.success() {
            bail!(
                "failed to initialize Git test repository: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    fn commit_git_repository(path: &Path) -> Result<()> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args([
                "-c",
                "user.name=Memzoi Test",
                "-c",
                "user.email=memzoi-test@example.invalid",
                "commit",
                "-qm",
                "baseline",
            ])
            .output()?;
        if !output.status.success() {
            bail!(
                "failed to commit Git test repository: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }
}
