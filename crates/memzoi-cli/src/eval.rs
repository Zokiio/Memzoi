use std::path::PathBuf;

use anyhow::{Result, bail};
use memzoi_core::{
    CaptureEvalBaselineStatus, CaptureEvalReport, RecallEvalBaselineStatus, RecallEvalForbiddenIds,
    RecallEvalIntegrityMetric, RecallEvalLeakageMetric, RecallEvalRatioMetric, RecallEvalReport,
    RecallEvalSurface, attach_capture_eval_baseline, attach_recall_eval_baseline, run_capture_eval,
    run_recall_eval, write_capture_eval_baseline, write_recall_eval_baseline,
};

use crate::output::print_json;

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
