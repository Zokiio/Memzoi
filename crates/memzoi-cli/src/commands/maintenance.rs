use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use memzoi_core::{
    MAINTENANCE_REQUEST_SCHEMA, MaintenancePlan, MaintenancePlanRequest, MemoryPaths,
    discover_paths, plan_maintenance, runtime_home,
};

use super::normalize_absolute_path;
use crate::output::print_json;

const MAINTENANCE_ARTIFACT_MAX_BYTES: usize = 2 * 1024 * 1024;

pub(super) fn plan_command(
    record_ids: Vec<String>,
    evaluated_at: Option<String>,
    output: Option<PathBuf>,
    as_json: bool,
) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let paths = discover_paths(&cwd)?;
    let plan = plan_maintenance(
        &paths,
        MaintenancePlanRequest {
            schema: MAINTENANCE_REQUEST_SCHEMA.to_owned(),
            evaluated_at,
            record_ids,
        },
    )?;
    let serialized = serde_json::to_value(&plan).context("failed to serialize maintenance plan")?;

    if let Some(destination) = output.as_deref() {
        write_maintenance_artifact(&serialized, destination, &paths)?;
    }

    if as_json {
        print_json(&serialized)
    } else {
        print_maintenance_human(&plan);
        Ok(())
    }
}

fn print_maintenance_human(plan: &MaintenancePlan) {
    println!("maintenance-plan");
    println!("plan_id\t{}", plan.plan_id);
    println!("evaluated_at\t{}", plan.evaluated_at);
    println!("not_after\t{}", plan.not_after);
    println!("records\t{}", plan.summary.records);
    println!("exact_duplicates\t{}", plan.summary.exact_duplicates);
    println!("contradictions\t{}", plan.summary.contradictions);
    println!("stale\t{}", plan.summary.stale);
    println!("expired\t{}", plan.summary.expired);
    println!("renewal_candidates\t{}", plan.summary.renewal_candidates);
    println!("action_candidates\t{}", plan.summary.action_candidates);
}

fn maintenance_json_bytes(plan: &serde_json::Value) -> Result<Vec<u8>> {
    let mut bytes =
        serde_json::to_vec_pretty(plan).context("failed to serialize maintenance plan")?;
    bytes.push(b'\n');
    if bytes.len() > MAINTENANCE_ARTIFACT_MAX_BYTES {
        bail!("serialized maintenance plan exceeds the 2 MiB artifact limit");
    }
    Ok(bytes)
}

fn write_maintenance_artifact(
    plan: &serde_json::Value,
    destination: &Path,
    paths: &MemoryPaths,
) -> Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("maintenance artifact destination must not be a symlink")
        }
        Ok(_) => bail!("maintenance artifact destination already exists"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).context("failed to inspect maintenance artifact destination");
        }
    }

    let requested_parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let metadata = fs::symlink_metadata(requested_parent)
        .context("failed to inspect maintenance artifact destination parent")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("maintenance artifact destination parent must be a real existing directory");
    }

    let destination = maintenance_artifact_target(destination, paths)?;
    let parent = destination
        .parent()
        .context("maintenance artifact destination has no parent")?;

    let bytes = maintenance_json_bytes(plan)?;
    let mut staged =
        tempfile::NamedTempFile::new_in(parent).context("failed to stage maintenance artifact")?;
    staged
        .write_all(&bytes)
        .and_then(|_| staged.as_file().sync_all())
        .context("failed to write maintenance artifact")?;
    staged.persist_noclobber(&destination).map_err(|error| {
        anyhow::Error::new(error.error).context("failed to install maintenance artifact")
    })?;
    Ok(())
}

fn maintenance_artifact_target(destination: &Path, paths: &MemoryPaths) -> Result<PathBuf> {
    let current = std::env::current_dir().context("failed to read current directory")?;
    let absolute = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        current.join(destination)
    };
    let parent = absolute
        .parent()
        .context("maintenance artifact destination has no parent")?;
    let canonical_parent = parent
        .canonicalize()
        .context("failed to resolve maintenance artifact destination parent")?;
    let file_name = absolute
        .file_name()
        .context("maintenance artifact destination has no file name")?;
    let resolved_destination = canonical_parent.join(file_name);

    let worktree_root = resolved_boundary(&paths.project_root, &current)?;
    if resolved_destination.starts_with(&worktree_root) {
        bail!("maintenance artifacts cannot be saved inside the Git worktree");
    }

    for protected in [
        runtime_home(),
        paths.repository_runtime_dir.clone(),
        paths.memory_dir.clone(),
    ] {
        let protected = resolved_boundary(&protected, &current)?;
        if resolved_destination.starts_with(protected) {
            bail!("maintenance artifacts cannot be saved under Memzoi-managed runtime state");
        }
    }

    Ok(resolved_destination)
}

fn resolved_boundary(path: &Path, current: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current.join(path)
    };
    let normalized = normalize_absolute_path(&absolute);
    let mut existing_ancestor = normalized.clone();
    let mut missing_suffix = Vec::new();
    loop {
        match existing_ancestor.canonicalize() {
            Ok(mut resolved) => {
                for component in missing_suffix.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = existing_ancestor
                    .file_name()
                    .context("maintenance artifact boundary has no existing ancestor")?
                    .to_os_string();
                missing_suffix.push(component);
                if !existing_ancestor.pop() {
                    bail!("maintenance artifact boundary has no existing ancestor");
                }
            }
            Err(error) => {
                return Err(error).context("failed to resolve maintenance artifact boundary");
            }
        }
    }
}
