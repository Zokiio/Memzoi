use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    CaptureProvenance, MemoryDestination, MemoryDraft, MemoryLane, MemoryStatus, MemoryType,
    OkfProposalSensitivity, OkfRecordFile, OriginDescriptor, REPOSITORY_WRITE_SAFETY_SCHEMA,
    RecordLineage, RepositoryContentClass, RetentionFacts, ScopeKind, Visibility,
    capture::validate_capture_provenance, retention::evaluate_retention,
};

pub const REPOSITORY_MATERIALIZATION_PLAN_SCHEMA: &str = "memzoi/repository-materialization-plan";
pub const REPOSITORY_MATERIALIZATION_DECISION_SCHEMA: &str =
    "memzoi/repository-materialization-decision";
pub const REPOSITORY_MATERIALIZATION_RESULT_SCHEMA: &str =
    "memzoi/repository-materialization-result";
pub const CANONICAL_REVISION_SCHEMA: &str = "memzoi/repository-canonical-revision";
pub const MATERIALIZATION_METADATA_SCHEMA: &str = "memzoi/repository-materialization";
pub const MAX_MATERIALIZATION_REASON_BYTES: usize = 280;
pub const REPOSITORY_MATERIALIZATION_CANDIDATE_SCHEMA: &str =
    "memzoi/repository-materialization-candidate";

const PLAN_ID_DOMAIN: &str = "memzoi.repository-materialization.plan-id";
const DECISION_ID_DOMAIN: &str = "memzoi.repository-materialization.decision-id";
const REVISION_ID_DOMAIN: &str = "memzoi.repository-materialization.revision-id";
const CANDIDATE_ID_DOMAIN: &str = "memzoi.repository-materialization.candidate-id";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationAction {
    Create,
    Update,
    Supersede,
    Tombstone,
}

impl MaterializationAction {
    pub fn counterpart_relationship(self) -> Option<MaterializationCounterpartRelationship> {
        match self {
            Self::Create | Self::Update => None,
            Self::Supersede => Some(MaterializationCounterpartRelationship::Supersedes),
            Self::Tombstone => Some(MaterializationCounterpartRelationship::Tombstones),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationCounterpartRelationship {
    Supersedes,
    Tombstones,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationOutputRole {
    CanonicalRecord,
    LifecycleCounterpart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationAuthorizationCapability {
    ExplicitCli,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationOutputOutcome {
    Written,
    AlreadyCurrent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalRevision {
    pub schema: String,
    pub revision_hash: String,
}

impl CanonicalRevision {
    pub fn validate(&self) -> Result<()> {
        validate_schema(
            &self.schema,
            CANONICAL_REVISION_SCHEMA,
            "canonical revision",
        )?;
        validate_materialization_identity(&self.revision_hash, "canonical revision hash")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "revision", rename_all = "snake_case")]
pub enum ExpectedPriorRevision {
    Absent,
    Revision(CanonicalRevision),
}

impl ExpectedPriorRevision {
    fn validate(&self) -> Result<()> {
        if let Self::Revision(revision) = self {
            revision.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializationOutputIntent {
    pub path: String,
    pub record_id: String,
    pub action: MaterializationAction,
    pub expected_prior_revision: ExpectedPriorRevision,
    pub intended_semantic_revision: CanonicalRevision,
    pub role: MaterializationOutputRole,
}

impl MaterializationOutputIntent {
    pub fn validate(&self) -> Result<()> {
        validate_repository_relative_path(&self.path)?;
        validate_canonical_record_id(&self.record_id)?;
        self.expected_prior_revision.validate()?;
        self.intended_semantic_revision.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryMaterializationPlan {
    pub schema: String,
    pub plan_id: String,
    pub candidate_id: String,
    pub outputs: Vec<MaterializationOutputIntent>,
}

impl RepositoryMaterializationPlan {
    pub fn validate(&self) -> Result<()> {
        validate_plan_payload(self)?;
        validate_materialization_identity(&self.plan_id, "plan_id")?;
        let expected = repository_materialization_plan_id(self)?;
        if self.plan_id != expected {
            bail!("plan_id does not match the repository materialization plan identity");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializationPolicy {
    pub policy_id: String,
    pub safety_contract: String,
}

impl MaterializationPolicy {
    fn validate(&self) -> Result<()> {
        validate_nonempty(&self.policy_id, "policy_id")?;
        validate_nonempty(&self.safety_contract, "safety_contract")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryMaterializationDecision {
    pub schema: String,
    pub decision_id: String,
    pub plan_id: String,
    pub candidate_id: String,
    pub decision_at: String,
    pub policy: MaterializationPolicy,
    pub authorization_capability: MaterializationAuthorizationCapability,
    pub outputs: Vec<MaterializationOutputIntent>,
}

impl RepositoryMaterializationDecision {
    pub fn validate(&self) -> Result<()> {
        validate_decision_payload(self)?;
        validate_materialization_identity(&self.decision_id, "decision_id")?;
        let expected = repository_materialization_decision_id(self)?;
        if self.decision_id != expected {
            bail!("decision_id does not match the repository materialization decision identity");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializationOutputResult {
    pub path: String,
    pub record_id: String,
    pub action: MaterializationAction,
    pub semantic_revision: CanonicalRevision,
    pub role: MaterializationOutputRole,
    pub outcome: MaterializationOutputOutcome,
}

impl MaterializationOutputResult {
    fn validate(&self) -> Result<()> {
        validate_repository_relative_path(&self.path)?;
        validate_canonical_record_id(&self.record_id)?;
        self.semantic_revision.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryMaterializationResult {
    pub schema: String,
    pub decision_id: String,
    pub outputs: Vec<MaterializationOutputResult>,
}

impl RepositoryMaterializationResult {
    pub fn validate(&self) -> Result<()> {
        validate_schema(
            &self.schema,
            REPOSITORY_MATERIALIZATION_RESULT_SCHEMA,
            "repository materialization result",
        )?;
        validate_materialization_identity(&self.decision_id, "decision_id")?;
        if self.outputs.is_empty() {
            bail!("repository materialization result must contain at least one output");
        }
        validate_unique_paths(self.outputs.iter().map(|output| output.path.as_str()))?;
        for output in &self.outputs {
            output.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializationTarget {
    pub record_id: String,
    pub expected_revision: CanonicalRevision,
}

impl MaterializationTarget {
    fn validate(&self) -> Result<()> {
        validate_canonical_record_id(&self.record_id)?;
        self.expected_revision.validate()
    }
}

/// Compact, Git-rebuildable audit metadata for one newly materialized canonical revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializationMetadata {
    pub schema: String,
    pub action: MaterializationAction,
    pub plan_id: String,
    pub candidate_id: String,
    pub decision_id: String,
    pub decision_at: String,
    pub safety_contract: String,
    pub revision: CanonicalRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<MaterializationTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl MaterializationMetadata {
    pub fn validate(&self) -> Result<()> {
        validate_schema(
            &self.schema,
            MATERIALIZATION_METADATA_SCHEMA,
            "materialization metadata",
        )?;
        validate_materialization_identity(&self.plan_id, "materialization plan_id")?;
        validate_materialization_identity(&self.candidate_id, "materialization candidate_id")?;
        validate_materialization_identity(&self.decision_id, "materialization decision_id")?;
        validate_rfc3339_timestamp(&self.decision_at, "materialization decision_at")?;
        validate_nonempty(&self.safety_contract, "materialization safety_contract")?;
        self.revision.validate()?;
        validate_lifecycle_shape(self.action, self.target.as_ref(), self.reason.as_deref())
    }

    pub fn lifecycle_projection(&self) -> CanonicalLifecycleProjection {
        CanonicalLifecycleProjection {
            action: Some(self.action),
            target_expected_revision: self
                .target
                .as_ref()
                .map(|target| target.expected_revision.clone()),
            counterpart_record_id: self.target.as_ref().map(|target| target.record_id.clone()),
            counterpart_relationship: self.action.counterpart_relationship(),
            reason: self.reason.clone(),
        }
    }
}

/// Complete semantic record content supplied by a direct repository candidate.
///
/// This deliberately does not contain materialization metadata. A candidate is
/// an input to a future decision, never a caller-supplied attestation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryMaterializationCandidateRecord {
    pub concept_id: String,
    pub draft: MemoryDraft,
    pub status: MemoryStatus,
    pub applies_to: Vec<String>,
    pub created: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes_id: Option<String>,
    pub retention: RetentionFacts,
    pub origin: OriginDescriptor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineage: Option<RecordLineage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<CaptureProvenance>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryMaterializationCandidate {
    pub schema: String,
    pub candidate_id: String,
    pub destination: MemoryDestination,
    pub action: MaterializationAction,
    pub output_path: String,
    pub expected_prior_revision: ExpectedPriorRevision,
    pub target: Option<MaterializationTarget>,
    pub reason: Option<String>,
    pub record: RepositoryMaterializationCandidateRecord,
}

impl RepositoryMaterializationCandidate {
    pub fn validate(&self) -> Result<()> {
        validate_candidate_payload(self)?;
        validate_materialization_identity(&self.candidate_id, "candidate_id")?;
        let expected = repository_materialization_candidate_id(self)?;
        if self.candidate_id != expected {
            bail!("candidate_id does not match the repository materialization candidate identity");
        }
        Ok(())
    }
}

/// The canonical semantic record projection used to derive a revision identity.
/// It intentionally excludes materialization audit metadata and rendered YAML bytes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CanonicalRecordSemanticContent {
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub scope_kind: ScopeKind,
    pub scope_id: Option<String>,
    pub visibility: Visibility,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub source_kind: Option<String>,
    pub source_ref: Option<String>,
    pub content_class: RepositoryContentClass,
    pub confidence: f64,
    pub status: MemoryStatus,
    pub applies_to: Vec<String>,
    pub created: String,
    pub updated: Option<String>,
    pub supersedes_id: Option<String>,
    pub retention: RetentionFacts,
    pub origin: OriginDescriptor,
    pub lineage: Option<RecordLineage>,
    pub proposal_id: Option<String>,
    pub capture: Option<CaptureProvenance>,
}

impl From<&OkfRecordFile> for CanonicalRecordSemanticContent {
    fn from(record: &OkfRecordFile) -> Self {
        Self {
            memory_type: record.draft.memory_type,
            lane: record.draft.lane,
            scope_kind: record.draft.scope_kind,
            scope_id: record.draft.scope_id.clone(),
            visibility: record.draft.visibility,
            title: record.draft.title.clone(),
            body: record.draft.body.clone(),
            tags: record.draft.tags.clone(),
            source_kind: record.draft.source_kind.clone(),
            source_ref: record.draft.source_ref.clone(),
            content_class: record.draft.content_class,
            confidence: record.draft.confidence,
            status: record.status,
            applies_to: record.applies_to.clone(),
            created: record.created.clone(),
            updated: record.updated.clone(),
            supersedes_id: record.supersedes_id.clone(),
            retention: record.retention.clone(),
            origin: record.origin.clone(),
            lineage: record.lineage.clone(),
            proposal_id: record.proposal_id.clone(),
            capture: record.capture.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CanonicalLifecycleProjection {
    pub action: Option<MaterializationAction>,
    pub target_expected_revision: Option<CanonicalRevision>,
    pub counterpart_record_id: Option<String>,
    pub counterpart_relationship: Option<MaterializationCounterpartRelationship>,
    pub reason: Option<String>,
}

impl CanonicalLifecycleProjection {
    fn validate(&self) -> Result<()> {
        match self.action {
            None => {
                if self.target_expected_revision.is_some()
                    || self.counterpart_record_id.is_some()
                    || self.counterpart_relationship.is_some()
                    || self.reason.is_some()
                {
                    bail!("unattested canonical lifecycle cannot contain materialization fields");
                }
            }
            Some(action) => {
                let target = match (
                    self.counterpart_record_id.as_deref(),
                    self.target_expected_revision.as_ref(),
                ) {
                    (Some(record_id), Some(revision)) => Some(MaterializationTarget {
                        record_id: record_id.to_owned(),
                        expected_revision: revision.clone(),
                    }),
                    (None, None) => None,
                    _ => bail!("canonical lifecycle target record and revision must be paired"),
                };
                validate_lifecycle_shape(action, target.as_ref(), self.reason.as_deref())?;
                if self.counterpart_relationship != action.counterpart_relationship() {
                    bail!("canonical lifecycle counterpart relationship does not match action");
                }
            }
        }
        Ok(())
    }
}

/// The versioned, cycle-free input to a canonical revision hash.
/// Decision identity, this revision's hash, rendered bytes, and authorization capability are absent.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CanonicalRevisionProjection {
    pub schema: String,
    pub record_id: String,
    pub record: CanonicalRecordSemanticContent,
    pub lifecycle: CanonicalLifecycleProjection,
}

impl CanonicalRevisionProjection {
    pub fn for_okf_record(record: &OkfRecordFile) -> Self {
        Self {
            schema: CANONICAL_REVISION_SCHEMA.to_owned(),
            record_id: record.concept_id.clone(),
            record: CanonicalRecordSemanticContent::from(record),
            lifecycle: record
                .materialization
                .as_ref()
                .map(MaterializationMetadata::lifecycle_projection)
                .unwrap_or_default(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        validate_schema(
            &self.schema,
            CANONICAL_REVISION_SCHEMA,
            "canonical revision projection",
        )?;
        validate_canonical_record_id(&self.record_id)?;
        self.lifecycle.validate()
    }
}

pub fn build_repository_materialization_plan(
    candidate_id: String,
    outputs: Vec<MaterializationOutputIntent>,
) -> Result<RepositoryMaterializationPlan> {
    let mut plan = RepositoryMaterializationPlan {
        schema: REPOSITORY_MATERIALIZATION_PLAN_SCHEMA.to_owned(),
        plan_id: String::new(),
        candidate_id,
        outputs,
    };
    plan.plan_id = repository_materialization_plan_id(&plan)?;
    plan.validate()?;
    Ok(plan)
}

pub fn repository_materialization_plan_id(plan: &RepositoryMaterializationPlan) -> Result<String> {
    validate_plan_payload(plan)?;
    domain_separated_identity(
        PLAN_ID_DOMAIN,
        &PlanIdentityProjection {
            schema: &plan.schema,
            candidate_id: &plan.candidate_id,
            outputs: &plan.outputs,
        },
    )
}

pub fn build_repository_materialization_decision(
    plan: &RepositoryMaterializationPlan,
    decision_at: String,
    policy: MaterializationPolicy,
    authorization_capability: MaterializationAuthorizationCapability,
) -> Result<RepositoryMaterializationDecision> {
    plan.validate()?;
    let mut decision = RepositoryMaterializationDecision {
        schema: REPOSITORY_MATERIALIZATION_DECISION_SCHEMA.to_owned(),
        decision_id: String::new(),
        plan_id: plan.plan_id.clone(),
        candidate_id: plan.candidate_id.clone(),
        decision_at,
        policy,
        authorization_capability,
        outputs: plan.outputs.clone(),
    };
    decision.decision_id = repository_materialization_decision_id(&decision)?;
    decision.validate()?;
    Ok(decision)
}

/// Derives the decision ID from pinned decision inputs, never from rendered output bytes.
pub fn repository_materialization_decision_id(
    decision: &RepositoryMaterializationDecision,
) -> Result<String> {
    validate_decision_payload(decision)?;
    domain_separated_identity(
        DECISION_ID_DOMAIN,
        &DecisionIdentityProjection {
            schema: &decision.schema,
            plan_id: &decision.plan_id,
            candidate_id: &decision.candidate_id,
            decision_at: &decision.decision_at,
            policy: &decision.policy,
            authorization_capability: decision.authorization_capability,
            outputs: &decision.outputs,
        },
    )
}

pub fn canonical_revision_for_projection(
    projection: &CanonicalRevisionProjection,
) -> Result<CanonicalRevision> {
    projection.validate()?;
    Ok(CanonicalRevision {
        schema: CANONICAL_REVISION_SCHEMA.to_owned(),
        revision_hash: domain_separated_identity(REVISION_ID_DOMAIN, projection)?,
    })
}

pub fn canonical_revision_for_okf_record(record: &OkfRecordFile) -> Result<CanonicalRevision> {
    canonical_revision_for_projection(&CanonicalRevisionProjection::for_okf_record(record))
}

/// Builds a repository-only direct candidate and derives its immutable identity.
pub fn build_repository_materialization_candidate(
    record: RepositoryMaterializationCandidateRecord,
    action: MaterializationAction,
    expected_prior_revision: ExpectedPriorRevision,
    target: Option<MaterializationTarget>,
    reason: Option<String>,
) -> Result<RepositoryMaterializationCandidate> {
    let mut candidate = RepositoryMaterializationCandidate {
        schema: REPOSITORY_MATERIALIZATION_CANDIDATE_SCHEMA.to_owned(),
        candidate_id: String::new(),
        destination: MemoryDestination::Repo,
        action,
        output_path: canonical_record_output_path(&record.concept_id),
        expected_prior_revision,
        target,
        reason,
        record,
    };
    candidate.candidate_id = repository_materialization_candidate_id(&candidate)?;
    candidate.validate()?;
    Ok(candidate)
}

/// Derives the direct candidate identity from semantic input, never from its own ID.
pub fn repository_materialization_candidate_id(
    candidate: &RepositoryMaterializationCandidate,
) -> Result<String> {
    validate_candidate_payload(candidate)?;
    domain_separated_identity(
        CANDIDATE_ID_DOMAIN,
        &CandidateIdentityProjection {
            schema: &candidate.schema,
            destination: candidate.destination,
            action: candidate.action,
            output_path: &candidate.output_path,
            expected_prior_revision: &candidate.expected_prior_revision,
            target: candidate.target.as_ref(),
            reason: candidate.reason.as_deref(),
            record: &candidate.record,
        },
    )
}

/// Converts a validated direct candidate to a typed canonical record without an attestation.
pub fn repository_materialization_candidate_to_okf_record(
    candidate: &RepositoryMaterializationCandidate,
) -> Result<OkfRecordFile> {
    candidate.validate()?;
    Ok(OkfRecordFile {
        concept_id: candidate.record.concept_id.clone(),
        draft: candidate.record.draft.clone(),
        status: candidate.record.status,
        applies_to: candidate.record.applies_to.clone(),
        created: candidate.record.created.clone(),
        updated: candidate.record.updated.clone(),
        supersedes_id: candidate.record.supersedes_id.clone(),
        retention: candidate.record.retention.clone(),
        origin: candidate.record.origin.clone(),
        lineage: candidate.record.lineage.clone(),
        proposal_id: candidate.record.proposal_id.clone(),
        capture: candidate.record.capture.clone(),
        materialization: None,
    })
}

/// Builds the one canonical-record output intent for a validated direct candidate.
pub fn repository_materialization_candidate_plan(
    candidate: &RepositoryMaterializationCandidate,
) -> Result<RepositoryMaterializationPlan> {
    candidate.validate()?;
    let record = repository_materialization_candidate_to_okf_record(candidate)?;
    let revision = canonical_revision_for_projection(&CanonicalRevisionProjection {
        schema: CANONICAL_REVISION_SCHEMA.to_owned(),
        record_id: record.concept_id.clone(),
        record: CanonicalRecordSemanticContent::from(&record),
        lifecycle: candidate_lifecycle_projection(candidate),
    })?;
    build_repository_materialization_plan(
        candidate.candidate_id.clone(),
        vec![MaterializationOutputIntent {
            path: candidate.output_path.clone(),
            record_id: record.concept_id,
            action: candidate.action,
            expected_prior_revision: candidate.expected_prior_revision.clone(),
            intended_semantic_revision: revision,
            role: MaterializationOutputRole::CanonicalRecord,
        }],
    )
}

/// Returns the immutable policy label for repository materialization decisions.
pub fn repository_materialization_policy() -> MaterializationPolicy {
    MaterializationPolicy {
        policy_id: REPOSITORY_WRITE_SAFETY_SCHEMA.to_owned(),
        safety_contract: REPOSITORY_WRITE_SAFETY_SCHEMA.to_owned(),
    }
}

/// Validates the wire format used by all materialization identities.
pub fn validate_materialization_identity(identity: &str, label: &str) -> Result<()> {
    let Some(digest) = identity.strip_prefix("blake3:") else {
        bail!("{label} must use a blake3 identity");
    };
    if digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || digest.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        bail!("{label} must contain a lowercase 32-byte blake3 digest");
    }
    Ok(())
}

pub fn validate_canonical_record_id(record_id: &str) -> Result<()> {
    if record_id.is_empty() {
        bail!("canonical record_id cannot be empty");
    }
    for segment in record_id.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            bail!("canonical record_id contains an invalid path segment");
        }
        if !segment
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        {
            bail!(
                "canonical record_id segments must use lowercase ASCII letters, digits, and hyphens"
            );
        }
        let starts_and_ends_alphanumeric = segment
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
            && segment
                .chars()
                .last()
                .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit());
        if !starts_and_ends_alphanumeric {
            bail!("canonical record_id segments must start and end with a letter or digit");
        }
    }
    Ok(())
}

pub fn validate_repository_relative_path(path: &str) -> Result<()> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        bail!("materialization output path must be a non-empty slash-separated relative path");
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            bail!("materialization output path contains an invalid path segment");
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct PlanIdentityProjection<'a> {
    schema: &'a str,
    candidate_id: &'a str,
    outputs: &'a [MaterializationOutputIntent],
}

#[derive(Serialize)]
struct DecisionIdentityProjection<'a> {
    schema: &'a str,
    plan_id: &'a str,
    candidate_id: &'a str,
    decision_at: &'a str,
    policy: &'a MaterializationPolicy,
    authorization_capability: MaterializationAuthorizationCapability,
    outputs: &'a [MaterializationOutputIntent],
}

#[derive(Serialize)]
struct CandidateIdentityProjection<'a> {
    schema: &'a str,
    destination: MemoryDestination,
    action: MaterializationAction,
    output_path: &'a str,
    expected_prior_revision: &'a ExpectedPriorRevision,
    target: Option<&'a MaterializationTarget>,
    reason: Option<&'a str>,
    record: &'a RepositoryMaterializationCandidateRecord,
}

fn validate_plan_payload(plan: &RepositoryMaterializationPlan) -> Result<()> {
    validate_schema(
        &plan.schema,
        REPOSITORY_MATERIALIZATION_PLAN_SCHEMA,
        "repository materialization plan",
    )?;
    validate_materialization_identity(&plan.candidate_id, "candidate_id")?;
    if plan.outputs.is_empty() {
        bail!("repository materialization plan must contain at least one output");
    }
    validate_unique_paths(plan.outputs.iter().map(|output| output.path.as_str()))?;
    for output in &plan.outputs {
        output.validate()?;
    }
    Ok(())
}

fn validate_decision_payload(decision: &RepositoryMaterializationDecision) -> Result<()> {
    validate_schema(
        &decision.schema,
        REPOSITORY_MATERIALIZATION_DECISION_SCHEMA,
        "repository materialization decision",
    )?;
    validate_materialization_identity(&decision.plan_id, "plan_id")?;
    validate_materialization_identity(&decision.candidate_id, "candidate_id")?;
    validate_rfc3339_timestamp(&decision.decision_at, "decision_at")?;
    decision.policy.validate()?;
    if decision.outputs.is_empty() {
        bail!("repository materialization decision must contain at least one output");
    }
    validate_unique_paths(decision.outputs.iter().map(|output| output.path.as_str()))?;
    for output in &decision.outputs {
        output.validate()?;
    }
    Ok(())
}

fn validate_candidate_payload(candidate: &RepositoryMaterializationCandidate) -> Result<()> {
    validate_schema(
        &candidate.schema,
        REPOSITORY_MATERIALIZATION_CANDIDATE_SCHEMA,
        "repository materialization candidate",
    )?;
    if candidate.destination != MemoryDestination::Repo {
        bail!("repository materialization candidate destination must be repo");
    }
    validate_candidate_record(&candidate.record)?;
    validate_repository_relative_path(&candidate.output_path)?;
    let expected_path = canonical_record_output_path(&candidate.record.concept_id);
    if candidate.output_path != expected_path {
        bail!("repository materialization candidate output_path must be {expected_path}");
    }
    candidate.expected_prior_revision.validate()?;
    validate_lifecycle_shape(
        candidate.action,
        candidate.target.as_ref(),
        candidate.reason.as_deref(),
    )?;
    validate_candidate_action_shape(candidate)
}

fn validate_candidate_record(record: &RepositoryMaterializationCandidateRecord) -> Result<()> {
    validate_canonical_record_id(&record.concept_id)?;
    validate_candidate_draft(&record.draft)?;
    validate_rfc3339_timestamp(&record.created, "candidate record created")?;
    if let Some(updated) = record.updated.as_deref() {
        validate_rfc3339_timestamp(updated, "candidate record updated")?;
    }
    if let Some(supersedes_id) = record.supersedes_id.as_deref() {
        validate_canonical_record_id(supersedes_id)?;
    }
    let created_at = OffsetDateTime::parse(&record.created, &Rfc3339)
        .with_context(|| "candidate record created is invalid")?;
    evaluate_retention(
        &record.concept_id,
        record.draft.lane,
        &record.retention,
        created_at,
    )
    .with_context(|| "candidate record retention is invalid")?;
    record
        .origin
        .validate()
        .with_context(|| "candidate record origin is invalid")?;
    if let Some(lineage) = record.lineage.as_ref() {
        lineage
            .validate()
            .with_context(|| "candidate record lineage is invalid")?;
    }
    if let Some(proposal_id) = record.proposal_id.as_deref() {
        validate_nonempty(proposal_id, "candidate record proposal_id")?;
    }
    for path in &record.applies_to {
        validate_repository_relative_path(path)
            .with_context(|| "candidate record applies_to contains an invalid path")?;
    }
    if let Some(capture) = record.capture.as_ref() {
        if capture.destination != MemoryDestination::Repo
            || capture.sensitivity != OkfProposalSensitivity::RepoSafe
            || capture.classification.content_class != RepositoryContentClass::GeneralRepoKnowledge
        {
            bail!("candidate record capture provenance is not repository-safe");
        }
        validate_capture_provenance(capture)
            .context("candidate record capture provenance is invalid")?;
    }
    Ok(())
}

fn validate_candidate_draft(draft: &MemoryDraft) -> Result<()> {
    if draft.scope_kind != ScopeKind::Repo {
        bail!("repository materialization candidate scope_kind must be repo");
    }
    if draft.visibility != Visibility::Repo {
        bail!("repository materialization candidate visibility must be repo");
    }
    if draft.sensitivity != OkfProposalSensitivity::RepoSafe {
        bail!("repository materialization candidate sensitivity must be repo-safe");
    }
    if draft.content_class != RepositoryContentClass::GeneralRepoKnowledge {
        bail!("repository materialization candidate content_class must be general_repo_knowledge");
    }
    validate_nonempty(&draft.title, "candidate record title")?;
    if draft.body.trim().is_empty() {
        bail!("candidate record body must not be empty");
    }
    if let Some(scope_id) = draft.scope_id.as_deref() {
        validate_nonempty(scope_id, "candidate record scope_id")?;
    }
    if let Some(source_kind) = draft.source_kind.as_deref() {
        validate_nonempty(source_kind, "candidate record source_kind")?;
    }
    if let Some(source_ref) = draft.source_ref.as_deref() {
        validate_nonempty(source_ref, "candidate record source_ref")?;
    }
    for tag in &draft.tags {
        validate_nonempty(tag, "candidate record tag")?;
    }
    if !draft.confidence.is_finite() || !(0.0..=1.0).contains(&draft.confidence) {
        bail!("candidate record confidence must be a finite value between zero and one");
    }
    Ok(())
}

fn validate_candidate_action_shape(candidate: &RepositoryMaterializationCandidate) -> Result<()> {
    let record = &candidate.record;
    match candidate.action {
        MaterializationAction::Create => {
            if record.status != MemoryStatus::Active {
                bail!("create materialization candidates must produce active records");
            }
            if !matches!(
                &candidate.expected_prior_revision,
                ExpectedPriorRevision::Absent
            ) {
                bail!("create materialization candidates must expect an absent prior revision");
            }
            if record.supersedes_id.is_some() {
                bail!("create materialization candidates cannot name a supersedes record");
            }
        }
        MaterializationAction::Update => {
            if record.status != MemoryStatus::Active {
                bail!("update materialization candidates must produce active records");
            }
            if !matches!(
                &candidate.expected_prior_revision,
                ExpectedPriorRevision::Revision(_)
            ) {
                bail!("update materialization candidates must expect a prior revision");
            }
            if record.supersedes_id.is_some() {
                bail!("update materialization candidates cannot name a supersedes record");
            }
        }
        MaterializationAction::Supersede => {
            if record.status != MemoryStatus::Active {
                bail!("supersede materialization candidates must produce active records");
            }
            if !matches!(
                &candidate.expected_prior_revision,
                ExpectedPriorRevision::Absent
            ) {
                bail!("supersede materialization candidates must expect an absent prior revision");
            }
            let target = candidate
                .target
                .as_ref()
                .expect("lifecycle shape validates supersede targets");
            if target.record_id == record.concept_id {
                bail!("supersede materialization candidate target must differ from its concept_id");
            }
            if record.supersedes_id.as_deref() != Some(target.record_id.as_str()) {
                bail!("supersede materialization candidate supersedes_id must match its target");
            }
        }
        MaterializationAction::Tombstone => {
            if record.status != MemoryStatus::Tombstoned {
                bail!("tombstone materialization candidates must produce tombstoned records");
            }
            if record.supersedes_id.is_some() {
                bail!("tombstone materialization candidates cannot name a supersedes record");
            }
            let target = candidate
                .target
                .as_ref()
                .expect("lifecycle shape validates tombstone targets");
            if target.record_id != record.concept_id {
                bail!("tombstone materialization candidate target must match its concept_id");
            }
            match &candidate.expected_prior_revision {
                ExpectedPriorRevision::Revision(expected)
                    if expected == &target.expected_revision => {}
                ExpectedPriorRevision::Revision(_) => bail!(
                    "tombstone materialization candidate prior revision must match its target"
                ),
                ExpectedPriorRevision::Absent => {
                    bail!("tombstone materialization candidates must expect a prior revision")
                }
            }
        }
    }
    Ok(())
}

fn candidate_lifecycle_projection(
    candidate: &RepositoryMaterializationCandidate,
) -> CanonicalLifecycleProjection {
    CanonicalLifecycleProjection {
        action: Some(candidate.action),
        target_expected_revision: candidate
            .target
            .as_ref()
            .map(|target| target.expected_revision.clone()),
        counterpart_record_id: candidate
            .target
            .as_ref()
            .map(|target| target.record_id.clone()),
        counterpart_relationship: candidate.action.counterpart_relationship(),
        reason: candidate.reason.clone(),
    }
}

fn canonical_record_output_path(concept_id: &str) -> String {
    format!(".memzoi/records/{concept_id}.md")
}

fn validate_lifecycle_shape(
    action: MaterializationAction,
    target: Option<&MaterializationTarget>,
    reason: Option<&str>,
) -> Result<()> {
    match action {
        MaterializationAction::Create | MaterializationAction::Update => {
            if target.is_some() || reason.is_some() {
                bail!(
                    "create and update materialization metadata cannot contain lifecycle target or reason"
                );
            }
        }
        MaterializationAction::Supersede | MaterializationAction::Tombstone => {
            let target = target.ok_or_else(|| {
                anyhow::anyhow!("supersede and tombstone materialization metadata require a target")
            })?;
            target.validate()?;
            validate_materialization_reason(reason.ok_or_else(|| {
                anyhow::anyhow!("supersede and tombstone materialization metadata require a reason")
            })?)?;
        }
    }
    Ok(())
}

fn validate_materialization_reason(reason: &str) -> Result<()> {
    if reason.is_empty() || reason != reason.trim() {
        bail!("materialization reason must be a non-empty trimmed string");
    }
    if reason.len() > MAX_MATERIALIZATION_REASON_BYTES {
        bail!("materialization reason exceeds the {MAX_MATERIALIZATION_REASON_BYTES}-byte limit");
    }
    if reason.contains('\n') || reason.contains('\r') {
        bail!("materialization reason must be a single line");
    }
    Ok(())
}

fn validate_schema(actual: &str, expected: &str, label: &str) -> Result<()> {
    if actual != expected {
        bail!("unsupported {label} schema {actual:?}");
    }
    Ok(())
}

fn validate_nonempty(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value != value.trim() {
        bail!("{label} must be a non-empty trimmed string");
    }
    Ok(())
}

fn validate_rfc3339_timestamp(value: &str, label: &str) -> Result<()> {
    if value != value.trim() {
        bail!("{label} must not have surrounding whitespace");
    }
    OffsetDateTime::parse(value, &Rfc3339)
        .with_context(|| format!("{label} must be an RFC 3339 timestamp"))?;
    Ok(())
}

fn validate_unique_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Result<()> {
    let mut seen = BTreeSet::new();
    for path in paths {
        if !seen.insert(path) {
            bail!("materialization outputs must not share a path");
        }
    }
    Ok(())
}

fn domain_separated_identity(domain: &str, value: &impl Serialize) -> Result<String> {
    let bytes = serde_json_canonicalizer::to_vec(value)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_bytes());
    hasher.update(&[0]);
    hasher.update(&bytes);
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CaptureClassification, CaptureEvidence, CaptureEvidenceSpan, CaptureExtractorIdentity,
        CaptureProvenance, CaptureReviewOutcome, CaptureSourceLocator, MemoryDraft,
        OkfProposalSensitivity,
    };

    fn identity(fill: char) -> String {
        format!("blake3:{}", fill.to_string().repeat(64))
    }

    fn revision(fill: char) -> CanonicalRevision {
        CanonicalRevision {
            schema: CANONICAL_REVISION_SCHEMA.to_owned(),
            revision_hash: identity(fill),
        }
    }

    fn output(path: &str) -> MaterializationOutputIntent {
        MaterializationOutputIntent {
            path: path.to_owned(),
            record_id: "team/install-risk".to_owned(),
            action: MaterializationAction::Create,
            expected_prior_revision: ExpectedPriorRevision::Absent,
            intended_semantic_revision: revision('a'),
            role: MaterializationOutputRole::CanonicalRecord,
        }
    }

    fn attested_record(decision_id: String) -> OkfRecordFile {
        OkfRecordFile {
            concept_id: "team/install-risk".to_owned(),
            draft: MemoryDraft {
                memory_type: MemoryType::Risk,
                lane: MemoryLane::Semantic,
                scope_kind: ScopeKind::Repo,
                scope_id: None,
                visibility: Visibility::Repo,
                title: "Risk: package install".to_owned(),
                body: "Package installs require review.".to_owned(),
                tags: vec!["security".to_owned()],
                source_kind: Some("human-authored".to_owned()),
                source_ref: Some("issue://100".to_owned()),
                sensitivity: OkfProposalSensitivity::RepoSafe,
                content_class: RepositoryContentClass::GeneralRepoKnowledge,
                confidence: 0.8,
            },
            status: MemoryStatus::Active,
            applies_to: vec!["Cargo.toml".to_owned()],
            created: "2026-07-16T12:00:00Z".to_owned(),
            updated: None,
            supersedes_id: None,
            retention: RetentionFacts {
                occurred_at: None,
                started_at: None,
                last_continued_at: None,
                closed_at: None,
                explicit_expires_at: None,
                episodic_extension: None,
            },
            origin: OriginDescriptor::new(
                "repository-materialization:test-attested",
                crate::OriginRoute::RepositoryMaterialization,
            ),
            lineage: None,
            proposal_id: None,
            capture: None,
            materialization: Some(MaterializationMetadata {
                schema: MATERIALIZATION_METADATA_SCHEMA.to_owned(),
                action: MaterializationAction::Create,
                plan_id: identity('b'),
                candidate_id: identity('c'),
                decision_id,
                decision_at: "2026-07-16T12:00:00Z".to_owned(),
                safety_contract: REPOSITORY_WRITE_SAFETY_SCHEMA.to_owned(),
                revision: revision('d'),
                target: None,
                reason: None,
            }),
        }
    }

    #[test]
    fn decision_identity_is_deterministic_and_binds_ordered_output_intents() -> Result<()> {
        let plan = build_repository_materialization_plan(
            identity('1'),
            vec![output(".memzoi/records/team/install-risk.md")],
        )?;
        let policy = MaterializationPolicy {
            policy_id: "repo-safe".to_owned(),
            safety_contract: REPOSITORY_WRITE_SAFETY_SCHEMA.to_owned(),
        };
        let first = build_repository_materialization_decision(
            &plan,
            "2026-07-16T12:00:00Z".to_owned(),
            policy.clone(),
            MaterializationAuthorizationCapability::ExplicitCli,
        )?;
        let second = build_repository_materialization_decision(
            &plan,
            "2026-07-16T12:00:00Z".to_owned(),
            policy,
            MaterializationAuthorizationCapability::ExplicitCli,
        )?;
        assert_eq!(first.decision_id, second.decision_id);

        let mut changed = first.clone();
        changed.outputs[0].path = ".memzoi/records/team/updated-install-risk.md".to_owned();
        assert_ne!(
            repository_materialization_decision_id(&first)?,
            repository_materialization_decision_id(&changed)?,
        );
        Ok(())
    }

    #[test]
    fn canonical_revision_excludes_materialization_decision_identity() -> Result<()> {
        let first = attested_record(identity('e'));
        let mut second = first.clone();
        let metadata = second
            .materialization
            .as_mut()
            .expect("test record includes materialization metadata");
        metadata.plan_id = identity('f');
        metadata.candidate_id = identity('0');
        metadata.decision_id = identity('1');
        metadata.decision_at = "2026-07-17T12:00:00Z".to_owned();
        metadata.safety_contract = "foreign/repository-write-safety".to_owned();

        assert_eq!(
            canonical_revision_for_okf_record(&first)?,
            canonical_revision_for_okf_record(&second)?,
        );
        Ok(())
    }
    fn candidate_record() -> RepositoryMaterializationCandidateRecord {
        RepositoryMaterializationCandidateRecord {
            concept_id: "team/install-risk".to_owned(),
            draft: MemoryDraft {
                memory_type: MemoryType::Risk,
                lane: MemoryLane::Semantic,
                scope_kind: ScopeKind::Repo,
                scope_id: None,
                visibility: Visibility::Repo,
                title: "Risk: package install".to_owned(),
                body: "Package installs require review.".to_owned(),
                tags: vec!["security".to_owned()],
                source_kind: Some("human-authored".to_owned()),
                source_ref: Some("issue://100".to_owned()),
                sensitivity: OkfProposalSensitivity::RepoSafe,
                content_class: RepositoryContentClass::GeneralRepoKnowledge,
                confidence: 0.8,
            },
            status: MemoryStatus::Active,
            applies_to: vec!["Cargo.toml".to_owned()],
            created: "2026-07-16T12:00:00Z".to_owned(),
            updated: None,
            supersedes_id: None,
            retention: RetentionFacts {
                occurred_at: None,
                started_at: None,
                last_continued_at: None,
                closed_at: None,
                explicit_expires_at: None,
                episodic_extension: None,
            },
            origin: OriginDescriptor::new(
                "repository-materialization:test-candidate",
                crate::OriginRoute::RepositoryMaterialization,
            ),
            lineage: None,
            proposal_id: Some("direct-proposal-100".to_owned()),
            capture: None,
        }
    }

    fn capture_provenance() -> CaptureProvenance {
        CaptureProvenance {
            schema: crate::CAPTURE_PROVENANCE_SCHEMA.to_owned(),
            plan_id: identity('a'),
            review_id: identity('b'),
            claim_id: identity('c'),
            reviewed_claim_id: identity('d'),
            candidate_id: identity('e'),
            reviewed_candidate_id: identity('f'),
            extraction: CaptureExtractorIdentity {
                kind: "markdown".to_owned(),
                id: "markdown".to_owned(),
                implementation_digest: identity('0'),
            },
            evidence: vec![CaptureEvidence {
                source_id: "source-1".to_owned(),
                locator: CaptureSourceLocator::ProjectPath {
                    path: "notes.md".to_owned(),
                },
                source_content_hash: identity('1'),
                span: CaptureEvidenceSpan {
                    byte_start: 0,
                    byte_end: 12,
                    line_start: 1,
                    line_end: 1,
                },
                evidence_content_hash: identity('2'),
                text: None,
                heading_path: vec!["Risk".to_owned()],
                section_kind: "fact".to_owned(),
                semantic_location: None,
            }],
            confidence: "0.82".to_owned(),
            classification: CaptureClassification {
                destination: MemoryDestination::Repo,
                destination_reason: "repository-safe evidence".to_owned(),
                sensitivity: OkfProposalSensitivity::RepoSafe,
                sensitivity_reason: "reviewed".to_owned(),
                content_class: RepositoryContentClass::GeneralRepoKnowledge,
                policy: MemoryDestination::Repo.policy(),
            },
            destination: MemoryDestination::Repo,
            sensitivity: OkfProposalSensitivity::RepoSafe,
            review_outcome: CaptureReviewOutcome::Accept,
            review_reason_code: None,
            reviewed_by: "reviewer".to_owned(),
            reviewed_at: "2026-07-16T12:00:00Z".to_owned(),
            routed_by: "direct-candidate-test".to_owned(),
        }
    }

    #[test]
    fn direct_candidate_id_is_stable_and_tamper_evident() -> Result<()> {
        let first = build_repository_materialization_candidate(
            candidate_record(),
            MaterializationAction::Create,
            ExpectedPriorRevision::Absent,
            None,
            None,
        )?;
        let second = build_repository_materialization_candidate(
            candidate_record(),
            MaterializationAction::Create,
            ExpectedPriorRevision::Absent,
            None,
            None,
        )?;
        assert_eq!(first.candidate_id, second.candidate_id);

        let mut tampered = first;
        tampered
            .record
            .draft
            .body
            .push_str(" Changed after identification.");
        assert!(
            tampered.validate().is_err(),
            "candidate identity must attest to semantic record content"
        );
        Ok(())
    }

    #[test]
    fn direct_candidate_rejects_non_repo_safe_input() -> Result<()> {
        let mut candidate = build_repository_materialization_candidate(
            candidate_record(),
            MaterializationAction::Create,
            ExpectedPriorRevision::Absent,
            None,
            None,
        )?;
        candidate.destination = MemoryDestination::Local;
        assert!(
            candidate.validate().is_err(),
            "repository candidates must reject non-repository destinations"
        );

        let mut candidate = build_repository_materialization_candidate(
            candidate_record(),
            MaterializationAction::Create,
            ExpectedPriorRevision::Absent,
            None,
            None,
        )?;
        candidate.record.draft.sensitivity = OkfProposalSensitivity::Unknown;
        assert!(
            candidate.validate().is_err(),
            "repository candidates must reject unknown sensitivity"
        );
        Ok(())
    }

    #[test]
    fn direct_candidate_enforces_lifecycle_shapes() -> Result<()> {
        let mut invalid = build_repository_materialization_candidate(
            candidate_record(),
            MaterializationAction::Create,
            ExpectedPriorRevision::Absent,
            None,
            None,
        )?;
        invalid.action = MaterializationAction::Supersede;
        assert!(
            invalid.validate().is_err(),
            "supersede candidates require a target and bounded reason"
        );

        let mut replacement = candidate_record();
        replacement.concept_id = "team/install-risk-v2".to_owned();
        replacement.supersedes_id = Some("team/install-risk".to_owned());
        let supersede = build_repository_materialization_candidate(
            replacement,
            MaterializationAction::Supersede,
            ExpectedPriorRevision::Absent,
            Some(MaterializationTarget {
                record_id: "team/install-risk".to_owned(),
                expected_revision: revision('3'),
            }),
            Some("The replacement reflects the current package policy.".to_owned()),
        )?;
        supersede.validate()?;

        let mut tombstone = candidate_record();
        tombstone.status = MemoryStatus::Tombstoned;
        tombstone.supersedes_id = None;
        let target_revision = revision('4');
        let tombstone = build_repository_materialization_candidate(
            tombstone,
            MaterializationAction::Tombstone,
            ExpectedPriorRevision::Revision(target_revision.clone()),
            Some(MaterializationTarget {
                record_id: "team/install-risk".to_owned(),
                expected_revision: target_revision,
            }),
            Some("The guidance is no longer applicable.".to_owned()),
        )?;
        tombstone.validate()?;
        Ok(())
    }

    #[test]
    fn direct_candidate_plan_hashes_lifecycle_semantics() -> Result<()> {
        let mut replacement = candidate_record();
        replacement.concept_id = "team/install-risk-v2".to_owned();
        replacement.supersedes_id = Some("team/install-risk".to_owned());
        let candidate = build_repository_materialization_candidate(
            replacement,
            MaterializationAction::Supersede,
            ExpectedPriorRevision::Absent,
            Some(MaterializationTarget {
                record_id: "team/install-risk".to_owned(),
                expected_revision: revision('5'),
            }),
            Some("The replacement reflects the current package policy.".to_owned()),
        )?;

        let plan = repository_materialization_candidate_plan(&candidate)?;
        assert_eq!(plan.outputs.len(), 1);
        assert_eq!(plan.outputs[0].path, candidate.output_path);
        assert_eq!(plan.outputs[0].action, MaterializationAction::Supersede);
        assert_ne!(
            plan.outputs[0].intended_semantic_revision,
            canonical_revision_for_okf_record(
                &repository_materialization_candidate_to_okf_record(&candidate)?
            )?,
            "candidate lifecycle semantics must affect the intended revision"
        );
        Ok(())
    }

    #[test]
    fn direct_candidate_rejects_malformed_capture_provenance() {
        let mut record = candidate_record();
        let mut capture = capture_provenance();
        capture.confidence = "not-a-number".to_owned();
        record.capture = Some(capture);

        let error = build_repository_materialization_candidate(
            record,
            MaterializationAction::Create,
            ExpectedPriorRevision::Absent,
            None,
            None,
        )
        .expect_err("malformed capture provenance must invalidate a direct candidate");

        assert!(
            format!("{error:#}").contains("capture provenance confidence is invalid"),
            "unexpected malformed capture error: {error:#}"
        );
    }

    #[test]
    fn direct_candidate_conversion_preserves_capture_provenance() -> Result<()> {
        let mut record = candidate_record();
        record.capture = Some(capture_provenance());
        let candidate = build_repository_materialization_candidate(
            record.clone(),
            MaterializationAction::Create,
            ExpectedPriorRevision::Absent,
            None,
            None,
        )?;

        let converted = repository_materialization_candidate_to_okf_record(&candidate)?;
        assert_eq!(converted.concept_id, record.concept_id);
        assert_eq!(converted.draft, record.draft);
        assert_eq!(converted.capture, record.capture);
        assert_eq!(converted.materialization, None);
        assert_eq!(
            repository_materialization_policy().safety_contract,
            REPOSITORY_WRITE_SAFETY_SCHEMA
        );
        assert_eq!(
            repository_materialization_policy().policy_id,
            REPOSITORY_WRITE_SAFETY_SCHEMA
        );
        Ok(())
    }
}
