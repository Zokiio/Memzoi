use std::path::PathBuf;

use anyhow::{Result, bail};
use memzoi_core::{RecallEvalReport, run_recall_eval};

use crate::output::print_json;

pub(crate) fn recall_eval_command(corpus: PathBuf, as_json: bool) -> Result<()> {
    let report = run_recall_eval(&corpus)?;
    if as_json {
        print_json(&serde_json::to_value(&report)?)?;
    } else {
        print_human_report(&report);
    }

    if !report.aggregate.passed {
        bail!("recall evaluation thresholds failed");
    }
    Ok(())
}

fn print_human_report(report: &RecallEvalReport) {
    println!("Memzoi recall evaluation ({})", report.version);
    println!("corpus:\t{}", report.corpus_name);
    println!("evaluated_at:\t{}", report.evaluated_at);
    println!("fixture_records:\t{}", report.fixture_record_count);
    println!();

    for case in &report.cases {
        let status = if case.passed { "PASS" } else { "FAIL" };
        println!("{status}\t{}", case.id);
        println!("  query:\t{}", case.query);
        println!("  retrieved_ids:\t{}", joined_or_none(&case.retrieved_ids));
        println!("  recall@{}:\t{:.6}", case.k, case.recall_at_k);
        println!("  mrr:\t{:.6}", case.mrr);
        println!(
            "  forbidden_hits:\t{}",
            joined_or_none(&case.forbidden_hits)
        );
        println!("  latency_ms:\t{:.3}", case.latency_ms);
        for retrieved in &case.retrieved {
            for citation in &retrieved.citations {
                println!(
                    "  citation:\t{}\t{}\t{}",
                    retrieved.record_id,
                    citation.source_ref.as_deref().unwrap_or("none"),
                    citation.path.as_deref().unwrap_or("none")
                );
            }
        }
    }

    println!();
    println!("aggregate");
    println!(
        "  mean_recall_at_k:\t{:.6}\trequired >= {:.6}\t{}",
        report.aggregate.mean_recall_at_k,
        report.aggregate.thresholds.min_mean_recall_at_k,
        pass_label(report.aggregate.threshold_results.min_mean_recall_at_k)
    );
    println!(
        "  mean_mrr:\t{:.6}\trequired >= {:.6}\t{}",
        report.aggregate.mean_mrr,
        report.aggregate.thresholds.min_mean_mrr,
        pass_label(report.aggregate.threshold_results.min_mean_mrr)
    );
    println!(
        "  total_forbidden_hits:\t{}\trequired <= {}\t{}",
        report.aggregate.total_forbidden_hits,
        report.aggregate.thresholds.max_forbidden_hits,
        pass_label(report.aggregate.threshold_results.max_forbidden_hits)
    );
    if let Some(maximum) = report.aggregate.thresholds.max_mean_latency_ms {
        println!(
            "  mean_latency_ms:\t{:.3}\trequired <= {:.3}\t{}",
            report.aggregate.mean_latency_ms,
            maximum,
            pass_label(
                report
                    .aggregate
                    .threshold_results
                    .max_mean_latency_ms
                    .unwrap_or(false)
            )
        );
    } else {
        println!(
            "  mean_latency_ms:\t{:.3}\tno threshold",
            report.aggregate.mean_latency_ms
        );
    }
    println!(
        "overall:\t{}",
        if report.aggregate.passed {
            "PASS"
        } else {
            "FAIL"
        }
    );
}

fn joined_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

fn pass_label(passed: bool) -> &'static str {
    if passed { "PASS" } else { "FAIL" }
}
