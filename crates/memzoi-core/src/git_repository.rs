use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitRepositoryIdentity {
    pub worktree_root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
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

pub(crate) fn list_git_worktree_roots(start: &Path) -> Result<Vec<PathBuf>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(start)
        .args(["worktree", "list", "--porcelain", "-z"]);
    configure_discovery_command(&mut command);
    let output = command.output().context("failed to list Git worktrees")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Git worktree discovery failed: {}",
            stderr.trim().trim_end_matches('.')
        );
    }
    parse_git_worktree_roots(&output.stdout)
}

fn parse_git_worktree_roots(stdout: &[u8]) -> Result<Vec<PathBuf>> {
    if stdout.last() != Some(&0) {
        bail!("Git worktree discovery returned malformed non-NUL-terminated output");
    }
    let mut roots = Vec::new();
    for field in stdout.split(|byte| *byte == 0) {
        let Some(path) = field.strip_prefix(b"worktree ") else {
            continue;
        };
        if path.is_empty() {
            bail!("Git worktree discovery returned an empty worktree path");
        }
        let path = git_output_path(path)?;
        roots.push(path.canonicalize().unwrap_or(path));
    }
    roots.sort();
    roots.dedup();
    if roots.is_empty() {
        bail!("Git worktree discovery returned no worktrees");
    }
    Ok(roots)
}

#[cfg(unix)]
fn git_output_path(bytes: &[u8]) -> Result<PathBuf> {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn git_output_path(bytes: &[u8]) -> Result<PathBuf> {
    let path = std::str::from_utf8(bytes)
        .context("Git worktree discovery returned a path unsupported by this platform")?;
    Ok(PathBuf::from(path))
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
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn worktree_listing_failure_is_not_treated_as_an_empty_repository() {
        let temp = tempfile::TempDir::new().unwrap();

        let result = list_git_worktree_roots(temp.path());

        assert!(
            result.is_err(),
            "a failed Git command must not look like a repository with no worktrees"
        );
    }

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
    fn nul_porcelain_parser_preserves_newlines_in_worktree_paths() -> Result<()> {
        let roots = parse_git_worktree_roots(
            b"worktree /tmp/memzoi-worktree\nwith-newline\0HEAD deadbeef\0detached\0\0",
        )?;

        assert_eq!(
            roots,
            vec![PathBuf::from(OsStr::new(
                "/tmp/memzoi-worktree\nwith-newline"
            ))]
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn nul_porcelain_parser_preserves_non_utf8_worktree_paths() -> Result<()> {
        use std::os::unix::ffi::OsStrExt;

        let roots = parse_git_worktree_roots(
            b"worktree /tmp/memzoi-worktree-\xff\0HEAD deadbeef\0detached\0\0",
        )?;

        assert_eq!(
            roots[0].as_os_str().as_bytes(),
            b"/tmp/memzoi-worktree-\xff"
        );
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
}
