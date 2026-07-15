use crate::{CapturePlan, CaptureProvenance, CaptureReview};

pub(super) fn capture_provenance(
    plan: &CapturePlan,
    review: &CaptureReview,
    decision: &crate::CaptureReviewDecision,
    candidate: &crate::CaptureCandidate,
    actor: &str,
) -> CaptureProvenance {
    let original = plan
        .candidates
        .iter()
        .find(|original| original.candidate_id == decision.candidate_id)
        .expect("validated capture review decision must name a plan candidate");
    CaptureProvenance {
        schema: crate::CAPTURE_PROVENANCE_SCHEMA.to_owned(),
        plan_id: review.plan_id.clone(),
        review_id: review.review_id.clone(),
        claim_id: original.claim_id.clone(),
        reviewed_claim_id: candidate.claim_id.clone(),
        candidate_id: decision.candidate_id.clone(),
        reviewed_candidate_id: candidate.candidate_id.clone(),
        extraction: candidate.extraction.clone(),
        evidence: candidate.evidence.clone(),
        confidence: candidate.confidence.to_string(),
        classification: candidate.classification.clone(),
        destination: candidate.classification.destination,
        sensitivity: candidate.classification.sensitivity,
        review_outcome: decision.outcome,
        review_reason_code: decision.reason_code.clone(),
        reviewed_by: review.reviewed_by.clone(),
        reviewed_at: review.reviewed_at.clone(),
        routed_by: actor.to_owned(),
    }
}
