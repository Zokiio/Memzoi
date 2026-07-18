use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use memzoi_core::{
    MaterializationAction, MaterializationAuthorizationCapability,
    RepositoryMaterializationCandidate, RepositoryMaterializationDecision,
    RepositoryMaterializationPlan, build_repository_materialization_decision,
    repository_materialization_candidate_plan, repository_materialization_policy,
    validate_materialization_identity,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;

use super::{normalize_absolute_path, open_service};
use crate::output::print_json;

const MATERIALIZATION_ARTIFACT_MAX_BYTES: usize = 2 * 1024 * 1024;

const MATERIALIZATION_APPLY_REPORT_SCHEMA: &str =
    "memzoi/repository-materialization-apply-report-v2";

#[derive(Serialize)]
struct MaterializationReviewCommand {
    path: String,
    visibility: &'static str,
    command_argv: Vec<String>,
}

pub(super) fn plan_command(
    candidate_file: PathBuf,
    output: Option<PathBuf>,
    as_json: bool,
) -> Result<()> {
    let candidate = read_candidate_artifact(&candidate_file)?;
    let plan = repository_materialization_candidate_plan(&candidate)
        .context("failed to derive materialization plan from candidate artifact")?;

    if let Some(destination) = output.as_deref() {
        write_materialization_artifact(&plan, destination)?;
    }

    if as_json {
        print_json(
            &serde_json::to_value(&plan).context("failed to serialize materialization plan")?,
        )
    } else {
        print_plan_human(&plan);
        Ok(())
    }
}

pub(super) fn decide_command(
    candidate_file: PathBuf,
    plan_file: PathBuf,
    decision_at: String,
    output: Option<PathBuf>,
    as_json: bool,
) -> Result<()> {
    let candidate = read_candidate_artifact(&candidate_file)?;
    let plan = read_plan_artifact(&plan_file)?;
    ensure_plan_matches_candidate(&candidate, &plan)?;
    let decision = build_repository_materialization_decision(
        &plan,
        decision_at,
        repository_materialization_policy(),
        MaterializationAuthorizationCapability::ExplicitCli,
    )
    .context("failed to build repository materialization decision")?;

    if let Some(destination) = output.as_deref() {
        write_materialization_artifact(&decision, destination)?;
    }

    if as_json {
        print_json(
            &serde_json::to_value(&decision)
                .context("failed to serialize materialization decision")?,
        )
    } else {
        print_decision_human(&decision);
        Ok(())
    }
}

pub(super) struct ApplyCommand {
    pub(super) candidate_file: PathBuf,
    pub(super) plan_file: PathBuf,
    pub(super) decision_file: PathBuf,
    pub(super) candidate_id: String,
    pub(super) plan_id: String,
    pub(super) decision_id: String,
    pub(super) as_json: bool,
}

pub(super) fn apply_command(command: ApplyCommand) -> Result<()> {
    let ApplyCommand {
        candidate_file,
        plan_file,
        decision_file,
        candidate_id,
        plan_id,
        decision_id,
        as_json,
    } = command;
    let candidate = read_candidate_artifact(&candidate_file)?;
    let plan = read_plan_artifact(&plan_file)?;
    let decision = read_decision_artifact(&decision_file)?;

    verify_supplied_identity("candidate", &candidate_id, &candidate.candidate_id)?;
    verify_supplied_identity("plan", &plan_id, &plan.plan_id)?;
    verify_supplied_identity("decision", &decision_id, &decision.decision_id)?;
    ensure_plan_matches_candidate(&candidate, &plan)?;
    ensure_decision_matches_plan(&plan, &decision)?;
    ensure_supported_apply_action(candidate.action)?;

    let service = open_service()?;
    let result = service.apply_repository_materialization(&plan, &decision, &candidate)?;
    let review = materialization_review_commands(service.paths().project_root.as_path(), &result)?;

    if as_json {
        print_json(&json!({
            "schema": MATERIALIZATION_APPLY_REPORT_SCHEMA,
            "result": result,
            "review": review,
        }))
    } else {
        print_apply_human(&result, &review);
        Ok(())
    }
}

fn read_candidate_artifact(path: &Path) -> Result<RepositoryMaterializationCandidate> {
    let candidate: RepositoryMaterializationCandidate =
        read_materialization_artifact(path, "candidate")?;
    candidate
        .validate()
        .context("candidate artifact failed core validation")?;
    Ok(candidate)
}

fn read_plan_artifact(path: &Path) -> Result<RepositoryMaterializationPlan> {
    let plan: RepositoryMaterializationPlan = read_materialization_artifact(path, "plan")?;
    plan.validate()
        .context("plan artifact failed core validation")?;
    Ok(plan)
}

fn read_decision_artifact(path: &Path) -> Result<RepositoryMaterializationDecision> {
    let decision: RepositoryMaterializationDecision =
        read_materialization_artifact(path, "decision")?;
    decision
        .validate()
        .context("decision artifact failed core validation")?;
    Ok(decision)
}

fn read_materialization_artifact<T: DeserializeOwned>(path: &Path, label: &str) -> Result<T> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {label} artifact"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} artifact must be a regular non-symlink file");
    }
    if metadata.len() > MATERIALIZATION_ARTIFACT_MAX_BYTES as u64 {
        bail!(
            "{label} artifact exceeds the {} MiB materialization artifact limit",
            MATERIALIZATION_ARTIFACT_MAX_BYTES / (1024 * 1024)
        );
    }

    let file = open_regular_materialization_artifact(path, label)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MATERIALIZATION_ARTIFACT_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {label} artifact"))?;
    if bytes.len() > MATERIALIZATION_ARTIFACT_MAX_BYTES {
        bail!(
            "{label} artifact exceeds the {} MiB materialization artifact limit",
            MATERIALIZATION_ARTIFACT_MAX_BYTES / (1024 * 1024)
        );
    }
    let text =
        String::from_utf8(bytes).with_context(|| format!("{label} artifact must be UTF-8 JSON"))?;
    serde_json::from_str(&text)
        .with_context(|| format!("failed to deserialize {label} artifact as strict JSON"))
}

#[cfg(unix)]
fn open_regular_materialization_artifact(path: &Path, label: &str) -> Result<fs::File> {
    use rustix::fs::{CWD, Mode, OFlags, openat};

    let file = openat(
        CWD,
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .with_context(|| format!("failed to open {label} artifact without following symlinks"))?;
    let file = fs::File::from(file);
    if !file
        .metadata()
        .with_context(|| format!("failed to inspect opened {label} artifact"))?
        .is_file()
    {
        bail!("{label} artifact must be a regular non-symlink file");
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_regular_materialization_artifact(path: &Path, label: &str) -> Result<fs::File> {
    let file = fs::File::open(path).with_context(|| format!("failed to open {label} artifact"))?;
    if !file
        .metadata()
        .with_context(|| format!("failed to inspect opened {label} artifact"))?
        .is_file()
    {
        bail!("{label} artifact must be a regular non-symlink file");
    }
    Ok(file)
}

fn write_materialization_artifact<T: Serialize + ?Sized>(
    value: &T,
    destination: &Path,
) -> Result<()> {
    let destination = materialization_artifact_destination(destination)?;
    match fs::symlink_metadata(&destination) {
        Ok(_) => bail!("materialization artifact destination already exists"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).context("failed to inspect materialization artifact destination");
        }
    }

    let mut bytes =
        serde_json::to_vec_pretty(value).context("failed to serialize materialization artifact")?;
    bytes.push(b'\n');
    if bytes.len() > MATERIALIZATION_ARTIFACT_MAX_BYTES {
        bail!(
            "serialized materialization artifact exceeds the {} MiB limit",
            MATERIALIZATION_ARTIFACT_MAX_BYTES / (1024 * 1024)
        );
    }

    let parent = destination
        .parent()
        .context("materialization artifact destination has no parent")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .context("failed to stage materialization artifact in its destination directory")?;
    staged
        .write_all(&bytes)
        .and_then(|_| staged.as_file_mut().sync_all())
        .context("failed to write materialization artifact")?;
    staged.persist_noclobber(&destination).map_err(|error| {
        anyhow::Error::new(error.error).context("failed to install materialization artifact")
    })?;
    Ok(())
}

fn materialization_artifact_destination(destination: &Path) -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let paths = memzoi_core::discover_paths(&cwd)?;
    let absolute = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        cwd.join(destination)
    };
    let normalized = normalize_absolute_path(&absolute);
    let normalized_memory_dir = normalize_absolute_path(&paths.memory_dir);
    if normalized.starts_with(&normalized_memory_dir) {
        bail!("materialization artifacts cannot be saved inside .memzoi");
    }

    let parent = normalized
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("materialization artifact destination has no parent")?;
    let parent_metadata = fs::symlink_metadata(parent)
        .context("failed to inspect materialization artifact destination parent")?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        bail!("materialization artifact destination parent must be a real directory");
    }
    let canonical_parent = parent
        .canonicalize()
        .context("failed to resolve materialization artifact destination parent")?;
    let file_name = normalized
        .file_name()
        .context("materialization artifact destination has no file name")?;
    let resolved = canonical_parent.join(file_name);
    let resolved_memory_dir = resolve_materialization_boundary(&paths.memory_dir)?;
    if resolved.starts_with(resolved_memory_dir) {
        bail!("materialization artifacts cannot be saved inside .memzoi");
    }
    Ok(resolved)
}

fn resolve_materialization_boundary(path: &Path) -> Result<PathBuf> {
    let mut existing_ancestor = normalize_absolute_path(path);
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
                    .context("materialization artifact boundary has no existing ancestor")?
                    .to_os_string();
                missing_suffix.push(component);
                if !existing_ancestor.pop() {
                    bail!("materialization artifact boundary has no existing ancestor");
                }
            }
            Err(error) => {
                return Err(error).context("failed to resolve materialization artifact boundary");
            }
        }
    }
}

fn ensure_plan_matches_candidate(
    candidate: &RepositoryMaterializationCandidate,
    plan: &RepositoryMaterializationPlan,
) -> Result<()> {
    let expected = repository_materialization_candidate_plan(candidate)
        .context("failed to derive expected plan from candidate artifact")?;
    if plan != &expected {
        bail!("plan artifact does not exactly match the candidate artifact");
    }
    Ok(())
}

fn ensure_decision_matches_plan(
    plan: &RepositoryMaterializationPlan,
    decision: &RepositoryMaterializationDecision,
) -> Result<()> {
    let expected = build_repository_materialization_decision(
        plan,
        decision.decision_at.clone(),
        repository_materialization_policy(),
        MaterializationAuthorizationCapability::ExplicitCli,
    )
    .context("failed to derive expected decision from plan artifact")?;
    if decision != &expected {
        bail!(
            "decision artifact does not exactly match the plan and repository materialization policy"
        );
    }
    Ok(())
}

fn verify_supplied_identity(label: &str, supplied: &str, actual: &str) -> Result<()> {
    validate_materialization_identity(supplied, &format!("supplied --{label}-id"))
        .with_context(|| format!("supplied --{label}-id must be a BLAKE3 identity"))?;
    if supplied != actual {
        bail!("supplied --{label}-id does not match the parsed {label} artifact");
    }
    Ok(())
}

fn ensure_supported_apply_action(action: MaterializationAction) -> Result<()> {
    if matches!(
        action,
        MaterializationAction::Create | MaterializationAction::Update
    ) {
        return Ok(());
    }
    bail!("materialize apply supports only create and update actions")
}

fn print_plan_human(plan: &RepositoryMaterializationPlan) {
    println!("materialization plan: {}", plan.plan_id);
    println!("candidate: {}", plan.candidate_id);
    for output in &plan.outputs {
        println!(
            "output: {}\taction: {}\trecord_id: {}\tsemantic_revision: {}",
            output.path,
            materialization_action_name(output.action),
            output.record_id,
            output.intended_semantic_revision.revision_hash,
        );
    }
}

fn print_decision_human(decision: &RepositoryMaterializationDecision) {
    println!("materialization decision: {}", decision.decision_id);
    println!("plan: {}", decision.plan_id);
    println!("candidate: {}", decision.candidate_id);
    println!("decision_at: {}", decision.decision_at);
}

fn materialization_review_commands(
    project_root: &Path,
    result: &memzoi_core::RepositoryMaterializationResult,
) -> Result<Vec<MaterializationReviewCommand>> {
    result
        .outputs
        .iter()
        .map(|output| {
            let tracked = git_path_is_tracked(project_root, &output.path)?;
            let (visibility, command_argv) = if tracked {
                (
                    "tracked",
                    vec![
                        "git".to_owned(),
                        "diff".to_owned(),
                        "--".to_owned(),
                        output.path.clone(),
                    ],
                )
            } else {
                (
                    "untracked_and_not_ignored",
                    vec![
                        "git".to_owned(),
                        "diff".to_owned(),
                        "--no-index".to_owned(),
                        "--".to_owned(),
                        "/dev/null".to_owned(),
                        output.path.clone(),
                    ],
                )
            };
            Ok(MaterializationReviewCommand {
                path: output.path.clone(),
                visibility,
                command_argv,
            })
        })
        .collect()
}

fn git_path_is_tracked(project_root: &Path, relative_path: &str) -> Result<bool> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["ls-files", "--error-unmatch", "--", relative_path])
        .output()
        .context("failed to inspect materialized path in Git")?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    bail!(
        "failed to determine materialized path Git visibility: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn print_apply_human(
    result: &memzoi_core::RepositoryMaterializationResult,
    review: &[MaterializationReviewCommand],
) {
    for (output, review) in result.outputs.iter().zip(review) {
        println!(
            "changed: {}\taction: {}\trecord_id: {}\tsemantic_revision: {}",
            output.path,
            materialization_action_name(output.action),
            output.record_id,
            output.semantic_revision.revision_hash,
        );
        println!("review: {}", review.command_argv.join(" "));
    }
}

fn materialization_action_name(action: MaterializationAction) -> &'static str {
    match action {
        MaterializationAction::Create => "create",
        MaterializationAction::Update => "update",
        MaterializationAction::Supersede => "supersede",
        MaterializationAction::Tombstone => "tombstone",
    }
}
