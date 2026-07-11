use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const RECALL_COMPETITOR_EVIDENCE_VERSION: &str = "memzoi-recall-competitor-evidence/v1";
pub const RECALL_COMPETITOR_REPORT_VERSION: &str = "memzoi-recall-competitor-report/v1";
pub const RECALL_INTERNAL_GATE_STATEMENT: &str = "D56-4 depends on safely beating Memzoi's lexical baseline; competitor ranking is informative only.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallCompetitorTrack {
    Retrieval,
    EndToEndAgent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallCompetitorHosting {
    Local,
    Hosted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallCompetitorProtocol {
    pub frozen_at: String,
    pub corpus_digest: String,
    pub judgment_digest: String,
    pub query_or_task_set_digest: String,
    pub top_k: usize,
    pub context_budget: usize,
    pub hardware: String,
    pub network_measurement: String,
    pub latency_measurement: String,
    pub cost_measurement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallCompetitorProduct {
    pub id: String,
    pub product: String,
    pub version: String,
    pub hosting: RecallCompetitorHosting,
    pub model: String,
    pub configuration_digest: String,
    pub import_procedure: String,
    pub api_invocation: String,
    pub track: RecallCompetitorTrack,
    pub network_used: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallRetrievalTrackResult {
    pub product_id: String,
    pub ndcg_at_10: f64,
    pub recall_at_k: f64,
    pub mrr: f64,
    pub eligible_cases: usize,
    pub forbidden_hits: usize,
    pub citations_supported: bool,
    pub citation_integrity: Option<f64>,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
    pub peak_memory_bytes: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallEndToEndTrackResult {
    pub product_id: String,
    pub task_count: usize,
    pub task_completion_rate: f64,
    pub answer_correctness: f64,
    pub required_memory_use: f64,
    pub citation_quality: Option<f64>,
    pub forbidden_memory_uses: usize,
    pub context_tokens: usize,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallCompetitorLimitation {
    pub product_id: String,
    pub capability: String,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallCompetitorArtifact {
    pub product_id: String,
    pub raw_results_digest: String,
    pub reproduction_instructions: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallCompetitorEvidence {
    pub version: String,
    pub protocol: RecallCompetitorProtocol,
    pub products: Vec<RecallCompetitorProduct>,
    pub retrieval_results: Vec<RecallRetrievalTrackResult>,
    pub end_to_end_results: Vec<RecallEndToEndTrackResult>,
    pub limitations: Vec<RecallCompetitorLimitation>,
    pub artifacts: Vec<RecallCompetitorArtifact>,
    pub locked_test_accessed: bool,
    pub product_specific_tuning: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallCompetitorReport {
    pub version: String,
    pub evidence_digest: String,
    pub protocol_digest: String,
    pub products: Vec<RecallCompetitorProduct>,
    pub retrieval_results: Vec<RecallRetrievalTrackResult>,
    pub end_to_end_results: Vec<RecallEndToEndTrackResult>,
    pub limitations: Vec<RecallCompetitorLimitation>,
    pub artifacts: Vec<RecallCompetitorArtifact>,
    pub unsupported_by_product: BTreeMap<String, Vec<String>>,
    pub internal_release_gate: String,
    pub gates: RecallCompetitorGates,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallCompetitorGates {
    pub protocol_frozen: bool,
    pub product_identity_complete: bool,
    pub track_results_complete: bool,
    pub limitations_explicit: bool,
    pub raw_results_reproducible: bool,
    pub no_locked_test_access: bool,
    pub no_product_specific_tuning: bool,
}

impl RecallCompetitorGates {
    fn passed(&self) -> bool {
        self.protocol_frozen
            && self.product_identity_complete
            && self.track_results_complete
            && self.limitations_explicit
            && self.raw_results_reproducible
            && self.no_locked_test_access
            && self.no_product_specific_tuning
    }
}

pub fn run_recall_competitor_eval(path: impl AsRef<Path>) -> Result<RecallCompetitorReport> {
    let path = path.as_ref();
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read competitor evidence {}", path.display()))?;
    let evidence: RecallCompetitorEvidence = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse competitor evidence {}", path.display()))?;
    validate_evidence(&evidence)?;

    let product_ids = evidence
        .products
        .iter()
        .map(|product| product.id.as_str())
        .collect::<BTreeSet<_>>();
    let retrieval_products = evidence
        .products
        .iter()
        .filter(|product| product.track == RecallCompetitorTrack::Retrieval)
        .map(|product| product.id.as_str())
        .collect::<BTreeSet<_>>();
    let end_to_end_products = evidence
        .products
        .iter()
        .filter(|product| product.track == RecallCompetitorTrack::EndToEndAgent)
        .map(|product| product.id.as_str())
        .collect::<BTreeSet<_>>();
    let retrieval_results = evidence
        .retrieval_results
        .iter()
        .map(|result| result.product_id.as_str())
        .collect::<BTreeSet<_>>();
    let end_to_end_results = evidence
        .end_to_end_results
        .iter()
        .map(|result| result.product_id.as_str())
        .collect::<BTreeSet<_>>();
    let artifact_products = evidence
        .artifacts
        .iter()
        .map(|artifact| artifact.product_id.as_str())
        .collect::<BTreeSet<_>>();
    let limitation_products = evidence
        .limitations
        .iter()
        .map(|limitation| limitation.product_id.as_str())
        .collect::<BTreeSet<_>>();
    let unsupported_by_product = evidence.limitations.iter().fold(
        BTreeMap::<String, Vec<String>>::new(),
        |mut grouped, limitation| {
            grouped
                .entry(limitation.product_id.clone())
                .or_default()
                .push(limitation.capability.clone());
            grouped
        },
    );
    let gates = RecallCompetitorGates {
        protocol_frozen: !evidence.protocol.frozen_at.trim().is_empty(),
        product_identity_complete: !evidence.products.is_empty(),
        track_results_complete: retrieval_products == retrieval_results
            && end_to_end_products == end_to_end_results,
        limitations_explicit: limitation_products == product_ids,
        raw_results_reproducible: artifact_products == product_ids,
        no_locked_test_access: !evidence.locked_test_accessed,
        no_product_specific_tuning: !evidence.product_specific_tuning,
    };
    let passed = gates.passed();
    let evidence_digest = digest_json(&evidence)?;
    Ok(RecallCompetitorReport {
        version: RECALL_COMPETITOR_REPORT_VERSION.into(),
        evidence_digest,
        protocol_digest: digest_json(&evidence.protocol)?,
        products: evidence.products,
        retrieval_results: evidence.retrieval_results,
        end_to_end_results: evidence.end_to_end_results,
        limitations: evidence.limitations,
        artifacts: evidence.artifacts,
        unsupported_by_product,
        internal_release_gate: RECALL_INTERNAL_GATE_STATEMENT.into(),
        gates,
        passed,
    })
}

fn validate_evidence(evidence: &RecallCompetitorEvidence) -> Result<()> {
    if evidence.version != RECALL_COMPETITOR_EVIDENCE_VERSION
        || evidence.protocol.corpus_digest.trim().is_empty()
        || evidence.protocol.judgment_digest.trim().is_empty()
        || evidence.protocol.query_or_task_set_digest.trim().is_empty()
        || evidence.protocol.frozen_at.trim().is_empty()
        || evidence.protocol.hardware.trim().is_empty()
        || evidence.protocol.network_measurement.trim().is_empty()
        || evidence.protocol.latency_measurement.trim().is_empty()
        || evidence.protocol.cost_measurement.trim().is_empty()
        || evidence.protocol.top_k == 0
        || evidence.protocol.context_budget == 0
    {
        bail!("invalid competitor evidence or protocol identity");
    }
    let mut product_ids = BTreeSet::new();
    for product in &evidence.products {
        if !product_ids.insert(product.id.as_str())
            || [
                product.product.as_str(),
                product.version.as_str(),
                product.model.as_str(),
                product.configuration_digest.as_str(),
                product.import_procedure.as_str(),
                product.api_invocation.as_str(),
            ]
            .iter()
            .any(|value| value.trim().is_empty())
        {
            bail!("invalid or duplicate competitor product {:?}", product.id);
        }
    }
    validate_result_ids(
        evidence
            .retrieval_results
            .iter()
            .map(|result| result.product_id.as_str()),
        &product_ids,
        "retrieval",
    )?;
    validate_result_ids(
        evidence
            .end_to_end_results
            .iter()
            .map(|result| result.product_id.as_str()),
        &product_ids,
        "end-to-end",
    )?;
    for result in &evidence.retrieval_results {
        validate_ratios(&[result.ndcg_at_10, result.recall_at_k, result.mrr])?;
        match (result.citations_supported, result.citation_integrity) {
            (true, Some(integrity)) => validate_ratios(&[integrity])?,
            (false, None) => {}
            _ => {
                bail!(
                    "retrieval result {:?} must provide citation_integrity exactly when citations are supported",
                    result.product_id
                );
            }
        }
        validate_observations(&[
            result.latency_p50_ms,
            result.latency_p95_ms,
            result.cost_usd,
        ])?;
        if result.latency_p50_ms > result.latency_p95_ms || result.eligible_cases == 0 {
            bail!(
                "invalid retrieval-track observations for {:?}",
                result.product_id
            );
        }
    }
    for result in &evidence.end_to_end_results {
        validate_ratios(&[
            result.task_completion_rate,
            result.answer_correctness,
            result.required_memory_use,
            result.citation_quality.unwrap_or(1.0),
        ])?;
        validate_observations(&[
            result.latency_p50_ms,
            result.latency_p95_ms,
            result.cost_usd,
        ])?;
        if result.latency_p50_ms > result.latency_p95_ms || result.task_count == 0 {
            bail!(
                "invalid end-to-end observations for {:?}",
                result.product_id
            );
        }
    }
    let mut artifact_ids = BTreeSet::new();
    if evidence.limitations.iter().any(|limitation| {
        !product_ids.contains(limitation.product_id.as_str())
            || limitation.capability.trim().is_empty()
            || limitation.explanation.trim().is_empty()
    }) || evidence.artifacts.iter().any(|artifact| {
        !product_ids.contains(artifact.product_id.as_str())
            || !artifact_ids.insert(artifact.product_id.as_str())
            || artifact.raw_results_digest.trim().is_empty()
            || artifact.reproduction_instructions.trim().is_empty()
    }) {
        bail!("invalid competitor limitation or artifact reference");
    }
    Ok(())
}

fn validate_result_ids<'a>(
    ids: impl Iterator<Item = &'a str>,
    products: &BTreeSet<&str>,
    label: &str,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if !products.contains(id) || !seen.insert(id) {
            bail!("invalid or duplicate {label} result product {:?}", id);
        }
    }
    Ok(())
}

fn validate_ratios(values: &[f64]) -> Result<()> {
    if values
        .iter()
        .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
    {
        bail!("competitor ratio observations must be finite and between zero and one");
    }
    Ok(())
}

fn validate_observations(values: &[f64]) -> Result<()> {
    if values
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
    {
        bail!("competitor observations must be finite and non-negative");
    }
    Ok(())
}

fn digest_json(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json_canonicalizer::to_vec(value)?)
        .to_hex()
        .to_string())
}
