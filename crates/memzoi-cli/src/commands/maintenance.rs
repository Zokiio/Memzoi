use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use memzoi_core::{
    MAINTENANCE_REQUEST_SCHEMA, MaintenancePlan, MaintenancePlanRequest, MemoryPaths,
    REPOSITORY_MAINTENANCE_MATERIALIZATION_REQUEST_SCHEMA,
    RepositoryMaintenanceMaterializationRequest, discover_paths, parse_maintenance_plan,
    plan_maintenance, runtime_home, validate_repository_maintenance_selection,
};

use super::{normalize_absolute_path, open_service};
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

pub(super) fn materialize_command(
    plan_file: PathBuf,
    plan_id: String,
    mut action_ids: Vec<String>,
    decision_at: String,
    as_json: bool,
) -> Result<()> {
    let plan = read_maintenance_plan(&plan_file)?;
    ensure!(
        plan.plan_id == plan_id,
        "explicit maintenance plan ID does not match the artifact"
    );
    let unique = action_ids.iter().collect::<BTreeSet<_>>();
    ensure!(
        unique.len() == action_ids.len(),
        "duplicate --action-id values are not allowed"
    );
    action_ids.sort();
    let request = RepositoryMaintenanceMaterializationRequest {
        schema: REPOSITORY_MAINTENANCE_MATERIALIZATION_REQUEST_SCHEMA.to_owned(),
        plan_id,
        selected_action_ids: action_ids,
        decision_at,
    };
    request.validate()?;
    validate_repository_maintenance_selection(&plan, &request)?;
    let service = open_service()?;
    let result = service.apply_repository_maintenance_materialization(&plan, &request)?;
    if as_json {
        print_json(&serde_json::to_value(result)?)
    } else {
        println!("repository-maintenance-materialization");
        println!("plan_id\t{}", result.plan_id);
        println!("selection_id\t{}", result.selection_id);
        println!("decision_id\t{}", result.decision_id);
        println!("decision_at\t{}", result.decision_at);
        for output in result.outputs {
            println!(
                "output\t{}\t{}\t{}\t{}\t{}\t{}",
                output.path,
                output.record_id,
                enum_text(output.action)?,
                enum_text(output.role)?,
                output.semantic_revision.revision_hash,
                enum_text(output.outcome)?,
            );
        }
        for command in result.review_commands {
            println!(
                "review\t{}\t{}",
                command.program,
                serde_json::to_string(&command.args)?
            );
        }
        Ok(())
    }
}

fn enum_text(value: impl serde::Serialize) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .context("maintenance result enum did not serialize as a string")
}

fn read_maintenance_plan(path: &Path) -> Result<MaintenancePlan> {
    let file = open_regular_maintenance_artifact(path)?;
    let metadata = file
        .metadata()
        .context("failed to inspect opened maintenance plan artifact")?;
    ensure!(
        metadata.len() <= MAINTENANCE_ARTIFACT_MAX_BYTES as u64,
        "maintenance plan artifact exceeds the 2 MiB limit"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAINTENANCE_ARTIFACT_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("failed to read maintenance plan artifact")?;
    ensure!(
        bytes.len() <= MAINTENANCE_ARTIFACT_MAX_BYTES,
        "maintenance plan artifact exceeds the 2 MiB limit"
    );
    let text = String::from_utf8(bytes).context("maintenance plan artifact must be UTF-8")?;
    parse_maintenance_plan(&text).context("invalid memzoi/maintenance-plan artifact")
}

#[cfg(unix)]
fn open_regular_maintenance_artifact(path: &Path) -> Result<fs::File> {
    use rustix::fs::{CWD, Mode, OFlags, openat};

    let file = openat(
        CWD,
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .context("failed to open maintenance plan artifact without following symlinks")?;
    let file = fs::File::from(file);
    ensure!(
        file.metadata()?.is_file(),
        "maintenance plan artifact must be a regular non-symlink file"
    );
    Ok(file)
}

#[cfg(not(unix))]
fn open_regular_maintenance_artifact(_path: &Path) -> Result<fs::File> {
    bail!("secure maintenance plan artifact reads are unavailable on this platform")
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
