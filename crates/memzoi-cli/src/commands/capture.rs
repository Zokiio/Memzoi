use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use memzoi_core::{
    CaptureRequest, CaptureSourceInputs, CaptureSourceLocator, build_capture_review_with_inputs,
    build_capture_review_with_prior_and_inputs, discover_paths, parse_capture_plan,
    parse_capture_request, parse_capture_review, parse_capture_review_input,
    plan_capture_with_inputs,
};
use serde_json::json;

use super::{normalize_absolute_path, open_service};
use crate::output::print_json;

fn capture_source_inputs(
    request: &CaptureRequest,
    source_bytes: Option<&Path>,
) -> Result<CaptureSourceInputs> {
    let supplied = request.sources.first().is_some_and(|source| {
        matches!(&source.locator, CaptureSourceLocator::SuppliedBytes { .. })
    });
    match (supplied, source_bytes) {
        (false, None) => Ok(CaptureSourceInputs::new()),
        (false, Some(_)) => bail!("--source-bytes is valid only for a supplied_bytes source"),
        (true, None) => bail!("supplied_bytes capture requires --source-bytes <PATH|->"),
        (true, Some(path)) => {
            let source_id = request
                .sources
                .first()
                .context("capture request source is missing")?
                .source_id
                .clone();
            let bytes = read_capture_source_bytes(path)?;
            let mut inputs = CaptureSourceInputs::new();
            inputs.insert_supplied_bytes(source_id, bytes)?;
            Ok(inputs)
        }
    }
}

fn read_capture_source_bytes(path: &Path) -> Result<Vec<u8>> {
    fn read_bounded(reader: &mut impl Read) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        reader
            .take(memzoi_core::MAX_DIFF_SOURCE_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("failed to read supplied capture bytes")?;
        if bytes.len() as u64 > memzoi_core::MAX_DIFF_SOURCE_BYTES {
            bail!("supplied capture bytes exceed the configured size limit");
        }
        Ok(bytes)
    }

    if path == Path::new("-") {
        let stdin = std::io::stdin();
        return read_bounded(&mut stdin.lock());
    }
    let metadata =
        fs::symlink_metadata(path).context("failed to inspect supplied capture byte transport")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("supplied capture byte transport must be a regular file");
    }
    let mut file =
        fs::File::open(path).context("failed to open supplied capture byte transport")?;
    read_bounded(&mut file)
}

const CAPTURE_ARTIFACT_MAX_BYTES: usize = 2 * 1024 * 1024;

fn read_capture_artifact(path: &Path, label: &str) -> Result<String> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} artifact"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} artifact must be a regular file");
    }
    if metadata.len() > CAPTURE_ARTIFACT_MAX_BYTES as u64 {
        bail!("{label} artifact exceeds the 2 MiB capture artifact limit");
    }

    let file = fs::File::open(path).with_context(|| format!("failed to open {label} artifact"))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((CAPTURE_ARTIFACT_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {label} artifact"))?;
    if bytes.len() > CAPTURE_ARTIFACT_MAX_BYTES {
        bail!("{label} artifact exceeds the 2 MiB capture artifact limit");
    }
    String::from_utf8(bytes).with_context(|| format!("{label} artifact must be UTF-8 JSON"))
}

fn capture_json_bytes<T: ?Sized + serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes =
        serde_json::to_vec_pretty(value).context("failed to serialize capture artifact")?;
    bytes.push(b'\n');
    if bytes.len() > CAPTURE_ARTIFACT_MAX_BYTES {
        bail!("serialized capture artifact exceeds the 2 MiB limit");
    }
    Ok(bytes)
}

fn write_capture_artifact<T: ?Sized + serde::Serialize>(
    value: &T,
    destination: &Path,
) -> Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(_) => bail!("capture artifact destination already exists"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).context("failed to inspect capture artifact destination");
        }
    }
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let metadata = fs::symlink_metadata(parent)
        .context("failed to inspect capture artifact destination parent")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("capture artifact destination parent must be a real directory");
    }
    let bytes = capture_json_bytes(value)?;
    let mut staged =
        tempfile::NamedTempFile::new_in(parent).context("failed to stage capture artifact")?;
    staged
        .write_all(&bytes)
        .and_then(|_| staged.as_file().sync_all())
        .context("failed to write capture artifact")?;
    staged.persist_noclobber(destination).map_err(|error| {
        anyhow::Error::new(error.error).context("failed to install capture artifact")
    })?;
    Ok(())
}

fn capture_artifact_data_class<T: ?Sized + serde::Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value).context("failed to inspect capture data class")?;
    value
        .get("data_class")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .context("capture artifact is missing data_class")
}

fn write_classified_capture_artifact<T: ?Sized + serde::Serialize>(
    value: &T,
    destination: &Path,
    paths: &memzoi_core::MemoryPaths,
) -> Result<()> {
    let data_class = capture_artifact_data_class(value)?;
    match data_class.as_str() {
        "repo_safe" => {
            let target = repo_safe_capture_artifact_target(destination, paths)?;
            write_capture_artifact(value, &target)
        }
        "blocked" => bail!("blocked capture artifacts may only be emitted to stdout"),
        "private" => {
            let target = private_capture_artifact_target(destination, paths)?;
            write_private_capture_artifact(value, &target)
        }
        _ => bail!("capture artifact has an unsupported data class"),
    }
}

fn repo_safe_capture_artifact_target(
    destination: &Path,
    paths: &memzoi_core::MemoryPaths,
) -> Result<PathBuf> {
    let current = std::env::current_dir().context("failed to read current directory")?;
    let absolute = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        current.join(destination)
    };
    let parent = absolute
        .parent()
        .context("capture artifact destination has no parent")?;
    let canonical_parent = parent
        .canonicalize()
        .context("failed to resolve capture artifact destination parent")?;
    let file_name = absolute
        .file_name()
        .context("capture artifact destination has no file name")?;
    let resolved_destination = canonical_parent.join(file_name);
    for protected in [&paths.memory_dir, &paths.runtime_dir, &paths.exports_dir] {
        let protected = resolved_capture_boundary(protected, &current)?;
        if resolved_destination.starts_with(protected) {
            bail!("capture artifacts cannot be saved under Memzoi-managed state");
        }
    }
    Ok(resolved_destination)
}

fn resolved_capture_boundary(path: &Path, current: &Path) -> Result<PathBuf> {
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
                    .context("Memzoi-managed artifact boundary has no existing ancestor")?
                    .to_os_string();
                missing_suffix.push(component);
                if !existing_ancestor.pop() {
                    bail!("Memzoi-managed artifact boundary has no existing ancestor");
                }
            }
            Err(error) => {
                return Err(error).context("failed to resolve Memzoi-managed artifact boundary");
            }
        }
    }
}

struct PrivateCaptureArtifactTarget {
    runtime_root: PathBuf,
    relative_destination: PathBuf,
}

fn private_capture_artifact_target(
    destination: &Path,
    paths: &memzoi_core::MemoryPaths,
) -> Result<PrivateCaptureArtifactTarget> {
    let current = std::env::current_dir().context("failed to read current directory")?;
    let absolute = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        current.join(destination)
    };
    let parent = absolute
        .parent()
        .context("private capture artifact destination has no parent")?;
    let canonical_parent = parent
        .canonicalize()
        .context("failed to resolve private capture artifact destination parent")?;
    let canonical_runtime = paths
        .runtime_dir
        .canonicalize()
        .context("private runtime directory does not exist")?;
    let file_name = absolute
        .file_name()
        .context("private capture artifact destination has no file name")?;
    let resolved_destination = canonical_parent.join(file_name);
    if !resolved_destination.starts_with(&canonical_runtime) {
        bail!("private capture artifacts may only be saved under the private runtime directory");
    }
    let canonical_project = resolved_capture_boundary(&paths.project_root, &current)?;
    if resolved_destination.starts_with(canonical_project) {
        bail!("private capture artifacts cannot be saved under the project root");
    }
    let canonical_exports = resolved_capture_boundary(&paths.exports_dir, &current)?;
    if resolved_destination.starts_with(canonical_exports) {
        bail!("private capture artifacts cannot be saved under generated exports");
    }
    let relative_destination = resolved_destination
        .strip_prefix(&canonical_runtime)
        .context("private capture artifact escaped the runtime directory")?
        .to_path_buf();
    Ok(PrivateCaptureArtifactTarget {
        runtime_root: canonical_runtime,
        relative_destination,
    })
}

#[cfg(unix)]
fn write_private_capture_artifact<T: ?Sized + serde::Serialize>(
    value: &T,
    target: &PrivateCaptureArtifactTarget,
) -> Result<()> {
    use rustix::fs::{AtFlags, CWD, Mode, OFlags, fsync, linkat, openat, unlinkat};

    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = openat(CWD, &target.runtime_root, directory_flags, Mode::empty())
        .context("failed to open private runtime directory safely")?;
    let parent = target
        .relative_destination
        .parent()
        .unwrap_or_else(|| Path::new(""));
    for component in parent.components() {
        let std::path::Component::Normal(component) = component else {
            bail!("private capture artifact path contains an unsafe component");
        };
        directory = openat(&directory, component, directory_flags, Mode::empty())
            .context("failed to open private capture artifact directory safely")?;
    }
    let file_name = target
        .relative_destination
        .file_name()
        .context("private capture artifact destination has no file name")?;
    let bytes = capture_json_bytes(value)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut staged_name = None;
    let mut staged_file = None;
    for attempt in 0..100_u32 {
        let candidate = format!(
            ".memzoi-capture-{}-{nonce}-{attempt}.tmp",
            std::process::id()
        );
        match openat(
            &directory,
            candidate.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(file) => {
                staged_name = Some(candidate);
                staged_file = Some(fs::File::from(file));
                break;
            }
            Err(error) if error == rustix::io::Errno::EXIST => continue,
            Err(error) => {
                return Err(error).context("failed to stage private capture artifact safely");
            }
        }
    }
    let staged_name = staged_name.context("failed to reserve private capture artifact staging")?;
    let mut staged_file = staged_file.expect("staged name and file are set together");
    if let Err(error) = staged_file
        .write_all(&bytes)
        .and_then(|_| staged_file.sync_all())
    {
        drop(staged_file);
        let _ = unlinkat(&directory, staged_name.as_str(), AtFlags::empty());
        return Err(error).context("failed to write private capture artifact");
    }
    drop(staged_file);
    if let Err(error) = linkat(
        &directory,
        staged_name.as_str(),
        &directory,
        file_name,
        AtFlags::empty(),
    ) {
        let _ = unlinkat(&directory, staged_name.as_str(), AtFlags::empty());
        if error == rustix::io::Errno::EXIST {
            bail!("capture artifact destination already exists");
        }
        return Err(error).context("failed to install private capture artifact safely");
    }
    unlinkat(&directory, staged_name.as_str(), AtFlags::empty())
        .context("failed to remove private capture artifact staging file")?;
    fsync(&directory).context("failed to sync private capture artifact directory")?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private_capture_artifact<T: ?Sized + serde::Serialize>(
    _value: &T,
    _target: &PrivateCaptureArtifactTarget,
) -> Result<()> {
    bail!(
        "secure private capture artifact writes are unavailable on this platform; write fails closed"
    )
}

fn print_capture_human<T: ?Sized + serde::Serialize>(artifact_kind: &str, value: &T) -> Result<()> {
    let value = serde_json::to_value(value).context("failed to render capture artifact")?;
    println!("{artifact_kind}");
    print_capture_human_value("", &value)?;
    Ok(())
}

fn print_capture_human_value(prefix: &str, value: &serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::Object(fields) if fields.is_empty() => println!("{prefix}\t{{}}"),
        serde_json::Value::Object(fields) => {
            for (name, value) in fields {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                print_capture_human_value(&path, value)?;
            }
        }
        serde_json::Value::Array(values) if values.is_empty() => println!("{prefix}\t[]"),
        serde_json::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                print_capture_human_value(&format!("{prefix}[{index}]"), value)?;
            }
        }
        scalar => println!("{prefix}\t{}", serde_json::to_string(scalar)?),
    }
    Ok(())
}

/// Plans capture from either the convenience path form or a checked request artifact.
///
/// The command boundary lives here so subsequent capture subcommands can move together
/// without changing their CLI contract or artifact-handling safeguards.
pub(super) fn plan_command(
    source: Option<String>,
    request_file: Option<PathBuf>,
    source_bytes: Option<PathBuf>,
    source_id: String,
    output: Option<PathBuf>,
    as_json: bool,
) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let paths = discover_paths(&cwd)?;
    let request = match (source, request_file) {
        (Some(source), None) => serde_json::from_value::<CaptureRequest>(json!({
            "schema": "memzoi/capture-request-v1",
            "sources": [{
                "source_id": source_id,
                "locator": {
                    "kind": "project_path",
                    "path": source,
                },
                "media_type": "text/markdown",
            }],
            "extractor": {
                "profile": "markdown-deterministic",
            },
        }))
        .context("failed to construct capture request")?,
        (None, Some(request_file)) => {
            parse_capture_request(&read_capture_artifact(&request_file, "capture request")?)?
        }
        _ => bail!("capture plan requires exactly one --source or --request-file"),
    };
    let source_inputs = capture_source_inputs(&request, source_bytes.as_deref())?;
    let plan = plan_capture_with_inputs(&paths, request, &source_inputs)?;
    let write_result = output
        .as_deref()
        .map(|destination| write_classified_capture_artifact(&plan, destination, &paths))
        .transpose();

    if as_json {
        print_json(&serde_json::to_value(&plan).context("failed to serialize capture plan")?)?;
    } else {
        print_capture_human("capture-plan", &plan)?;
    }
    write_result.map(|_| ())
}

pub(super) struct ReviewCommand {
    pub(super) plan_file: PathBuf,
    pub(super) decisions_file: PathBuf,
    pub(super) prior_review_file: Option<PathBuf>,
    pub(super) source_bytes: Option<PathBuf>,
    pub(super) reviewed_by: String,
    pub(super) reviewed_at: String,
    pub(super) output: Option<PathBuf>,
    pub(super) as_json: bool,
}

pub(super) fn review_command(command: ReviewCommand) -> Result<()> {
    let ReviewCommand {
        plan_file,
        decisions_file,
        prior_review_file,
        source_bytes,
        reviewed_by,
        reviewed_at,
        output,
        as_json,
    } = command;
    let plan = parse_capture_plan(&read_capture_artifact(&plan_file, "capture plan")?)?;
    let input = parse_capture_review_input(&read_capture_artifact(
        &decisions_file,
        "capture decisions",
    )?)?;
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let paths = discover_paths(&cwd)?;
    let source_inputs = capture_source_inputs(&plan.request, source_bytes.as_deref())?;
    let review = match prior_review_file {
        Some(prior_review_file) => {
            let prior = parse_capture_review(&read_capture_artifact(
                &prior_review_file,
                "prior capture review",
            )?)?;
            build_capture_review_with_prior_and_inputs(
                &paths,
                &plan,
                input,
                &prior,
                &source_inputs,
                &reviewed_by,
                &reviewed_at,
            )?
        }
        None => build_capture_review_with_inputs(
            &paths,
            &plan,
            input,
            &source_inputs,
            &reviewed_by,
            &reviewed_at,
        )?,
    };
    let write_result = output
        .as_deref()
        .map(|destination| write_classified_capture_artifact(&review, destination, &paths))
        .transpose();

    if as_json {
        print_json(&serde_json::to_value(&review).context("failed to serialize capture review")?)?;
    } else {
        print_capture_human("capture-review", &review)?;
    }
    write_result.map(|_| ())
}

pub(super) struct ApplyCommand {
    pub(super) plan_file: PathBuf,
    pub(super) review_file: PathBuf,
    pub(super) prior_review_file: Option<PathBuf>,
    pub(super) source_bytes: Option<PathBuf>,
    pub(super) plan_id: String,
    pub(super) review_id: String,
    pub(super) actor: String,
    pub(super) as_json: bool,
}

pub(super) fn apply_command(command: ApplyCommand) -> Result<()> {
    let ApplyCommand {
        plan_file,
        review_file,
        prior_review_file,
        source_bytes,
        plan_id,
        review_id,
        actor,
        as_json,
    } = command;
    let plan = parse_capture_plan(&read_capture_artifact(&plan_file, "capture plan")?)?;
    let review = parse_capture_review(&read_capture_artifact(&review_file, "capture review")?)?;
    let source_inputs = capture_source_inputs(&plan.request, source_bytes.as_deref())?;
    let service = open_service()?;
    let result = match prior_review_file {
        Some(prior_review_file) => {
            let prior = parse_capture_review(&read_capture_artifact(
                &prior_review_file,
                "prior capture review",
            )?)?;
            service.apply_capture_with_prior_and_inputs(
                &actor,
                plan,
                review,
                &prior,
                &source_inputs,
                &plan_id,
                &review_id,
            )?
        }
        None => service.apply_capture_with_inputs(
            &actor,
            plan,
            review,
            &source_inputs,
            &plan_id,
            &review_id,
        )?,
    };
    if as_json {
        print_json(
            &serde_json::to_value(&result).context("failed to serialize capture apply result")?,
        )
    } else {
        print_capture_human("capture-apply", &result)
    }
}
