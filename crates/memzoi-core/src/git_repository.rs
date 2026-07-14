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
        .args(["worktree", "list", "--porcelain"]);
    configure_discovery_command(&mut command);
    let output = command.output().context("failed to list Git worktrees")?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .context("Git worktree discovery returned non-UTF-8 output")?;
    let mut roots = Vec::new();
    for line in stdout.lines() {
        let Some(path) = line.strip_prefix("worktree ") else {
            continue;
        };
        let path = PathBuf::from(path);
        if let Ok(path) = path.canonicalize() {
            roots.push(path);
        }
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
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
