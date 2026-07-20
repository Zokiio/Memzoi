use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use memzoi_core::{
    MaintenancePlan, MemoryPaths, PRIVATE_LIFECYCLE_MAX_ARTIFACT_BYTES,
    PrivateLifecycleApplyService, PrivateLifecycleService, discover_paths, parse_maintenance_plan,
    parse_private_lifecycle_request, parse_strict_lifecycle_artifact, runtime_home,
};
use serde_json::Value;

use super::normalize_absolute_path;
use crate::output::print_json;

pub(super) fn maintenance_enable_command(as_json: bool) -> Result<()> {
    let service = PrivateLifecycleService::open_paths_for_authority(lifecycle_paths()?)?;
    print_maintenance_result(service.enable_private_maintenance()?, as_json)
}

pub(super) fn maintenance_disable_command(as_json: bool) -> Result<()> {
    let service = PrivateLifecycleService::open_paths_for_authority(lifecycle_paths()?)?;
    print_maintenance_result(service.disable_private_maintenance()?, as_json)
}

pub(super) fn maintenance_reconcile_command(as_json: bool) -> Result<()> {
    let service = PrivateLifecycleService::open_paths_for_authority(lifecycle_paths()?)?;
    print_maintenance_result(service.reconcile_private_maintenance()?, as_json)
}

pub(super) fn maintenance_inspect_command(as_json: bool) -> Result<()> {
    let service = PrivateLifecycleService::open_paths_for_read(lifecycle_paths()?)?;
    let inspection = service.inspect_private_maintenance()?;
    if as_json {
        print_json(&serde_json::to_value(&inspection)?)
    } else {
        println!("private-maintenance");
        println!("state\t{}", json_enum(inspection.projection.state)?);
        println!("members\t{}", inspection.projection.member_count);
        println!("edges\t{}", inspection.projection.edge_count);
        Ok(())
    }
}

fn print_maintenance_result(
    result: memzoi_core::PrivateMaintenanceResult,
    as_json: bool,
) -> Result<()> {
    if as_json {
        print_json(&serde_json::to_value(&result)?)
    } else {
        println!("private-maintenance");
        println!("outcome\t{}", json_enum(result.outcome)?);
        println!("state\t{}", json_enum(result.projection.state)?);
        println!("members\t{}", result.projection.member_count);
        println!("edges\t{}", result.projection.edge_count);
        Ok(())
    }
}

fn json_enum(value: impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_value(value)?
        .as_str()
        .unwrap_or("unknown")
        .to_owned())
}

pub(super) fn plan_command(
    record_ids: Vec<String>,
    evaluated_at: Option<String>,
    output: Option<PathBuf>,
    as_json: bool,
) -> Result<()> {
    let paths = lifecycle_paths()?;
    let service = PrivateLifecycleService::open_paths_for_read(paths.clone())?;
    let plan = service.plan_private_lifecycle(record_ids, evaluated_at)?;
    let value =
        serde_json::to_value(&plan).context("failed to serialize private lifecycle plan")?;
    if let Some(destination) = output.as_deref() {
        write_private_plan(&value, destination, &paths)?;
    }
    if as_json {
        print_json(&value)
    } else {
        println!("private-lifecycle-plan");
        println!("plan_id\t{}", plan.plan_id);
        println!("evaluated_at\t{}", plan.evaluated_at);
        println!("not_after\t{}", plan.not_after);
        println!("records\t{}", plan.summary.records);
        println!("action_candidates\t{}", plan.summary.action_candidates);
        Ok(())
    }
}

pub(super) fn authorize_command(
    request_file: PathBuf,
    plan_file: Option<PathBuf>,
    expires_at: Option<String>,
    as_json: bool,
) -> Result<()> {
    let request = parse_private_lifecycle_request(&read_artifact(&request_file, "request")?)?;
    let plan = plan_file
        .as_deref()
        .map(|path| read_plan(path, "plan"))
        .transpose()?;
    let service = PrivateLifecycleService::open_paths_for_authority(lifecycle_paths()?)?;
    let grant =
        service.authorize_private_lifecycle(&request, plan.as_ref(), expires_at.as_deref())?;
    if as_json {
        print_json(&serde_json::to_value(&grant)?)
    } else {
        println!("private-lifecycle-grant");
        println!("grant_id\t{}", grant.grant_id);
        println!("request_id\t{}", grant.request_id);
        println!("state\t{}", grant.state.as_str());
        println!("authorized_at\t{}", grant.authorized_at);
        println!("expires_at\t{}", grant.expires_at);
        Ok(())
    }
}

pub(super) fn revoke_command(grant_id: &str, as_json: bool) -> Result<()> {
    let service = PrivateLifecycleService::open_paths_for_authority(lifecycle_paths()?)?;
    let result = service.revoke_private_lifecycle(grant_id)?;
    if as_json {
        print_json(&serde_json::to_value(&result)?)
    } else {
        println!("private-lifecycle-revoke");
        println!("grant_id\t{}", result.grant_id);
        println!(
            "outcome\t{}",
            serde_json::to_value(result.outcome)?
                .as_str()
                .unwrap_or("unknown")
        );
        Ok(())
    }
}

pub(super) fn inspect_record_command(record_id: &str, as_json: bool) -> Result<()> {
    let service = PrivateLifecycleService::open_paths_for_read(lifecycle_paths()?)?;
    let inspection = service.inspect_private_lifecycle_record(record_id)?;
    if as_json {
        print_json(&serde_json::to_value(&inspection)?)
    } else {
        println!("private-lifecycle-record");
        println!("record_id\t{}", inspection.record.id);
        println!("version\t{}", inspection.version);
        println!("status\t{}", inspection.record.status.as_str());
        println!("quarantined\t{}", inspection.state.quarantined);
        println!("pinned\t{}", inspection.state.pinned);
        println!("title\t{}", inspection.record.title);
        println!("body\t{}", inspection.record.body);
        Ok(())
    }
}

pub(super) fn inspect_grant_command(grant_id: &str, as_json: bool) -> Result<()> {
    let service = PrivateLifecycleService::open_paths_for_read(lifecycle_paths()?)?;
    let grant = service.inspect_private_lifecycle_grant(grant_id)?;
    if as_json {
        print_json(&serde_json::to_value(&grant)?)
    } else {
        println!("private-lifecycle-grant");
        println!("grant_id\t{}", grant.grant_id);
        println!("request_id\t{}", grant.request_id);
        println!("state\t{}", grant.state.as_str());
        println!("authorized_at\t{}", grant.authorized_at);
        println!("expires_at\t{}", grant.expires_at);
        Ok(())
    }
}

pub(super) fn apply_command(
    request_file: PathBuf,
    grant_id: &str,
    plan_file: Option<PathBuf>,
    as_json: bool,
) -> Result<()> {
    let request = parse_private_lifecycle_request(&read_artifact(&request_file, "request")?)?;
    let plan = plan_file
        .as_deref()
        .map(|path| read_plan(path, "plan"))
        .transpose()?;
    let service = PrivateLifecycleApplyService::open_paths(lifecycle_paths()?)?;
    let result = service.apply_private_lifecycle(&request, grant_id, plan.as_ref())?;
    if as_json {
        print_json(&serde_json::to_value(&result)?)
    } else {
        println!("private-lifecycle-application");
        println!("application_id\t{}", result.application_id);
        println!("operation_id\t{}", result.operation_id);
        println!("grant_id\t{}", result.grant_id);
        println!("lifecycle_generation\t{}", result.lifecycle_generation);
        println!("replayed\t{}", result.replayed);
        Ok(())
    }
}

fn lifecycle_paths() -> Result<MemoryPaths> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    discover_paths(&cwd)
}

fn read_plan(path: &Path, label: &str) -> Result<MaintenancePlan> {
    let input = read_artifact(path, label)?;
    let value: Value = parse_strict_lifecycle_artifact(&input, label)?;
    let normalized = serde_json::to_string(&value)?;
    parse_maintenance_plan(&normalized)
}

fn read_artifact(path: &Path, label: &str) -> Result<String> {
    let file = open_regular_lifecycle_artifact(path, label)?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect opened private lifecycle {label} artifact"))?;
    ensure!(
        metadata.len() <= PRIVATE_LIFECYCLE_MAX_ARTIFACT_BYTES as u64,
        "private lifecycle {label} artifact exceeds the 2 MiB limit"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((PRIVATE_LIFECYCLE_MAX_ARTIFACT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read private lifecycle {label} artifact"))?;
    ensure!(
        bytes.len() <= PRIVATE_LIFECYCLE_MAX_ARTIFACT_BYTES,
        "private lifecycle {label} artifact exceeds the 2 MiB limit"
    );
    String::from_utf8(bytes)
        .with_context(|| format!("private lifecycle {label} artifact must be UTF-8"))
}

#[cfg(unix)]
fn open_regular_lifecycle_artifact(path: &Path, label: &str) -> Result<fs::File> {
    use rustix::fs::{Mode, OFlags, openat};

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to read current directory")?
            .join(path)
    };
    let parent = absolute
        .parent()
        .context("private lifecycle artifact has no parent")?
        .canonicalize()
        .with_context(|| format!("failed to resolve private lifecycle {label} artifact parent"))?;
    let directory = open_real_directory(&parent)?;
    let file_name = absolute
        .file_name()
        .context("private lifecycle artifact has no file name")?;
    let file = openat(
        &directory,
        file_name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .with_context(|| {
        format!("failed to open private lifecycle {label} artifact without following symlinks")
    })?;
    let file = fs::File::from(file);
    ensure!(
        file.metadata()
            .with_context(|| format!(
                "failed to inspect opened private lifecycle {label} artifact"
            ))?
            .is_file(),
        "private lifecycle {label} artifact must be a regular, non-symlink file"
    );
    Ok(file)
}

#[cfg(not(unix))]
fn open_regular_lifecycle_artifact(_path: &Path, _label: &str) -> Result<fs::File> {
    bail!("secure private lifecycle artifact reads are unavailable on this platform")
}

fn write_private_plan(plan: &Value, destination: &Path, paths: &MemoryPaths) -> Result<()> {
    let destination = private_plan_target(destination, paths)?;
    let mut bytes = serde_json::to_vec_pretty(plan)?;
    bytes.push(b'\n');
    ensure!(
        bytes.len() <= PRIVATE_LIFECYCLE_MAX_ARTIFACT_BYTES,
        "serialized private lifecycle plan exceeds the 2 MiB limit"
    );
    install_private_plan_no_clobber(&destination, &bytes)
}

#[cfg(unix)]
fn install_private_plan_no_clobber(destination: &Path, bytes: &[u8]) -> Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags, fsync, linkat, openat, unlinkat};

    let parent = destination
        .parent()
        .context("private lifecycle plan destination has no parent")?;
    let directory = open_real_directory(parent)?;
    let file_name = destination
        .file_name()
        .context("private lifecycle plan destination has no file name")?;
    let staged_name = format!(".memzoi-lifecycle-plan-{}.tmp", uuid::Uuid::now_v7());
    let staged = openat(
        &directory,
        staged_name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .context("failed to stage private lifecycle plan safely")?;
    let mut staged = fs::File::from(staged);
    if let Err(error) = staged.write_all(bytes).and_then(|_| staged.sync_all()) {
        drop(staged);
        let _ = unlinkat(&directory, staged_name.as_str(), AtFlags::empty());
        return Err(error).context("failed to write private lifecycle plan");
    }
    drop(staged);
    if let Err(error) = linkat(
        &directory,
        staged_name.as_str(),
        &directory,
        file_name,
        AtFlags::empty(),
    ) {
        let _ = unlinkat(&directory, staged_name.as_str(), AtFlags::empty());
        if error == rustix::io::Errno::EXIST {
            bail!("private lifecycle plan destination already exists");
        }
        return Err(error).context("failed to atomically install private lifecycle plan");
    }
    unlinkat(&directory, staged_name.as_str(), AtFlags::empty())
        .context("failed to remove private lifecycle plan staging file")?;
    fsync(&directory).context("failed to sync private lifecycle plan directory")?;
    Ok(())
}

#[cfg(unix)]
fn open_real_directory(path: &Path) -> Result<std::os::fd::OwnedFd> {
    use rustix::fs::{CWD, Mode, OFlags, openat};

    ensure!(
        path.is_absolute(),
        "private lifecycle plan parent must be absolute"
    );
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = openat(CWD, Path::new("/"), flags, Mode::empty())
        .context("failed to open filesystem root safely")?;
    for component in path.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(component) => {
                directory = openat(&directory, component, flags, Mode::empty())
                    .context("private lifecycle plan parent must be a real existing directory")?;
            }
            _ => bail!("private lifecycle plan parent contains an unsafe component"),
        }
    }
    Ok(directory)
}

#[cfg(not(unix))]
fn install_private_plan_no_clobber(_destination: &Path, _bytes: &[u8]) -> Result<()> {
    bail!("secure private lifecycle plan writes are unavailable on this platform")
}

fn private_plan_target(destination: &Path, paths: &MemoryPaths) -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let absolute = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        cwd.join(destination)
    };
    let parent = absolute
        .parent()
        .context("private lifecycle plan destination has no parent")?;
    let resolved = parent
        .canonicalize()
        .context("failed to resolve private lifecycle plan destination parent")?
        .join(
            absolute
                .file_name()
                .context("private lifecycle plan destination has no file name")?,
        );
    let worktree = paths
        .project_root
        .canonicalize()
        .unwrap_or_else(|_| normalize_absolute_path(&paths.project_root));
    ensure!(
        !resolved.starts_with(worktree),
        "private lifecycle plans cannot be saved inside the Git worktree"
    );
    for protected in [
        runtime_home(),
        paths.repository_runtime_dir.clone(),
        paths.memory_dir.clone(),
    ] {
        let protected = protected
            .canonicalize()
            .unwrap_or_else(|_| normalize_absolute_path(&protected));
        ensure!(
            !resolved.starts_with(protected),
            "private lifecycle plans cannot be saved under Memzoi-managed runtime state"
        );
    }
    Ok(resolved)
}
