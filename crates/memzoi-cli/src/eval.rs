use std::{fs, path::Path};
use std::{path::PathBuf, time::Duration};

use anyhow::Context;
use anyhow::{Result, bail};
use memzoi_core::{
    CaptureEvalBaselineStatus, CaptureEvalReport, ManifestDrivenRecallCandidate,
    RecallCompetitorReport, RecallDevelopmentLogV2, RecallEvalBaselineStatus,
    RecallEvalForbiddenIds, RecallEvalIntegrityMetric, RecallEvalLeakageMetric,
    RecallEvalRatioMetric, RecallEvalReport, RecallEvalSurface, RecallModelProfile,
    RecallOperationalReport, RecallV3Candidate, RecallV3Report, attach_capture_eval_baseline,
    attach_recall_eval_baseline, freeze_development, inspect_recall_model,
    install_recall_model_with, prepare_recall_v3_locked_commitment,
    require_recall_v3_candidates_ready, run_capture_eval, run_recall_competitor_eval,
    run_recall_eval, run_recall_operational_eval, run_recall_v3_eval,
    run_recall_v3_eval_with_candidates, verify_recall_v3_locked_commitment,
    write_capture_eval_baseline, write_recall_eval_baseline, write_recall_v3_commitment,
    write_recall_v3_locked_commitment,
};
use url::Url;

const MODEL_DOWNLOAD_REDIRECT_LIMIT: usize = 5;

use crate::{
    cli::{
        RecallV3CandidateCommands, RecallV3Commands, RecallV3DevelopmentCommands,
        RecallV3ModelCommands,
    },
    output::print_json,
};

pub(crate) fn recall_v3_subcommand(command: RecallV3Commands) -> Result<()> {
    match command {
        RecallV3Commands::Model { command } => match command {
            RecallV3ModelCommands::Install {
                profile,
                model_root,
                force,
                json,
            } => {
                let profile = RecallModelProfile::load(&profile)?;
                let agent = ureq::AgentBuilder::new()
                    .redirects(0)
                    .timeout_connect(Duration::from_secs(10))
                    .timeout_read(Duration::from_secs(60))
                    .timeout_write(Duration::from_secs(30))
                    .build();
                let installed = install_recall_model_with(&profile, &model_root, force, |url| {
                    let response = model_download_response(&agent, &profile, url)?;
                    Ok(response.into_reader())
                })?;
                if json {
                    print_json(
                        &serde_json::json!({"profile_id": profile.id, "path": installed, "verified": true}),
                    )?;
                } else {
                    println!("installed:\t{}\nverified:\ttrue", installed.display());
                }
                Ok(())
            }
            RecallV3ModelCommands::Inspect {
                profile,
                model_root,
                json,
            } => {
                let profile = RecallModelProfile::load(&profile)?;
                let manifest = inspect_recall_model(&profile, &model_root.join(&profile.id))?;
                if json {
                    print_json(&serde_json::to_value(&manifest)?)?;
                } else {
                    println!(
                        "profile:\t{}\nverified:\ttrue\nfiles:\t{}",
                        profile.id,
                        manifest.files.len()
                    );
                }
                Ok(())
            }
        },
        RecallV3Commands::Development { command } => match command {
            RecallV3DevelopmentCommands::Run {
                matrix,
                corpus,
                model_root,
                output,
                attempted_at,
                generation,
                json,
            } => recall_v3_development_run(
                matrix,
                corpus,
                model_root,
                output,
                attempted_at,
                generation,
                json,
            ),
            RecallV3DevelopmentCommands::Freeze {
                log,
                output,
                frozen_at,
                json,
            } => {
                if output.exists() {
                    bail!("refusing to overwrite an existing freeze artifact");
                }
                let evidence_root = log.parent().unwrap_or_else(|| std::path::Path::new("."));
                let log: RecallDevelopmentLogV2 = serde_json::from_slice(&std::fs::read(&log)?)?;
                let matrix = memzoi_core::RecallDevelopmentMatrix::load(
                    evidence_root.join("development-matrix.json"),
                )?;
                let report: RecallV3Report = serde_json::from_slice(&std::fs::read(
                    evidence_root.join("matrix-report.json"),
                )?)?;
                memzoi_core::verify_development_evidence(&log, &matrix, &report, evidence_root)?;
                let freeze = freeze_development(&log, &frozen_at)?;
                write_json_new(&output, &freeze)?;
                if json {
                    print_json(&serde_json::to_value(&freeze)?)?;
                } else {
                    println!(
                        "frozen:\t{}\nfinalists:\t{}",
                        output.display(),
                        freeze.finalists.len()
                    );
                }
                Ok(())
            }
            RecallV3DevelopmentCommands::Publish { run, output, json } => {
                recall_v3_development_publish(&run, &output, json)
            }
        },
        RecallV3Commands::Candidate { command } => match command {
            RecallV3CandidateCommands::Build {
                profile,
                matrix,
                corpus,
                model_root,
                template,
                output,
                generation,
                json,
            } => recall_v3_candidate_build(RecallCandidateBuildRequest {
                profile,
                matrix,
                corpus,
                model_root,
                template,
                output,
                generation,
                json,
            }),
        },
    }
}

#[cfg_attr(not(feature = "recall-models"), allow(dead_code))]
struct RecallCandidateBuildRequest {
    profile: PathBuf,
    matrix: PathBuf,
    corpus: PathBuf,
    model_root: PathBuf,
    template: String,
    output: PathBuf,
    generation: String,
    json: bool,
}

#[cfg(not(feature = "recall-models"))]
fn recall_v3_candidate_build(_request: RecallCandidateBuildRequest) -> Result<()> {
    bail!("candidate build requires --features recall-models")
}

#[cfg(not(feature = "recall-models"))]
fn recall_v3_development_run(
    _matrix: PathBuf,
    _corpus: PathBuf,
    _model_root: PathBuf,
    _output: PathBuf,
    _attempted_at: String,
    _generation: String,
    _json: bool,
) -> Result<()> {
    bail!("development run requires --features recall-models")
}

#[cfg(feature = "recall-models")]
#[derive(Debug)]
struct BuiltCandidate {
    path: PathBuf,
    profile_id: String,
    template: String,
    architecture: memzoi_core::RecallCandidateArchitecture,
    artifact_digest: String,
    candidate_digest: String,
}

#[cfg(feature = "recall-models")]
fn build_candidate_bundle(
    profile_path: &Path,
    matrix: &memzoi_core::RecallDevelopmentMatrix,
    corpus: &Path,
    model_root: &Path,
    template: &str,
    output: &Path,
    generation: &str,
) -> Result<Vec<BuiltCandidate>> {
    if !matrix.templates.iter().any(|value| value == template) {
        bail!("template is not declared by the development matrix");
    }
    let profile = memzoi_core::RecallModelProfile::load(profile_path)?;
    let model_dir = model_root.join(&profile.id);
    let install = memzoi_core::inspect_recall_model(&profile, &model_dir)?;
    let model_digest = memzoi_core::recall_model_install_digest(&install)?;
    let embedding_corpus = memzoi_core::load_recall_v3_embedding_corpus(corpus)?;
    let mut embedder = memzoi_core::LocalRecallEmbedder::load_with_threads(
        profile.clone(),
        &model_dir,
        matrix.parameters.threads,
    )?;
    let artifact = memzoi_core::build_recall_vector_artifact(
        &profile,
        template,
        &model_digest,
        generation,
        &embedding_corpus.inputs,
        matrix.parameters.batch_size,
        &mut embedder,
    )?;
    fs::create_dir_all(output)?;
    let artifact_digest =
        memzoi_core::write_recall_vector_artifact(&artifact, output, Path::new("vectors.json"))?;
    matrix
        .architectures
        .iter()
        .copied()
        .map(|architecture| {
            let manifest = memzoi_core::build_recall_candidate_manifest(
                &profile,
                template,
                architecture,
                &matrix.parameters,
                &artifact,
                &artifact_digest,
                PathBuf::from("vectors.json"),
            )?;
            let candidate_digest = memzoi_core::recall_candidate_manifest_digest(&manifest)?;
            let name = match architecture {
                memzoi_core::RecallCandidateArchitecture::SemanticOnly => "semantic_only.json",
                memzoi_core::RecallCandidateArchitecture::LexicalRerank => "lexical_rerank.json",
                memzoi_core::RecallCandidateArchitecture::LexicalSemanticUnion => {
                    "lexical_semantic_union.json"
                }
            };
            let path = output.join(name);
            write_json_new(&path, &manifest)?;
            Ok(BuiltCandidate {
                path,
                profile_id: profile.id.clone(),
                template: template.into(),
                architecture,
                artifact_digest: artifact_digest.clone(),
                candidate_digest,
            })
        })
        .collect()
}

#[cfg(feature = "recall-models")]
fn recall_v3_candidate_build(request: RecallCandidateBuildRequest) -> Result<()> {
    let RecallCandidateBuildRequest {
        profile,
        matrix,
        corpus,
        model_root,
        template,
        output,
        generation,
        json,
    } = request;
    let matrix = memzoi_core::RecallDevelopmentMatrix::load(matrix)?;
    let built = build_candidate_bundle(
        &profile,
        &matrix,
        &corpus,
        &model_root,
        &template,
        &output,
        &generation,
    )?;
    if json {
        print_json(&serde_json::json!({"candidate_count": built.len(), "output": output}))?;
    } else {
        println!(
            "candidates:\t{}\noutput:\t{}",
            built.len(),
            output.display()
        );
    }
    Ok(())
}

#[cfg(feature = "recall-models")]
fn recall_v3_development_run(
    matrix_path: PathBuf,
    corpus: PathBuf,
    model_root: PathBuf,
    output: PathBuf,
    attempted_at: String,
    generation: String,
    json: bool,
) -> Result<()> {
    use memzoi_core::RecallV3Candidate as _;
    memzoi_core::FixedClock::from_rfc3339(&attempted_at)?;
    let matrix = memzoi_core::RecallDevelopmentMatrix::load(&matrix_path)?;
    fs::create_dir_all(&output)?;
    write_json_new(&output.join("development-matrix.json"), &matrix)?;
    let matrix_root = matrix_path.parent().unwrap_or_else(|| Path::new("."));
    let mut built = Vec::new();
    for profile in &matrix.profiles {
        for template in &matrix.templates {
            let profile_path = matrix_root.join(profile);
            let profile_id = memzoi_core::RecallModelProfile::load(&profile_path)?.id;
            let bundle_root = output
                .join("candidates")
                .join(profile_id)
                .join(template.replace('/', "-"));
            built.extend(build_candidate_bundle(
                &profile_path,
                &matrix,
                &corpus,
                &model_root,
                template,
                &bundle_root,
                &generation,
            )?);
        }
    }
    if built.len() != 18 {
        bail!("development matrix did not produce exactly 18 candidates");
    }
    let mut candidates = built
        .iter()
        .map(|candidate| ManifestDrivenRecallCandidate::load(&candidate.path))
        .collect::<Result<Vec<_>>>()?;
    for candidate in &candidates {
        candidate.require_ready()?;
    }
    let mut refs = candidates
        .iter_mut()
        .map(|candidate| candidate as &mut dyn RecallV3Candidate)
        .collect::<Vec<_>>();
    let report = run_recall_v3_eval_with_candidates(&corpus, &mut refs)?;
    memzoi_core::validate_development_report(&report)?;
    memzoi_core::require_recall_v3_candidates_ready(&report)?;
    let report_path = output.join("matrix-report.json");
    write_json_new(&report_path, &report)?;
    let reports = report
        .candidates
        .iter()
        .skip(1)
        .map(|candidate| (candidate.manifest.id.as_str(), candidate))
        .collect::<std::collections::BTreeMap<_, _>>();
    let environment_digest = format!(
        "{}-{}-threads-{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        matrix.parameters.threads
    );
    let attempts = built
        .iter()
        .map(|candidate| {
            let manifest = ManifestDrivenRecallCandidate::load(&candidate.path)?.manifest();
            let candidate_report = reports
                .get(manifest.id.as_str())
                .context("matrix report omitted a built candidate")?;
            Ok(memzoi_core::RecallDevelopmentAttemptV2 {
                attempted_at: attempted_at.clone(),
                candidate_id: manifest.id,
                candidate_digest: candidate.candidate_digest.clone(),
                profile_id: candidate.profile_id.clone(),
                template: candidate.template.clone(),
                architecture: candidate.architecture,
                candidate_manifest: Some(candidate.path.strip_prefix(&output)?.to_owned()),
                vector_artifact: Some(
                    candidate
                        .path
                        .parent()
                        .context("candidate manifest has no parent")?
                        .join("vectors.json")
                        .strip_prefix(&output)?
                        .to_owned(),
                ),
                outcome: memzoi_core::RecallDevelopmentOutcome::Completed,
                reason_code: None,
                report: Some((*candidate_report).clone()),
                report_digest: Some(memzoi_core::recall_candidate_report_digest(
                    candidate_report,
                )?),
                artifact_digest: Some(candidate.artifact_digest.clone()),
                environment_digest: environment_digest.clone(),
                trust_eligible: memzoi_core::recall_candidate_trust_eligible(candidate_report),
                development_quality_passed: candidate_report.passed,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let log = memzoi_core::RecallDevelopmentLogV2 {
        version: memzoi_core::RECALL_DEVELOPMENT_LOG_V2.into(),
        corpus_digest: report.digests.corpus.clone(),
        judgment_digest: report.digests.judgments.clone(),
        matrix_digest: memzoi_core::recall_development_matrix_digest(&matrix)?,
        runner_digest: report.digests.runner.clone(),
        attempts,
    };
    log.validate()?;
    write_json_new(&output.join("development-log.json"), &log)?;
    write_json_new(
        &output.join("environment.json"),
        &serde_json::json!({
            "target_os": std::env::consts::OS,
            "target_arch": std::env::consts::ARCH,
            "threads": matrix.parameters.threads,
            "digest": environment_digest,
        }),
    )?;
    if json {
        print_json(
            &serde_json::json!({"candidate_count": built.len(), "output": output, "passed": report.passed}),
        )?;
    } else {
        println!(
            "candidates:\t{}\noutput:\t{}\npassed:\t{}",
            built.len(),
            output.display(),
            report.passed
        );
    }
    Ok(())
}

fn write_json_new(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    if path.exists() {
        bail!("refusing to overwrite {}", path.display());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist_noclobber(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

fn recall_v3_development_publish(run: &Path, output: &Path, json: bool) -> Result<()> {
    if output.exists() {
        bail!("refusing to overwrite published development evidence");
    }
    let log: RecallDevelopmentLogV2 =
        serde_json::from_slice(&fs::read(run.join("development-log.json"))?)?;
    let matrix = memzoi_core::RecallDevelopmentMatrix::load(run.join("development-matrix.json"))?;
    let report: RecallV3Report =
        serde_json::from_slice(&fs::read(run.join("matrix-report.json"))?)?;
    memzoi_core::verify_development_evidence(&log, &matrix, &report, run)?;
    let freeze: memzoi_core::RecallDevelopmentFreeze =
        serde_json::from_slice(&fs::read(run.join("frozen-candidates.json"))?)?;
    if memzoi_core::freeze_development(&log, &freeze.frozen_at)? != freeze {
        bail!("frozen candidate artifact does not match verified development evidence");
    }
    fs::create_dir_all(output.join("frozen-manifests"))?;
    write_json_new(&output.join("development-matrix.json"), &matrix)?;
    write_json_new(&output.join("matrix-report.json"), &report)?;
    write_json_new(&output.join("development-log.json"), &log)?;
    write_json_new(&output.join("frozen-candidates.json"), &freeze)?;
    let environment: serde_json::Value =
        serde_json::from_slice(&fs::read(run.join("environment.json"))?)?;
    write_json_new(&output.join("environment.json"), &environment)?;
    for finalist in freeze.finalists.iter().skip(1) {
        let attempt = log
            .attempts
            .iter()
            .find(|attempt| attempt.candidate_id == finalist.candidate_id)
            .context("frozen finalist is missing from development attempts")?;
        let source = run.join(
            attempt
                .candidate_manifest
                .as_deref()
                .context("frozen finalist omitted its manifest path")?,
        );
        let manifest: memzoi_core::RecallRetrievalCandidateManifest =
            serde_json::from_slice(&fs::read(source)?)?;
        write_json_new(
            &output
                .join("frozen-manifests")
                .join(format!("{}.json", finalist.architecture)),
            &manifest,
        )?;
    }
    fs::write(
        output.join("README.md"),
        "# Recall-v3 observed development evidence\n\nThis directory contains the verified 18-candidate development run and frozen manifests for issue #77. Model weights and vector artifacts are intentionally excluded. Reproduce with `make recall-v3-model-install`, `RECALL_V3_ATTEMPTED_AT=<RFC3339> make recall-v3-development-run`, and `RECALL_V3_FROZEN_AT=<RFC3339> make recall-v3-development-freeze`. Publish a verified copy to a new directory with `RECALL_V3_PUBLISH_OUTPUT=<path> make recall-v3-development-publish`. The freeze and publish commands recompute every candidate, artifact, report, matrix, corpus, runner, and environment binding from the ignored run directory.\n",
    )?;
    if json {
        print_json(&serde_json::json!({"output": output, "finalists": freeze.finalists.len()}))?;
    } else {
        println!(
            "published:\t{}\nfinalists:\t{}",
            output.display(),
            freeze.finalists.len()
        );
    }
    Ok(())
}

fn model_download_response(
    agent: &ureq::Agent,
    profile: &RecallModelProfile,
    initial_url: &str,
) -> Result<ureq::Response> {
    let mut url = Url::parse(initial_url)?;
    for redirects_followed in 0..=MODEL_DOWNLOAD_REDIRECT_LIMIT {
        if !profile.permits_url(url.as_str())? {
            bail!("model download URL has a non-allowlisted origin");
        }
        let response = agent
            .get(url.as_str())
            .set("User-Agent", concat!("memzoi/", env!("CARGO_PKG_VERSION")))
            .call()
            .map_err(|error| anyhow::anyhow!("model download failed: {error}"))?;
        if !(300..400).contains(&response.status()) {
            return Ok(response);
        }
        if redirects_followed == MODEL_DOWNLOAD_REDIRECT_LIMIT {
            bail!("model download exceeded the redirect limit");
        }
        let location = response
            .header("Location")
            .ok_or_else(|| anyhow::anyhow!("model download redirect omitted Location"))?;
        let redirected = url.join(location)?;
        if !profile.permits_url(redirected.as_str())? {
            bail!("model download redirect targets a non-allowlisted origin");
        }
        url = redirected;
    }
    unreachable!("bounded redirect loop returns or fails")
}

pub(crate) fn recall_operational_eval_command(evidence: PathBuf, as_json: bool) -> Result<()> {
    let report = run_recall_operational_eval(evidence)?;
    if as_json {
        print_json(&serde_json::to_value(&report)?)?;
    } else {
        print_operational_human_report(&report);
    }
    if !report.passed {
        bail!("recall operational evaluation gates failed");
    }
    Ok(())
}

pub(crate) fn recall_competitor_eval_command(evidence: PathBuf, as_json: bool) -> Result<()> {
    let report = run_recall_competitor_eval(evidence)?;
    if as_json {
        print_json(&serde_json::to_value(&report)?)?;
    } else {
        print_competitor_human_report(&report);
    }
    if !report.passed {
        bail!("recall competitor evaluation gates failed");
    }
    Ok(())
}

fn print_operational_human_report(report: &RecallOperationalReport) {
    println!("Memzoi recall operational evaluation ({})", report.version);
    println!("candidate_digest:\t{}", report.candidate_digest);
    println!(
        "task_utility_pass_rate:\t{:.6}",
        report.task_utility_pass_rate
    );
    println!(
        "operational_pass_rate:\t{:.6}",
        report.operational_pass_rate
    );
    println!("fallback_parity:\t{:.6}", report.fallback_parity);
    println!("warm_p95_ms:\t{:.3}", report.performance.warm_query_p95_ms);
    println!("result:\t{}", pass_label(report.passed));
}

fn print_competitor_human_report(report: &RecallCompetitorReport) {
    println!("Memzoi recall competitor evaluation ({})", report.version);
    println!("protocol_digest:\t{}", report.protocol_digest);
    println!("products:\t{}", report.products.len());
    println!("retrieval_tracks:\t{}", report.retrieval_results.len());
    println!("end_to_end_tracks:\t{}", report.end_to_end_results.len());
    println!("evidence_kind:\t{:?}", report.evidence_kind);
    println!(
        "eligible_for_ship_decision:\t{}",
        report.eligible_for_ship_decision
    );
    println!("release_gate:\t{}", report.internal_release_gate);
    println!("result:\t{}", pass_label(report.passed));
}

pub(crate) fn recall_v3_eval_command(
    corpus: PathBuf,
    candidate_paths: Vec<PathBuf>,
    commitment: Option<PathBuf>,
    prepare_locked_commitment: Option<PathBuf>,
    verify_locked_commitment: Option<PathBuf>,
    require_ready_candidates: bool,
    as_json: bool,
) -> Result<()> {
    let mut candidates = candidate_paths
        .into_iter()
        .map(ManifestDrivenRecallCandidate::load)
        .collect::<Result<Vec<_>>>()?;
    let candidate_manifests = candidates
        .iter()
        .map(|candidate| candidate.manifest())
        .collect::<Vec<_>>();
    let require_ready = require_ready_candidates
        || prepare_locked_commitment.is_some()
        || verify_locked_commitment.is_some();
    if require_ready {
        for candidate in &candidates {
            candidate.require_ready()?;
        }
    }
    if let Some(path) = prepare_locked_commitment {
        let prepared = prepare_recall_v3_locked_commitment(&corpus, &candidate_manifests)?;
        write_recall_v3_locked_commitment(&prepared, path)?;
        if as_json {
            print_json(&serde_json::to_value(&prepared)?)?;
        } else {
            println!("locked_commitment:\tprepared");
            println!("corpus_digest:\t{}", prepared.corpus_digest);
            println!("judgment_digest:\t{}", prepared.judgment_digest);
        }
        return Ok(());
    }
    let verified_locked_commitment = verify_locked_commitment
        .as_deref()
        .map(|path| verify_recall_v3_locked_commitment(&corpus, path, &candidate_manifests))
        .transpose()?;
    let report = if candidates.is_empty() {
        run_recall_v3_eval(&corpus)?
    } else {
        let mut candidate_refs = candidates
            .iter_mut()
            .map(|candidate| candidate as &mut dyn RecallV3Candidate)
            .collect::<Vec<_>>();
        run_recall_v3_eval_with_candidates(&corpus, &mut candidate_refs)?
    };
    if let Some(expected) = verified_locked_commitment
        && report.commitment != expected
    {
        bail!("locked recall commitment changed during evaluation");
    }
    if let Some(path) = commitment {
        write_recall_v3_commitment(&report, path)?;
    }
    if as_json {
        print_json(&serde_json::to_value(&report)?)?;
    } else {
        print_recall_v3_human_report(&report);
    }
    if !report.passed {
        bail!("recall-v3 evaluation gates failed");
    }
    if require_ready {
        require_recall_v3_candidates_ready(&report)?;
    }
    Ok(())
}

fn print_recall_v3_human_report(report: &RecallV3Report) {
    println!("Memzoi recall v3 evaluation ({})", report.version);
    println!("corpus:\t{} ({:?})", report.corpus.name, report.corpus.kind);
    println!("corpus_digest:\t{}", report.digests.corpus);
    println!("judgment_digest:\t{}", report.digests.judgments);
    for candidate in &report.candidates {
        println!();
        println!("candidate:\t{}", candidate.manifest.id);
        println!("ndcg@10:\t{:.6}", candidate.aggregate.mean_ndcg_at_10);
        println!("recall@k:\t{:.6}", candidate.aggregate.mean_recall_at_k);
        println!("mrr:\t{:.6}", candidate.aggregate.mean_mrr);
        println!("result:\t{}", pass_label(candidate.passed));
    }
    println!("isolated_state:\t{}", report.isolated_state);
    println!("result:\t{}", pass_label(report.passed));
}

pub(crate) fn recall_eval_command(
    corpus: PathBuf,
    baseline: Option<PathBuf>,
    update_baseline: bool,
    as_json: bool,
) -> Result<()> {
    let mut report = run_recall_eval(&corpus)?;
    let thresholds_passed = report.passed;

    if update_baseline && thresholds_passed {
        let Some(baseline_path) = baseline.as_ref() else {
            bail!("--update-baseline requires --baseline <PATH>");
        };
        write_recall_eval_baseline(&report, baseline_path)?;
    }
    if let Some(baseline_path) = baseline.as_ref()
        && (!update_baseline || thresholds_passed)
    {
        attach_recall_eval_baseline(&mut report, baseline_path)?;
    }

    if as_json {
        print_json(&serde_json::to_value(&report)?)?;
    } else {
        print_human_report(&report);
    }

    if update_baseline && !thresholds_passed {
        bail!("recall evaluation thresholds failed; baseline was not modified");
    }
    if !report.passed {
        bail!("recall evaluation thresholds or baseline compatibility failed");
    }
    Ok(())
}

pub(crate) fn capture_eval_command(
    corpus: PathBuf,
    baseline: Option<PathBuf>,
    update_baseline: bool,
    as_json: bool,
) -> Result<()> {
    let mut report = run_capture_eval(&corpus)?;
    let gates_passed = report.gates_passed;

    if update_baseline && gates_passed {
        let Some(baseline_path) = baseline.as_ref() else {
            bail!("--update-baseline requires --baseline <PATH>");
        };
        write_capture_eval_baseline(&report, baseline_path)?;
    }
    if let Some(baseline_path) = baseline.as_ref()
        && (!update_baseline || gates_passed)
    {
        attach_capture_eval_baseline(&mut report, baseline_path)?;
    }

    if as_json {
        print_json(&serde_json::to_value(&report)?)?;
    } else {
        print_capture_human_report(&report);
    }

    if update_baseline && !gates_passed {
        bail!("capture evaluation gates failed; baseline was not modified");
    }
    if !report.passed {
        bail!("capture evaluation gates or baseline equality failed");
    }
    Ok(())
}

fn print_capture_human_report(report: &CaptureEvalReport) {
    println!("Memzoi capture evaluation ({})", report.version);
    println!("corpus:\t{}", report.corpus.name);
    println!("corpus_version:\t{}", report.corpus.version);
    println!("corpus_digest:\t{}", report.corpus.digest);
    println!("profiles:\t{}", report.corpus.profile_count);
    println!("cases:\t{}", report.corpus.case_count);
    println!();
    println!("case results");
    for case in &report.cases {
        println!("{}\t{}\t{}", pass_label(case.passed), case.profile, case.id);
        println!(
            "  candidates:\ttp={} fp={} fn={} forbidden={}",
            case.true_positives,
            case.false_positives,
            case.false_negatives,
            case.forbidden_hits.len()
        );
        println!(
            "  evidence/classification:\t{:.6}/{:.6}/{:.6}",
            case.evidence_validity.rate,
            case.destination_accuracy.rate,
            case.sensitivity_accuracy.rate
        );
    }
    println!();
    println!("aggregate");
    println!(
        "candidate_precision:\t{:.6}",
        report.metrics.candidate_precision.value
    );
    println!(
        "candidate_recall:\t{:.6}",
        report.metrics.candidate_recall.value
    );
    println!(
        "evidence_validity:\t{:.6}",
        report.metrics.evidence_validity.rate
    );
    println!(
        "case_pass_rate:\t{:.6}",
        report.metrics.case_pass_rate.value
    );
    println!("hard_gates:\t{}", pass_label(report.hard_gates.passed()));
    if let Some(results) = &report.threshold_results {
        println!("thresholds:\t{}", pass_label(results.passed));
    }
    if let Some(baseline) = &report.baseline {
        let status = match baseline.status {
            CaptureEvalBaselineStatus::Match => "match",
            CaptureEvalBaselineStatus::Changed => "changed",
            CaptureEvalBaselineStatus::Incompatible => "incompatible",
        };
        println!("baseline:\t{status}");
    }
    println!("result:\t{}", pass_label(report.passed));
}

fn print_human_report(report: &RecallEvalReport) {
    println!("Memzoi recall evaluation ({})", report.version);
    println!("corpus:\t{}", report.corpus.name);
    println!("corpus_version:\t{}", report.corpus.version);
    println!("corpus_digest:\t{}", report.corpus.digest);
    println!("evaluated_at:\t{}", report.corpus.evaluated_at);
    println!(
        "fixtures:\trepo={} runtime={} proposals={}",
        report.corpus.fixture_record_count,
        report.corpus.runtime_fixture_count,
        report.corpus.proposal_fixture_count
    );
    println!(
        "runtime:\tmemzoi={} {}/{} {} sqlite={} isolated={} network_required={}",
        report.runtime.memzoi_version,
        report.runtime.target_os,
        report.runtime.target_arch,
        report.runtime.build_profile,
        report.runtime.sqlite_version,
        report.runtime.isolated_state,
        report.runtime.network_required
    );

    if !report.proposal_fixtures.is_empty() {
        println!();
        println!("proposal fixtures");
        for fixture in &report.proposal_fixtures {
            println!(
                "{}	{} -> {}",
                pass_label(fixture.passed),
                fixture.proposal_id,
                fixture.record_id
            );
            println!(
                "  evidence:\t{} {}",
                fixture.source_kind, fixture.source_ref
            );
            println!(
                "  checks:\tsource={} resolution={} lineage={} separate={} applicability={}",
                pass_label(fixture.source_preserved),
                pass_label(fixture.resolution_preserved),
                pass_label(fixture.lineage_preserved),
                pass_label(fixture.lineage_separate_from_evidence),
                pass_label(fixture.applicability_preserved)
            );
        }
    }

    println!();
    println!("cases");
    for case in &report.cases {
        println!(
            "{}\t{}\t{}",
            pass_label(case.passed),
            surface_label(case.surface),
            case.id
        );
        if let Some(query) = &case.query {
            println!("  query:\t{query}");
        }
        println!("  retrieved_ids:\t{}", joined_or_none(&case.retrieved_ids));
        println!(
            "  classification:\ttp={} fp={} fn={}",
            case.true_positives, case.false_positives, case.false_negatives
        );
        if let Some(recall_at_k) = case.recall_at_k {
            println!("  recall_at_k:\t{recall_at_k:.6}");
        }
        if let Some(mrr) = case.mrr {
            println!("  mrr:\t{mrr:.6}");
        }
        print_forbidden_hits(&case.forbidden_hits);
        println!(
            "  citation_integrity:\t{}/{} ({:.6})",
            case.citation_integrity.valid,
            case.citation_integrity.checked,
            case.citation_integrity.rate
        );
        println!(
            "  provenance_integrity:\t{}/{} ({:.6})",
            case.provenance_integrity.valid,
            case.provenance_integrity.checked,
            case.provenance_integrity.rate
        );
        if let Some(estimated_usage) = case.estimated_usage {
            println!("  estimated_usage:\t{estimated_usage}");
        }
        println!("  latency_ms:\t{:.3}", case.latency_ms);
        for (assertion, passed) in &case.assertions {
            println!("  assertion.{assertion}:\t{}", pass_label(*passed));
        }
    }

    print_metrics(report);
    print_baseline(report);
    println!();
    println!("overall:\t{}", pass_label(report.passed));
}

fn print_metrics(report: &RecallEvalReport) {
    let metrics = &report.metrics;
    let thresholds = &report.thresholds;
    let results = &report.threshold_results;

    println!();
    println!("metrics and thresholds");
    println!("  search.case_count:\t{}", metrics.search.case_count);
    print_minimum(
        "search.mean_recall_at_k",
        metrics.search.mean_recall_at_k,
        thresholds.min_mean_recall_at_k,
        results.min_mean_recall_at_k,
    );
    print_minimum(
        "search.mean_mrr",
        metrics.search.mean_mrr,
        thresholds.min_mean_mrr,
        results.min_mean_mrr,
    );

    println!("  precheck.case_count:\t{}", metrics.precheck.case_count);
    println!(
        "  precheck.counts:\ttp={} fp={} fn={}",
        metrics.precheck.true_positives,
        metrics.precheck.false_positives,
        metrics.precheck.false_negatives
    );
    print_ratio_minimum(
        "precheck.precision",
        &metrics.precheck.precision,
        thresholds.min_precheck_precision,
        results.min_precheck_precision,
    );
    print_ratio_minimum(
        "precheck.recall",
        &metrics.precheck.recall,
        thresholds.min_precheck_recall,
        results.min_precheck_recall,
    );

    print_leakage_maximum(
        "leakage.stale",
        &metrics.leakage.stale,
        thresholds.max_stale_leakage_rate,
        results.max_stale_leakage_rate,
    );
    print_leakage_maximum(
        "leakage.expired",
        &metrics.leakage.expired,
        thresholds.max_expired_leakage_rate,
        results.max_expired_leakage_rate,
    );
    print_leakage_maximum(
        "leakage.scope",
        &metrics.leakage.scope,
        thresholds.max_scope_leakage_rate,
        results.max_scope_leakage_rate,
    );
    print_leakage("leakage.prohibited", &metrics.leakage.prohibited);
    print_leakage("leakage.destination", &metrics.leakage.destination);
    print_leakage_maximum(
        "leakage.forbidden",
        &metrics.leakage.forbidden,
        thresholds.max_forbidden_hit_rate,
        results.max_forbidden_hit_rate,
    );

    print_integrity_minimum(
        "citation_integrity",
        &metrics.citation_integrity,
        thresholds.min_citation_integrity,
        results.min_citation_integrity,
    );
    print_integrity_minimum(
        "provenance_integrity",
        &metrics.provenance_integrity,
        thresholds.min_provenance_integrity,
        results.min_provenance_integrity,
    );

    println!(
        "  token_usage:\tunit={} estimator={} samples={} total={} mean={:.3} p50={} p95={} max={}",
        metrics.token_usage.unit,
        metrics.token_usage.estimator,
        metrics.token_usage.sample_count,
        metrics.token_usage.total,
        metrics.token_usage.mean,
        metrics.token_usage.p50,
        metrics.token_usage.p95,
        metrics.token_usage.max
    );
    match thresholds.max_estimated_usage {
        Some(maximum) => println!(
            "  token_usage.max_threshold:\t{} required <= {} {}",
            metrics.token_usage.max,
            maximum,
            pass_label(results.max_estimated_usage.unwrap_or(false))
        ),
        None => println!("  token_usage.max_threshold:\tno threshold"),
    }

    println!(
        "  latency:\tunit={} timer={} samples={} p50={:.3} p95={:.3}",
        metrics.latency.unit,
        metrics.latency.timer,
        metrics.latency.sample_count,
        metrics.latency.p50,
        metrics.latency.p95
    );
    match thresholds.max_p95_latency_ms {
        Some(maximum) => println!(
            "  latency.p95_threshold:\t{:.3} required <= {:.3} {}",
            metrics.latency.p95,
            maximum,
            pass_label(results.max_p95_latency_ms.unwrap_or(false))
        ),
        None => println!("  latency.p95_threshold:\tno threshold"),
    }

    print_ratio_minimum(
        "case_pass_rate",
        &metrics.case_pass_rate,
        thresholds.min_case_pass_rate,
        results.min_case_pass_rate,
    );
}

fn print_baseline(report: &RecallEvalReport) {
    println!();
    println!("baseline");
    let Some(baseline) = &report.baseline else {
        println!("  status:\tnot_compared");
        return;
    };

    println!("  status:\t{}", baseline_status_label(baseline.status));
    println!("  compatible:\t{}", baseline.compatible);
    println!("  deterministic_match:\t{}", baseline.deterministic_match);
    println!(
        "  changed_cases:\t{}",
        joined_or_none(&baseline.changed_cases)
    );
    for delta in baseline
        .metric_deltas
        .iter()
        .filter(|delta| delta.delta != 0.0)
    {
        println!(
            "  delta.{}:\t{:.6} -> {:.6} ({:+.6})",
            delta.metric, delta.baseline, delta.current, delta.delta
        );
    }
}

fn print_forbidden_hits(forbidden: &RecallEvalForbiddenIds) {
    let groups = [
        ("stale", &forbidden.stale),
        ("expired", &forbidden.expired),
        ("scope", &forbidden.scope),
        ("prohibited", &forbidden.prohibited),
        ("destination", &forbidden.destination),
        ("other", &forbidden.other),
    ];
    if groups.iter().all(|(_, ids)| ids.is_empty()) {
        println!("  forbidden_hits:\tnone");
        return;
    }
    for (category, ids) in groups {
        if !ids.is_empty() {
            println!("  forbidden_hits.{category}:\t{}", joined_or_none(ids));
        }
    }
}

fn print_minimum(label: &str, value: f64, minimum: f64, passed: bool) {
    println!(
        "  {label}:\t{value:.6} required >= {minimum:.6} {}",
        pass_label(passed)
    );
}

fn print_ratio_minimum(label: &str, metric: &RecallEvalRatioMetric, minimum: f64, passed: bool) {
    println!(
        "  {label}:\t{}/{} = {:.6} required >= {:.6} {}",
        metric.numerator,
        metric.denominator,
        metric.value,
        minimum,
        pass_label(passed)
    );
}

fn print_leakage(label: &str, metric: &RecallEvalLeakageMetric) {
    println!(
        "  {label}:\t{}/{} = {:.6} no direct threshold",
        metric.hits, metric.opportunities, metric.rate
    );
}

fn print_leakage_maximum(
    label: &str,
    metric: &RecallEvalLeakageMetric,
    maximum: f64,
    passed: bool,
) {
    println!(
        "  {label}:\t{}/{} = {:.6} required <= {:.6} {}",
        metric.hits,
        metric.opportunities,
        metric.rate,
        maximum,
        pass_label(passed)
    );
}

fn print_integrity_minimum(
    label: &str,
    metric: &RecallEvalIntegrityMetric,
    minimum: f64,
    passed: bool,
) {
    println!(
        "  {label}:\t{}/{} = {:.6} required >= {:.6} {}",
        metric.valid,
        metric.checked,
        metric.rate,
        minimum,
        pass_label(passed)
    );
}

fn joined_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

fn surface_label(surface: RecallEvalSurface) -> &'static str {
    match surface {
        RecallEvalSurface::Search => "search",
        RecallEvalSurface::Precheck => "precheck",
        RecallEvalSurface::Context => "context",
        RecallEvalSurface::WriteGate => "write_gate",
    }
}

fn baseline_status_label(status: RecallEvalBaselineStatus) -> &'static str {
    match status {
        RecallEvalBaselineStatus::Match => "match",
        RecallEvalBaselineStatus::Changed => "changed",
        RecallEvalBaselineStatus::Incompatible => "incompatible",
    }
}

fn pass_label(passed: bool) -> &'static str {
    if passed { "PASS" } else { "FAIL" }
}
