use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use memzoi_core::{
    CheckpointInput, CloseCheckpointCommand, ContextPackInput, ContinueCheckpointCommand,
    CreateCheckpointCommand, CreateCheckpointSuccessorCommand, ExportFormat, ExportInput,
    HandoffInput, ImportApplyResult, ImportPlan, InitRequest, LocalMemoryInput, MemoryDestination,
    MemoryDraft, MemoryLane, MemoryRecord, MemoryService, MemoryType, OkfProposalSensitivity,
    OkfProposalStatus, PrecheckInput, Proposal, ProposalApprovalOverride, ProposalInboxSummary,
    ProposalStatus, ProposalStatusFilter, ProposeOptions, REPOSITORY_WRITE_MAX_BLOB_BYTES,
    ScopeKind, SearchInput, SearchResult, SessionEndFromCheckpointCommand, SessionEndResult,
    SessionEndWrite, Visibility, discover_paths, lifecycle_transaction_artifact_count,
    parse_import_document, parse_session_end_document, scan_file_proposal_inventory,
    scan_managed_repository_blob,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;

use crate::{
    cli::{
        CaptureCommands, CheckpointCommands, Cli, Commands, DraftCommand, EvalCommands,
        EventCommands, ImportCommands, IntegrateCommands, LifecycleCommands,
        LifecycleInspectCommands, LifecycleMaintenanceCommands, LocalCommands, MaintenanceCommands,
        MaterializeCommands, McpCommands, ProposalCommands, ProposalFileCommands, SafetyCommands,
    },
    eval, integrate, mcp,
    output::{print_json, print_jsonl_row},
    update,
};

mod capture;
mod lifecycle;
mod maintenance;
mod materialize;
mod proposal_files;

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) =>
            {
                normalized.pop();
            }
            Component::ParentDir => {}
            Component::Normal(value) => normalized.push(value),
        }
    }
    normalized
}

const NON_UTF8_GIT_PATH_SENTINEL: &str = ".memzoi/memory/<non-utf8-git-path>";
const MAX_SAFETY_SCAN_GIT_DIFF_BYTES: usize = 4 * 1024 * 1024;
const MAX_SAFETY_SCAN_BLOBS: usize = 4_096;
const MAX_GIT_OBJECT_SIZE_OUTPUT_BYTES: usize = 128;
const GIT_SAFETY_SCAN_TIMEOUT: Duration = Duration::from_secs(30);

fn safety_scan_command(
    staged: bool,
    range: Option<String>,
    file: Option<PathBuf>,
    as_json: bool,
) -> Result<()> {
    let selected = usize::from(staged) + usize::from(range.is_some()) + usize::from(file.is_some());
    if selected != 1 {
        bail!("safety scan requires exactly one of --staged, --range, or --file");
    }
    let cwd = std::env::current_dir().context("failed to inspect current directory")?;
    let paths = discover_paths(&cwd)?;
    let project_identity = paths.project_root.as_os_str().as_encoded_bytes();
    let blobs = if staged {
        load_staged_memory_blobs(&paths.project_root)?
    } else if let Some(range) = range.as_deref() {
        load_range_memory_blobs(&paths.project_root, range)?
    } else {
        vec![load_working_tree_memory_blob(
            &paths.project_root,
            file.as_deref().context("--file disappeared")?,
        )?]
    };

    let mut reports = Vec::with_capacity(blobs.len());
    for blob in blobs {
        let report = scan_safety_blob(&paths.project_root, project_identity, &blob)?;
        reports.push((blob, report));
    }
    let allowed = reports.iter().all(|(_, report)| report.allowed);
    if as_json {
        print_json(&json!({
            "schema": "memzoi/repository-safety-scan-v1",
            "allowed": allowed,
            "files": reports.iter().map(|(blob, report)| json!({
                "path": safety_scan_report_path(blob, report),
                "report": report,
            })).collect::<Vec<_>>(),
        }))?;
    } else if allowed {
        println!("repository safety scan allowed {} blob(s)", reports.len());
    } else {
        println!("repository safety scan blocked");
        for (blob, report) in &reports {
            let display_path = safety_scan_report_path(blob, report);
            for finding in &report.findings {
                println!(
                    "{}: {} at {} ({})",
                    display_path,
                    finding.code.as_str(),
                    finding.field.0,
                    finding.fingerprint
                );
            }
        }
    }
    if !allowed {
        std::process::exit(2);
    }
    Ok(())
}

fn safety_scan_report_path(
    blob: &SafetyScanBlob,
    report: &memzoi_core::RepositoryWriteSafetyReport,
) -> String {
    let display = blob.path.to_string_lossy();
    if report.allowed
        || matches!(
            blob.path_provenance,
            SafetyScanPathProvenance::SyntheticNonUtf8GitPath
        )
    {
        return display.into_owned();
    }
    format!(
        "<redacted-path:{}>",
        blake3::hash(blob.path.as_os_str().as_encoded_bytes()).to_hex()
    )
}

enum SafetyScanPathProvenance {
    Repository,
    SyntheticNonUtf8GitPath,
}

enum SafetyScanBlobSource {
    WorkingTree,
    GitObject(String),
    Inline(Vec<u8>),
    Unsupported,
}

struct SafetyScanBlob {
    path: PathBuf,
    source: SafetyScanBlobSource,
    path_provenance: SafetyScanPathProvenance,
}

fn scan_safety_blob(
    project_root: &Path,
    project_identity: &[u8],
    blob: &SafetyScanBlob,
) -> Result<memzoi_core::RepositoryWriteSafetyReport> {
    let bytes = match &blob.source {
        SafetyScanBlobSource::WorkingTree => {
            let Some(bytes) = read_working_tree_blob(project_root, &blob.path)? else {
                return Ok(scan_unsupported_safety_blob(project_identity));
            };
            bytes
        }
        SafetyScanBlobSource::GitObject(oid) => read_git_blob(project_root, oid)?,
        SafetyScanBlobSource::Inline(bytes) => bytes.clone(),
        SafetyScanBlobSource::Unsupported => {
            return Ok(scan_unsupported_safety_blob(project_identity));
        }
    };
    Ok(scan_managed_repository_blob(
        project_identity,
        &blob.path,
        &bytes,
    ))
}

fn scan_unsupported_safety_blob(
    project_identity: &[u8],
) -> memzoi_core::RepositoryWriteSafetyReport {
    scan_managed_repository_blob(project_identity, Path::new("../unsupported-git-entry"), b"")
}

fn load_working_tree_memory_blob(project_root: &Path, path: &Path) -> Result<SafetyScanBlob> {
    let relative = if path.is_absolute() {
        path.strip_prefix(project_root)
            .context("safety scan file must be inside the current repository")?
            .to_path_buf()
    } else {
        path.to_path_buf()
    };
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("safety scan path contains an unsafe path component");
    }
    require_memory_scan_path(&relative)?;
    Ok(SafetyScanBlob {
        path: relative,
        source: SafetyScanBlobSource::WorkingTree,
        path_provenance: SafetyScanPathProvenance::Repository,
    })
}

#[cfg(unix)]
fn read_working_tree_blob(project_root: &Path, relative: &Path) -> Result<Option<Vec<u8>>> {
    use rustix::{
        fs::{AtFlags, CWD, FileType, Mode, OFlags, openat, statat},
        io::Errno,
    };

    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(component) => Ok(component),
            _ => bail!("safety scan path contains an unsafe path component"),
        })
        .collect::<Result<Vec<_>>>()?;
    let (file_name, ancestors) = components
        .split_last()
        .context("safety scan path must name a managed file")?;
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = openat(CWD, project_root, directory_flags, Mode::empty())
        .context("failed to open safety scan repository without following symbolic links")?;
    for component in ancestors {
        let stat = statat(&directory, *component, AtFlags::SYMLINK_NOFOLLOW)
            .context("failed to inspect safety scan file ancestor")?;
        if !FileType::from_raw_mode(stat.st_mode).is_dir() {
            return Ok(None);
        }
        directory = match openat(&directory, *component, directory_flags, Mode::empty()) {
            Ok(directory) => directory,
            Err(Errno::LOOP | Errno::NOTDIR) => return Ok(None),
            Err(error) => {
                return Err(error).context("failed to open safety scan file ancestor");
            }
        };
    }
    let stat = statat(&directory, *file_name, AtFlags::SYMLINK_NOFOLLOW)
        .context("failed to inspect safety scan file")?;
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Ok(None);
    }
    let file_flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let file = match openat(&directory, *file_name, file_flags, Mode::empty()) {
        Ok(file) => file,
        Err(Errno::LOOP | Errno::NOTDIR) => return Ok(None),
        Err(error) => return Err(error).context("failed to open safety scan file"),
    };
    let file = fs::File::from(file);
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    read_bounded_file(file).map(Some)
}

#[cfg(not(unix))]
fn read_working_tree_blob(project_root: &Path, relative: &Path) -> Result<Option<Vec<u8>>> {
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(component) => Ok(component),
            _ => bail!("safety scan path contains an unsafe path component"),
        })
        .collect::<Result<Vec<_>>>()?;
    let (file_name, ancestors) = components
        .split_last()
        .context("safety scan path must name a managed file")?;
    let mut current = project_root.to_path_buf();
    for component in ancestors {
        current.push(*component);
        let metadata = fs::symlink_metadata(&current)
            .context("failed to inspect safety scan file ancestor")?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Ok(None);
        }
    }
    current.push(*file_name);
    let metadata = fs::symlink_metadata(&current).context("failed to inspect safety scan file")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(None);
    }
    read_bounded_file(fs::File::open(current)?).map(Some)
}

fn read_bounded_file(file: fs::File) -> Result<Vec<u8>> {
    let size = file.metadata()?.len();
    if size > REPOSITORY_WRITE_MAX_BLOB_BYTES as u64 {
        return Ok(oversized_blob_probe());
    }
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(REPOSITORY_WRITE_MAX_BLOB_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .context("failed to read safety scan file")?;
    if bytes.len() > REPOSITORY_WRITE_MAX_BLOB_BYTES {
        return Ok(oversized_blob_probe());
    }
    Ok(bytes)
}

fn load_staged_memory_blobs(project_root: &Path) -> Result<Vec<SafetyScanBlob>> {
    let diff = git_output_bounded(
        project_root,
        &[
            "diff",
            "--raw",
            "-z",
            "--no-renames",
            "--no-abbrev",
            "--no-ext-diff",
            "--cached",
            "--diff-filter=ACMT",
            "--",
            ".memzoi/records",
            ".memzoi/proposals",
            ".memzoi/memory",
        ],
        MAX_SAFETY_SCAN_GIT_DIFF_BYTES,
        "changed-path metadata",
    )?;
    load_git_memory_blobs(&diff)
}

fn load_range_memory_blobs(project_root: &Path, range: &str) -> Result<Vec<SafetyScanBlob>> {
    let (base, head) = range
        .split_once("...")
        .filter(|(base, head)| !base.is_empty() && !head.is_empty() && !head.contains("..."))
        .context("--range must use BASE...HEAD syntax")?;
    validate_git_revision(base)?;
    validate_git_revision(head)?;
    let normalized = format!("{base}...{head}");
    let diff = git_output_bounded(
        project_root,
        &[
            "diff",
            "--raw",
            "-z",
            "--no-renames",
            "--no-abbrev",
            "--no-ext-diff",
            "--diff-filter=ACMT",
            &normalized,
            "--",
            ".memzoi/records",
            ".memzoi/proposals",
            ".memzoi/memory",
        ],
        MAX_SAFETY_SCAN_GIT_DIFF_BYTES,
        "changed-path metadata",
    )?;
    load_git_memory_blobs(&diff)
}

fn validate_git_revision(revision: &str) -> Result<()> {
    if revision.starts_with('-')
        || revision.contains('\0')
        || revision.chars().any(char::is_whitespace)
    {
        bail!("--range contains an unsafe Git revision token");
    }
    Ok(())
}

fn load_git_memory_blobs(diff: &[u8]) -> Result<Vec<SafetyScanBlob>> {
    let mut blobs = Vec::new();
    let mut fields = diff.split(|byte| *byte == 0);
    while let Some(header) = fields.next() {
        if header.is_empty() {
            break;
        }
        let raw_path = fields
            .next()
            .context("Git safety scan returned a raw entry without a path")?;
        if blobs.len() == MAX_SAFETY_SCAN_BLOBS {
            bail!("Git safety scan contains more than {MAX_SAFETY_SCAN_BLOBS} managed blobs");
        }
        let (new_mode, new_oid) = parse_raw_git_diff_header(header)?;
        if !is_raw_memory_scan_path(raw_path) {
            continue;
        }
        let Ok(path) = std::str::from_utf8(raw_path) else {
            blobs.push(SafetyScanBlob {
                path: PathBuf::from(NON_UTF8_GIT_PATH_SENTINEL),
                source: SafetyScanBlobSource::Inline(raw_path.to_vec()),
                path_provenance: SafetyScanPathProvenance::SyntheticNonUtf8GitPath,
            });
            continue;
        };
        let relative = PathBuf::from(path);
        if !is_memory_scan_path(&relative) {
            continue;
        }
        let source = if matches!(new_mode, "100644" | "100755") {
            SafetyScanBlobSource::GitObject(new_oid.to_owned())
        } else {
            SafetyScanBlobSource::Unsupported
        };
        blobs.push(SafetyScanBlob {
            path: relative,
            source,
            path_provenance: SafetyScanPathProvenance::Repository,
        });
    }
    blobs.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(blobs)
}

fn parse_raw_git_diff_header(header: &[u8]) -> Result<(&str, &str)> {
    let header = std::str::from_utf8(header)
        .context("Git safety scan returned a non-UTF-8 raw entry header")?;
    let mut parts = header.split_ascii_whitespace();
    let old_mode = parts
        .next()
        .and_then(|mode| mode.strip_prefix(':'))
        .context("Git safety scan returned an invalid raw entry header")?;
    let new_mode = parts
        .next()
        .context("Git safety scan raw entry omitted the new mode")?;
    let old_oid = parts
        .next()
        .context("Git safety scan raw entry omitted the old object ID")?;
    let new_oid = parts
        .next()
        .context("Git safety scan raw entry omitted the new object ID")?;
    let status = parts
        .next()
        .context("Git safety scan raw entry omitted the status")?;
    if parts.next().is_some()
        || !matches!(old_mode.len(), 6)
        || !matches!(new_mode.len(), 6)
        || !matches!(status, "A" | "C" | "M" | "T")
        || old_oid.len() != new_oid.len()
        || !(40..=64).contains(&new_oid.len())
        || !old_oid.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !new_oid.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("Git safety scan returned an invalid raw entry header");
    }
    Ok((new_mode, new_oid))
}

fn is_raw_memory_scan_path(path: &[u8]) -> bool {
    path.starts_with(b".memzoi/records/")
        || path.starts_with(b".memzoi/proposals/")
        || path.starts_with(b".memzoi/memory/")
}

fn git_output_bounded(
    project_root: &Path,
    args: &[&str],
    max_stdout_bytes: usize,
    output_label: &str,
) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(project_root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        command.process_group(0);
    }
    let child = command
        .spawn()
        .context("failed to run Git for repository safety scan")?;
    child_output_bounded(
        child,
        max_stdout_bytes,
        output_label,
        GIT_SAFETY_SCAN_TIMEOUT,
    )
}

fn child_output_bounded(
    mut child: Child,
    max_stdout_bytes: usize,
    output_label: &str,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("Git safety scan timeout exceeds the supported duration")?;
    let mut stdout = child
        .stdout
        .take()
        .context("Git safety scan command did not expose stdout")?;
    let (read_sender, read_receiver) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut output = Vec::with_capacity(max_stdout_bytes.min(64 * 1024));
        let result = (&mut stdout)
            .take(max_stdout_bytes as u64 + 1)
            .read_to_end(&mut output)
            .map(|_| output);
        let _ = read_sender.send(result);
    });
    let output =
        match read_receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                terminate_child_process_group(&mut child);
                let _ = reader.join();
                return Err(error).context("failed to read bounded Git safety scan output");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                terminate_child_process_group(&mut child);
                let _ = reader.join();
                bail!("Git safety scan {output_label} timed out");
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                terminate_child_process_group(&mut child);
                let _ = reader.join();
                bail!("Git safety scan {output_label} reader failed");
            }
        };
    if reader.join().is_err() {
        terminate_child_process_group(&mut child);
        bail!("Git safety scan {output_label} reader failed");
    }
    if output.len() > max_stdout_bytes {
        terminate_child_process_group(&mut child);
        bail!("Git safety scan {output_label} exceeds the supported size limit");
    }
    let status = wait_for_child_until(&mut child, deadline, output_label)?;
    if !status.success() {
        bail!("Git safety scan command failed with status {status}");
    }
    Ok(output)
}

fn wait_for_child_until(
    child: &mut Child,
    deadline: Instant,
    output_label: &str,
) -> Result<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                terminate_child_process_group(child);
                return Err(error).context("failed to wait for Git repository safety scan");
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            terminate_child_process_group(child);
            bail!("Git safety scan {output_label} timed out");
        }
        thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

fn terminate_child_process_group(child: &mut Child) {
    #[cfg(unix)]
    {
        use rustix::process::{Pid, Signal, kill_process_group};

        let _ = kill_process_group(Pid::from_child(child), Signal::KILL);
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

fn read_git_blob(project_root: &Path, oid: &str) -> Result<Vec<u8>> {
    let size = git_output_bounded(
        project_root,
        &["cat-file", "-s", oid],
        MAX_GIT_OBJECT_SIZE_OUTPUT_BYTES,
        "object-size response",
    )?;
    let size = std::str::from_utf8(&size)
        .context("Git safety scan returned a non-UTF-8 object size")?
        .trim()
        .parse::<u64>()
        .context("Git safety scan returned an invalid object size")?;
    if size > REPOSITORY_WRITE_MAX_BLOB_BYTES as u64 {
        return Ok(oversized_blob_probe());
    }
    let bytes = git_output_bounded(
        project_root,
        &["cat-file", "blob", oid],
        REPOSITORY_WRITE_MAX_BLOB_BYTES,
        "blob",
    )?;
    if bytes.len() as u64 != size {
        bail!("Git safety scan object size changed during immutable blob read");
    }
    Ok(bytes)
}

fn oversized_blob_probe() -> Vec<u8> {
    vec![b'\n'; REPOSITORY_WRITE_MAX_BLOB_BYTES + 1]
}

fn require_memory_scan_path(path: &Path) -> Result<()> {
    if !is_memory_scan_path(path) {
        bail!(
            "safety scan path is outside managed repository memory: {}",
            path.display()
        );
    }
    Ok(())
}

fn is_memory_scan_path(path: &Path) -> bool {
    let mut components = path.components();
    let managed_root = matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(root)), Some(Component::Normal(area)))
            if root == ".memzoi"
                && matches!(area.to_str(), Some("records" | "proposals" | "memory"))
    );
    managed_root
        && components.next().is_some()
        && components.all(|component| matches!(component, Component::Normal(_)))
}

pub(crate) fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Init { force, json } => init_command(force, json),
        Commands::Propose {
            memory_type,
            scope_kind,
            visibility,
            source_kind,
            source_ref,
            sensitivity,
            content_class,
            title,
            body,
            actor,
            manual,
            auto_approve,
            apply,
            json,
        } => propose_command(
            DraftCommand {
                memory_type,
                scope_kind,
                visibility,
                source_kind,
                source_ref,
                sensitivity,
                content_class,
                title,
                body,
            },
            &actor,
            ProposeFlags {
                manual,
                auto_approve,
                apply,
            },
            json,
        ),
        Commands::Approve {
            proposal_id,
            actor,
            json,
        } => approve_command(&proposal_id, &actor, json),
        Commands::Reject {
            proposal_id,
            reason,
            actor,
            json,
        } => reject_command(&proposal_id, &reason, &actor, json),
        Commands::Apply {
            proposal_id,
            actor,
            json,
        } => apply_command(&proposal_id, &actor, json),
        Commands::Proposals { command } => match command {
            ProposalCommands::List { status, json } => proposals_list_command(&status, json),
            ProposalCommands::Show { proposal_id, json } => {
                proposals_show_command(&proposal_id, json)
            }
            ProposalCommands::Apply {
                all_approved,
                actor,
                json,
            } => proposals_apply_command(all_approved, &actor, json),
        },
        Commands::Import { command } => match command {
            ImportCommands::Plan {
                from_file,
                actor,
                json,
            } => import_plan_command(from_file, &actor, json),
            ImportCommands::Apply {
                from_file,
                plan_id,
                actor,
                json,
            } => import_apply_command(from_file, &plan_id, &actor, json),
        },
        Commands::Capture { command } => match command {
            CaptureCommands::Plan {
                source,
                request_file,
                source_bytes,
                source_id,
                output,
                json,
            } => capture::plan_command(source, request_file, source_bytes, source_id, output, json),
            CaptureCommands::Review {
                plan_file,
                decisions_file,
                prior_review_file,
                source_bytes,
                reviewed_by,
                reviewed_at,
                output,
                json,
            } => capture::review_command(capture::ReviewCommand {
                plan_file,
                decisions_file,
                prior_review_file,
                source_bytes,
                reviewed_by,
                reviewed_at,
                output,
                as_json: json,
            }),
            CaptureCommands::Apply {
                plan_file,
                review_file,
                prior_review_file,
                source_bytes,
                plan_id,
                review_id,
                actor,
                json,
            } => capture::apply_command(capture::ApplyCommand {
                plan_file,
                review_file,
                prior_review_file,
                source_bytes,
                plan_id,
                review_id,
                actor,
                as_json: json,
            }),
        },
        Commands::Maintenance { command } => match command {
            MaintenanceCommands::Plan {
                record_ids,
                evaluated_at,
                output,
                json,
            } => maintenance::plan_command(record_ids, evaluated_at, output, json),
            MaintenanceCommands::Materialize {
                plan_file,
                plan_id,
                action_ids,
                decision_at,
                json,
            } => {
                maintenance::materialize_command(plan_file, plan_id, action_ids, decision_at, json)
            }
        },
        Commands::Lifecycle { command } => match command {
            LifecycleCommands::Maintenance { command } => match command {
                LifecycleMaintenanceCommands::Enable { json } => {
                    lifecycle::maintenance_enable_command(json)
                }
                LifecycleMaintenanceCommands::Disable { json } => {
                    lifecycle::maintenance_disable_command(json)
                }
                LifecycleMaintenanceCommands::Inspect { json } => {
                    lifecycle::maintenance_inspect_command(json)
                }
                LifecycleMaintenanceCommands::Reconcile { json } => {
                    lifecycle::maintenance_reconcile_command(json)
                }
            },
            LifecycleCommands::Plan {
                record_ids,
                evaluated_at,
                output,
                json,
            } => lifecycle::plan_command(record_ids, evaluated_at, output, json),
            LifecycleCommands::Authorize {
                request_file,
                plan_file,
                expires_at,
                json,
            } => lifecycle::authorize_command(request_file, plan_file, expires_at, json),
            LifecycleCommands::Revoke { grant_id, json } => {
                lifecycle::revoke_command(&grant_id, json)
            }
            LifecycleCommands::Inspect { command } => match command {
                LifecycleInspectCommands::Record { record_id, json } => {
                    lifecycle::inspect_record_command(&record_id, json)
                }
                LifecycleInspectCommands::Grant { grant_id, json } => {
                    lifecycle::inspect_grant_command(&grant_id, json)
                }
            },
            LifecycleCommands::Apply {
                request_file,
                grant_id,
                plan_file,
                json,
            } => lifecycle::apply_command(request_file, &grant_id, plan_file, json),
        },
        Commands::Materialize { command } => match command {
            MaterializeCommands::Plan {
                candidate_file,
                output,
                json,
            } => materialize::plan_command(candidate_file, output, json),
            MaterializeCommands::Decide {
                candidate_file,
                plan_file,
                decision_at,
                output,
                json,
            } => materialize::decide_command(candidate_file, plan_file, decision_at, output, json),
            MaterializeCommands::Apply {
                candidate_file,
                plan_file,
                decision_file,
                candidate_id,
                plan_id,
                decision_id,
                json,
            } => materialize::apply_command(materialize::ApplyCommand {
                candidate_file,
                plan_file,
                decision_file,
                candidate_id,
                plan_id,
                decision_id,
                as_json: json,
            }),
        },
        Commands::ProposalFiles { command } => match command {
            ProposalFileCommands::List { json } => proposal_files::list(json),
            ProposalFileCommands::Show { proposal_id, json } => {
                proposal_files::show(&proposal_id, json)
            }
            ProposalFileCommands::Validate { json } => proposal_files::validate(json),
            ProposalFileCommands::Apply {
                proposal_id,
                actor,
                json,
            } => proposal_files::apply(&proposal_id, &actor, json),
            ProposalFileCommands::Reject {
                proposal_id,
                reason,
                actor,
                json,
            } => proposal_files::reject(&proposal_id, &reason, &actor, json),
        },
        Commands::Local { command } => match command {
            LocalCommands::Add {
                memory_type,
                title,
                body,
                actor,
                json,
            } => local_add_command(&memory_type, title, body, &actor, json),
            LocalCommands::List { json } => local_list_command(json),
            LocalCommands::Search { query, limit, json } => {
                local_search_command(query, limit, json)
            }
        },
        Commands::Checkpoint { command } => match command {
            CheckpointCommands::Add {
                task,
                note,
                from_file,
                successor_of,
                operation_id,
                expected_version,
                actor,
                json,
            } => checkpoint_add_command(
                CheckpointAddOptions {
                    task,
                    note,
                    from_file,
                    successor_of,
                    operation_id,
                    expected_version,
                },
                &actor,
                json,
            ),
            CheckpointCommands::Continue {
                checkpoint_id,
                operation_id,
                expected_version,
                actor,
                json,
            } => checkpoint_continue_command(
                checkpoint_id,
                operation_id,
                expected_version,
                &actor,
                json,
            ),
            CheckpointCommands::Close {
                checkpoint_id,
                operation_id,
                expected_version,
                actor,
                json,
            } => checkpoint_close_command(
                checkpoint_id,
                operation_id,
                expected_version,
                &actor,
                json,
            ),
            CheckpointCommands::List { json } => checkpoint_list_command(json),
        },
        Commands::Events { command } => match command {
            EventCommands::Export { jsonl } => events_export_command(jsonl),
        },
        Commands::SessionEnd {
            from_file,
            from_checkpoint,
            operation_id,
            expected_version,
            actor,
            json,
        } => session_end_command(
            from_file,
            from_checkpoint,
            operation_id,
            expected_version,
            &actor,
            json,
        ),
        Commands::Supersede {
            record_id,
            memory_type,
            scope_kind,
            visibility,
            source_kind,
            source_ref,
            sensitivity,
            content_class,
            title,
            body,
            actor,
            json,
        } => supersede_command(
            &record_id,
            DraftCommand {
                memory_type,
                scope_kind,
                visibility,
                source_kind,
                source_ref,
                sensitivity,
                content_class,
                title,
                body,
            },
            &actor,
            json,
        ),
        Commands::Tombstone {
            record_id,
            reason,
            actor,
            json,
        } => tombstone_command(&record_id, &reason, &actor, json),
        Commands::Search {
            query,
            scope_kind,
            memory_type,
            path,
            limit,
            json,
        } => search_command(query, scope_kind, memory_type, path, limit, json),
        Commands::Expiry { record_id, json } => expiry_command(&record_id, json),
        Commands::Context {
            task,
            path,
            token_budget,
            include_local,
            include_session,
            json,
        } => context_command(
            task,
            path,
            token_budget,
            include_local,
            include_session,
            json,
        ),
        Commands::Handoff {
            task,
            path,
            token_budget,
            include_local,
            include_session,
            json,
        } => handoff_command(
            task,
            path,
            token_budget,
            include_local,
            include_session,
            json,
        ),
        Commands::Precheck {
            path,
            action,
            command,
            scope_kind,
            json,
        } => precheck_command(path, action, command, scope_kind, json),
        Commands::Safety { command } => match command {
            SafetyCommands::Scan {
                staged,
                range,
                file,
                json,
            } => safety_scan_command(staged, range, file, json),
        },
        Commands::Export {
            format,
            scope_kind,
            json,
        } => export_command(&format, &scope_kind, json),
        Commands::Rebuild { json } => rebuild_command(json),
        Commands::Doctor { project_root, json } => doctor_command(project_root, json),
        Commands::Eval { command } => {
            match command {
                EvalCommands::RecallOperational { evidence, json } => {
                    eval::recall_operational_eval_command(evidence, json)
                }
                EvalCommands::RecallCompetitors { evidence, json } => {
                    eval::recall_competitor_eval_command(evidence, json)
                }
                EvalCommands::RecallV3 {
                    command,
                    corpus,
                    candidates,
                    artifact_roots,
                    commitment,
                    prepare_locked_commitment,
                    verify_locked_commitment,
                    require_ready_candidates,
                    json,
                } => {
                    if let Some(command) = command {
                        eval::recall_v3_subcommand(*command)
                    } else {
                        let corpus = corpus.ok_or_else(|| anyhow::anyhow!(
                        "--corpus <PATH> is required when no recall-v3 subcommand is selected"
                    ))?;
                        eval::recall_v3_eval_command(eval::RecallV3EvalRequest {
                            corpus,
                            candidate_paths: candidates,
                            artifact_roots,
                            commitment,
                            prepare_locked_commitment,
                            verify_locked_commitment,
                            require_ready_candidates,
                            as_json: json,
                        })
                    }
                }
                EvalCommands::Recall {
                    corpus,
                    baseline,
                    update_baseline,
                    json,
                } => eval::recall_eval_command(corpus, baseline, update_baseline, json),
                EvalCommands::Capture {
                    corpus,
                    baseline,
                    update_baseline,
                    json,
                } => eval::capture_eval_command(corpus, baseline, update_baseline, json),
            }
        }
        Commands::Quickstart { apply_sample, json } => quickstart_command(apply_sample, json),
        Commands::Update {
            check,
            reference,
            json,
        } => update::update_command(check, &reference, json),
        Commands::Mcp { command } => match command {
            McpCommands::Config { project_root } => mcp::mcp_config_command(project_root),
        },
        Commands::Integrate { command } => match command {
            IntegrateCommands::List { json } => integrate::integrate_list_command(json),
            IntegrateCommands::Prompt { profile } => integrate::integrate_prompt_command(profile),
            IntegrateCommands::Instructions {
                profile,
                file,
                json,
            } => integrate::integrate_instructions_command(profile, file, json),
        },
    }
}

fn init_command(force: bool, as_json: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let result = MemoryService::initialize(&cwd, InitRequest { force })?;
    let paths = result.paths;

    if as_json {
        print_json(&json!({
            "project_root": paths.project_root,
            "memory_dir": paths.memory_dir,
            "records_dir": paths.records_dir(),
            "repository_runtime_dir": paths.repository_runtime_dir,
            "worktree_runtime_dir": paths.worktree_runtime_dir,
            "shared_db_path": paths.shared_db_path,
            "index_db_path": paths.index_db_path,
            "runtime_dir": paths.runtime_dir,
            "config_path": paths.config_path,
            "db_path": paths.db_path,
            "exports_dir": paths.exports_dir,
        }))?;
    } else {
        println!("Initialized Memzoi bundle");
        println!("  memory: {}", paths.memory_dir.display());
        println!("  records: {}", paths.records_dir().display());
        println!("  runtime: {}", paths.runtime_dir.display());
        println!("  config: {}", paths.config_path.display());
        println!("  database: {}", paths.db_path.display());
        println!("  exports: {}", paths.exports_dir.display());
    }

    Ok(())
}

fn import_plan_command(from_file: PathBuf, actor: &str, as_json: bool) -> Result<()> {
    let manifest = fs::read_to_string(&from_file).with_context(|| {
        format!(
            "failed to read import manifest from {}",
            from_file.display()
        )
    })?;
    let document = parse_import_document(&manifest)
        .with_context(|| format!("failed to parse import manifest {}", from_file.display()))?;
    let service = open_service()?;
    let plan = service.plan_import(actor, document)?;
    if as_json {
        let output = import_plan_json(
            &plan,
            &from_file,
            service.paths().project_root.as_path(),
            actor,
        )?;
        print_json(&output)
    } else {
        println!("plan\t{}", plan.plan_id);
        print_import_plan_human(&plan);
        Ok(())
    }
}

fn import_apply_command(
    from_file: PathBuf,
    plan_id: &str,
    actor: &str,
    as_json: bool,
) -> Result<()> {
    if plan_id.trim().is_empty() {
        bail!("import apply requires --plan-id");
    }
    let manifest = fs::read_to_string(&from_file).with_context(|| {
        format!(
            "failed to read import manifest from {}",
            from_file.display()
        )
    })?;
    let document = parse_import_document(&manifest)
        .with_context(|| format!("failed to parse import manifest {}", from_file.display()))?;
    let service = open_service()?;
    let result = service.apply_import(actor, document, plan_id)?;
    if as_json {
        let output = import_apply_json(
            &result,
            &from_file,
            service.paths().project_root.as_path(),
            actor,
            plan_id,
        )?;
        print_json(&output)
    } else {
        println!("applied\t{}", result.plan.plan_id);
        print_import_plan_human(&result.plan);
        Ok(())
    }
}

fn import_plan_json(
    plan: &ImportPlan,
    from_file: &Path,
    project_root: &Path,
    actor: &str,
) -> Result<serde_json::Value> {
    let mut output = serde_json::to_value(plan).context("failed to serialize import plan")?;
    if let serde_json::Value::Object(fields) = &mut output {
        fields.insert("mode".to_owned(), json!("plan"));
        fields.insert("actor".to_owned(), json!(actor));
        fields.insert(
            "source_file".to_owned(),
            json!(safe_import_source_file(from_file, project_root)),
        );
    }
    Ok(output)
}

fn import_apply_json(
    result: &ImportApplyResult,
    from_file: &Path,
    project_root: &Path,
    actor: &str,
    expected_plan_id: &str,
) -> Result<serde_json::Value> {
    let mut output =
        serde_json::to_value(&result.plan).context("failed to serialize import apply plan")?;
    if let serde_json::Value::Object(fields) = &mut output {
        fields.insert("mode".to_owned(), json!("apply"));
        fields.insert("actor".to_owned(), json!(actor));
        fields.insert(
            "source_file".to_owned(),
            json!(safe_import_source_file(from_file, project_root)),
        );
        fields.insert("expected_plan_id".to_owned(), json!(expected_plan_id));
        fields.insert("writes".to_owned(), json!(result.writes));
    }
    Ok(output)
}

fn safe_import_source_file(from_file: &Path, project_root: &Path) -> Option<PathBuf> {
    let manifest = from_file.canonicalize().ok()?;
    let root = project_root.canonicalize().ok()?;
    manifest.strip_prefix(root).ok().map(Path::to_path_buf)
}

fn print_import_plan_human(plan: &ImportPlan) {
    println!(
        "summary\t{}",
        serde_json::to_string(&plan.summary).unwrap_or_default()
    );
    for candidate in &plan.candidates {
        println!(
            "candidate\t{}\t{}\t{}\t{}",
            candidate.index,
            candidate.title,
            candidate.classification.destination.as_str(),
            serde_json::to_string(&candidate.action).unwrap_or_default()
        );
        println!("body\t{}", candidate.body);
    }
}

#[derive(Debug, Clone, Copy)]
struct ProposeFlags {
    manual: bool,
    auto_approve: bool,
    apply: bool,
}

fn propose_command(
    draft_args: DraftCommand,
    actor: &str,
    flags: ProposeFlags,
    as_json: bool,
) -> Result<()> {
    if flags.manual && flags.auto_approve {
        bail!("--manual and --auto-approve cannot be used together");
    }
    if flags.manual && flags.apply {
        bail!("--apply is incompatible with --manual");
    }

    let service = open_service()?;
    let draft = draft_from_args(draft_args)?;
    let approval_override = match (flags.manual, flags.auto_approve || flags.apply) {
        (true, false) => Some(ProposalApprovalOverride::Manual),
        (false, true) => Some(ProposalApprovalOverride::Auto),
        (false, false) => None,
        (true, true) => unreachable!("manual/auto conflict is checked above"),
    };
    let result = service.propose_memory_with_options(
        actor,
        draft,
        ProposeOptions {
            approval_override,
            apply: flags.apply,
        },
    )?;
    if as_json {
        let record_id = result.record.as_ref().map(|record| record.id.as_str());
        let record_status = result.record.as_ref().map(|record| record.status.as_str());
        print_json(&json!({
            "proposal_id": result.proposal.id,
            "status": result.proposal.status.as_str(),
            "record_id": record_id,
            "record_status": record_status,
            "validation": result.validation,
            "applied": result.applied,
            "sensitivity": result.proposal.payload.sensitivity.as_str(),
        }))
    } else {
        if let Some(record) = result.record {
            println!(
                "applied proposal {} as memory {}",
                result.proposal.id, record.id
            );
        } else if result.proposal.status == ProposalStatus::Approved {
            println!("approved proposal {}", result.proposal.id);
        } else {
            println!(
                "created {} proposal {}",
                result.proposal.status.as_str(),
                result.proposal.id
            );
        }
        if let Some(validation) = result.validation {
            for issue in validation.issues {
                println!("validation\t{}\t{}", issue.code, issue.message);
            }
        }
        Ok(())
    }
}

fn approve_command(proposal_id: &str, actor: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let proposal = service.approve_proposal(proposal_id, actor)?;
    if as_json {
        print_json(&json!({
            "proposal_id": proposal.id,
            "status": proposal.status.as_str(),
        }))
    } else {
        println!("approved proposal {}", proposal.id);
        Ok(())
    }
}

fn reject_command(proposal_id: &str, reason: &str, actor: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let proposal = service.reject_proposal(proposal_id, actor, reason)?;
    if as_json {
        print_json(&json!({
            "proposal_id": proposal.id,
            "status": proposal.status.as_str(),
        }))
    } else {
        println!("rejected proposal {}", proposal.id);
        Ok(())
    }
}

fn apply_command(proposal_id: &str, actor: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let proposal = service.show_proposal(proposal_id)?;
    if proposal.payload.sensitivity != OkfProposalSensitivity::RepoSafe {
        return blocked_repo_sensitivity_error("apply", proposal.payload.sensitivity, as_json);
    }
    let record = service.apply_proposal(proposal_id, actor)?;
    if as_json {
        print_json(&json!({
            "proposal_id": proposal_id,
            "record_id": record.id,
            "record_status": record.status.as_str(),
        }))
    } else {
        println!("applied proposal {proposal_id} as memory {}", record.id);
        Ok(())
    }
}

fn proposals_list_command(status: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let filter: ProposalStatusFilter = status.parse()?;
    let proposals = service.list_proposals(filter)?;
    if as_json {
        let proposals = proposals.iter().map(proposal_json).collect::<Vec<_>>();
        print_json(&json!({
            "status": status,
            "proposals": proposals,
        }))
    } else {
        for proposal in proposals {
            println!(
                "{}\t{}\t{}",
                proposal.status.as_str(),
                proposal.id,
                proposal.payload.title
            );
        }
        Ok(())
    }
}

fn proposals_show_command(proposal_id: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let proposal = service.show_proposal(proposal_id)?;
    if as_json {
        print_json(&proposal_json(&proposal))
    } else {
        println!("id:\t{}", proposal.id);
        println!("status:\t{}", proposal.status.as_str());
        println!("actor:\t{}", proposal.actor);
        println!("created:\t{}", proposal.created_at);
        println!("updated:\t{}", proposal.updated_at);
        println!("title:\t{}", proposal.payload.title);
        println!("body:\t{}", proposal.payload.body);
        if let Some(validation) = proposal.validation {
            println!(
                "validation:\t{}",
                if validation.is_valid {
                    "valid"
                } else {
                    "invalid"
                }
            );
            for issue in validation.issues {
                println!("validation_issue:\t{}\t{}", issue.code, issue.message);
            }
        }
        Ok(())
    }
}

fn proposals_apply_command(all_approved: bool, actor: &str, as_json: bool) -> Result<()> {
    if !all_approved {
        bail!("proposals apply requires --all-approved");
    }

    let service = open_service()?;
    let approved =
        service.list_proposals(ProposalStatusFilter::Status(ProposalStatus::Approved))?;
    let mut applied = Vec::new();
    let mut failed = None;
    for proposal in approved {
        match service.apply_proposal(&proposal.id, actor) {
            Ok(record) => {
                if !as_json {
                    println!("applied proposal {} as memory {}", proposal.id, record.id);
                }
                applied.push(json!({
                    "proposal_id": proposal.id,
                    "record_id": record.id,
                }));
            }
            Err(error) => {
                failed = Some(json!({
                    "proposal_id": proposal.id,
                    "error": error.to_string(),
                }));
                break;
            }
        }
    }

    let remaining_open_count: usize = service.open_proposal_counts()?.values().sum();
    if as_json {
        print_json(&json!({
            "applied": applied,
            "failed": failed,
            "remaining_open_count": remaining_open_count,
        }))?;
    }
    if let Some(failed) = failed {
        bail!(
            "failed to apply proposal {}: {}",
            failed["proposal_id"].as_str().unwrap_or("unknown"),
            failed["error"].as_str().unwrap_or("unknown error")
        );
    }
    Ok(())
}

fn proposal_json(proposal: &Proposal) -> serde_json::Value {
    json!({
        "id": proposal.id,
        "proposal_id": proposal.id,
        "operation": proposal.operation,
        "status": proposal.status.as_str(),
        "actor": proposal.actor,
        "created_at": proposal.created_at,
        "updated_at": proposal.updated_at,
        "payload": proposal.payload,
        "validation": proposal.validation,
    })
}

fn local_add_command(
    memory_type: &str,
    title: String,
    body: String,
    actor: &str,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let record = service.create_local_memory(
        actor,
        LocalMemoryInput {
            memory_type: parse_memory_type(memory_type)?,
            lane: MemoryLane::Semantic,
            title,
            body,
        },
    )?;
    if as_json {
        print_json(&runtime_record_json(&record))
    } else {
        println!("added\t{}\t{}", record.destination.as_str(), record.id);
        Ok(())
    }
}

fn local_list_command(as_json: bool) -> Result<()> {
    let service = open_service()?;
    let records = service.list_local_memory()?;
    if as_json {
        let records = records.iter().map(runtime_record_json).collect::<Vec<_>>();
        print_json(&json!({
            "destination": MemoryDestination::Local.as_str(),
            "records": records,
        }))
    } else {
        for record in records {
            println!(
                "{}\t{}\t{}\t{}",
                record.destination.as_str(),
                record.id,
                record.memory_type.as_str(),
                record.title
            );
        }
        Ok(())
    }
}

fn local_search_command(query: String, limit: usize, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let results = service.search_local_memory(query.clone(), limit)?;
    if as_json {
        print_json(&json!({
            "query": query,
            "destination": MemoryDestination::Local.as_str(),
            "records": results.iter().map(local_search_result_json).collect::<Vec<_>>(),
        }))
    } else {
        for result in results {
            println!(
                "{}\t{}\t{}\t{}",
                result.record.destination.as_str(),
                result.record.id,
                result.record.memory_type.as_str(),
                result.record.title
            );
        }
        Ok(())
    }
}

struct CheckpointAddOptions {
    task: String,
    note: Option<String>,
    from_file: Option<PathBuf>,
    successor_of: Option<String>,
    operation_id: Option<String>,
    expected_version: Option<String>,
}

fn checkpoint_add_command(options: CheckpointAddOptions, actor: &str, as_json: bool) -> Result<()> {
    let CheckpointAddOptions {
        task,
        note,
        from_file,
        successor_of,
        operation_id,
        expected_version,
    } = options;
    let note = checkpoint_note_from_args(note, from_file)?;
    let service = open_service()?;
    let operation_id = checkpoint_operation_id(operation_id, as_json)?;
    let input = CheckpointInput { task, note };
    let result = match successor_of {
        Some(predecessor_id) => {
            let expected_predecessor_version =
                checkpoint_expected_version(&service, &predecessor_id, expected_version, as_json)?;
            service.create_checkpoint_successor(
                actor,
                CreateCheckpointSuccessorCommand {
                    operation_id,
                    predecessor_id,
                    expected_predecessor_version,
                    input,
                },
            )?
        }
        None => {
            if expected_version.is_some() {
                bail!("--expected-version requires --successor-of");
            }
            service.create_checkpoint_command(
                actor,
                CreateCheckpointCommand {
                    operation_id,
                    input,
                },
            )?
        }
    };
    if as_json {
        print_json(&checkpoint_command_result_json(&service, &result)?)
    } else {
        println!(
            "checkpoint\t{}\t{}\t{}\t{}",
            MemoryDestination::Session.as_str(),
            result.checkpoint_id,
            result.operation_id,
            result.record_version
        );
        Ok(())
    }
}

fn checkpoint_continue_command(
    checkpoint_id: String,
    operation_id: Option<String>,
    expected_version: Option<String>,
    actor: &str,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let operation_id = checkpoint_operation_id(operation_id, as_json)?;
    let expected_version =
        checkpoint_expected_version(&service, &checkpoint_id, expected_version, as_json)?;
    let result = service.continue_checkpoint(
        actor,
        ContinueCheckpointCommand {
            operation_id,
            checkpoint_id,
            expected_version,
        },
    )?;
    print_checkpoint_command_result(&service, &result, as_json)
}

fn checkpoint_close_command(
    checkpoint_id: String,
    operation_id: Option<String>,
    expected_version: Option<String>,
    actor: &str,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let operation_id = checkpoint_operation_id(operation_id, as_json)?;
    let expected_version =
        checkpoint_expected_version(&service, &checkpoint_id, expected_version, as_json)?;
    let result = service.close_checkpoint(
        actor,
        CloseCheckpointCommand {
            operation_id,
            checkpoint_id,
            expected_version,
        },
    )?;
    print_checkpoint_command_result(&service, &result, as_json)
}

fn checkpoint_operation_id(operation_id: Option<String>, as_json: bool) -> Result<String> {
    match operation_id {
        Some(value) if !value.trim().is_empty() => Ok(value),
        Some(_) => bail!("--operation-id cannot be empty"),
        None if as_json => bail!("--operation-id is required with --json"),
        None => Ok(uuid::Uuid::now_v7().to_string()),
    }
}

fn checkpoint_expected_version(
    service: &MemoryService,
    checkpoint_id: &str,
    expected_version: Option<String>,
    as_json: bool,
) -> Result<String> {
    match expected_version {
        Some(value) if !value.trim().is_empty() => Ok(value),
        Some(_) => bail!("--expected-version cannot be empty"),
        None if as_json => bail!("--expected-version is required with --json"),
        None => service.checkpoint_record_version(checkpoint_id),
    }
}

fn checkpoint_command_result_json(
    service: &MemoryService,
    result: &memzoi_core::CheckpointCommandResult,
) -> Result<serde_json::Value> {
    let record = service.checkpoint_for_owner_operation(&result.checkpoint_id)?;
    let mut value = runtime_record_json(&record);
    let object = value
        .as_object_mut()
        .context("runtime checkpoint JSON must be an object")?;
    object.insert("operation_id".to_owned(), json!(&result.operation_id));
    object.insert("record_version".to_owned(), json!(&result.record_version));
    object.insert(
        "lifecycle_event_id".to_owned(),
        json!(&result.lifecycle_event_id),
    );
    object.insert("applied".to_owned(), json!(result.applied));
    object.insert("replayed".to_owned(), json!(result.replayed));
    Ok(value)
}

fn print_checkpoint_command_result(
    service: &MemoryService,
    result: &memzoi_core::CheckpointCommandResult,
    as_json: bool,
) -> Result<()> {
    if as_json {
        print_json(&checkpoint_command_result_json(service, result)?)
    } else {
        println!(
            "checkpoint\t{}\t{}\t{}\t{}",
            MemoryDestination::Session.as_str(),
            result.checkpoint_id,
            result.operation_id,
            result.record_version
        );
        Ok(())
    }
}

fn checkpoint_list_command(as_json: bool) -> Result<()> {
    let service = open_service()?;
    let records = service.list_checkpoints()?;
    if as_json {
        let records = records.iter().map(runtime_record_json).collect::<Vec<_>>();
        print_json(&json!({
            "destination": MemoryDestination::Session.as_str(),
            "records": records,
        }))
    } else {
        for record in records {
            println!(
                "{}\t{}\t{}\t{}",
                record.destination.as_str(),
                record.id,
                record.memory_type.as_str(),
                record.title
            );
        }
        Ok(())
    }
}

fn checkpoint_note_from_args(note: Option<String>, from_file: Option<PathBuf>) -> Result<String> {
    match (note, from_file) {
        (Some(_), Some(_)) => bail!("use either --note or --from-file, not both"),
        (Some(note), None) => Ok(note),
        (None, Some(path)) => fs::read_to_string(&path)
            .with_context(|| format!("failed to read checkpoint note from {}", path.display())),
        (None, None) => bail!("checkpoint add requires --note or --from-file"),
    }
}

fn session_end_command(
    from_file: Option<PathBuf>,
    from_checkpoint: Option<String>,
    operation_id: Option<String>,
    expected_version: Option<String>,
    actor: &str,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let (document, source, checkpoint_command) = match (from_file, from_checkpoint) {
        (Some(_), Some(_)) => bail!("use either --from-file or --from-checkpoint, not both"),
        (Some(path), None) => {
            if operation_id.is_some() || expected_version.is_some() {
                bail!("--operation-id and --expected-version require --from-checkpoint");
            }
            let body = fs::read_to_string(&path).with_context(|| {
                format!("failed to read session-end input from {}", path.display())
            })?;
            (
                parse_session_end_document(&body)?,
                json!({
                    "kind": "file",
                    "path": path,
                }),
                None,
            )
        }
        (None, Some(record_id)) => {
            let operation_id = checkpoint_operation_id(operation_id, as_json)?;
            let expected_version =
                checkpoint_expected_version(&service, &record_id, expected_version, as_json)?;
            let checkpoint = service.checkpoint_for_owner_operation(&record_id)?;
            (
                parse_session_end_document(&checkpoint.body)?,
                json!({
                    "kind": "checkpoint",
                    "record_id": &record_id,
                }),
                Some((operation_id, record_id, expected_version)),
            )
        }
        (None, None) => bail!("session-end requires --from-file or --from-checkpoint"),
    };

    let (result, closure) = match checkpoint_command {
        Some((operation_id, checkpoint_id, expected_version)) => {
            let promoted = service.promote_session_end_from_checkpoint(
                actor,
                SessionEndFromCheckpointCommand {
                    operation_id,
                    checkpoint_id,
                    expected_version,
                    document,
                },
            )?;
            (promoted.promotion, promoted.closure)
        }
        None => (service.promote_session_end(actor, document)?, None),
    };
    if as_json {
        let project_root = service.paths().project_root.as_path();
        let mut value = session_end_result_json(&result, source, project_root);
        if let Some(object) = value.as_object_mut() {
            object.insert("checkpoint_closure".to_owned(), json!(closure));
        }
        print_json(&value)
    } else {
        for candidate in &result.candidates {
            match &candidate.write {
                Some(SessionEndWrite::ProposalFile { proposal_id, path }) => {
                    let path = path
                        .strip_prefix(service.paths().project_root.as_path())
                        .unwrap_or(path);
                    println!(
                        "{}\t{}\t{}\t{}",
                        candidate.status.as_str(),
                        candidate.destination.as_str(),
                        proposal_id,
                        path.display()
                    );
                }
                Some(SessionEndWrite::RuntimeRecord {
                    record_id,
                    destination,
                }) => {
                    println!(
                        "{}\t{}\t{}\t{}",
                        candidate.status.as_str(),
                        destination.as_str(),
                        record_id,
                        candidate.title
                    );
                }
                None => {
                    println!(
                        "{}\t{}\t{}",
                        candidate.status.as_str(),
                        candidate.destination.as_str(),
                        candidate.title
                    );
                }
            }
        }
        if let Some(closure) = closure {
            println!(
                "closed\tsession\t{}\t{}\t{}",
                closure.checkpoint_id, closure.operation_id, closure.record_version
            );
        }
        Ok(())
    }
}

fn runtime_record_json(record: &MemoryRecord) -> serde_json::Value {
    json!({
        "id": &record.id,
        "record_id": &record.id,
        "type": record.memory_type.as_str(),
        "lane": record.lane.as_str(),
        "destination": record.destination.as_str(),
        "scope_kind": record.scope_kind.as_str(),
        "visibility": record.visibility.as_str(),
        "status": record.status.as_str(),
        "title": &record.title,
        "body": &record.body,
        "source_kind": &record.source_kind,
        "source_ref": &record.source_ref,
        "proposal_id": &record.proposal_id,
        "created_at": &record.created_at,
        "updated_at": &record.updated_at,
        "retention": &record.retention,
        "origin": &record.origin,
        "lineage": &record.lineage,
    })
}

fn local_search_result_json(result: &SearchResult) -> serde_json::Value {
    json!({
        "record": runtime_record_json(&result.record),
        "score": result.score,
        "snippet": &result.snippet,
        "rationale": &result.rationale,
        "paths": &result.paths,
        "citations": &result.citations,
    })
}

fn session_end_result_json(
    result: &SessionEndResult,
    source: serde_json::Value,
    project_root: &Path,
) -> serde_json::Value {
    json!({
        "task": &result.task,
        "source": source,
        "candidates": result.candidates.iter().map(|candidate| {
            let write = candidate.write.as_ref().map(|write| match write {
                SessionEndWrite::ProposalFile { proposal_id, path } => {
                    let path = path.strip_prefix(project_root).unwrap_or(path);
                    json!({
                        "kind": "proposal_file",
                        "proposal_id": proposal_id,
                        "path": path,
                    })
                }
                SessionEndWrite::RuntimeRecord { record_id, destination } => {
                    json!({
                        "kind": "runtime_record",
                        "record_id": record_id,
                        "destination": destination.as_str(),
                    })
                }
            });
            json!({
                "index": candidate.index,
                "destination": candidate.destination.as_str(),
                "type": candidate.memory_type.as_str(),
                "lane": candidate.lane.as_str(),
                "title": &candidate.title,
                "sensitivity": candidate.sensitivity.as_str(),
                "status": candidate.status.as_str(),
                "reason": &candidate.reason,
                "write": write,
            })
        }).collect::<Vec<_>>(),
    })
}

fn supersede_command(
    record_id: &str,
    draft_args: DraftCommand,
    actor: &str,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let draft = draft_from_args(draft_args)?;
    if draft.sensitivity != OkfProposalSensitivity::RepoSafe {
        return blocked_repo_sensitivity_error("supersede", draft.sensitivity, as_json);
    }
    let result = service.supersede_record(record_id, actor, draft)?;
    if as_json {
        print_json(&json!({
            "superseded_record_id": result.previous.id,
            "superseded_record_status": result.previous.status.as_str(),
            "record_id": result.replacement.id,
            "record_status": result.replacement.status.as_str(),
        }))
    } else {
        println!(
            "superseded memory {} with {}",
            result.previous.id, result.replacement.id
        );
        Ok(())
    }
}

fn tombstone_command(record_id: &str, reason: &str, actor: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let record = service.tombstone_record(record_id, actor, reason)?;
    if as_json {
        print_json(&json!({
            "record_id": record.id,
            "record_status": record.status.as_str(),
        }))
    } else {
        println!("tombstoned memory {}", record.id);
        Ok(())
    }
}

fn search_command(
    query: String,
    scope_kind: Option<String>,
    memory_type: Option<String>,
    path: Option<String>,
    limit: usize,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let results = service.search_memory(SearchInput {
        query: query.clone(),
        scope_kind: scope_kind.as_deref().map(parse_scope_kind).transpose()?,
        scope_id: None,
        memory_type: memory_type.as_deref().map(parse_memory_type).transpose()?,
        lane: None,
        destination: Some(MemoryDestination::Repo),
        path_prefix: path,
        limit,
        include_inactive: false,
    })?;

    if as_json {
        print_json(&json!({
            "query": query,
            "records": results,
        }))
    } else {
        for result in results {
            println!(
                "{}\t{}\t{}\t{}",
                result.record.id,
                result.record.memory_type.as_str(),
                result.record.scope_kind.as_str(),
                result.record.title
            );
        }
        Ok(())
    }
}

fn expiry_command(record_id: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let diagnostic = service.inspect_expiry(record_id)?;

    if as_json {
        print_json(&serde_json::to_value(&diagnostic)?)
    } else {
        println!("record_id:\t{}", diagnostic.record.id);
        println!("title:\t{}", diagnostic.record.title);
        println!("status:\t{}", diagnostic.record.status.as_str());
        println!(
            "explicit_expires_at:\t{}",
            diagnostic
                .record
                .retention
                .explicit_expires_at
                .as_deref()
                .unwrap_or("none")
        );
        println!("evaluated_at:\t{}", diagnostic.evaluated_at);
        println!("retention_state:\t{}", diagnostic.retention.state.as_str());
        println!(
            "effective_boundary:\t{}",
            diagnostic
                .retention
                .effective_boundary
                .as_deref()
                .unwrap_or("none")
        );
        println!("current_assertion:\t{}", diagnostic.current_assertion);
        println!(
            "excluded_from_normal_reads:\t{}",
            diagnostic.excluded_from_normal_reads
        );
        println!("reason:\t{}", diagnostic.reason);
        println!();
        println!("{}", diagnostic.record.body);
        Ok(())
    }
}

fn context_command(
    task: String,
    path: Option<String>,
    token_budget: Option<usize>,
    include_local: bool,
    include_session: bool,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let pack = service.build_context_pack(ContextPackInput {
        task,
        path_prefix: path,
        token_budget,
        include_local,
        include_session,
    })?;

    if as_json {
        print_json(&serde_json::to_value(&pack)?)
    } else {
        println!("{}", pack.prompt);
        Ok(())
    }
}

fn handoff_command(
    task: Option<String>,
    path: Option<String>,
    token_budget: Option<usize>,
    include_local: bool,
    include_session: bool,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let pack = service.build_handoff_pack(HandoffInput {
        task,
        path_prefix: path,
        token_budget,
        include_local,
        include_session,
    })?;

    if as_json {
        print_json(&serde_json::to_value(&pack)?)
    } else {
        println!("# Memzoi Handoff");
        println!();
        println!("Task: {}", pack.task);
        if let Some(path_prefix) = pack.path_prefix.as_deref() {
            println!("Path: {path_prefix}");
        }
        println!(
            "Proposal inbox: {}",
            proposal_inbox_text(&pack.proposal_inbox)
        );
        println!();
        println!("{}", pack.context.prompt);
        Ok(())
    }
}

fn proposal_inbox_text(proposal_inbox: &ProposalInboxSummary) -> String {
    if proposal_inbox.open_total == 0 {
        return "0 open DB proposals".to_owned();
    }
    format!(
        "{} open DB proposals (pending={}, validated={}, approved={})",
        proposal_inbox.open_total,
        proposal_inbox.pending,
        proposal_inbox.validated,
        proposal_inbox.approved
    )
}

fn export_command(format: &str, scope_kind: &str, as_json: bool) -> Result<()> {
    let service = open_service()?;
    let format: ExportFormat = format.parse()?;
    let result = service.export(ExportInput {
        format,
        scope_kind: parse_scope_kind(scope_kind)?,
    })?;

    if as_json {
        print_json(&json!({
            "format": result.format.as_str(),
            "written_paths": result.written_paths,
        }))
    } else {
        for path in result.written_paths {
            println!("{}", path.display());
        }
        Ok(())
    }
}

fn events_export_command(jsonl: bool) -> Result<()> {
    let service = open_service()?;

    if jsonl {
        service.for_each_event(|event| {
            print_jsonl_row(&event)?;
            Ok(())
        })?;
    } else {
        service.for_each_event(|event| {
            println!(
                "{}\t{}\t{}\t{}",
                event.created_at, event.id, event.event_type, event.actor
            );
            Ok(())
        })?;
    }

    Ok(())
}

fn rebuild_command(as_json: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let result = MemoryService::rebuild_at(&cwd)?;
    if as_json {
        print_json(&json!({
            "records_root": result.records_root,
            "db_path": result.db_path,
            "record_ids": result.record_ids,
        }))
    } else {
        println!("rebuilt {} records", result.record_ids.len());
        println!("database: {}", result.db_path.display());
        Ok(())
    }
}

fn precheck_command(
    path: Option<String>,
    action: Option<String>,
    command: Option<String>,
    scope_kind: Option<String>,
    as_json: bool,
) -> Result<()> {
    let service = open_service()?;
    let warnings = service.precheck(PrecheckInput {
        path: path.clone(),
        action: action.clone(),
        command: command.clone(),
        scope_kind: scope_kind.as_deref().map(parse_scope_kind).transpose()?,
    })?;

    if as_json {
        print_json(&json!({
            "path": path,
            "action": action,
            "command": command,
            "warnings": warnings,
        }))
    } else {
        if warnings.is_empty() {
            println!("No memory warnings.");
        } else {
            for warning in warnings {
                println!(
                    "{}\t{}\t{}\t{}",
                    warning.severity,
                    warning.record_id,
                    warning.message,
                    warning.suggested_next_step
                );
            }
        }
        Ok(())
    }
}

fn doctor_command(project_root: Option<PathBuf>, as_json: bool) -> Result<()> {
    let start = match project_root {
        Some(path) => path,
        None => std::env::current_dir().context("failed to read current directory")?,
    };
    let paths = discover_paths(start)?;
    let mut checks = Vec::new();
    let mut next_steps = Vec::new();

    checks.push(check("binary", "ok", "memzoi is running"));
    checks.push(check(
        "project_root",
        "ok",
        paths.project_root.display().to_string(),
    ));
    if paths.records_dir().is_dir() {
        checks.push(check(
            "records",
            "ok",
            paths.records_dir().display().to_string(),
        ));
    } else {
        checks.push(check(
            "records",
            "warning",
            format!("{} missing", paths.records_dir().display()),
        ));
        push_next_step(&mut next_steps, "memzoi init");
    }

    let shared_schema_is_ready = if paths.shared_db_path.is_file() {
        checks.push(check(
            "shared_database",
            "ok",
            paths.shared_db_path.display().to_string(),
        ));
        match schema_ready(&paths.shared_db_path) {
            Ok(true) => {
                checks.push(check(
                    "shared_schema",
                    "ok",
                    "shared runtime schema is initialized",
                ));
                true
            }
            Ok(false) => {
                checks.push(check(
                    "shared_schema",
                    "warning",
                    "shared runtime schema is missing tables",
                ));
                false
            }
            Err(error) => {
                checks.push(check("shared_schema", "warning", error.to_string()));
                false
            }
        }
    } else {
        checks.push(check(
            "shared_database",
            "warning",
            format!("{} missing", paths.shared_db_path.display()),
        ));
        checks.push(check(
            "shared_schema",
            "skip",
            "shared database missing; run init first",
        ));
        false
    };

    let schema_is_ready = if paths.index_db_path.is_file() {
        checks.push(check(
            "database",
            "ok",
            paths.index_db_path.display().to_string(),
        ));
        match schema_ready(&paths.index_db_path) {
            Ok(true) => {
                checks.push(check(
                    "schema",
                    "ok",
                    "worktree index schema is initialized",
                ));
                true
            }
            Ok(false) => {
                checks.push(check(
                    "schema",
                    "warning",
                    "worktree index schema is missing tables",
                ));
                false
            }
            Err(error) => {
                checks.push(check("schema", "warning", error.to_string()));
                false
            }
        }
    } else {
        checks.push(check(
            "database",
            "warning",
            format!("{} missing", paths.index_db_path.display()),
        ));
        checks.push(check(
            "schema",
            "skip",
            "worktree index missing; run init first",
        ));
        false
    };

    let proposal_inventory =
        if paths.config_path.is_file() && shared_schema_is_ready && schema_is_ready {
            MemoryService::open_paths(paths.clone())
                .and_then(|service| service.validate_file_proposal_inventory())
        } else {
            scan_file_proposal_inventory(&paths)
        };
    match proposal_inventory {
        Ok(inventory) => {
            let invalid = inventory.errors.len();
            if invalid > 0 {
                checks.push(check(
                    "proposal_files",
                    "warning",
                    format!(
                        "{invalid} invalid proposal packet{}: {}",
                        if invalid == 1 { "" } else { "s" },
                        inventory
                            .errors
                            .first()
                            .map(|error| error.error.as_str())
                            .unwrap_or("unknown proposal inventory error")
                    ),
                ));
                push_next_step(&mut next_steps, "memzoi proposal-files validate");
            } else if inventory.pending.is_empty() {
                let applied = inventory
                    .resolved
                    .iter()
                    .filter(|entry| entry.proposal.status == OkfProposalStatus::Applied)
                    .count();
                let rejected = inventory.resolved.len() - applied;
                checks.push(check(
                    "proposal_files",
                    "ok",
                    format!(
                        "no pending file proposals (resolved: applied={applied}, rejected={rejected})"
                    ),
                ));
            } else {
                checks.push(check(
                    "proposal_files",
                    "warning",
                    format!(
                        "{} pending file proposal{}",
                        inventory.pending.len(),
                        if inventory.pending.len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    ),
                ));
                push_next_step(&mut next_steps, "memzoi proposal-files list");
            }
        }
        Err(error) => {
            checks.push(check("proposal_files", "warning", error.to_string()));
        }
    }

    match lifecycle_transaction_artifact_count(&paths) {
        Ok(0) => checks.push(check(
            "lifecycle_transactions",
            "ok",
            "no hidden lifecycle transaction artifacts",
        )),
        Ok(count) => {
            checks.push(check(
                "lifecycle_transactions",
                "warning",
                format!(
                    "{} hidden lifecycle transaction artifact{} require inspection in local runtime storage or the current records/proposals roots",
                    count,
                    if count == 1 { "" } else { "s" },
                ),
            ));
            push_next_step(
                &mut next_steps,
                "inspect local Memzoi repository transaction artifacts before retrying",
            );
        }
        Err(error) => checks.push(check(
            "lifecycle_transactions",
            "warning",
            error.to_string(),
        )),
    }

    if paths.config_path.is_file() {
        checks.push(check(
            "config",
            "ok",
            paths.config_path.display().to_string(),
        ));
    } else {
        checks.push(check(
            "config",
            "warning",
            format!("{} missing", paths.config_path.display()),
        ));
        push_next_step(&mut next_steps, "memzoi init");
    }

    if shared_schema_is_ready && schema_is_ready {
        match MemoryService::open_paths(paths.clone())
            .and_then(|service| service.open_proposal_counts())
        {
            Ok(counts) => {
                let total: usize = counts.values().sum();
                if total == 0 {
                    checks.push(check("proposals", "ok", "no open proposals"));
                } else {
                    let parts = counts
                        .iter()
                        .filter(|(_, count)| **count > 0)
                        .map(|(status, count)| format!("{}={count}", status.as_str()))
                        .collect::<Vec<_>>()
                        .join(", ");
                    checks.push(check(
                        "proposals",
                        "warning",
                        format!("{total} open proposals ({parts})"),
                    ));
                    push_next_step(&mut next_steps, "memzoi proposals list --status open");
                    push_next_step(&mut next_steps, "memzoi proposals apply --all-approved");
                    push_next_step(
                        &mut next_steps,
                        "memzoi reject <proposal-id> --reason \"...\"",
                    );
                }
            }
            Err(error) => checks.push(check("proposals", "warning", error.to_string())),
        }

        match MemoryService::open_paths(paths.clone())
            .and_then(|service| service.repo_index_drift())
        {
            Ok(drift) if drift.is_current() => {
                checks.push(check("repo_index", "ok", "runtime repo index is current"));
            }
            Ok(drift) => {
                checks.push(check(
                    "repo_index",
                    "warning",
                    format!(
                        "runtime repo index is stale (missing={}, stale={}, changed={}, fts_out_of_sync={})",
                        drift.missing_from_index.len(),
                        drift.stale_in_index.len(),
                        drift.changed_in_index.len(),
                        drift.fts_out_of_sync,
                    ),
                ));
                push_next_step(&mut next_steps, "memzoi rebuild");
            }
            Err(error) => checks.push(check("repo_index", "warning", error.to_string())),
        }
    }

    if paths.exports_dir.is_dir() {
        checks.push(check(
            "exports",
            "ok",
            paths.exports_dir.display().to_string(),
        ));
    } else {
        checks.push(check(
            "exports",
            "warning",
            format!("{} missing", paths.exports_dir.display()),
        ));
    }

    match Command::new("memzoi-mcp").arg("--help").output() {
        Ok(output) if output.status.success() => {
            checks.push(check("mcp", "ok", "memzoi-mcp is available"));
        }
        Ok(_) => checks.push(check("mcp", "warning", "memzoi-mcp --help failed")),
        Err(_) => checks.push(check("mcp", "warning", "memzoi-mcp not found on PATH")),
    }

    push_next_step(&mut next_steps, "memzoi mcp config --project-root .");

    let status = if checks
        .iter()
        .any(|check| check["status"].as_str() == Some("warning"))
    {
        "warning"
    } else {
        "ok"
    };

    if as_json {
        print_json(&json!({
            "project_root": paths.project_root,
            "status": status,
            "checks": checks,
            "next_steps": next_steps,
        }))
    } else {
        println!("Memzoi doctor");
        println!("project: {}", paths.project_root.display());
        println!();
        for check in checks {
            println!(
                "{}\t{}\t{}",
                check["status"].as_str().unwrap_or("unknown").to_uppercase(),
                check["name"].as_str().unwrap_or("unknown"),
                check["message"].as_str().unwrap_or("")
            );
        }
        if !next_steps.is_empty() {
            println!();
            println!("Next:");
            for step in next_steps {
                println!("  {step}");
            }
        }
        Ok(())
    }
}

fn quickstart_command(apply_sample: bool, as_json: bool) -> Result<()> {
    if !apply_sample {
        if as_json {
            return print_json(&json!({
                "next_steps": quickstart_steps(),
            }));
        }
        println!("Memzoi quickstart");
        println!();
        for (index, step) in quickstart_steps().iter().enumerate() {
            println!("{}. {step}", index + 1);
        }
        return Ok(());
    }

    let service = open_service()?;
    if !service.paths().config_path.is_file() {
        bail!("memory bundle is not initialized; run memzoi init first");
    }

    let sample_title = "Use Memzoi quickstart sample";
    let sample_body = "This repo has completed the Memzoi quickstart workflow.";
    let mut search = service.search_memory(SearchInput {
        query: "quickstart".to_string(),
        scope_kind: Some(ScopeKind::Repo),
        scope_id: None,
        memory_type: Some(MemoryType::Decision),
        lane: None,
        destination: Some(MemoryDestination::Repo),
        path_prefix: None,
        limit: 10,
        include_inactive: false,
    })?;
    let existing_record = search
        .iter()
        .find(|result| {
            result.record.title == sample_title
                && result.record.source_kind.as_deref() == Some("quickstart")
        })
        .map(|result| result.record.id.clone());
    let (proposal_id, record_id, created) = if let Some(record_id) = existing_record {
        (None::<String>, record_id, false)
    } else {
        let draft = MemoryDraft {
            memory_type: MemoryType::Decision,
            lane: MemoryLane::Semantic,
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            visibility: Visibility::Repo,
            title: sample_title.to_string(),
            body: sample_body.to_string(),
            tags: Vec::new(),
            source_kind: Some("quickstart".to_string()),
            source_ref: Some("quickstart://built-in-sample".to_string()),
            sensitivity: OkfProposalSensitivity::RepoSafe,
            content_class: memzoi_core::RepositoryContentClass::GeneralRepoKnowledge,
            confidence: 1.0,
        };
        let proposal = service.propose_memory("quickstart", draft)?;
        service.approve_proposal(&proposal.id, "quickstart")?;
        let record = service.apply_proposal(&proposal.id, "quickstart")?;
        search = service.search_memory(SearchInput {
            query: "quickstart".to_string(),
            scope_kind: Some(ScopeKind::Repo),
            scope_id: None,
            memory_type: Some(MemoryType::Decision),
            lane: None,
            destination: Some(MemoryDestination::Repo),
            path_prefix: None,
            limit: 10,
            include_inactive: false,
        })?;
        (Some(proposal.id), record.id, true)
    };
    let export = service.export(ExportInput {
        format: ExportFormat::AgentsMd,
        scope_kind: ScopeKind::Repo,
    })?;
    let next_steps = vec!["memzoi mcp config --project-root .".to_string()];

    if as_json {
        print_json(&json!({
            "created": created,
            "proposal_id": proposal_id,
            "record_id": record_id,
            "search_count": search.len(),
            "written_paths": export.written_paths,
            "next_steps": next_steps,
        }))
    } else {
        if created {
            println!("created sample memory {record_id}");
        } else {
            println!("sample memory already exists: {record_id}");
        }
        println!("search_count: {}", search.len());
        for path in export.written_paths {
            println!("exported: {}", path.display());
        }
        println!("next: memzoi mcp config --project-root .");
        Ok(())
    }
}

fn schema_ready(db_path: &Path) -> Result<bool> {
    const REQUIRED_TABLES: &[&str] = &[
        "event_log",
        "memory_record",
        "scope_binding",
        "memory_path",
        "proposal",
        "memory_tag",
        "memory_capture",
        "runtime_mirror_state",
        "read_audit",
        "memory_fts",
    ];
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to open database {}", db_path.display()))?;
    let quick_check: String = conn.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if quick_check != "ok" {
        bail!(
            "database integrity check failed for {}: {quick_check}",
            db_path.display()
        );
    }
    for table in REQUIRED_TABLES {
        let exists = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Ok(false);
        }
    }
    Ok(true)
}

fn check(name: &str, status: &str, message: impl Into<String>) -> serde_json::Value {
    json!({
        "name": name,
        "status": status,
        "message": message.into(),
    })
}

fn push_next_step(next_steps: &mut Vec<String>, step: &str) {
    if !next_steps.iter().any(|existing| existing == step) {
        next_steps.push(step.to_owned());
    }
}

fn quickstart_steps() -> Vec<String> {
    vec![
        "memzoi init".to_string(),
        "memzoi quickstart --apply-sample".to_string(),
        "memzoi search quickstart".to_string(),
        "memzoi context --task \"remember quickstart setup\"".to_string(),
        "memzoi precheck --command \"rm -rf .memzoi\"".to_string(),
        "memzoi mcp config --project-root .".to_string(),
    ]
}

fn open_service() -> Result<MemoryService> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    MemoryService::open(&cwd)
}

fn draft_from_args(args: DraftCommand) -> Result<MemoryDraft> {
    let DraftCommand {
        memory_type,
        scope_kind,
        visibility,
        source_kind,
        source_ref,
        sensitivity,
        content_class,
        title,
        body,
    } = args;
    Ok(MemoryDraft {
        memory_type: parse_memory_type(&memory_type)?,
        lane: MemoryLane::Semantic,
        scope_kind: parse_scope_kind(&scope_kind)?,
        scope_id: None,
        visibility: parse_visibility(&visibility)?,
        title,
        body,
        tags: Vec::new(),
        source_kind: normalize_optional_metadata(source_kind, "source-kind")?,
        source_ref: normalize_optional_metadata(source_ref, "source-ref")?,
        sensitivity: sensitivity.parse().map_err(anyhow::Error::msg)?,
        content_class: content_class.parse().map_err(anyhow::Error::msg)?,
        confidence: 1.0,
    })
}

fn blocked_repo_sensitivity_error(
    operation: &str,
    sensitivity: OkfProposalSensitivity,
    as_json: bool,
) -> Result<()> {
    let next_step = repo_sensitivity_guidance(sensitivity);
    let message = if operation == "proposal_files_apply" {
        format!(
            "OKF proposal sensitivity {} cannot be applied into repo records; {next_step}",
            sensitivity.as_str()
        )
    } else {
        format!(
            "canonical repo apply requires sensitivity repo-safe; got {}; {next_step}",
            sensitivity.as_str()
        )
    };
    if as_json {
        print_json(&json!({
            "ok": false,
            "error": {
                "code": "repo_sensitivity_required",
                "operation": operation,
                "sensitivity": sensitivity.as_str(),
                "message": message,
                "next_step": next_step,
            }
        }))?;
    }
    bail!(message)
}

fn repo_sensitivity_guidance(sensitivity: OkfProposalSensitivity) -> &'static str {
    match sensitivity {
        OkfProposalSensitivity::RepoSafe => "repo-safe proposals may be applied after review",
        OkfProposalSensitivity::LocalOnly => {
            "local-only proposals belong in the future local/runtime memory plane"
        }
        OkfProposalSensitivity::Sensitive => {
            "classify or sanitize sensitive content before applying it to the repo plane"
        }
        OkfProposalSensitivity::Secret => "secret proposals must not become repo-shared memory",
        OkfProposalSensitivity::RawTranscript => {
            "raw transcripts must not become repo-shared memory"
        }
        OkfProposalSensitivity::PrivatePersonalData => {
            "private personal data must not become repo-shared memory"
        }
        OkfProposalSensitivity::TemporaryState => {
            "temporary task state belongs in local or session memory, not canonical repo memory"
        }
        OkfProposalSensitivity::Unknown => {
            "classify the proposal sensitivity before applying it to repo records"
        }
    }
}

fn normalize_optional_metadata(value: Option<String>, label: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        bail!("--{label} cannot be empty");
    }
    Ok(Some(value.to_owned()))
}

fn parse_memory_type(value: &str) -> Result<MemoryType> {
    value.parse().map_err(anyhow::Error::msg)
}

fn parse_scope_kind(value: &str) -> Result<ScopeKind> {
    value.parse().map_err(anyhow::Error::msg)
}

fn parse_visibility(value: &str) -> Result<Visibility> {
    value.parse().map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod safety_scan_limit_tests {
    use super::*;

    #[cfg(unix)]
    fn spawn_timeout_fixture(script: &str) -> Child {
        use std::os::unix::process::CommandExt;

        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command.spawn().expect("spawn timeout fixture")
    }

    #[cfg(unix)]
    #[test]
    fn bounded_git_subprocess_tree_times_out_while_stdout_is_open() {
        let child = spawn_timeout_fixture("sleep 10");
        let started = Instant::now();

        let error =
            child_output_bounded(child, 1_024, "timeout fixture", Duration::from_millis(50))
                .expect_err("a silent child process tree must time out");

        assert!(error.to_string().contains("timed out"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout must terminate the whole subprocess group"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_git_subprocess_tree_times_out_after_stdout_closes() {
        let child = spawn_timeout_fixture("exec 1>&-; sleep 10");
        let started = Instant::now();

        let error =
            child_output_bounded(child, 1_024, "timeout fixture", Duration::from_millis(50))
                .expect_err("a child that closes stdout but stays alive must time out");

        assert!(error.to_string().contains("timed out"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout must still cover process exit after stdout reaches EOF"
        );
    }

    #[test]
    fn raw_git_scan_rejects_excessive_managed_blob_count() {
        let mut diff = Vec::new();
        for index in 0..=MAX_SAFETY_SCAN_BLOBS {
            diff.extend_from_slice(
                b":100644 100644 0000000000000000000000000000000000000000 1111111111111111111111111111111111111111 A\0",
            );
            diff.extend_from_slice(format!(".memzoi/records/{index}.md\0").as_bytes());
        }

        let error = match load_git_memory_blobs(&diff) {
            Ok(_) => panic!("managed changed-path cardinality must be bounded"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("more than 4096 managed blobs"));
    }
}

#[cfg(test)]
mod path_normalization_tests {
    use std::path::{Path, PathBuf};

    use super::normalize_absolute_path;

    #[cfg(unix)]
    #[test]
    fn normalize_absolute_path_preserves_the_unix_root() {
        assert_eq!(
            normalize_absolute_path(Path::new("/directory/../artifact.json")),
            PathBuf::from("/artifact.json")
        );
        assert_eq!(
            normalize_absolute_path(Path::new("/../artifact.json")),
            PathBuf::from("/artifact.json")
        );
    }

    #[cfg(windows)]
    #[test]
    fn normalize_absolute_path_preserves_the_drive_root() {
        assert_eq!(
            normalize_absolute_path(Path::new(r"C:\directory\..\artifact.json")),
            PathBuf::from(r"C:\artifact.json")
        );
        assert_eq!(
            normalize_absolute_path(Path::new(r"C:\..\artifact.json")),
            PathBuf::from(r"C:\artifact.json")
        );
    }
}
