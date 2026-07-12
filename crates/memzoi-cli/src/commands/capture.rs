use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use memzoi_core::{
    CaptureRequest, build_capture_review_with_inputs, build_capture_review_with_prior_and_inputs,
    discover_paths, parse_capture_plan, parse_capture_request, parse_capture_review,
    parse_capture_review_input, plan_capture_with_inputs,
};
use serde_json::json;

use super::{
    capture_source_inputs, open_service, print_capture_human, read_capture_artifact,
    write_classified_capture_artifact,
};
use crate::output::print_json;

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
