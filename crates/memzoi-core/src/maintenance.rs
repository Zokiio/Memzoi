use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    io::Read,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use crate::{
    CanonicalRevision, CaptureReviewOutcome, MemoryDestination, MemoryLane, MemoryPaths,
    MemoryRecord, MemoryStatus, MemoryType, OkfRecordFile, RecordLineageKind,
    RepositoryContentClass, RetentionReason, RetentionState, ScopeKind, Visibility,
    canonical_revision_for_okf_record, evaluate_current_assertion,
};

pub const MAINTENANCE_REQUEST_SCHEMA: &str = "memzoi/maintenance-request";
pub const MAINTENANCE_PLAN_SCHEMA: &str = "memzoi/maintenance-plan";
pub const MAINTENANCE_POLICY_VERSION: &str = "maintenance-policy/1";
/// Upper bound while repository admission still performs synchronous Git review checks per file.
pub const MAINTENANCE_MAX_RECORDS: usize = 256;
pub const MAINTENANCE_MAX_ADMITTED_RECORDS: usize = MAINTENANCE_MAX_RECORDS;
pub const MAINTENANCE_MAX_INVENTORY_ENTRIES: usize = 10_000;
pub const MAINTENANCE_MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;
pub const MAINTENANCE_MAX_INPUT_FILE_BYTES: usize = 2 * 1024 * 1024;
pub const MAINTENANCE_MAX_DIRECTORY_DEPTH: usize = 64;
pub const MAINTENANCE_MAX_PAIR_COMPARISONS: usize = 1_000_000;
pub const MAINTENANCE_MAX_FINDINGS: usize = 4_096;
pub const MAINTENANCE_MAX_ACTION_CANDIDATES: usize = 4_096;
pub const MAINTENANCE_MAX_SERIALIZED_PLAN_BYTES: usize = 2 * 1024 * 1024 - 4096;

const MAX_VALIDITY_HOURS: i64 = 24;
const STALE_AFTER_DAYS: i64 = 180;
const DUPLICATE_DETECTOR_VERSION: &str = "exact-duplicate/1";
const CONTRADICTION_DETECTOR_VERSION: &str = "lexical-contradiction/1";
const STALENESS_DETECTOR_VERSION: &str = "age-staleness-180d/1";
const EXPIRY_DETECTOR_VERSION: &str = "retention-expiry/1";
const RENEWAL_DETECTOR_VERSION: &str = "capture-renewal/1";

#[derive(Debug, Clone)]
pub struct MaintenancePlanningControl {
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}

impl MaintenancePlanningControl {
    pub fn new(deadline: Instant) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline,
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            bail!("maintenance planning cancelled");
        }
        if Instant::now() >= self.deadline {
            bail!("maintenance planning timed out");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenancePlanRequest {
    pub schema: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluated_at: Option<String>,
    pub record_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PrivateMaintenanceRecordInput {
    pub record: MemoryRecord,
    pub applicability_paths: Vec<String>,
    pub version_token: String,
    pub current_assertion: bool,
    pub retention_state: RetentionState,
    pub retention_reason: RetentionReason,
    pub retention_boundary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MaintenanceScope {
    Repository {
        repository_fingerprint: String,
        target_record_ids: Vec<String>,
    },
    PrivateRuntime {
        runtime_fingerprint: String,
        target_record_ids: Vec<String>,
    },
}

impl MaintenanceScope {
    pub fn target_record_ids(&self) -> &[String] {
        match self {
            Self::Repository {
                target_record_ids, ..
            }
            | Self::PrivateRuntime {
                target_record_ids, ..
            } => target_record_ids,
        }
    }

    pub fn is_repository(&self) -> bool {
        matches!(self, Self::Repository { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenancePolicySnapshot {
    pub policy_version: String,
    pub maximum_validity_seconds: u64,
    pub stale_after_seconds: u64,
    pub policy_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceDetectorSnapshot {
    pub kind: MaintenanceFindingKind,
    pub version: String,
    pub configuration_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceAuthoritySnapshot {
    pub mode: MaintenanceAuthorityMode,
    pub grant_fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceAuthorityMode {
    ReportOnly,
    AutomaticPrivateMaintenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MaintenanceRecordVersion {
    CanonicalRepository {
        source_path: String,
        #[serde(with = "strict_canonical_revision")]
        revision: CanonicalRevision,
    },
    PrivateRuntime {
        version_token: String,
    },
}

impl MaintenanceRecordVersion {
    pub fn repository_revision(&self) -> Option<&CanonicalRevision> {
        match self {
            Self::CanonicalRepository { revision, .. } => Some(revision),
            Self::PrivateRuntime { .. } => None,
        }
    }

    pub fn private_version_token(&self) -> Option<&str> {
        match self {
            Self::CanonicalRepository { .. } => None,
            Self::PrivateRuntime { version_token } => Some(version_token),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceRecordSnapshot {
    pub record_id: String,
    pub version: MaintenanceRecordVersion,
    pub content_hash: String,
    pub claim_digest: String,
    pub applicability_digest: String,
    pub temporal_digest: String,
    pub status: MemoryStatus,
    pub current_assertion: bool,
    pub retention_state: RetentionState,
    pub retention_reason: RetentionReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_boundary: Option<String>,
    pub target: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceFindingKind {
    ExactDuplicate,
    HighConfidenceContradiction,
    Stale,
    Expired,
    RenewalCandidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceConfidence {
    Exact,
    High,
    ReportOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceEvidence {
    pub code: String,
    pub digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceConflictEdge {
    pub record_ids: [String; 2],
    pub evidence_digest: String,
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceFinding {
    pub finding_id: String,
    pub kind: MaintenanceFindingKind,
    pub record_ids: Vec<String>,
    pub comparison_set_digest: String,
    pub evidence: Vec<MaintenanceEvidence>,
    pub confidence: MaintenanceConfidence,
    pub conflict_edges: Vec<MaintenanceConflictEdge>,
    pub proposed_action_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceActionGroupKind {
    RepositoryMaterialization,
    PrivateDerivedState,
    OwnerAuthorizedPrivateMutation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceActionClass {
    ConsolidateExactDuplicates,
    CreateRenewalSuccessor,
    SuppressUnresolvedConflict,
    OwnerConsolidateExactDuplicates,
    OwnerCreateRenewalSuccessor,
    OwnerResolveContradiction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceActionPreconditions {
    pub comparison_set_digest: String,
    pub record_versions: BTreeMap<String, MaintenanceRecordVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceAction {
    pub action_id: String,
    pub class: MaintenanceActionClass,
    pub finding_id: String,
    pub record_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keeper_record_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predecessor_record_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_record_id: Option<String>,
    pub preconditions: MaintenanceActionPreconditions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceActionGroup {
    pub kind: MaintenanceActionGroupKind,
    pub actions: Vec<MaintenanceAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenancePreconditions {
    pub scope: MaintenanceScopeBinding,
    pub target_versions: BTreeMap<String, MaintenanceRecordVersion>,
    pub comparison_set_digest: String,
    pub detector_digest: String,
    pub policy_digest: String,
    pub grant_fingerprint: String,
    pub not_after: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MaintenanceScopeBinding {
    Repository { repository_fingerprint: String },
    PrivateRuntime { runtime_fingerprint: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenancePlanSummary {
    pub records: usize,
    pub exact_duplicates: usize,
    pub contradictions: usize,
    pub stale: usize,
    pub expired: usize,
    pub renewal_candidates: usize,
    pub action_candidates: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceDiagnostic {
    pub code: String,
    pub count: usize,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenancePlan {
    pub schema: String,
    pub plan_id: String,
    pub request: MaintenancePlanRequest,
    pub evaluated_at: String,
    pub not_after: String,
    pub scope: MaintenanceScope,
    pub policy: MaintenancePolicySnapshot,
    pub detectors: Vec<MaintenanceDetectorSnapshot>,
    pub authority: MaintenanceAuthoritySnapshot,
    pub records: Vec<MaintenanceRecordSnapshot>,
    pub comparison_set_digest: String,
    pub findings: Vec<MaintenanceFinding>,
    pub action_groups: Vec<MaintenanceActionGroup>,
    pub preconditions: MaintenancePreconditions,
    pub summary: MaintenancePlanSummary,
    pub diagnostics: Vec<MaintenanceDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceStaleReason {
    NotYetValid,
    PlanExpired,
    RepositoryChanged,
    TargetChanged,
    ComparisonNeighborhoodChanged,
    DetectorChanged,
    PolicyChanged,
    GrantChanged,
    TemporalBoundaryChanged,
    PlanChanged,
    SourceChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MaintenanceRevalidation {
    Current {
        plan_id: String,
    },
    Stale {
        plan_id: String,
        reasons: Vec<MaintenanceStaleReason>,
    },
}

#[derive(Debug, Clone)]
struct LoadedRecord {
    snapshot: MaintenanceRecordSnapshot,
    record: OkfRecordFile,
    duplicate_claim_digest: String,
    renewal_claim_digest: String,
    freshness: OffsetDateTime,
    eligible_at_evaluation: bool,
}

#[derive(Debug, Clone)]
struct PrivateLoadedRecord {
    snapshot: MaintenanceRecordSnapshot,
    record: MemoryRecord,
    applicability_paths: Vec<String>,
    duplicate_claim_digest: String,
    renewal_claim_digest: String,
    freshness: OffsetDateTime,
    eligible_at_evaluation: bool,
}

#[derive(Debug)]
struct MissingMaintenanceTarget {
    record_id: String,
}

impl fmt::Display for MissingMaintenanceTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "maintenance target record {} does not exist",
            self.record_id
        )
    }
}

impl std::error::Error for MissingMaintenanceTarget {}

mod strict_canonical_revision {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::CanonicalRevision;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct WireRevision {
        schema: String,
        revision_hash: String,
    }

    pub fn serialize<S>(revision: &CanonicalRevision, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        revision.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<CanonicalRevision, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireRevision::deserialize(deserializer)?;
        Ok(CanonicalRevision {
            schema: wire.schema,
            revision_hash: wire.revision_hash,
        })
    }
}

pub fn plan_maintenance(
    paths: &MemoryPaths,
    request: MaintenancePlanRequest,
) -> Result<MaintenancePlan> {
    plan_maintenance_at(paths, request, OffsetDateTime::now_utc())
}

pub fn plan_maintenance_at(
    paths: &MemoryPaths,
    request: MaintenancePlanRequest,
    now: OffsetDateTime,
) -> Result<MaintenancePlan> {
    plan_maintenance_inner(paths, request, now, None)
}

pub fn plan_maintenance_with_control(
    paths: &MemoryPaths,
    request: MaintenancePlanRequest,
    control: &MaintenancePlanningControl,
) -> Result<MaintenancePlan> {
    plan_maintenance_with_control_at(paths, request, OffsetDateTime::now_utc(), control)
}

pub fn plan_maintenance_with_control_at(
    paths: &MemoryPaths,
    request: MaintenancePlanRequest,
    now: OffsetDateTime,
    control: &MaintenancePlanningControl,
) -> Result<MaintenancePlan> {
    plan_maintenance_inner(paths, request, now, Some(control))
}

fn plan_maintenance_inner(
    paths: &MemoryPaths,
    mut request: MaintenancePlanRequest,
    fallback_now: OffsetDateTime,
    control: Option<&MaintenancePlanningControl>,
) -> Result<MaintenancePlan> {
    check_control(control)?;
    validate_request(&request)?;
    let evaluated_at = match request.evaluated_at.as_deref() {
        Some(value) => parse_timestamp(value, "maintenance evaluated_at")?,
        None => fallback_now,
    };
    let evaluated_at_text = format_timestamp(evaluated_at)?;
    request.evaluated_at = Some(evaluated_at_text.clone());
    request.record_ids = normalize_record_ids(request.record_ids)?;

    let source = load_stable_repository_snapshot(paths, control)?;
    let available_ids = source
        .iter()
        .map(|snapshot| snapshot.record.concept_id.as_str())
        .collect::<BTreeSet<_>>();
    for requested in &request.record_ids {
        if !available_ids.contains(requested.as_str()) {
            return Err(MissingMaintenanceTarget {
                record_id: requested.clone(),
            }
            .into());
        }
    }
    let selected = if request.record_ids.is_empty() {
        available_ids
            .iter()
            .map(|record_id| (*record_id).to_owned())
            .collect::<BTreeSet<_>>()
    } else {
        request.record_ids.iter().cloned().collect()
    };

    let mut loaded = Vec::with_capacity(source.len());
    let mut future_record_count = 0_usize;
    let maximum_not_after = evaluated_at
        .checked_add(Duration::hours(MAX_VALIDITY_HOURS))
        .context("maintenance evaluation time cannot represent the validity window")?;
    let mut not_after = maximum_not_after;
    for source_record in source {
        check_control(control)?;
        let record = source_record.record;
        validate_repository_record(&record)?;
        let decision = evaluate_current_assertion(
            &record.concept_id,
            record.status,
            record.draft.lane,
            &record.retention,
            evaluated_at,
            Vec::new(),
        )?;
        let freshness = record_freshness(&record)?;
        let eligible_at_evaluation = freshness <= evaluated_at;
        if !eligible_at_evaluation {
            future_record_count = future_record_count
                .checked_add(1)
                .context("maintenance future-record diagnostic count overflowed")?;
        }
        if eligible_at_evaluation
            && let Some(boundary) = decision.retention.effective_boundary.as_deref()
        {
            let boundary = parse_timestamp(boundary, "maintenance retention boundary")?;
            if boundary > evaluated_at && boundary < not_after {
                not_after = boundary;
            }
        }
        if eligible_at_evaluation
            && decision.is_current
            && record.status == MemoryStatus::Active
            && matches!(
                record.draft.lane,
                MemoryLane::Semantic | MemoryLane::Procedural
            )
        {
            let stale_at = freshness
                .checked_add(Duration::days(STALE_AFTER_DAYS))
                .context("maintenance freshness cannot represent the staleness boundary")?;
            if stale_at > evaluated_at && stale_at < not_after {
                not_after = stale_at;
            }
        }

        let duplicate_claim_digest = duplicate_claim_digest(&record)?;
        let renewal_claim_digest = renewal_claim_digest(&record)?;
        let applicability_digest = identity(
            "memzoi/maintenance/applicability",
            &serde_json::json!({
                "scope_kind": record.draft.scope_kind,
                "scope_id": record.draft.scope_id,
                "paths": sorted_strings(&record.applies_to),
            }),
        )?;
        let temporal_digest = identity(
            "memzoi/maintenance/temporal",
            &serde_json::json!({
                "status": record.status,
                "retention": record.retention,
                "supersedes_id": record.supersedes_id,
                "lineage": record.lineage,
            }),
        )?;
        let revision = canonical_revision_for_okf_record(&record)?;
        let source_path = source_record
            .path
            .strip_prefix(&paths.project_root)
            .context("canonical maintenance source escaped the project root")?
            .to_string_lossy()
            .replace('\\', "/");
        let snapshot = MaintenanceRecordSnapshot {
            record_id: record.concept_id.clone(),
            version: MaintenanceRecordVersion::CanonicalRepository {
                source_path,
                revision,
            },
            content_hash: identity("memzoi/maintenance/content", &record.draft.body)?,
            claim_digest: duplicate_claim_digest.clone(),
            applicability_digest,
            temporal_digest,
            status: record.status,
            current_assertion: decision.is_current && eligible_at_evaluation,
            retention_state: decision.retention.state,
            retention_reason: decision.retention.reason,
            retention_boundary: decision.retention.effective_boundary,
            target: selected.contains(&record.concept_id),
        };
        loaded.push(LoadedRecord {
            snapshot,
            record,
            duplicate_claim_digest,
            renewal_claim_digest,
            freshness,
            eligible_at_evaluation,
        });
    }
    loaded.sort_by(|left, right| left.snapshot.record_id.cmp(&right.snapshot.record_id));

    let records = loaded
        .iter()
        .map(|loaded| loaded.snapshot.clone())
        .collect::<Vec<_>>();
    let comparison_set_digest = comparison_digest(&records)?;
    let policy = maintenance_policy()?;
    let detectors = maintenance_detectors()?;
    let authority = MaintenanceAuthoritySnapshot {
        mode: MaintenanceAuthorityMode::ReportOnly,
        grant_fingerprint: identity("memzoi/maintenance/grant", &"report-only")?,
    };
    let repository_fingerprint =
        identity("memzoi/maintenance/repository", &paths.repository_key())?;
    let target_record_ids = records
        .iter()
        .filter(|record| record.target)
        .map(|record| record.record_id.clone())
        .collect::<Vec<_>>();

    let mut findings = Vec::new();
    let mut repository_actions = Vec::new();
    detect_exact_duplicates(
        &loaded,
        &selected,
        &comparison_set_digest,
        &mut findings,
        &mut repository_actions,
        control,
    )?;
    detect_contradictions(
        &loaded,
        &selected,
        &comparison_set_digest,
        &mut findings,
        control,
    )?;
    detect_staleness(
        &loaded,
        &selected,
        evaluated_at,
        &comparison_set_digest,
        &mut findings,
        control,
    )?;
    detect_expiry(
        &loaded,
        &selected,
        &comparison_set_digest,
        &mut findings,
        control,
    )?;
    detect_renewals(
        &loaded,
        &selected,
        evaluated_at,
        &comparison_set_digest,
        &mut findings,
        &mut repository_actions,
        control,
    )?;

    findings.sort_by(|left, right| left.finding_id.cmp(&right.finding_id));
    repository_actions.sort_by(|left, right| left.action_id.cmp(&right.action_id));
    for finding in &mut findings {
        finding.proposed_action_ids = repository_actions
            .iter()
            .filter(|action| action.finding_id == finding.finding_id)
            .map(|action| action.action_id.clone())
            .collect();
    }

    let action_groups = vec![
        MaintenanceActionGroup {
            kind: MaintenanceActionGroupKind::RepositoryMaterialization,
            actions: repository_actions,
        },
        MaintenanceActionGroup {
            kind: MaintenanceActionGroupKind::PrivateDerivedState,
            actions: Vec::new(),
        },
        MaintenanceActionGroup {
            kind: MaintenanceActionGroupKind::OwnerAuthorizedPrivateMutation,
            actions: Vec::new(),
        },
    ];
    let detector_digest = identity("memzoi/maintenance/detectors", &detectors)?;
    let target_versions = records
        .iter()
        .filter(|record| record.target)
        .map(|record| (record.record_id.clone(), record.version.clone()))
        .collect::<BTreeMap<_, _>>();
    let not_after_text = format_timestamp(not_after)?;
    let summary = summarize(&records, &findings, &action_groups);
    let mut diagnostics = vec![MaintenanceDiagnostic {
        code: "repository_snapshot_evaluated".to_owned(),
        count: records.len(),
        digest: comparison_set_digest.clone(),
    }];
    if future_record_count > 0 {
        diagnostics.push(MaintenanceDiagnostic {
            code: "records_after_evaluation_excluded".to_owned(),
            count: future_record_count,
            digest: identity(
                "memzoi/maintenance/future-record-count",
                &future_record_count,
            )?,
        });
    }
    let mut plan = MaintenancePlan {
        schema: MAINTENANCE_PLAN_SCHEMA.to_owned(),
        plan_id: String::new(),
        request,
        evaluated_at: evaluated_at_text,
        not_after: not_after_text.clone(),
        scope: MaintenanceScope::Repository {
            repository_fingerprint: repository_fingerprint.clone(),
            target_record_ids,
        },
        policy: policy.clone(),
        detectors,
        authority: authority.clone(),
        records,
        comparison_set_digest: comparison_set_digest.clone(),
        findings,
        action_groups,
        preconditions: MaintenancePreconditions {
            scope: MaintenanceScopeBinding::Repository {
                repository_fingerprint,
            },
            target_versions,
            comparison_set_digest,
            detector_digest,
            policy_digest: policy.policy_digest.clone(),
            grant_fingerprint: authority.grant_fingerprint.clone(),
            not_after: not_after_text,
        },
        summary,
        diagnostics,
    };
    plan.plan_id = maintenance_plan_id(&plan)?;
    plan.validate()?;
    ensure_plan_size(&plan)?;
    Ok(plan)
}

/// Build a read-only private-runtime maintenance plan from an already stable,
/// authoritative snapshot. The caller is responsible for taking that snapshot
/// without mutating runtime state. Private content is inspected only to derive
/// evidence digests and is never copied into the returned artifact.
pub fn plan_private_maintenance_at(
    runtime_fingerprint: String,
    request: MaintenancePlanRequest,
    inputs: Vec<PrivateMaintenanceRecordInput>,
    fallback_now: OffsetDateTime,
) -> Result<MaintenancePlan> {
    plan_private_maintenance_inner(
        runtime_fingerprint,
        request,
        inputs,
        fallback_now,
        MaintenanceAuthoritySnapshot {
            mode: MaintenanceAuthorityMode::ReportOnly,
            grant_fingerprint: identity("memzoi/maintenance/grant", &"report-only")?,
        },
    )
}

pub(crate) fn plan_private_maintenance_with_authority_at(
    runtime_fingerprint: String,
    request: MaintenancePlanRequest,
    inputs: Vec<PrivateMaintenanceRecordInput>,
    fallback_now: OffsetDateTime,
    grant_fingerprint: String,
) -> Result<MaintenancePlan> {
    crate::validate_materialization_identity(
        &grant_fingerprint,
        "private maintenance grant fingerprint",
    )?;
    plan_private_maintenance_inner(
        runtime_fingerprint,
        request,
        inputs,
        fallback_now,
        MaintenanceAuthoritySnapshot {
            mode: MaintenanceAuthorityMode::AutomaticPrivateMaintenance,
            grant_fingerprint,
        },
    )
}

fn plan_private_maintenance_inner(
    runtime_fingerprint: String,
    mut request: MaintenancePlanRequest,
    mut inputs: Vec<PrivateMaintenanceRecordInput>,
    fallback_now: OffsetDateTime,
    authority: MaintenanceAuthoritySnapshot,
) -> Result<MaintenancePlan> {
    validate_request(&request)?;
    crate::validate_materialization_identity(
        &runtime_fingerprint,
        "maintenance private-runtime fingerprint",
    )?;
    ensure!(
        inputs.len() <= MAINTENANCE_MAX_RECORDS,
        "maintenance private-runtime snapshot exceeds the admitted-record limit"
    );
    let evaluated_at = match request.evaluated_at.as_deref() {
        Some(value) => parse_timestamp(value, "maintenance evaluated_at")?,
        None => fallback_now,
    };
    let evaluated_at_text = format_timestamp(evaluated_at)?;
    request.evaluated_at = Some(evaluated_at_text.clone());
    request.record_ids = normalize_record_ids(request.record_ids)?;

    inputs.sort_by(|left, right| left.record.id.cmp(&right.record.id));
    ensure_sorted_unique(
        inputs.iter().map(|input| input.record.id.as_str()),
        "maintenance private-runtime records",
    )?;
    let available_ids = inputs
        .iter()
        .map(|input| input.record.id.as_str())
        .collect::<BTreeSet<_>>();
    for requested in &request.record_ids {
        if !available_ids.contains(requested.as_str()) {
            return Err(MissingMaintenanceTarget {
                record_id: requested.clone(),
            }
            .into());
        }
    }
    let selected = if request.record_ids.is_empty() {
        available_ids
            .iter()
            .map(|record_id| (*record_id).to_owned())
            .collect::<BTreeSet<_>>()
    } else {
        request.record_ids.iter().cloned().collect()
    };

    let maximum_not_after = evaluated_at
        .checked_add(Duration::hours(MAX_VALIDITY_HOURS))
        .context("maintenance evaluation time cannot represent the validity window")?;
    let mut not_after = maximum_not_after;
    let mut future_record_count = 0_usize;
    let mut loaded = Vec::with_capacity(inputs.len());
    for input in inputs {
        let PrivateMaintenanceRecordInput {
            record,
            mut applicability_paths,
            version_token,
            current_assertion,
            retention_state,
            retention_reason,
            retention_boundary,
        } = input;
        crate::validate_canonical_record_id(&record.id)?;
        applicability_paths.sort();
        applicability_paths.dedup();
        validate_private_record_version_token(&version_token)?;
        ensure!(
            matches!(
                record.destination,
                MemoryDestination::Local | MemoryDestination::Session
            ),
            "private maintenance accepts only local or session runtime records"
        );
        let freshness = parse_timestamp(&record.updated_at, "maintenance record updated_at")?;
        let eligible_at_evaluation = freshness <= evaluated_at;
        if !eligible_at_evaluation {
            future_record_count = future_record_count
                .checked_add(1)
                .context("maintenance future-record diagnostic count overflowed")?;
        }
        if let Some(boundary) = retention_boundary.as_deref() {
            let boundary = parse_timestamp(boundary, "maintenance retention boundary")?;
            if eligible_at_evaluation && boundary > evaluated_at && boundary < not_after {
                not_after = boundary;
            }
        }
        if eligible_at_evaluation
            && current_assertion
            && record.status == MemoryStatus::Active
            && matches!(record.lane, MemoryLane::Semantic | MemoryLane::Procedural)
        {
            let stale_at = freshness
                .checked_add(Duration::days(STALE_AFTER_DAYS))
                .context("maintenance freshness cannot represent the staleness boundary")?;
            if stale_at > evaluated_at && stale_at < not_after {
                not_after = stale_at;
            }
        }
        // Content-derived projections remain process-local detector inputs.
        // The private artifact exposes only identities derived from the opaque
        // record ID and random version token, preventing dictionary tests
        // against private titles, bodies, or evidence.
        let duplicate_claim_digest = private_duplicate_claim_digest(&record, &applicability_paths)?;
        let renewal_claim_digest = private_renewal_claim_digest(&record)?;
        let opaque_content_identity = identity(
            "memzoi/maintenance/private-content",
            &(&record.id, &version_token),
        )?;
        let opaque_claim_identity = identity(
            "memzoi/maintenance/private-claim",
            &(&record.id, &version_token),
        )?;
        let applicability_digest = identity(
            "memzoi/maintenance/applicability",
            &serde_json::json!({
                "destination": record.destination,
                "scope_kind": record.scope_kind,
                "scope_id": record.scope_id,
                "visibility": record.visibility,
                "paths": applicability_paths,
            }),
        )?;
        let temporal_digest = identity(
            "memzoi/maintenance/temporal",
            &serde_json::json!({
                "status": record.status,
                "retention": record.retention,
                "retention_state": retention_state,
                "retention_reason": retention_reason,
                "retention_boundary": retention_boundary,
                "supersedes_id": record.supersedes_id,
                "lineage": record.lineage,
            }),
        )?;
        let snapshot = MaintenanceRecordSnapshot {
            record_id: record.id.clone(),
            version: MaintenanceRecordVersion::PrivateRuntime { version_token },
            content_hash: opaque_content_identity,
            claim_digest: opaque_claim_identity,
            applicability_digest,
            temporal_digest,
            status: record.status,
            current_assertion: current_assertion && eligible_at_evaluation,
            retention_state,
            retention_reason,
            retention_boundary,
            target: selected.contains(&record.id),
        };
        loaded.push(PrivateLoadedRecord {
            snapshot,
            record,
            applicability_paths,
            duplicate_claim_digest,
            renewal_claim_digest,
            freshness,
            eligible_at_evaluation,
        });
    }

    let records = loaded
        .iter()
        .map(|record| record.snapshot.clone())
        .collect::<Vec<_>>();
    let comparison_set_digest = comparison_digest(&records)?;
    let policy = maintenance_policy()?;
    let detectors = maintenance_detectors()?;
    let mut findings = Vec::new();
    let mut derived_actions = Vec::new();
    let mut owner_actions = Vec::new();
    detect_private_exact_duplicates(
        &loaded,
        &selected,
        &comparison_set_digest,
        &mut findings,
        &mut owner_actions,
    )?;
    detect_private_contradictions(
        &loaded,
        &selected,
        &comparison_set_digest,
        &mut findings,
        &mut derived_actions,
        &mut owner_actions,
        authority.mode == MaintenanceAuthorityMode::AutomaticPrivateMaintenance,
    )?;
    detect_private_staleness(&loaded, &selected, evaluated_at, &mut findings)?;
    detect_private_expiry(&loaded, &selected, &mut findings)?;
    detect_private_renewals(
        &loaded,
        &selected,
        evaluated_at,
        &comparison_set_digest,
        &mut findings,
        &mut owner_actions,
    )?;
    findings.sort_by(|left, right| left.finding_id.cmp(&right.finding_id));
    derived_actions.sort_by(|left, right| left.action_id.cmp(&right.action_id));
    owner_actions.sort_by(|left, right| left.action_id.cmp(&right.action_id));
    for finding in &mut findings {
        finding.proposed_action_ids = derived_actions
            .iter()
            .chain(owner_actions.iter())
            .filter(|action| action.finding_id == finding.finding_id)
            .map(|action| action.action_id.clone())
            .collect();
        finding.proposed_action_ids.sort();
    }
    let action_groups = vec![
        MaintenanceActionGroup {
            kind: MaintenanceActionGroupKind::RepositoryMaterialization,
            actions: Vec::new(),
        },
        MaintenanceActionGroup {
            kind: MaintenanceActionGroupKind::PrivateDerivedState,
            actions: derived_actions,
        },
        MaintenanceActionGroup {
            kind: MaintenanceActionGroupKind::OwnerAuthorizedPrivateMutation,
            actions: owner_actions,
        },
    ];
    let detector_digest = identity("memzoi/maintenance/detectors", &detectors)?;
    let target_record_ids = records
        .iter()
        .filter(|record| record.target)
        .map(|record| record.record_id.clone())
        .collect::<Vec<_>>();
    let target_versions = records
        .iter()
        .filter(|record| record.target)
        .map(|record| (record.record_id.clone(), record.version.clone()))
        .collect::<BTreeMap<_, _>>();
    let not_after_text = format_timestamp(not_after)?;
    let summary = summarize(&records, &findings, &action_groups);
    let mut diagnostics = vec![MaintenanceDiagnostic {
        code: "private_runtime_snapshot_evaluated".to_owned(),
        count: records.len(),
        digest: comparison_set_digest.clone(),
    }];
    if future_record_count > 0 {
        diagnostics.push(MaintenanceDiagnostic {
            code: "records_after_evaluation_excluded".to_owned(),
            count: future_record_count,
            digest: identity(
                "memzoi/maintenance/future-record-count",
                &future_record_count,
            )?,
        });
    }
    let mut plan = MaintenancePlan {
        schema: MAINTENANCE_PLAN_SCHEMA.to_owned(),
        plan_id: String::new(),
        request,
        evaluated_at: evaluated_at_text,
        not_after: not_after_text.clone(),
        scope: MaintenanceScope::PrivateRuntime {
            runtime_fingerprint: runtime_fingerprint.clone(),
            target_record_ids,
        },
        policy: policy.clone(),
        detectors,
        authority: authority.clone(),
        records,
        comparison_set_digest: comparison_set_digest.clone(),
        findings,
        action_groups,
        preconditions: MaintenancePreconditions {
            scope: MaintenanceScopeBinding::PrivateRuntime {
                runtime_fingerprint,
            },
            target_versions,
            comparison_set_digest,
            detector_digest,
            policy_digest: policy.policy_digest.clone(),
            grant_fingerprint: authority.grant_fingerprint.clone(),
            not_after: not_after_text,
        },
        summary,
        diagnostics,
    };
    plan.plan_id = maintenance_plan_id(&plan)?;
    plan.validate()?;
    ensure_plan_size(&plan)?;
    Ok(plan)
}

/// Derive the opaque scope binding used by private-runtime maintenance plans.
/// Callers should supply the stable repository/runtime key, never private
/// record content or another user-provided secret.
pub fn private_maintenance_runtime_fingerprint(runtime_scope_key: &str) -> Result<String> {
    ensure!(
        !runtime_scope_key.trim().is_empty() && runtime_scope_key == runtime_scope_key.trim(),
        "maintenance private-runtime scope key must be canonical and non-empty"
    );
    identity("memzoi/maintenance/private-runtime", &runtime_scope_key)
}

fn detect_exact_duplicates(
    loaded: &[LoadedRecord],
    selected: &BTreeSet<String>,
    comparison_set_digest: &str,
    findings: &mut Vec<MaintenanceFinding>,
    actions: &mut Vec<MaintenanceAction>,
    control: Option<&MaintenancePlanningControl>,
) -> Result<()> {
    let mut groups = BTreeMap::<&str, Vec<&LoadedRecord>>::new();
    for record in loaded
        .iter()
        .filter(|record| record.snapshot.current_assertion)
    {
        check_control(control)?;
        groups
            .entry(record.duplicate_claim_digest.as_str())
            .or_default()
            .push(record);
    }
    for (claim_digest, group) in groups.into_iter().filter(|(_, group)| group.len() > 1) {
        check_control(control)?;
        let record_ids = group
            .iter()
            .map(|record| record.snapshot.record_id.clone())
            .collect::<Vec<_>>();
        if !record_ids
            .iter()
            .any(|record_id| selected.contains(record_id))
        {
            continue;
        }
        let member_digest = finding_comparison_digest(&group)?;
        let mut finding = new_finding(
            MaintenanceFindingKind::ExactDuplicate,
            record_ids.clone(),
            member_digest,
            vec![MaintenanceEvidence {
                code: "exact_claim_projection".to_owned(),
                digest: claim_digest.to_owned(),
                boundary: None,
            }],
            MaintenanceConfidence::Exact,
        )?;
        let action = new_action(
            MaintenanceActionClass::ConsolidateExactDuplicates,
            &finding.finding_id,
            record_ids.clone(),
            record_ids.first().cloned(),
            None,
            None,
            comparison_set_digest,
            &group,
        )?;
        finding.proposed_action_ids.push(action.action_id.clone());
        push_finding(findings, finding)?;
        push_action(actions, action)?;
    }
    Ok(())
}

fn detect_contradictions(
    loaded: &[LoadedRecord],
    selected: &BTreeSet<String>,
    _comparison_set_digest: &str,
    findings: &mut Vec<MaintenanceFinding>,
    control: Option<&MaintenancePlanningControl>,
) -> Result<()> {
    let mut candidates = Vec::new();
    for record in loaded {
        check_control(control)?;
        if contradiction_eligible(record)
            && let Some(value) = polarity_signature(&record.record.draft.body)
        {
            candidates.push((record, value));
        }
    }

    let mut adjacency = vec![BTreeSet::<usize>::new(); candidates.len()];
    let mut pair_comparisons = 0_usize;
    for left_index in 0..candidates.len() {
        check_control(control)?;
        for right_index in (left_index + 1)..candidates.len() {
            consume_pair_work(&mut pair_comparisons, control)?;
            let (left, (left_signature, left_negative)) = &candidates[left_index];
            let (right, (right_signature, right_negative)) = &candidates[right_index];
            if left_signature != right_signature || left_negative == right_negative {
                continue;
            }
            if !same_claim_context(&left.record, &right.record)
                || !paths_overlap(&left.record.applies_to, &right.record.applies_to)
                || records_are_temporally_related(&left.record, &right.record)
            {
                continue;
            }
            adjacency[left_index].insert(right_index);
            adjacency[right_index].insert(left_index);
        }
    }

    let mut visited = vec![false; candidates.len()];
    for start in 0..candidates.len() {
        check_control(control)?;
        if visited[start] || adjacency[start].is_empty() {
            continue;
        }
        let mut pending = vec![start];
        let mut component = Vec::new();
        while let Some(index) = pending.pop() {
            check_control(control)?;
            if visited[index] {
                continue;
            }
            visited[index] = true;
            component.push(index);
            pending.extend(
                adjacency[index]
                    .iter()
                    .rev()
                    .filter(|neighbor| !visited[**neighbor]),
            );
        }
        component.sort_unstable();
        let records = component
            .iter()
            .map(|index| candidates[*index].0)
            .collect::<Vec<_>>();
        let record_ids = records
            .iter()
            .map(|record| record.snapshot.record_id.clone())
            .collect::<Vec<_>>();
        if !record_ids
            .iter()
            .any(|record_id| selected.contains(record_id))
        {
            continue;
        }
        let signature_digest = identity(
            "memzoi/maintenance/contradiction-signature",
            &candidates[start].1.0,
        )?;
        let mut finding = new_finding(
            MaintenanceFindingKind::HighConfidenceContradiction,
            record_ids,
            finding_comparison_digest(&records)?,
            vec![MaintenanceEvidence {
                code: "allowlisted_symmetric_polarity".to_owned(),
                digest: signature_digest,
                boundary: None,
            }],
            MaintenanceConfidence::High,
        )?;
        for &left_index in &component {
            for &right_index in adjacency[left_index].range((left_index + 1)..) {
                if component.binary_search(&right_index).is_err() {
                    continue;
                }
                let mut edge_ids = [
                    candidates[left_index].0.snapshot.record_id.clone(),
                    candidates[right_index].0.snapshot.record_id.clone(),
                ];
                edge_ids.sort();
                finding.conflict_edges.push(MaintenanceConflictEdge {
                    evidence_digest: identity("memzoi/maintenance/contradiction-edge", &edge_ids)?,
                    record_ids: edge_ids,
                    reason_code: "high_confidence_unresolved_contradiction".to_owned(),
                });
            }
        }
        finding.conflict_edges.sort();
        finding.finding_id = maintenance_finding_id(&finding)?;
        push_finding(findings, finding)?;
    }
    Ok(())
}

fn detect_staleness(
    loaded: &[LoadedRecord],
    selected: &BTreeSet<String>,
    evaluated_at: OffsetDateTime,
    _comparison_set_digest: &str,
    findings: &mut Vec<MaintenanceFinding>,
    control: Option<&MaintenancePlanningControl>,
) -> Result<()> {
    for record in loaded.iter().filter(|record| {
        record.snapshot.current_assertion
            && record.record.status == MemoryStatus::Active
            && matches!(
                record.record.draft.lane,
                MemoryLane::Semantic | MemoryLane::Procedural
            )
            && selected.contains(&record.snapshot.record_id)
    }) {
        check_control(control)?;
        let stale_at = record
            .freshness
            .checked_add(Duration::days(STALE_AFTER_DAYS))
            .context("maintenance freshness cannot represent the staleness boundary")?;
        if evaluated_at < stale_at {
            continue;
        }
        push_finding(
            findings,
            new_finding(
                MaintenanceFindingKind::Stale,
                vec![record.snapshot.record_id.clone()],
                finding_comparison_digest(&[record])?,
                vec![MaintenanceEvidence {
                    code: "freshness_age_threshold".to_owned(),
                    digest: identity(
                        "memzoi/maintenance/freshness",
                        &format_timestamp(record.freshness)?,
                    )?,
                    boundary: Some(format_timestamp(stale_at)?),
                }],
                MaintenanceConfidence::ReportOnly,
            )?,
        )?;
    }
    Ok(())
}

fn detect_expiry(
    loaded: &[LoadedRecord],
    selected: &BTreeSet<String>,
    _comparison_set_digest: &str,
    findings: &mut Vec<MaintenanceFinding>,
    control: Option<&MaintenancePlanningControl>,
) -> Result<()> {
    for record in loaded.iter().filter(|record| {
        record.eligible_at_evaluation
            && (record.record.status == MemoryStatus::Expired
                || (record.record.status == MemoryStatus::Active
                    && record.snapshot.retention_state == RetentionState::QueryOnly))
            && selected.contains(&record.snapshot.record_id)
    }) {
        check_control(control)?;
        let mut evidence = Vec::new();
        if record.record.status == MemoryStatus::Expired {
            evidence.push(MaintenanceEvidence {
                code: "lifecycle_expired".to_owned(),
                digest: record.snapshot.temporal_digest.clone(),
                boundary: None,
            });
        }
        if record.snapshot.retention_state == RetentionState::QueryOnly {
            evidence.push(MaintenanceEvidence {
                code: format!("retention_{:?}", record.snapshot.retention_reason).to_lowercase(),
                digest: record.snapshot.temporal_digest.clone(),
                boundary: record.snapshot.retention_boundary.clone(),
            });
        }
        push_finding(
            findings,
            new_finding(
                MaintenanceFindingKind::Expired,
                vec![record.snapshot.record_id.clone()],
                finding_comparison_digest(&[record])?,
                evidence,
                MaintenanceConfidence::Exact,
            )?,
        )?;
    }
    Ok(())
}

fn detect_renewals(
    loaded: &[LoadedRecord],
    selected: &BTreeSet<String>,
    evaluated_at: OffsetDateTime,
    comparison_set_digest: &str,
    findings: &mut Vec<MaintenanceFinding>,
    actions: &mut Vec<MaintenanceAction>,
    control: Option<&MaintenancePlanningControl>,
) -> Result<()> {
    let mut pair_comparisons = 0_usize;
    for predecessor in loaded.iter().filter(|record| {
        renewal_eligible(record)
            && matches!(
                record.record.status,
                MemoryStatus::Active | MemoryStatus::Expired
            )
            && record.snapshot.retention_state == RetentionState::QueryOnly
            && record.snapshot.retention_boundary.is_some()
    }) {
        check_control(control)?;
        let boundary = parse_timestamp(
            predecessor
                .snapshot
                .retention_boundary
                .as_deref()
                .expect("renewal predecessor boundary checked"),
            "renewal predecessor boundary",
        )?;
        for evidence_record in loaded.iter().filter(|record| {
            renewal_eligible(record)
                && record.snapshot.current_assertion
                && record.renewal_claim_digest == predecessor.renewal_claim_digest
                && record.snapshot.record_id != predecessor.snapshot.record_id
        }) {
            consume_pair_work(&mut pair_comparisons, control)?;
            if !selected.contains(&predecessor.snapshot.record_id)
                && !selected.contains(&evidence_record.snapshot.record_id)
            {
                continue;
            }
            if evidence_record.record.origin == predecessor.record.origin
                || evidence_record
                    .record
                    .lineage
                    .as_ref()
                    .is_some_and(|lineage| {
                        lineage.kind == RecordLineageKind::Renewal
                            && lineage.predecessor_id == predecessor.snapshot.record_id
                    })
            {
                continue;
            }
            let Some(capture) = evidence_record.record.capture.as_ref() else {
                continue;
            };
            if !matches!(
                capture.review_outcome,
                CaptureReviewOutcome::Accept | CaptureReviewOutcome::Edit
            ) {
                continue;
            }
            let reviewed_at = parse_timestamp(&capture.reviewed_at, "renewal reviewed_at")?;
            if reviewed_at <= boundary || reviewed_at > evaluated_at {
                continue;
            }
            let evidence_digest =
                identity("memzoi/maintenance/capture-evidence", &capture.evidence)?;
            if predecessor.record.capture.as_ref().is_some_and(|prior| {
                identity("memzoi/maintenance/capture-evidence", &prior.evidence)
                    .is_ok_and(|digest| digest == evidence_digest)
            }) {
                continue;
            }
            let mut record_ids = vec![
                predecessor.snapshot.record_id.clone(),
                evidence_record.snapshot.record_id.clone(),
            ];
            record_ids.sort();
            let pair = vec![predecessor, evidence_record];
            let mut finding = new_finding(
                MaintenanceFindingKind::RenewalCandidate,
                record_ids.clone(),
                finding_comparison_digest(&pair)?,
                vec![MaintenanceEvidence {
                    code: "fresh_accepted_capture_evidence".to_owned(),
                    digest: evidence_digest,
                    boundary: Some(format_timestamp(boundary)?),
                }],
                MaintenanceConfidence::High,
            )?;
            let action = new_action(
                MaintenanceActionClass::CreateRenewalSuccessor,
                &finding.finding_id,
                record_ids,
                None,
                Some(predecessor.snapshot.record_id.clone()),
                Some(evidence_record.snapshot.record_id.clone()),
                comparison_set_digest,
                &pair,
            )?;
            finding.proposed_action_ids.push(action.action_id.clone());
            push_finding(findings, finding)?;
            push_action(actions, action)?;
        }
    }
    Ok(())
}

fn detect_private_exact_duplicates(
    loaded: &[PrivateLoadedRecord],
    selected: &BTreeSet<String>,
    comparison_set_digest: &str,
    findings: &mut Vec<MaintenanceFinding>,
    actions: &mut Vec<MaintenanceAction>,
) -> Result<()> {
    let mut groups = BTreeMap::<&str, Vec<&PrivateLoadedRecord>>::new();
    for record in loaded
        .iter()
        .filter(|record| record.snapshot.current_assertion)
    {
        groups
            .entry(record.duplicate_claim_digest.as_str())
            .or_default()
            .push(record);
    }
    for (_claim_digest, group) in groups.into_iter().filter(|(_, group)| group.len() > 1) {
        let record_ids = group
            .iter()
            .map(|record| record.snapshot.record_id.clone())
            .collect::<Vec<_>>();
        if !record_ids
            .iter()
            .any(|record_id| selected.contains(record_id))
        {
            continue;
        }
        let mut finding = new_finding(
            MaintenanceFindingKind::ExactDuplicate,
            record_ids.clone(),
            private_finding_comparison_digest(&group)?,
            vec![MaintenanceEvidence {
                code: "exact_claim_projection".to_owned(),
                digest: private_artifact_digest("exact_claim_projection", &group)?,
                boundary: None,
            }],
            MaintenanceConfidence::Exact,
        )?;
        let action = new_private_action(
            MaintenanceActionClass::OwnerConsolidateExactDuplicates,
            &finding.finding_id,
            record_ids,
            None,
            None,
            comparison_set_digest,
            &group,
        )?;
        finding.proposed_action_ids.push(action.action_id.clone());
        push_finding(findings, finding)?;
        push_action(actions, action)?;
    }
    Ok(())
}

fn detect_private_contradictions(
    loaded: &[PrivateLoadedRecord],
    selected: &BTreeSet<String>,
    comparison_set_digest: &str,
    findings: &mut Vec<MaintenanceFinding>,
    derived_actions: &mut Vec<MaintenanceAction>,
    owner_actions: &mut Vec<MaintenanceAction>,
    automatic_maintenance_enabled: bool,
) -> Result<()> {
    let candidates = loaded
        .iter()
        .filter(|record| {
            record.snapshot.current_assertion && private_contradiction_candidate(&record.record)
        })
        .collect::<Vec<_>>();
    let graph_inputs = candidates
        .iter()
        .map(|candidate| (&candidate.record, candidate.applicability_paths.as_slice()))
        .collect::<Vec<_>>();
    let adjacency = private_contradiction_adjacency(&graph_inputs)?;
    let mut visited = vec![false; candidates.len()];
    for start in 0..candidates.len() {
        if visited[start] || adjacency[start].is_empty() {
            continue;
        }
        let mut pending = vec![start];
        let mut component = Vec::new();
        while let Some(index) = pending.pop() {
            if visited[index] {
                continue;
            }
            visited[index] = true;
            component.push(index);
            pending.extend(
                adjacency[index]
                    .iter()
                    .rev()
                    .filter(|neighbor| !visited[**neighbor]),
            );
        }
        component.sort_unstable();
        let group = component
            .iter()
            .map(|index| candidates[*index])
            .collect::<Vec<_>>();
        let record_ids = group
            .iter()
            .map(|record| record.snapshot.record_id.clone())
            .collect::<Vec<_>>();
        if !record_ids
            .iter()
            .any(|record_id| selected.contains(record_id))
        {
            continue;
        }
        let mut finding = new_finding(
            MaintenanceFindingKind::HighConfidenceContradiction,
            record_ids.clone(),
            private_finding_comparison_digest(&group)?,
            vec![MaintenanceEvidence {
                code: "allowlisted_symmetric_polarity".to_owned(),
                digest: private_artifact_digest("allowlisted_symmetric_polarity", &group)?,
                boundary: None,
            }],
            MaintenanceConfidence::High,
        )?;
        let mut conflict_edges = Vec::new();
        for &left_index in &component {
            for &right_index in adjacency[left_index].range((left_index + 1)..) {
                if component.binary_search(&right_index).is_err() {
                    continue;
                }
                let pair = [candidates[left_index], candidates[right_index]];
                conflict_edges.push(MaintenanceConflictEdge {
                    record_ids: canonical_private_conflict_record_ids(
                        &pair[0].snapshot.record_id,
                        &pair[1].snapshot.record_id,
                    ),
                    evidence_digest: private_artifact_digest(
                        "allowlisted_symmetric_polarity_edge",
                        &pair,
                    )?,
                    reason_code: "high_confidence_unresolved_contradiction".to_owned(),
                });
            }
        }
        conflict_edges.sort();
        ensure!(
            !conflict_edges.is_empty(),
            "private contradiction component has no incompatibility edge"
        );
        finding.conflict_edges = conflict_edges;
        finding.finding_id = maintenance_finding_id(&finding)?;
        let owner_action = new_private_action(
            MaintenanceActionClass::OwnerResolveContradiction,
            &finding.finding_id,
            record_ids.clone(),
            None,
            None,
            comparison_set_digest,
            &group,
        )?;
        finding
            .proposed_action_ids
            .push(owner_action.action_id.clone());
        if automatic_maintenance_enabled {
            let suppression = new_private_action(
                MaintenanceActionClass::SuppressUnresolvedConflict,
                &finding.finding_id,
                record_ids,
                None,
                None,
                comparison_set_digest,
                &group,
            )?;
            finding
                .proposed_action_ids
                .push(suppression.action_id.clone());
            push_action(derived_actions, suppression)?;
        }
        finding.proposed_action_ids.sort();
        push_finding(findings, finding)?;
        push_action(owner_actions, owner_action)?;
    }
    Ok(())
}

fn detect_private_staleness(
    loaded: &[PrivateLoadedRecord],
    selected: &BTreeSet<String>,
    evaluated_at: OffsetDateTime,
    findings: &mut Vec<MaintenanceFinding>,
) -> Result<()> {
    for record in loaded.iter().filter(|record| {
        record.snapshot.current_assertion
            && record.record.status == MemoryStatus::Active
            && matches!(
                record.record.lane,
                MemoryLane::Semantic | MemoryLane::Procedural
            )
            && selected.contains(&record.snapshot.record_id)
    }) {
        let stale_at = record
            .freshness
            .checked_add(Duration::days(STALE_AFTER_DAYS))
            .context("maintenance freshness cannot represent the staleness boundary")?;
        if evaluated_at < stale_at {
            continue;
        }
        push_finding(
            findings,
            new_finding(
                MaintenanceFindingKind::Stale,
                vec![record.snapshot.record_id.clone()],
                private_finding_comparison_digest(&[record])?,
                vec![MaintenanceEvidence {
                    code: "freshness_age_threshold".to_owned(),
                    digest: identity(
                        "memzoi/maintenance/freshness",
                        &format_timestamp(record.freshness)?,
                    )?,
                    boundary: Some(format_timestamp(stale_at)?),
                }],
                MaintenanceConfidence::ReportOnly,
            )?,
        )?;
    }
    Ok(())
}

fn detect_private_expiry(
    loaded: &[PrivateLoadedRecord],
    selected: &BTreeSet<String>,
    findings: &mut Vec<MaintenanceFinding>,
) -> Result<()> {
    for record in loaded.iter().filter(|record| {
        record.eligible_at_evaluation
            && (record.record.status == MemoryStatus::Expired
                || (record.record.status == MemoryStatus::Active
                    && record.snapshot.retention_state == RetentionState::QueryOnly))
            && selected.contains(&record.snapshot.record_id)
    }) {
        let mut evidence = Vec::new();
        if record.record.status == MemoryStatus::Expired {
            evidence.push(MaintenanceEvidence {
                code: "lifecycle_expired".to_owned(),
                digest: record.snapshot.temporal_digest.clone(),
                boundary: None,
            });
        }
        if record.snapshot.retention_state == RetentionState::QueryOnly {
            evidence.push(MaintenanceEvidence {
                code: format!("retention_{:?}", record.snapshot.retention_reason).to_lowercase(),
                digest: record.snapshot.temporal_digest.clone(),
                boundary: record.snapshot.retention_boundary.clone(),
            });
        }
        push_finding(
            findings,
            new_finding(
                MaintenanceFindingKind::Expired,
                vec![record.snapshot.record_id.clone()],
                private_finding_comparison_digest(&[record])?,
                evidence,
                MaintenanceConfidence::Exact,
            )?,
        )?;
    }
    Ok(())
}

fn detect_private_renewals(
    loaded: &[PrivateLoadedRecord],
    selected: &BTreeSet<String>,
    evaluated_at: OffsetDateTime,
    comparison_set_digest: &str,
    findings: &mut Vec<MaintenanceFinding>,
    actions: &mut Vec<MaintenanceAction>,
) -> Result<()> {
    let mut pair_comparisons = 0_usize;
    for predecessor in loaded.iter().filter(|record| {
        private_renewal_eligible(record)
            && matches!(
                record.record.status,
                MemoryStatus::Active | MemoryStatus::Expired
            )
            && record.snapshot.retention_state == RetentionState::QueryOnly
            && record.snapshot.retention_boundary.is_some()
    }) {
        let boundary = parse_timestamp(
            predecessor
                .snapshot
                .retention_boundary
                .as_deref()
                .expect("renewal predecessor boundary checked"),
            "renewal predecessor boundary",
        )?;
        for evidence_record in loaded.iter().filter(|record| {
            private_renewal_eligible(record)
                && record.snapshot.current_assertion
                && record.renewal_claim_digest == predecessor.renewal_claim_digest
                && record.snapshot.record_id != predecessor.snapshot.record_id
        }) {
            consume_pair_work(&mut pair_comparisons, None)?;
            if !selected.contains(&predecessor.snapshot.record_id)
                && !selected.contains(&evidence_record.snapshot.record_id)
            {
                continue;
            }
            if evidence_record.record.origin == predecessor.record.origin
                || evidence_record
                    .record
                    .lineage
                    .as_ref()
                    .is_some_and(|lineage| {
                        lineage.kind == RecordLineageKind::Renewal
                            && lineage.predecessor_id == predecessor.snapshot.record_id
                    })
            {
                continue;
            }
            let Some(capture) = evidence_record.record.capture.as_ref() else {
                continue;
            };
            if !matches!(
                capture.review_outcome,
                CaptureReviewOutcome::Accept | CaptureReviewOutcome::Edit
            ) {
                continue;
            }
            let reviewed_at = parse_timestamp(&capture.reviewed_at, "renewal reviewed_at")?;
            if reviewed_at <= boundary || reviewed_at > evaluated_at {
                continue;
            }
            let private_evidence_digest =
                identity("memzoi/maintenance/capture-evidence", &capture.evidence)?;
            if predecessor.record.capture.as_ref().is_some_and(|prior| {
                identity("memzoi/maintenance/capture-evidence", &prior.evidence)
                    .is_ok_and(|digest| digest == private_evidence_digest)
            }) {
                continue;
            }
            let mut record_ids = vec![
                predecessor.snapshot.record_id.clone(),
                evidence_record.snapshot.record_id.clone(),
            ];
            record_ids.sort();
            let group = vec![predecessor, evidence_record];
            let mut finding = new_finding(
                MaintenanceFindingKind::RenewalCandidate,
                record_ids.clone(),
                private_finding_comparison_digest(&group)?,
                vec![MaintenanceEvidence {
                    code: "fresh_accepted_capture_evidence".to_owned(),
                    digest: private_artifact_digest("fresh_accepted_capture_evidence", &group)?,
                    boundary: Some(format_timestamp(boundary)?),
                }],
                MaintenanceConfidence::High,
            )?;
            let action = new_private_action(
                MaintenanceActionClass::OwnerCreateRenewalSuccessor,
                &finding.finding_id,
                record_ids,
                Some(predecessor.snapshot.record_id.clone()),
                Some(evidence_record.snapshot.record_id.clone()),
                comparison_set_digest,
                &group,
            )?;
            finding.proposed_action_ids.push(action.action_id.clone());
            push_finding(findings, finding)?;
            push_action(actions, action)?;
        }
    }
    Ok(())
}

fn new_private_action(
    class: MaintenanceActionClass,
    finding_id: &str,
    mut record_ids: Vec<String>,
    predecessor_record_id: Option<String>,
    evidence_record_id: Option<String>,
    comparison_set_digest: &str,
    records: &[&PrivateLoadedRecord],
) -> Result<MaintenanceAction> {
    record_ids.sort();
    record_ids.dedup();
    let preconditions = MaintenanceActionPreconditions {
        comparison_set_digest: comparison_set_digest.to_owned(),
        record_versions: records
            .iter()
            .map(|record| {
                (
                    record.snapshot.record_id.clone(),
                    record.snapshot.version.clone(),
                )
            })
            .collect(),
    };
    let mut action = MaintenanceAction {
        action_id: String::new(),
        class,
        finding_id: finding_id.to_owned(),
        record_ids,
        keeper_record_id: None,
        predecessor_record_id,
        evidence_record_id,
        preconditions,
    };
    action.action_id = maintenance_action_id(&action)?;
    Ok(action)
}

fn private_finding_comparison_digest(records: &[&PrivateLoadedRecord]) -> Result<String> {
    let mut values = records
        .iter()
        .map(|record| (record.snapshot.record_id.as_str(), &record.snapshot.version))
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.0.cmp(right.0));
    identity("memzoi/maintenance/finding-comparison-set", &values)
}

fn private_artifact_digest(label: &str, records: &[&PrivateLoadedRecord]) -> Result<String> {
    let mut values = records
        .iter()
        .map(|record| (record.snapshot.record_id.as_str(), &record.snapshot.version))
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.0.cmp(right.0));
    identity(
        "memzoi/maintenance/private-artifact-evidence",
        &(label, values),
    )
}

fn private_renewal_eligible(record: &PrivateLoadedRecord) -> bool {
    record.eligible_at_evaluation
        && matches!(
            record.record.lane,
            MemoryLane::Semantic | MemoryLane::Procedural
        )
        && claim_bearing_type(record.record.memory_type)
}

pub(crate) fn private_contradiction_candidate(record: &MemoryRecord) -> bool {
    matches!(record.lane, MemoryLane::Semantic | MemoryLane::Procedural)
        && claim_bearing_type(record.memory_type)
        && !contains_conditional_or_temporal_language(&record.body)
        && polarity_signature(&record.body).is_some()
}

pub(crate) fn private_contradiction_adjacency(
    candidates: &[(&MemoryRecord, &[String])],
) -> Result<Vec<BTreeSet<usize>>> {
    let mut adjacency = vec![BTreeSet::<usize>::new(); candidates.len()];
    let mut pair_comparisons = 0_usize;
    for left_index in 0..candidates.len() {
        for right_index in (left_index + 1)..candidates.len() {
            consume_pair_work(&mut pair_comparisons, None)?;
            let (left, left_paths) = candidates[left_index];
            let (right, right_paths) = candidates[right_index];
            let Some((left_signature, left_negative)) = polarity_signature(&left.body) else {
                continue;
            };
            let Some((right_signature, right_negative)) = polarity_signature(&right.body) else {
                continue;
            };
            if left_signature != right_signature
                || left_negative == right_negative
                || !same_private_claim_context(left, right)
                || !paths_overlap(left_paths, right_paths)
                || private_records_are_temporally_related(left, right)
            {
                continue;
            }
            adjacency[left_index].insert(right_index);
            adjacency[right_index].insert(left_index);
        }
    }
    Ok(adjacency)
}

fn same_private_claim_context(left: &MemoryRecord, right: &MemoryRecord) -> bool {
    left.memory_type == right.memory_type
        && left.lane == right.lane
        && left.destination == right.destination
        && normalize_text(&left.title) == normalize_text(&right.title)
        && left.scope_kind == right.scope_kind
        && left.scope_id == right.scope_id
}

fn canonical_private_conflict_record_ids(left: &str, right: &str) -> [String; 2] {
    if left < right {
        [left.to_owned(), right.to_owned()]
    } else {
        [right.to_owned(), left.to_owned()]
    }
}

fn private_records_are_temporally_related(left: &MemoryRecord, right: &MemoryRecord) -> bool {
    left.supersedes_id.as_deref() == Some(right.id.as_str())
        || right.supersedes_id.as_deref() == Some(left.id.as_str())
        || left
            .lineage
            .as_ref()
            .is_some_and(|lineage| lineage.predecessor_id == right.id)
        || right
            .lineage
            .as_ref()
            .is_some_and(|lineage| lineage.predecessor_id == left.id)
}

fn private_duplicate_claim_digest(
    record: &MemoryRecord,
    applicability_paths: &[String],
) -> Result<String> {
    identity(
        "memzoi/maintenance/duplicate-claim",
        &serde_json::json!({
            "memory_type": record.memory_type,
            "lane": record.lane,
            "destination": record.destination,
            "scope_kind": record.scope_kind,
            "scope_id": record.scope_id,
            "visibility": record.visibility,
            "title": normalize_text(&record.title),
            "body": record.body.trim(),
            "confidence": record.confidence,
            "retention": record.retention,
            "applicability_paths": applicability_paths,
        }),
    )
}

fn private_renewal_claim_digest(record: &MemoryRecord) -> Result<String> {
    identity(
        "memzoi/maintenance/renewal-claim",
        &serde_json::json!({
            "memory_type": record.memory_type,
            "lane": record.lane,
            "destination": record.destination,
            "scope_kind": record.scope_kind,
            "scope_id": record.scope_id,
            "visibility": record.visibility,
            "title": normalize_text(&record.title),
            "body": record.body.trim(),
        }),
    )
}

fn new_finding(
    kind: MaintenanceFindingKind,
    mut record_ids: Vec<String>,
    comparison_set_digest: String,
    evidence: Vec<MaintenanceEvidence>,
    confidence: MaintenanceConfidence,
) -> Result<MaintenanceFinding> {
    record_ids.sort();
    record_ids.dedup();
    let finding_id = identity(
        "memzoi/maintenance/finding",
        &serde_json::json!({
            "kind": kind,
            "record_ids": record_ids,
            "comparison_set_digest": comparison_set_digest,
            "evidence": evidence,
            "confidence": confidence,
            "conflict_edges": [],
        }),
    )?;
    Ok(MaintenanceFinding {
        finding_id,
        kind,
        record_ids,
        comparison_set_digest,
        evidence,
        confidence,
        conflict_edges: Vec::new(),
        proposed_action_ids: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn new_action(
    class: MaintenanceActionClass,
    finding_id: &str,
    mut record_ids: Vec<String>,
    keeper_record_id: Option<String>,
    predecessor_record_id: Option<String>,
    evidence_record_id: Option<String>,
    comparison_set_digest: &str,
    records: &[&LoadedRecord],
) -> Result<MaintenanceAction> {
    record_ids.sort();
    record_ids.dedup();
    let record_versions = records
        .iter()
        .map(|record| {
            (
                record.snapshot.record_id.clone(),
                record.snapshot.version.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let preconditions = MaintenanceActionPreconditions {
        comparison_set_digest: comparison_set_digest.to_owned(),
        record_versions,
    };
    let action_id = identity(
        "memzoi/maintenance/action",
        &serde_json::json!({
            "class": class,
            "finding_id": finding_id,
            "record_ids": record_ids,
            "keeper_record_id": keeper_record_id,
            "predecessor_record_id": predecessor_record_id,
            "evidence_record_id": evidence_record_id,
            "preconditions": preconditions,
        }),
    )?;
    Ok(MaintenanceAction {
        action_id,
        class,
        finding_id: finding_id.to_owned(),
        record_ids,
        keeper_record_id,
        predecessor_record_id,
        evidence_record_id,
        preconditions,
    })
}

fn summarize(
    records: &[MaintenanceRecordSnapshot],
    findings: &[MaintenanceFinding],
    action_groups: &[MaintenanceActionGroup],
) -> MaintenancePlanSummary {
    let count = |kind| {
        findings
            .iter()
            .filter(|finding| finding.kind == kind)
            .count()
    };
    MaintenancePlanSummary {
        records: records.len(),
        exact_duplicates: count(MaintenanceFindingKind::ExactDuplicate),
        contradictions: count(MaintenanceFindingKind::HighConfidenceContradiction),
        stale: count(MaintenanceFindingKind::Stale),
        expired: count(MaintenanceFindingKind::Expired),
        renewal_candidates: count(MaintenanceFindingKind::RenewalCandidate),
        action_candidates: action_groups.iter().map(|group| group.actions.len()).sum(),
    }
}

fn contradiction_eligible(record: &LoadedRecord) -> bool {
    record.snapshot.current_assertion
        && matches!(
            record.record.draft.lane,
            MemoryLane::Semantic | MemoryLane::Procedural
        )
        && claim_bearing_type(record.record.draft.memory_type)
        && !contains_conditional_or_temporal_language(&record.record.draft.body)
}

fn renewal_eligible(record: &LoadedRecord) -> bool {
    record.eligible_at_evaluation
        && matches!(
            record.record.draft.lane,
            MemoryLane::Semantic | MemoryLane::Procedural
        )
        && claim_bearing_type(record.record.draft.memory_type)
}

fn claim_bearing_type(memory_type: MemoryType) -> bool {
    matches!(
        memory_type,
        MemoryType::Fact
            | MemoryType::Preference
            | MemoryType::Decision
            | MemoryType::Procedure
            | MemoryType::InstructionProjection
    )
}

fn same_claim_context(left: &OkfRecordFile, right: &OkfRecordFile) -> bool {
    left.draft.memory_type == right.draft.memory_type
        && left.draft.lane == right.draft.lane
        && normalize_text(&left.draft.title) == normalize_text(&right.draft.title)
        && left.draft.scope_kind == right.draft.scope_kind
        && left.draft.scope_id == right.draft.scope_id
}

fn records_are_temporally_related(left: &OkfRecordFile, right: &OkfRecordFile) -> bool {
    left.supersedes_id.as_deref() == Some(right.concept_id.as_str())
        || right.supersedes_id.as_deref() == Some(left.concept_id.as_str())
        || left
            .lineage
            .as_ref()
            .is_some_and(|lineage| lineage.predecessor_id == right.concept_id)
        || right
            .lineage
            .as_ref()
            .is_some_and(|lineage| lineage.predecessor_id == left.concept_id)
}

fn paths_overlap(left: &[String], right: &[String]) -> bool {
    left.is_empty()
        || right.is_empty()
        || left.iter().any(|left_path| {
            right.iter().any(|right_path| {
                crate::search::path_matches_request(left_path, right_path)
                    || crate::search::path_matches_request(right_path, left_path)
            })
        })
}

fn polarity_signature(body: &str) -> Option<(String, bool)> {
    let mut tokens = normalized_tokens(body);
    if tokens.is_empty()
        || tokens
            .iter()
            .any(|token| token == "not" && tokens.len() == 1)
    {
        return None;
    }
    let boolean_positions = tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.as_str() == "true" || token.as_str() == "false")
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if boolean_positions.len() == 1 {
        let index = boolean_positions[0];
        let terminal_predicate = index + 1 == tokens.len()
            && index > 0
            && (matches!(tokens[index - 1].as_str(), "is" | "are")
                || (tokens[index - 1] == "be"
                    && index > 1
                    && matches!(tokens[index - 2].as_str(), "must" | "should")));
        if !terminal_predicate {
            return None;
        }
        let negative = tokens[index] == "false";
        tokens[index] = "<boolean>".to_owned();
        return Some((tokens.join(" "), negative));
    }
    if !boolean_positions.is_empty() {
        return None;
    }
    let not_positions = tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.as_str() == "not")
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if not_positions.len() > 1 {
        return None;
    }
    if let Some(index) = not_positions.first().copied() {
        if index == 0 || !matches!(tokens[index - 1].as_str(), "is" | "are" | "must" | "should") {
            return None;
        }
        tokens.remove(index);
        return Some((tokens.join(" "), true));
    }
    tokens
        .iter()
        .any(|token| matches!(token.as_str(), "is" | "are" | "must" | "should"))
        .then(|| (tokens.join(" "), false))
}

fn contains_conditional_or_temporal_language(body: &str) -> bool {
    let tokens = normalized_tokens(body);
    const BLOCKED: &[&str] = &[
        "if",
        "when",
        "whenever",
        "unless",
        "provided",
        "providing",
        "assuming",
        "depending",
        "where",
        "wherever",
        "while",
        "except",
        "otherwise",
        "until",
        "before",
        "after",
        "since",
        "once",
        "now",
        "today",
        "tomorrow",
        "yesterday",
        "formerly",
        "current",
        "currently",
        "previously",
        "temporarily",
        "during",
    ];
    tokens.iter().any(|token| {
        BLOCKED.contains(&token.as_str())
            || (token.len() == 4 && token.bytes().all(|byte| byte.is_ascii_digit()))
    }) || tokens
        .windows(2)
        .any(|window| window[0] == "as" && window[1] == "of")
}

fn normalized_tokens(value: &str) -> Vec<String> {
    value
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

fn normalize_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn duplicate_claim_digest(record: &OkfRecordFile) -> Result<String> {
    identity(
        "memzoi/maintenance/duplicate-claim",
        &serde_json::json!({
            "memory_type": record.draft.memory_type,
            "lane": record.draft.lane,
            "scope_kind": record.draft.scope_kind,
            "scope_id": record.draft.scope_id,
            "visibility": record.draft.visibility,
            "title": normalize_text(&record.draft.title),
            "body": record.draft.body.trim(),
            "tags": sorted_strings(&record.draft.tags),
            "sensitivity": record.draft.sensitivity,
            "content_class": record.draft.content_class,
            "confidence": record.draft.confidence,
            "applies_to": sorted_strings(&record.applies_to),
            "retention": record.retention,
        }),
    )
}

fn renewal_claim_digest(record: &OkfRecordFile) -> Result<String> {
    identity(
        "memzoi/maintenance/renewal-claim",
        &serde_json::json!({
            "memory_type": record.draft.memory_type,
            "lane": record.draft.lane,
            "scope_kind": record.draft.scope_kind,
            "scope_id": record.draft.scope_id,
            "visibility": record.draft.visibility,
            "title": normalize_text(&record.draft.title),
            "body": record.draft.body.trim(),
            "tags": sorted_strings(&record.draft.tags),
            "sensitivity": record.draft.sensitivity,
            "content_class": record.draft.content_class,
            "confidence": record.draft.confidence,
            "applies_to": sorted_strings(&record.applies_to),
        }),
    )
}

fn load_stable_repository_snapshot(
    paths: &MemoryPaths,
    control: Option<&MaintenancePlanningControl>,
) -> Result<Vec<crate::okf::OkfRecordSnapshot>> {
    check_control(control)?;
    let records_root = paths.records_dir();
    if records_root.exists() {
        crate::service::ensure_repository_records_root_safe(paths)?;
    }
    let first = read_bounded_repository_snapshot(&records_root, control)?;
    check_control(control)?;
    let second = read_bounded_repository_snapshot(&records_root, control)?;
    for snapshot in &second {
        check_control(control)?;
        crate::service::admit_repository_record_snapshot(paths, snapshot)?;
    }
    let first_state = first
        .iter()
        .map(|snapshot| (&snapshot.path, &snapshot.bytes))
        .collect::<Vec<_>>();
    let second_state = second
        .iter()
        .map(|snapshot| (&snapshot.path, &snapshot.bytes))
        .collect::<Vec<_>>();
    if first_state != second_state {
        bail!("canonical repository memory changed during maintenance planning");
    }
    Ok(second)
}

fn read_bounded_repository_snapshot(
    records_root: &Path,
    control: Option<&MaintenancePlanningControl>,
) -> Result<Vec<crate::okf::OkfRecordSnapshot>> {
    if !records_root.exists() {
        return Ok(Vec::new());
    }
    let root_metadata = fs::symlink_metadata(records_root)
        .context("failed to inspect canonical repository memory root")?;
    ensure!(
        root_metadata.file_type().is_dir() && !root_metadata.file_type().is_symlink(),
        "canonical repository memory root must be a real directory"
    );

    let mut pending = vec![(records_root.to_path_buf(), 0_usize)];
    let mut files = Vec::new();
    let mut inventory_entries = 0_usize;
    while let Some((directory, depth)) = pending.pop() {
        check_control(control)?;
        let entries = fs::read_dir(&directory)
            .with_context(|| format!("failed to scan {}", directory.display()))?;
        for entry in entries {
            check_control(control)?;
            let entry = entry?;
            inventory_entries = inventory_entries
                .checked_add(1)
                .context("maintenance snapshot inventory count overflowed")?;
            if inventory_entries > MAINTENANCE_MAX_INVENTORY_ENTRIES {
                bail!("canonical repository memory exceeds the maintenance inventory limit");
            }
            let path = entry.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.') && name != ".")
            {
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                let next_depth = depth
                    .checked_add(1)
                    .context("maintenance snapshot directory depth overflowed")?;
                if next_depth > MAINTENANCE_MAX_DIRECTORY_DEPTH {
                    bail!("canonical repository memory exceeds the maintenance depth limit");
                }
                pending.push((path, next_depth));
                continue;
            }
            if !file_type.is_file()
                || path.extension().and_then(|extension| extension.to_str()) != Some("md")
                || matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some("index.md" | "log.md")
                )
            {
                continue;
            }
            files.push(path);
            if files.len() > MAINTENANCE_MAX_ADMITTED_RECORDS {
                bail!("canonical repository memory exceeds the maintenance admitted-record limit");
            }
        }
    }
    files.sort();

    let mut snapshots = Vec::with_capacity(files.len());
    let mut total_bytes = 0_usize;
    for path in files {
        check_control(control)?;
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        ensure!(
            metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
            "canonical maintenance source is not a regular file"
        );
        let metadata_len = usize::try_from(metadata.len())
            .context("canonical maintenance source is too large for this platform")?;
        if metadata_len > MAINTENANCE_MAX_INPUT_FILE_BYTES {
            bail!("canonical repository record exceeds the maintenance file limit");
        }
        let remaining = MAINTENANCE_MAX_INPUT_BYTES
            .checked_sub(total_bytes)
            .context("canonical repository memory exceeds the maintenance input limit")?;
        let read_limit = remaining
            .min(MAINTENANCE_MAX_INPUT_FILE_BYTES)
            .checked_add(1)
            .context("maintenance input read limit overflowed")?;
        let mut file =
            fs::File::open(&path).with_context(|| format!("failed to open {}", path.display()))?;
        let mut bytes = Vec::with_capacity(metadata_len.min(read_limit));
        file.by_ref()
            .take(read_limit as u64)
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if bytes.len() > MAINTENANCE_MAX_INPUT_FILE_BYTES {
            bail!("canonical repository record exceeds the maintenance file limit");
        }
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .context("canonical maintenance snapshot byte count overflowed")?;
        if total_bytes > MAINTENANCE_MAX_INPUT_BYTES {
            bail!("canonical repository memory exceeds the maintenance input limit");
        }
        let markdown = std::str::from_utf8(&bytes)
            .with_context(|| format!("OKF record {} is not UTF-8", path.display()))?;
        if let Some(record) = crate::parse_okf_record_markdown(records_root, &path, markdown)? {
            snapshots.push(crate::okf::OkfRecordSnapshot {
                path,
                bytes,
                record,
            });
        }
    }
    snapshots.sort_by(|left, right| left.record.concept_id.cmp(&right.record.concept_id));
    ensure!(
        snapshots.len() <= MAINTENANCE_MAX_ADMITTED_RECORDS,
        "canonical repository memory exceeds the maintenance admitted-record limit"
    );
    Ok(snapshots)
}

fn validate_repository_record(record: &OkfRecordFile) -> Result<()> {
    ensure!(
        record.draft.scope_kind == ScopeKind::Repo,
        "repository maintenance refuses a non-repository scope"
    );
    ensure!(
        record.draft.visibility == Visibility::Repo,
        "repository maintenance refuses non-repository visibility"
    );
    ensure!(
        record.draft.content_class == RepositoryContentClass::GeneralRepoKnowledge,
        "repository maintenance refuses unclassified repository content"
    );
    Ok(())
}

fn record_freshness(record: &OkfRecordFile) -> Result<OffsetDateTime> {
    let mut freshness = parse_timestamp(
        record.updated.as_deref().unwrap_or(&record.created),
        "maintenance record freshness",
    )?;
    if let Some(capture) = record.capture.as_ref().filter(|capture| {
        matches!(
            capture.review_outcome,
            CaptureReviewOutcome::Accept | CaptureReviewOutcome::Edit
        )
    }) {
        let reviewed_at = parse_timestamp(&capture.reviewed_at, "capture reviewed_at")?;
        if reviewed_at > freshness {
            freshness = reviewed_at;
        }
    }
    Ok(freshness)
}

fn maintenance_policy() -> Result<MaintenancePolicySnapshot> {
    let mut policy = MaintenancePolicySnapshot {
        policy_version: MAINTENANCE_POLICY_VERSION.to_owned(),
        maximum_validity_seconds: (MAX_VALIDITY_HOURS * 60 * 60) as u64,
        stale_after_seconds: (STALE_AFTER_DAYS * 24 * 60 * 60) as u64,
        policy_digest: String::new(),
    };
    policy.policy_digest = maintenance_policy_digest(&policy)?;
    Ok(policy)
}

pub(crate) fn validate_current_maintenance_snapshots(plan: &MaintenancePlan) -> Result<()> {
    let current_policy = maintenance_policy()?;
    ensure!(
        plan.policy == current_policy
            && plan.preconditions.policy_digest == current_policy.policy_digest,
        "maintenance plan does not use the current policy snapshot"
    );
    let current_detectors = maintenance_detectors()?;
    let current_detector_digest = identity("memzoi/maintenance/detectors", &current_detectors)?;
    ensure!(
        plan.detectors == current_detectors
            && plan.preconditions.detector_digest == current_detector_digest,
        "maintenance plan does not use the current detector snapshot"
    );
    let current_grant = identity("memzoi/maintenance/grant", &"report-only")?;
    ensure!(
        plan.authority.mode == MaintenanceAuthorityMode::ReportOnly
            && plan.authority.grant_fingerprint == current_grant
            && plan.preconditions.grant_fingerprint == current_grant,
        "maintenance plan does not use the current report-only grant snapshot"
    );
    Ok(())
}

fn maintenance_policy_digest(policy: &MaintenancePolicySnapshot) -> Result<String> {
    identity(
        "memzoi/maintenance/policy",
        &serde_json::json!({
            "policy_version": policy.policy_version,
            "maximum_validity_seconds": policy.maximum_validity_seconds,
            "stale_after_seconds": policy.stale_after_seconds,
        }),
    )
}

fn maintenance_detectors() -> Result<Vec<MaintenanceDetectorSnapshot>> {
    let versions = [
        (
            MaintenanceFindingKind::ExactDuplicate,
            DUPLICATE_DETECTOR_VERSION,
        ),
        (
            MaintenanceFindingKind::HighConfidenceContradiction,
            CONTRADICTION_DETECTOR_VERSION,
        ),
        (MaintenanceFindingKind::Stale, STALENESS_DETECTOR_VERSION),
        (MaintenanceFindingKind::Expired, EXPIRY_DETECTOR_VERSION),
        (
            MaintenanceFindingKind::RenewalCandidate,
            RENEWAL_DETECTOR_VERSION,
        ),
    ];
    versions
        .into_iter()
        .map(|(kind, version)| {
            Ok(MaintenanceDetectorSnapshot {
                kind,
                version: version.to_owned(),
                configuration_digest: identity(
                    "memzoi/maintenance/detector",
                    &serde_json::json!({"kind": kind, "version": version}),
                )?,
            })
        })
        .collect()
}

pub(crate) fn current_maintenance_detector_digest() -> Result<String> {
    identity("memzoi/maintenance/detectors", &maintenance_detectors()?)
}

fn comparison_digest(records: &[MaintenanceRecordSnapshot]) -> Result<String> {
    identity(
        "memzoi/maintenance/comparison-set",
        &records
            .iter()
            .map(|record| {
                (
                    &record.record_id,
                    &record.version,
                    &record.claim_digest,
                    &record.applicability_digest,
                    &record.temporal_digest,
                )
            })
            .collect::<Vec<_>>(),
    )
}

fn finding_comparison_digest(records: &[&LoadedRecord]) -> Result<String> {
    let mut values = records
        .iter()
        .map(|record| (record.snapshot.record_id.as_str(), &record.snapshot.version))
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.0.cmp(right.0));
    identity("memzoi/maintenance/finding-comparison-set", &values)
}

fn finding_snapshot_comparison_digest(
    records: &[MaintenanceRecordSnapshot],
    record_ids: &[String],
) -> Result<String> {
    let values = record_ids
        .iter()
        .map(|record_id| {
            let record = records
                .binary_search_by_key(&record_id.as_str(), |record| record.record_id.as_str())
                .ok()
                .map(|index| &records[index])
                .with_context(|| {
                    format!("maintenance finding references unknown record {record_id}")
                })?;
            Ok((record.record_id.as_str(), &record.version))
        })
        .collect::<Result<Vec<_>>>()?;
    identity("memzoi/maintenance/finding-comparison-set", &values)
}

fn sorted_strings(values: &[String]) -> Vec<String> {
    let mut values = values.to_vec();
    values.sort();
    values.dedup();
    values
}

fn normalize_record_ids(record_ids: Vec<String>) -> Result<Vec<String>> {
    let mut normalized = Vec::with_capacity(record_ids.len());
    for record_id in record_ids {
        let record_id = record_id.trim();
        if record_id.is_empty() {
            bail!("maintenance record IDs must not be empty");
        }
        crate::validate_canonical_record_id(record_id)?;
        normalized.push(record_id.to_owned());
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn validate_request(request: &MaintenancePlanRequest) -> Result<()> {
    ensure!(
        request.schema == MAINTENANCE_REQUEST_SCHEMA,
        "maintenance request schema must be {MAINTENANCE_REQUEST_SCHEMA}"
    );
    if let Some(evaluated_at) = request.evaluated_at.as_deref() {
        parse_timestamp(evaluated_at, "maintenance evaluated_at")?;
    }
    let normalized_record_ids = normalize_record_ids(request.record_ids.clone())?;
    if normalized_record_ids.len() > MAINTENANCE_MAX_ADMITTED_RECORDS {
        bail!("maintenance request exceeds the admitted-record selector limit");
    }
    Ok(())
}

fn check_control(control: Option<&MaintenancePlanningControl>) -> Result<()> {
    if let Some(control) = control {
        control.check()?;
    }
    Ok(())
}

fn consume_pair_work(
    comparisons: &mut usize,
    control: Option<&MaintenancePlanningControl>,
) -> Result<()> {
    check_control(control)?;
    *comparisons = comparisons
        .checked_add(1)
        .context("maintenance pair-comparison count overflowed")?;
    if *comparisons > MAINTENANCE_MAX_PAIR_COMPARISONS {
        bail!("maintenance planning exceeds the pair-comparison work limit");
    }
    Ok(())
}

fn push_finding(findings: &mut Vec<MaintenanceFinding>, finding: MaintenanceFinding) -> Result<()> {
    if findings.len() >= MAINTENANCE_MAX_FINDINGS {
        bail!("maintenance planning exceeds the finding output limit");
    }
    findings.push(finding);
    Ok(())
}

fn push_action(actions: &mut Vec<MaintenanceAction>, action: MaintenanceAction) -> Result<()> {
    if actions.len() >= MAINTENANCE_MAX_ACTION_CANDIDATES {
        bail!("maintenance planning exceeds the action-candidate output limit");
    }
    actions.push(action);
    Ok(())
}

fn parse_timestamp(value: &str, label: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(value.trim(), &Rfc3339)
        .with_context(|| format!("{label} must be an RFC 3339 timestamp"))
}

fn parse_canonical_timestamp(value: &str, label: &str) -> Result<OffsetDateTime> {
    let parsed = parse_timestamp(value, label)?;
    ensure!(
        format_timestamp(parsed)? == value,
        "{label} must use canonical UTC RFC 3339 form"
    );
    Ok(parsed)
}

fn validate_private_record_version_token(token: &str) -> Result<()> {
    ensure!(
        (32..=64).contains(&token.len())
            && token.bytes().all(|byte| {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || byte == b'-'
            })
            && token.bytes().any(|byte| byte != b'-'),
        "maintenance private-runtime version token is invalid"
    );
    Ok(())
}

fn format_timestamp(value: OffsetDateTime) -> Result<String> {
    value
        .to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .context("failed to format maintenance timestamp")
}

fn identity(domain: &str, value: &impl Serialize) -> Result<String> {
    let canonical = serde_json_canonicalizer::to_vec(value)
        .context("failed to canonicalize maintenance identity input")?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_bytes());
    hasher.update(&[0]);
    hasher.update(&canonical);
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

pub fn parse_maintenance_plan(input: &str) -> Result<MaintenancePlan> {
    if input.len() > MAINTENANCE_MAX_SERIALIZED_PLAN_BYTES {
        bail!("maintenance plan exceeds the supported size limit");
    }
    let plan: MaintenancePlan =
        serde_json::from_str(input).context("invalid memzoi/maintenance-plan artifact")?;
    plan.validate()?;
    ensure_plan_size(&plan)?;
    Ok(plan)
}

pub fn revalidate_maintenance_plan(
    paths: &MemoryPaths,
    plan: &MaintenancePlan,
) -> Result<MaintenanceRevalidation> {
    revalidate_maintenance_plan_at(paths, plan, OffsetDateTime::now_utc())
}

pub fn revalidate_maintenance_plan_at(
    paths: &MemoryPaths,
    plan: &MaintenancePlan,
    now: OffsetDateTime,
) -> Result<MaintenanceRevalidation> {
    plan.validate()?;
    let repository_fingerprint = match &plan.scope {
        MaintenanceScope::Repository {
            repository_fingerprint,
            ..
        } => repository_fingerprint,
        MaintenanceScope::PrivateRuntime { .. } => {
            bail!("repository maintenance revalidation requires a repository-scoped plan")
        }
    };
    let precondition_repository_fingerprint = match &plan.preconditions.scope {
        MaintenanceScopeBinding::Repository {
            repository_fingerprint,
        } => repository_fingerprint,
        MaintenanceScopeBinding::PrivateRuntime { .. } => {
            bail!("repository maintenance revalidation requires repository preconditions")
        }
    };
    let evaluated_at = parse_timestamp(&plan.evaluated_at, "maintenance evaluated_at")?;
    let not_after = parse_timestamp(&plan.not_after, "maintenance not_after")?;
    let mut reasons = BTreeSet::new();
    if now < evaluated_at {
        reasons.insert(MaintenanceStaleReason::NotYetValid);
    }
    if now >= not_after {
        reasons.insert(MaintenanceStaleReason::PlanExpired);
    }

    let current_policy = maintenance_policy()?;
    if plan.policy != current_policy
        || plan.preconditions.policy_digest != current_policy.policy_digest
    {
        reasons.insert(MaintenanceStaleReason::PolicyChanged);
    }
    let current_detectors = maintenance_detectors()?;
    let current_detector_digest = identity("memzoi/maintenance/detectors", &current_detectors)?;
    if plan.detectors != current_detectors
        || plan.preconditions.detector_digest != current_detector_digest
    {
        reasons.insert(MaintenanceStaleReason::DetectorChanged);
    }
    let current_grant = identity("memzoi/maintenance/grant", &"report-only")?;
    if plan.authority.mode != MaintenanceAuthorityMode::ReportOnly
        || plan.authority.grant_fingerprint != current_grant
        || plan.preconditions.grant_fingerprint != current_grant
    {
        reasons.insert(MaintenanceStaleReason::GrantChanged);
    }
    let current_repository = identity("memzoi/maintenance/repository", &paths.repository_key())?;
    if repository_fingerprint != &current_repository
        || precondition_repository_fingerprint != &current_repository
    {
        reasons.insert(MaintenanceStaleReason::RepositoryChanged);
    }

    let mut request = plan.request.clone();
    request.evaluated_at = Some(plan.evaluated_at.clone());
    match plan_maintenance_at(paths, request, evaluated_at) {
        Ok(current) => {
            let prior_versions = plan
                .records
                .iter()
                .map(|record| (record.record_id.as_str(), &record.version))
                .collect::<BTreeMap<_, _>>();
            let current_versions = current
                .records
                .iter()
                .map(|record| (record.record_id.as_str(), &record.version))
                .collect::<BTreeMap<_, _>>();
            let target_ids = plan
                .scope
                .target_record_ids()
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            if target_ids.iter().any(|record_id| {
                prior_versions.get(record_id).copied() != current_versions.get(record_id).copied()
            }) {
                reasons.insert(MaintenanceStaleReason::TargetChanged);
            }
            let prior_neighborhood = prior_versions
                .iter()
                .filter(|(record_id, _)| !target_ids.contains(**record_id))
                .collect::<BTreeMap<_, _>>();
            let current_neighborhood = current_versions
                .iter()
                .filter(|(record_id, _)| !target_ids.contains(**record_id))
                .collect::<BTreeMap<_, _>>();
            if prior_neighborhood != current_neighborhood
                || (plan.comparison_set_digest != current.comparison_set_digest
                    && !reasons.contains(&MaintenanceStaleReason::TargetChanged))
            {
                reasons.insert(MaintenanceStaleReason::ComparisonNeighborhoodChanged);
            }
            if plan.not_after != current.not_after
                || plan.preconditions.not_after != current.preconditions.not_after
            {
                reasons.insert(MaintenanceStaleReason::TemporalBoundaryChanged);
            }
            if plan.plan_id != current.plan_id
                && !reasons.iter().any(|reason| {
                    matches!(
                        reason,
                        MaintenanceStaleReason::TargetChanged
                            | MaintenanceStaleReason::ComparisonNeighborhoodChanged
                            | MaintenanceStaleReason::DetectorChanged
                            | MaintenanceStaleReason::PolicyChanged
                            | MaintenanceStaleReason::GrantChanged
                            | MaintenanceStaleReason::RepositoryChanged
                            | MaintenanceStaleReason::TemporalBoundaryChanged
                    )
                })
            {
                reasons.insert(MaintenanceStaleReason::PlanChanged);
            }
        }
        Err(error) => {
            if error.downcast_ref::<MissingMaintenanceTarget>().is_some() {
                reasons.insert(MaintenanceStaleReason::TargetChanged);
            } else {
                reasons.insert(MaintenanceStaleReason::SourceChanged);
            }
        }
    }

    if reasons.is_empty() {
        Ok(MaintenanceRevalidation::Current {
            plan_id: plan.plan_id.clone(),
        })
    } else {
        Ok(MaintenanceRevalidation::Stale {
            plan_id: plan.plan_id.clone(),
            reasons: reasons.into_iter().collect(),
        })
    }
}

impl MaintenancePlan {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == MAINTENANCE_PLAN_SCHEMA,
            "maintenance plan schema must be {MAINTENANCE_PLAN_SCHEMA}"
        );
        ensure!(
            self.policy.policy_version == MAINTENANCE_POLICY_VERSION,
            "maintenance policy snapshot does not use the current policy"
        );
        validate_request(&self.request)?;
        ensure!(
            self.request.evaluated_at.as_deref() == Some(self.evaluated_at.as_str()),
            "maintenance request evaluation time does not match the plan"
        );
        let evaluated_at =
            parse_canonical_timestamp(&self.evaluated_at, "maintenance evaluated_at")?;
        let not_after = parse_canonical_timestamp(&self.not_after, "maintenance not_after")?;
        ensure!(
            not_after > evaluated_at,
            "maintenance validity window must be non-empty"
        );
        let maximum_not_after = evaluated_at
            .checked_add(Duration::hours(MAX_VALIDITY_HOURS))
            .context("maintenance evaluation time cannot represent the validity window")?;
        ensure!(
            not_after <= maximum_not_after,
            "maintenance validity exceeds the maximum policy window"
        );
        let (scope_target_record_ids, expected_scope_binding, snapshot_diagnostic_code) =
            match &self.scope {
                MaintenanceScope::Repository {
                    repository_fingerprint,
                    target_record_ids,
                } => {
                    crate::validate_materialization_identity(
                        repository_fingerprint,
                        "maintenance repository fingerprint",
                    )?;
                    (
                        target_record_ids,
                        MaintenanceScopeBinding::Repository {
                            repository_fingerprint: repository_fingerprint.clone(),
                        },
                        "repository_snapshot_evaluated",
                    )
                }
                MaintenanceScope::PrivateRuntime {
                    runtime_fingerprint,
                    target_record_ids,
                } => {
                    crate::validate_materialization_identity(
                        runtime_fingerprint,
                        "maintenance private-runtime fingerprint",
                    )?;
                    (
                        target_record_ids,
                        MaintenanceScopeBinding::PrivateRuntime {
                            runtime_fingerprint: runtime_fingerprint.clone(),
                        },
                        "private_runtime_snapshot_evaluated",
                    )
                }
            };
        crate::validate_materialization_identity(
            &self.authority.grant_fingerprint,
            "maintenance grant fingerprint",
        )?;
        ensure!(
            !matches!(self.scope, MaintenanceScope::Repository { .. })
                || self.authority.mode == MaintenanceAuthorityMode::ReportOnly,
            "repository maintenance must remain report-only"
        );
        ensure_sorted_unique(
            self.records.iter().map(|record| record.record_id.as_str()),
            "maintenance record snapshots",
        )?;
        for record in &self.records {
            crate::validate_canonical_record_id(&record.record_id)?;
            match (&self.scope, &record.version) {
                (
                    MaintenanceScope::Repository { .. },
                    MaintenanceRecordVersion::CanonicalRepository {
                        source_path,
                        revision,
                    },
                ) => {
                    revision.validate()?;
                    ensure!(
                        source_path == &format!(".memzoi/records/{}.md", record.record_id),
                        "maintenance record source path does not match its canonical identity"
                    );
                }
                (
                    MaintenanceScope::PrivateRuntime { .. },
                    MaintenanceRecordVersion::PrivateRuntime { version_token },
                ) => validate_private_record_version_token(version_token)?,
                _ => bail!("maintenance record version kind does not match the plan scope"),
            }
            for (digest, label) in [
                (&record.content_hash, "maintenance content hash"),
                (&record.claim_digest, "maintenance claim digest"),
                (
                    &record.applicability_digest,
                    "maintenance applicability digest",
                ),
                (&record.temporal_digest, "maintenance temporal digest"),
            ] {
                crate::validate_materialization_identity(digest, label)?;
            }
            if let Some(boundary) = record.retention_boundary.as_deref() {
                parse_canonical_timestamp(boundary, "maintenance retention boundary")?;
            }
            ensure!(
                !record.current_assertion
                    || (record.status == MemoryStatus::Active
                        && record.retention_state == RetentionState::Current),
                "maintenance current-assertion snapshot conflicts with lifecycle or retention"
            );
        }
        ensure!(
            self.request.record_ids == normalize_record_ids(self.request.record_ids.clone())?,
            "maintenance request selectors must use canonical sorted form"
        );
        ensure_sorted_unique(
            self.request.record_ids.iter().map(String::as_str),
            "maintenance request record IDs",
        )?;
        ensure_sorted_unique(
            scope_target_record_ids.iter().map(String::as_str),
            "maintenance target record IDs",
        )?;
        let record_targets = self
            .records
            .iter()
            .filter(|record| record.target)
            .map(|record| record.record_id.clone())
            .collect::<Vec<_>>();
        ensure!(
            record_targets == *scope_target_record_ids,
            "maintenance scope targets do not match record target markers"
        );
        let expected_request_targets = if self.request.record_ids.is_empty() {
            self.records
                .iter()
                .map(|record| record.record_id.clone())
                .collect::<Vec<_>>()
        } else {
            self.request.record_ids.clone()
        };
        ensure!(
            expected_request_targets == *scope_target_record_ids,
            "maintenance request selectors do not match plan scope targets"
        );
        ensure_sorted_unique(
            self.findings
                .iter()
                .map(|finding| finding.finding_id.as_str()),
            "maintenance findings",
        )?;
        ensure!(
            self.action_groups.len() == 3
                && self.action_groups[0].kind
                    == MaintenanceActionGroupKind::RepositoryMaterialization
                && self.action_groups[1].kind == MaintenanceActionGroupKind::PrivateDerivedState
                && self.action_groups[2].kind
                    == MaintenanceActionGroupKind::OwnerAuthorizedPrivateMutation,
            "maintenance action groups must preserve the three execution domains"
        );
        for group in &self.action_groups {
            for action in &group.actions {
                let class_matches_group = match group.kind {
                    MaintenanceActionGroupKind::RepositoryMaterialization => matches!(
                        action.class,
                        MaintenanceActionClass::ConsolidateExactDuplicates
                            | MaintenanceActionClass::CreateRenewalSuccessor
                    ),
                    MaintenanceActionGroupKind::PrivateDerivedState => {
                        action.class == MaintenanceActionClass::SuppressUnresolvedConflict
                    }
                    MaintenanceActionGroupKind::OwnerAuthorizedPrivateMutation => matches!(
                        action.class,
                        MaintenanceActionClass::OwnerConsolidateExactDuplicates
                            | MaintenanceActionClass::OwnerCreateRenewalSuccessor
                            | MaintenanceActionClass::OwnerResolveContradiction
                    ),
                };
                ensure!(
                    class_matches_group,
                    "maintenance action class does not match its execution domain"
                );
            }
        }
        ensure!(
            self.comparison_set_digest == comparison_digest(&self.records)?,
            "maintenance comparison-set digest does not match record snapshots"
        );
        for finding in &self.findings {
            ensure_sorted_unique(
                finding.record_ids.iter().map(String::as_str),
                "maintenance finding record IDs",
            )?;
            ensure_sorted_unique(
                finding.proposed_action_ids.iter().map(String::as_str),
                "maintenance finding action IDs",
            )?;
            ensure!(
                finding.finding_id == maintenance_finding_id(finding)?,
                "maintenance finding identity does not match its evidence"
            );
            ensure!(
                finding.comparison_set_digest
                    == finding_snapshot_comparison_digest(&self.records, &finding.record_ids)?,
                "maintenance finding comparison digest does not match its record snapshots"
            );
            for evidence in &finding.evidence {
                ensure!(
                    !evidence.code.trim().is_empty() && evidence.code == evidence.code.trim(),
                    "maintenance evidence code must be canonical and non-empty"
                );
                crate::validate_materialization_identity(
                    &evidence.digest,
                    "maintenance evidence digest",
                )?;
                if let Some(boundary) = evidence.boundary.as_deref() {
                    parse_canonical_timestamp(boundary, "maintenance evidence boundary")?;
                }
            }
            if finding.kind == MaintenanceFindingKind::HighConfidenceContradiction {
                ensure!(
                    !finding.conflict_edges.is_empty(),
                    "maintenance contradiction finding must preserve incompatibility edges"
                );
            } else {
                ensure!(
                    finding.conflict_edges.is_empty(),
                    "only maintenance contradiction findings may contain incompatibility edges"
                );
            }
            ensure!(
                finding
                    .conflict_edges
                    .windows(2)
                    .all(|window| window[0] < window[1]),
                "maintenance conflict edges must be sorted and unique"
            );
            for edge in &finding.conflict_edges {
                ensure!(
                    edge.record_ids[0] < edge.record_ids[1]
                        && edge.record_ids.iter().all(|record_id| {
                            finding.record_ids.binary_search(record_id).is_ok()
                        })
                        && edge.reason_code == "high_confidence_unresolved_contradiction",
                    "maintenance conflict edge is not canonical"
                );
                crate::validate_materialization_identity(
                    &edge.evidence_digest,
                    "maintenance conflict edge evidence digest",
                )?;
            }
            ensure!(
                finding.record_ids.iter().all(|record_id| {
                    self.records
                        .binary_search_by_key(&record_id.as_str(), |record| {
                            record.record_id.as_str()
                        })
                        .is_ok()
                }),
                "maintenance finding references an unknown record"
            );
        }
        let mut all_action_ids = BTreeSet::new();
        for group in &self.action_groups {
            ensure_sorted_unique(
                group.actions.iter().map(|action| action.action_id.as_str()),
                "maintenance actions",
            )?;
            for action in &group.actions {
                ensure_sorted_unique(
                    action.record_ids.iter().map(String::as_str),
                    "maintenance action record IDs",
                )?;
                ensure!(
                    action.action_id == maintenance_action_id(action)?,
                    "maintenance action identity does not match its candidate"
                );
                ensure!(
                    self.findings
                        .binary_search_by_key(&action.finding_id.as_str(), |finding| {
                            finding.finding_id.as_str()
                        })
                        .is_ok(),
                    "maintenance action references an unknown finding"
                );
                let finding = self
                    .findings
                    .iter()
                    .find(|finding| finding.finding_id == action.finding_id)
                    .expect("action finding existence checked");
                ensure!(
                    action.record_ids == finding.record_ids,
                    "maintenance action records do not match its finding"
                );
                let class_matches_finding = match action.class {
                    MaintenanceActionClass::ConsolidateExactDuplicates
                    | MaintenanceActionClass::OwnerConsolidateExactDuplicates => {
                        finding.kind == MaintenanceFindingKind::ExactDuplicate
                    }
                    MaintenanceActionClass::CreateRenewalSuccessor
                    | MaintenanceActionClass::OwnerCreateRenewalSuccessor => {
                        finding.kind == MaintenanceFindingKind::RenewalCandidate
                    }
                    MaintenanceActionClass::SuppressUnresolvedConflict => {
                        finding.kind == MaintenanceFindingKind::HighConfidenceContradiction
                    }
                    MaintenanceActionClass::OwnerResolveContradiction => {
                        finding.kind == MaintenanceFindingKind::HighConfidenceContradiction
                    }
                };
                ensure!(
                    class_matches_finding,
                    "maintenance action class does not match its finding kind"
                );
                validate_action_shape(action)?;
                ensure!(
                    action.record_ids.iter().all(|record_id| {
                        let expected = self
                            .records
                            .iter()
                            .find(|record| record.record_id == *record_id)
                            .map(|record| &record.version);
                        action.preconditions.record_versions.get(record_id) == expected
                    }) && action.preconditions.record_versions.len() == action.record_ids.len()
                        && action.preconditions.comparison_set_digest == self.comparison_set_digest,
                    "maintenance action record versions do not match its targets"
                );
                ensure!(
                    all_action_ids.insert(action.action_id.as_str()),
                    "maintenance action IDs must be unique"
                );
            }
        }
        for finding in &self.findings {
            let mut expected_actions = self
                .action_groups
                .iter()
                .flat_map(|group| &group.actions)
                .filter(|action| action.finding_id == finding.finding_id)
                .map(|action| action.action_id.clone())
                .collect::<Vec<_>>();
            expected_actions.sort();
            ensure!(
                finding.proposed_action_ids == expected_actions
                    && finding
                        .proposed_action_ids
                        .iter()
                        .all(|action_id| all_action_ids.contains(action_id.as_str())),
                "maintenance finding action references do not match plan actions"
            );
        }
        ensure!(
            self.summary == summarize(&self.records, &self.findings, &self.action_groups),
            "maintenance summary does not match plan contents"
        );
        let mut diagnostic_codes = BTreeSet::new();
        for diagnostic in &self.diagnostics {
            ensure!(
                !diagnostic.code.is_empty()
                    && diagnostic.code.bytes().all(|byte| byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'_')
                    && diagnostic_codes.insert(diagnostic.code.as_str()),
                "maintenance diagnostics must use unique content-free codes"
            );
            ensure!(
                diagnostic.count <= MAINTENANCE_MAX_INVENTORY_ENTRIES,
                "maintenance diagnostic count exceeds the bounded inventory"
            );
            crate::validate_materialization_identity(
                &diagnostic.digest,
                "maintenance diagnostic digest",
            )?;
        }
        ensure!(
            self.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == snapshot_diagnostic_code
                    && diagnostic.count == self.records.len()
                    && diagnostic.digest == self.comparison_set_digest
            }),
            "maintenance repository-snapshot diagnostic does not match the plan"
        );
        ensure!(
            self.preconditions.scope == expected_scope_binding
                && self.preconditions.comparison_set_digest == self.comparison_set_digest
                && self.preconditions.policy_digest == self.policy.policy_digest
                && self.preconditions.grant_fingerprint == self.authority.grant_fingerprint
                && self.preconditions.not_after == self.not_after,
            "maintenance plan preconditions do not match the bound artifact"
        );
        ensure!(
            self.policy.policy_digest == maintenance_policy_digest(&self.policy)?,
            "maintenance policy digest does not match the policy snapshot"
        );
        ensure!(
            self.policy.policy_version == MAINTENANCE_POLICY_VERSION
                && self.policy.maximum_validity_seconds > 0
                && self.policy.stale_after_seconds > 0,
            "maintenance policy snapshot does not use the current policy"
        );
        ensure!(
            self.detectors
                .windows(2)
                .all(|window| window[0].kind < window[1].kind),
            "maintenance detector snapshots must be sorted and unique"
        );
        for detector in &self.detectors {
            ensure!(
                !detector.version.trim().is_empty() && detector.version == detector.version.trim(),
                "maintenance detector version must be canonical and non-empty"
            );
            ensure!(
                detector.configuration_digest
                    == identity(
                        "memzoi/maintenance/detector",
                        &serde_json::json!({
                            "kind": detector.kind,
                            "version": detector.version,
                        }),
                    )?,
                "maintenance detector configuration digest does not match its snapshot"
            );
        }
        let expected_target_versions = self
            .records
            .iter()
            .filter(|record| record.target)
            .map(|record| (record.record_id.clone(), record.version.clone()))
            .collect::<BTreeMap<_, _>>();
        ensure!(
            self.preconditions.target_versions == expected_target_versions,
            "maintenance target-version preconditions do not match the selected records"
        );
        ensure!(
            self.preconditions.detector_digest
                == identity("memzoi/maintenance/detectors", &self.detectors)?,
            "maintenance detector precondition does not match detector snapshots"
        );
        ensure!(
            self.plan_id == maintenance_plan_id(self)?,
            "maintenance plan identity does not match its semantic contents"
        );
        Ok(())
    }
}

fn maintenance_finding_id(finding: &MaintenanceFinding) -> Result<String> {
    identity(
        "memzoi/maintenance/finding",
        &serde_json::json!({
            "kind": finding.kind,
            "record_ids": finding.record_ids,
            "comparison_set_digest": finding.comparison_set_digest,
            "evidence": finding.evidence,
            "confidence": finding.confidence,
            "conflict_edges": finding.conflict_edges,
        }),
    )
}

fn maintenance_action_id(action: &MaintenanceAction) -> Result<String> {
    identity(
        "memzoi/maintenance/action",
        &serde_json::json!({
            "class": action.class,
            "finding_id": action.finding_id,
            "record_ids": action.record_ids,
            "keeper_record_id": action.keeper_record_id,
            "predecessor_record_id": action.predecessor_record_id,
            "evidence_record_id": action.evidence_record_id,
            "preconditions": action.preconditions,
        }),
    )
}

fn validate_action_shape(action: &MaintenanceAction) -> Result<()> {
    match action.class {
        MaintenanceActionClass::ConsolidateExactDuplicates => {
            ensure!(
                action.record_ids.len() >= 2
                    && action.keeper_record_id.as_ref() == action.record_ids.first()
                    && action.predecessor_record_id.is_none()
                    && action.evidence_record_id.is_none(),
                "maintenance duplicate-consolidation action has an invalid shape"
            );
        }
        MaintenanceActionClass::OwnerConsolidateExactDuplicates
        | MaintenanceActionClass::OwnerResolveContradiction => {
            ensure!(
                action.record_ids.len() >= 2
                    && action.keeper_record_id.is_none()
                    && action.predecessor_record_id.is_none()
                    && action.evidence_record_id.is_none(),
                "maintenance owner-selection action must not choose a winner"
            );
        }
        MaintenanceActionClass::CreateRenewalSuccessor
        | MaintenanceActionClass::OwnerCreateRenewalSuccessor => {
            let predecessor = action.predecessor_record_id.as_deref();
            let evidence = action.evidence_record_id.as_deref();
            ensure!(
                action.record_ids.len() == 2
                    && action.keeper_record_id.is_none()
                    && predecessor.is_some()
                    && evidence.is_some()
                    && predecessor != evidence
                    && predecessor.is_some_and(|record_id| {
                        action
                            .record_ids
                            .binary_search_by(|candidate| candidate.as_str().cmp(record_id))
                            .is_ok()
                    })
                    && evidence.is_some_and(|record_id| {
                        action
                            .record_ids
                            .binary_search_by(|candidate| candidate.as_str().cmp(record_id))
                            .is_ok()
                    }),
                "maintenance renewal action has an invalid shape"
            );
        }
        MaintenanceActionClass::SuppressUnresolvedConflict => {
            ensure!(
                action.record_ids.len() >= 2
                    && action.keeper_record_id.is_none()
                    && action.predecessor_record_id.is_none()
                    && action.evidence_record_id.is_none(),
                "maintenance conflict-suppression action has an invalid shape"
            );
        }
    }
    Ok(())
}

fn maintenance_plan_id(plan: &MaintenancePlan) -> Result<String> {
    let mut projection = plan.clone();
    projection.plan_id.clear();
    identity("memzoi/maintenance/plan", &projection)
}

fn ensure_plan_size(plan: &MaintenancePlan) -> Result<()> {
    let size = serde_json::to_vec_pretty(plan)
        .context("failed to serialize maintenance plan")?
        .len()
        .checked_add(1)
        .context("maintenance plan size overflowed")?;
    if size > MAINTENANCE_MAX_SERIALIZED_PLAN_BYTES {
        bail!("serialized maintenance plan exceeds the supported size limit");
    }
    Ok(())
}

fn ensure_sorted_unique<'a>(values: impl IntoIterator<Item = &'a str>, label: &str) -> Result<()> {
    let values = values.into_iter().collect::<Vec<_>>();
    ensure!(
        values.windows(2).all(|window| window[0] < window[1]),
        "{label} must be sorted and unique"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;
    use crate::{
        CAPTURE_PROVENANCE_SCHEMA, CaptureClassification, CaptureEvidence, CaptureEvidenceSpan,
        CaptureExtractorIdentity, CaptureProvenance, CaptureSourceLocator, MemoryDestination,
        MemoryDraft, OkfProposalSensitivity, OriginDescriptor, OriginRoute, RetentionFacts,
        render_okf_record_markdown,
    };

    struct Fixture {
        _repo: tempfile::TempDir,
        _home: tempfile::TempDir,
        paths: MemoryPaths,
    }

    impl Fixture {
        fn new() -> Result<Self> {
            let repo = tempfile::tempdir()?;
            let home = tempfile::tempdir()?;
            let paths = MemoryPaths::with_runtime_home(
                repo.path().canonicalize()?,
                home.path().canonicalize()?,
            );
            fs::create_dir_all(paths.records_dir())?;
            Ok(Self {
                _repo: repo,
                _home: home,
                paths,
            })
        }

        fn write(&self, record: &OkfRecordFile) -> Result<()> {
            let markdown = render_okf_record_markdown(record)?;
            fs::write(
                self.paths
                    .records_dir()
                    .join(format!("{}.md", record.concept_id)),
                markdown,
            )?;
            Ok(())
        }

        fn plan(&self, evaluated_at: &str, record_ids: Vec<String>) -> Result<MaintenancePlan> {
            plan_maintenance(
                &self.paths,
                MaintenancePlanRequest {
                    schema: MAINTENANCE_REQUEST_SCHEMA.to_owned(),
                    evaluated_at: Some(evaluated_at.to_owned()),
                    record_ids,
                },
            )
        }
    }

    fn retention(explicit_expires_at: Option<&str>) -> RetentionFacts {
        RetentionFacts {
            occurred_at: None,
            started_at: None,
            last_continued_at: None,
            closed_at: None,
            explicit_expires_at: explicit_expires_at.map(str::to_owned),
            episodic_extension: None,
        }
    }

    fn record(id: &str, title: &str, body: &str) -> OkfRecordFile {
        OkfRecordFile {
            concept_id: id.to_owned(),
            draft: MemoryDraft {
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                scope_kind: ScopeKind::Repo,
                scope_id: None,
                visibility: Visibility::Repo,
                title: title.to_owned(),
                body: body.to_owned(),
                tags: vec!["maintenance".to_owned()],
                source_kind: Some("test".to_owned()),
                source_ref: None,
                sensitivity: OkfProposalSensitivity::RepoSafe,
                content_class: RepositoryContentClass::GeneralRepoKnowledge,
                confidence: 1.0,
            },
            status: MemoryStatus::Active,
            applies_to: Vec::new(),
            created: "2026-07-01T00:00:00Z".to_owned(),
            updated: None,
            supersedes_id: None,
            retention: retention(None),
            origin: OriginDescriptor::new(
                format!("maintenance-test:{id}"),
                OriginRoute::RepositoryProposal,
            ),
            lineage: None,
            proposal_id: None,
            capture: None,
            materialization: None,
        }
    }

    fn private_record(id: &str, title: &str, body: &str) -> MemoryRecord {
        MemoryRecord {
            id: id.to_owned(),
            memory_type: MemoryType::Fact,
            lane: MemoryLane::Semantic,
            destination: MemoryDestination::Local,
            scope_kind: ScopeKind::Repo,
            scope_id: None,
            visibility: Visibility::Private,
            title: title.to_owned(),
            body: body.to_owned(),
            status: MemoryStatus::Active,
            confidence: 1.0,
            source_kind: Some("test".to_owned()),
            source_ref: None,
            proposal_id: None,
            capture: None,
            content_hash: "RAW_PRIVATE_HASH_SENTINEL".to_owned(),
            created_at: "2026-07-01T00:00:00Z".to_owned(),
            updated_at: "2026-07-01T00:00:00Z".to_owned(),
            supersedes_id: None,
            retention: retention(None),
            origin: OriginDescriptor::new(
                format!("private-maintenance-test:{id}"),
                OriginRoute::LocalMemory,
            ),
            lineage: None,
        }
    }

    fn private_input(record: MemoryRecord, token_fill: char) -> PrivateMaintenanceRecordInput {
        PrivateMaintenanceRecordInput {
            record,
            applicability_paths: Vec::new(),
            version_token: token_fill.to_string().repeat(32),
            current_assertion: true,
            retention_state: RetentionState::Current,
            retention_reason: RetentionReason::NoAgeLimit,
            retention_boundary: None,
        }
    }

    fn identity_fixture(fill: char) -> String {
        format!("blake3:{}", fill.to_string().repeat(64))
    }

    fn capture(reviewed_at: &str, evidence_fill: char) -> CaptureProvenance {
        CaptureProvenance {
            schema: CAPTURE_PROVENANCE_SCHEMA.to_owned(),
            plan_id: identity_fixture('a'),
            review_id: identity_fixture('b'),
            claim_id: identity_fixture('c'),
            reviewed_claim_id: identity_fixture('d'),
            candidate_id: identity_fixture('e'),
            reviewed_candidate_id: identity_fixture('f'),
            extraction: CaptureExtractorIdentity {
                kind: "markdown".to_owned(),
                id: "markdown".to_owned(),
                implementation_digest: identity_fixture('0'),
            },
            evidence: vec![CaptureEvidence {
                source_id: "source-1".to_owned(),
                locator: CaptureSourceLocator::ProjectPath {
                    path: "evidence.md".to_owned(),
                },
                source_content_hash: identity_fixture(evidence_fill),
                span: CaptureEvidenceSpan {
                    byte_start: 0,
                    byte_end: 8,
                    line_start: 1,
                    line_end: 1,
                },
                evidence_content_hash: identity_fixture(evidence_fill),
                text: None,
                heading_path: vec!["Evidence".to_owned()],
                section_kind: "fact".to_owned(),
                semantic_location: None,
            }],
            confidence: "1.0".to_owned(),
            classification: CaptureClassification {
                destination: MemoryDestination::Repo,
                destination_reason: "repository-safe test evidence".to_owned(),
                sensitivity: OkfProposalSensitivity::RepoSafe,
                sensitivity_reason: "test review".to_owned(),
                content_class: RepositoryContentClass::GeneralRepoKnowledge,
                policy: MemoryDestination::Repo.policy(),
            },
            destination: MemoryDestination::Repo,
            sensitivity: OkfProposalSensitivity::RepoSafe,
            review_outcome: CaptureReviewOutcome::Accept,
            review_reason_code: None,
            reviewed_by: "reviewer:test".to_owned(),
            reviewed_at: reviewed_at.to_owned(),
            routed_by: "maintenance-test".to_owned(),
        }
    }

    fn finding_count(plan: &MaintenancePlan, kind: MaintenanceFindingKind) -> usize {
        plan.findings
            .iter()
            .filter(|finding| finding.kind == kind)
            .count()
    }

    #[test]
    fn private_planning_is_deterministic_content_free_and_requires_owner_keeper_selection()
    -> Result<()> {
        let private_body = "PRIVATE_DUPLICATE_BODY_SENTINEL must remain private.";
        let probe = private_record("private-duplicate-a", "Private duplicate", private_body);
        let raw_content_digest = identity("memzoi/maintenance/content", &probe.body)?;
        let raw_claim_digest = private_duplicate_claim_digest(&probe, &[])?;
        let left = private_input(probe, 'a');
        let right = private_input(
            private_record("private-duplicate-b", "Private duplicate", private_body),
            'b',
        );
        let request = MaintenancePlanRequest {
            schema: MAINTENANCE_REQUEST_SCHEMA.to_owned(),
            evaluated_at: Some("2026-07-18T12:00:00Z".to_owned()),
            record_ids: Vec::new(),
        };
        let first = plan_private_maintenance_at(
            identity_fixture('1'),
            request.clone(),
            vec![right.clone(), left.clone()],
            parse_timestamp("2026-07-18T12:00:00Z", "test")?,
        )?;
        let repeated = plan_private_maintenance_at(
            identity_fixture('1'),
            request,
            vec![left, right],
            parse_timestamp("2026-07-18T12:00:00Z", "test")?,
        )?;
        assert_eq!(
            serde_json::to_vec_pretty(&first)?,
            serde_json::to_vec_pretty(&repeated)?
        );
        assert!(matches!(
            &first.scope,
            MaintenanceScope::PrivateRuntime { .. }
        ));
        assert_eq!(first.summary.exact_duplicates, 1);
        assert!(first.action_groups[0].actions.is_empty());
        assert!(first.action_groups[1].actions.is_empty());
        let action = &first.action_groups[2].actions[0];
        assert_eq!(
            action.class,
            MaintenanceActionClass::OwnerConsolidateExactDuplicates
        );
        assert!(action.keeper_record_id.is_none());
        let rendered = serde_json::to_string(&first)?;
        assert!(!rendered.contains(private_body));
        assert!(!rendered.contains("Private duplicate"));
        assert!(!rendered.contains("RAW_PRIVATE_HASH_SENTINEL"));
        assert!(!rendered.contains(&raw_content_digest));
        assert!(!rendered.contains(&raw_claim_digest));
        assert!(rendered.contains("version_token"));
        Ok(())
    }

    #[test]
    fn private_contradiction_candidates_never_choose_a_winner() -> Result<()> {
        let positive = private_input(
            private_record(
                "private-conflict-a",
                "Private conflict",
                "Authentication is required.",
            ),
            'c',
        );
        let negative = private_input(
            private_record(
                "private-conflict-b",
                "Private conflict",
                "Authentication is not required.",
            ),
            'd',
        );
        let plan = plan_private_maintenance_at(
            identity_fixture('2'),
            MaintenancePlanRequest {
                schema: MAINTENANCE_REQUEST_SCHEMA.to_owned(),
                evaluated_at: Some("2026-07-18T12:00:00Z".to_owned()),
                record_ids: Vec::new(),
            },
            vec![positive, negative],
            parse_timestamp("2026-07-18T12:00:00Z", "test")?,
        )?;
        assert_eq!(plan.summary.contradictions, 1);
        let action = &plan.action_groups[2].actions[0];
        assert_eq!(
            action.class,
            MaintenanceActionClass::OwnerResolveContradiction
        );
        assert!(action.keeper_record_id.is_none());
        assert!(action.predecessor_record_id.is_none());
        assert!(action.evidence_record_id.is_none());
        Ok(())
    }

    #[test]
    fn automatic_private_authority_emits_edges_and_suppression() -> Result<()> {
        let inputs = vec![
            private_input(
                private_record(
                    "private-conflict-a",
                    "Private conflict",
                    "Authentication is required.",
                ),
                'a',
            ),
            private_input(
                private_record(
                    "private-conflict-b",
                    "Private conflict",
                    "Authentication is not required.",
                ),
                'b',
            ),
        ];
        let plan = plan_private_maintenance_with_authority_at(
            identity_fixture('3'),
            MaintenancePlanRequest {
                schema: MAINTENANCE_REQUEST_SCHEMA.to_owned(),
                evaluated_at: Some("2026-07-18T12:00:00Z".to_owned()),
                record_ids: Vec::new(),
            },
            inputs,
            parse_timestamp("2026-07-18T12:00:00Z", "test")?,
            identity_fixture('4'),
        )?;
        let finding = plan
            .findings
            .iter()
            .find(|finding| finding.kind == MaintenanceFindingKind::HighConfidenceContradiction)
            .context("missing contradiction finding")?;
        assert_eq!(finding.conflict_edges.len(), 1);
        assert_eq!(plan.action_groups[1].actions.len(), 1);
        assert_eq!(
            plan.action_groups[1].actions[0].class,
            MaintenanceActionClass::SuppressUnresolvedConflict
        );
        Ok(())
    }

    #[test]
    fn private_conflict_edge_record_ids_are_canonical_in_reverse_input_order() {
        assert_eq!(
            canonical_private_conflict_record_ids("private-z", "private-a"),
            ["private-a".to_owned(), "private-z".to_owned()]
        );
    }

    #[test]
    fn private_exact_duplicates_require_equal_applicability_paths() -> Result<()> {
        let mut frontend = private_input(
            private_record(
                "private-path-duplicate-a",
                "Private path duplicate",
                "Authentication uses the configured provider.",
            ),
            'a',
        );
        frontend.applicability_paths = vec!["frontend/**".to_owned()];
        let mut backend = private_input(
            private_record(
                "private-path-duplicate-b",
                "Private path duplicate",
                "Authentication uses the configured provider.",
            ),
            'b',
        );
        backend.applicability_paths = vec!["backend/**".to_owned()];
        let plan = plan_private_maintenance_at(
            identity_fixture('8'),
            MaintenancePlanRequest {
                schema: MAINTENANCE_REQUEST_SCHEMA.to_owned(),
                evaluated_at: Some("2026-07-18T12:00:00Z".to_owned()),
                record_ids: Vec::new(),
            },
            vec![frontend, backend],
            parse_timestamp("2026-07-18T12:00:00Z", "test")?,
        )?;
        assert_eq!(plan.summary.exact_duplicates, 0);
        assert!(!plan.action_groups.iter().any(|group| {
            group.actions.iter().any(|action| {
                action.class == MaintenanceActionClass::OwnerConsolidateExactDuplicates
            })
        }));
        Ok(())
    }

    #[test]
    fn private_contradictions_require_overlapping_applicability_paths() -> Result<()> {
        let plan_for = |left_paths: &[&str], right_paths: &[&str]| {
            let mut positive = private_input(
                private_record(
                    "private-path-conflict-a",
                    "Private path conflict",
                    "Authentication is required.",
                ),
                'a',
            );
            positive.applicability_paths =
                left_paths.iter().map(|path| (*path).to_owned()).collect();
            let mut negative = private_input(
                private_record(
                    "private-path-conflict-b",
                    "Private path conflict",
                    "Authentication is not required.",
                ),
                'b',
            );
            negative.applicability_paths =
                right_paths.iter().map(|path| (*path).to_owned()).collect();
            plan_private_maintenance_at(
                identity_fixture('7'),
                MaintenancePlanRequest {
                    schema: MAINTENANCE_REQUEST_SCHEMA.to_owned(),
                    evaluated_at: Some("2026-07-18T12:00:00Z".to_owned()),
                    record_ids: Vec::new(),
                },
                vec![positive, negative],
                parse_timestamp("2026-07-18T12:00:00Z", "test")?,
            )
        };

        assert_eq!(
            plan_for(&["frontend/**"], &["backend/**"])?
                .summary
                .contradictions,
            0
        );
        assert_eq!(
            plan_for(&["src/**"], &["src/auth/**"])?
                .summary
                .contradictions,
            1
        );
        assert_eq!(plan_for(&[], &["backend/**"])?.summary.contradictions, 1);
        Ok(())
    }

    #[test]
    fn equal_members_with_different_edge_topology_have_distinct_identities() -> Result<()> {
        let plan = |b_body: &str, c_body: &str| {
            plan_private_maintenance_with_authority_at(
                identity_fixture('5'),
                MaintenancePlanRequest {
                    schema: MAINTENANCE_REQUEST_SCHEMA.to_owned(),
                    evaluated_at: Some("2026-07-18T12:00:00Z".to_owned()),
                    record_ids: Vec::new(),
                },
                vec![
                    private_input(
                        private_record("edge-a", "Auth policy", "Authentication is required."),
                        'a',
                    ),
                    private_input(private_record("edge-b", "Auth policy", b_body), 'b'),
                    private_input(private_record("edge-c", "Auth policy", c_body), 'c'),
                ],
                parse_timestamp("2026-07-18T12:00:00Z", "test")?,
                identity_fixture('6'),
            )
        };
        let first = plan(
            "Authentication is required.",
            "Authentication is not required.",
        )?;
        let second = plan(
            "Authentication is not required.",
            "Authentication is not required.",
        )?;
        let first_finding = first
            .findings
            .iter()
            .find(|finding| finding.kind == MaintenanceFindingKind::HighConfidenceContradiction)
            .expect("contradiction finding");
        let second_finding = second
            .findings
            .iter()
            .find(|finding| finding.kind == MaintenanceFindingKind::HighConfidenceContradiction)
            .expect("contradiction finding");
        assert_eq!(first_finding.record_ids, second_finding.record_ids);
        assert_ne!(first_finding.conflict_edges, second_finding.conflict_edges);
        assert_ne!(first_finding.finding_id, second_finding.finding_id);
        assert_ne!(first.plan_id, second.plan_id);
        Ok(())
    }

    #[test]
    fn replay_is_byte_identical_and_duplicate_is_not_a_contradiction() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write(&record(
            "maintenance-a",
            "Auth policy",
            "Authentication is required.",
        ))?;
        fixture.write(&record(
            "maintenance-b",
            "Auth policy",
            "Authentication is required.",
        ))?;
        let before = snapshot_tree(&fixture.paths.memory_dir)?;
        assert!(!fixture.paths.repository_runtime_dir.exists());

        let first = fixture.plan(
            "2026-07-18T12:00:00Z",
            vec!["maintenance-b".to_owned(), "maintenance-a".to_owned()],
        )?;
        let repeated = fixture.plan(
            "2026-07-18T12:00:00Z",
            vec!["maintenance-a".to_owned(), "maintenance-b".to_owned()],
        )?;
        assert_eq!(
            serde_json::to_vec_pretty(&first)?,
            serde_json::to_vec_pretty(&repeated)?
        );
        assert_eq!(
            finding_count(&first, MaintenanceFindingKind::ExactDuplicate),
            1
        );
        assert_eq!(
            finding_count(&first, MaintenanceFindingKind::HighConfidenceContradiction),
            0
        );
        assert_eq!(first.action_groups.len(), 3);
        assert_eq!(first.action_groups[0].actions.len(), 1);
        assert!(first.action_groups[1].actions.is_empty());
        assert!(first.action_groups[2].actions.is_empty());
        assert_eq!(first.schema, MAINTENANCE_PLAN_SCHEMA);
        assert_eq!(first.request.schema, MAINTENANCE_REQUEST_SCHEMA);
        assert_eq!(first.policy.policy_version, "maintenance-policy/1");
        assert!(matches!(&first.scope, MaintenanceScope::Repository { .. }));
        assert!(first.records.iter().all(|record| matches!(
            &record.version,
            MaintenanceRecordVersion::CanonicalRepository { .. }
        )));
        let rendered = serde_json::to_string(&first)?;
        assert!(!rendered.contains("Authentication is required"));
        assert_eq!(snapshot_tree(&fixture.paths.memory_dir)?, before);
        assert!(!fixture.paths.repository_runtime_dir.exists());
        Ok(())
    }

    #[test]
    fn maintenance_plan_is_current_format_only_and_rejects_removed_fields() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write(&record(
            "maintenance-current-contract",
            "Current contract",
            "The current maintenance contract is required.",
        ))?;
        let plan = fixture.plan("2026-07-18T12:00:00Z", Vec::new())?;
        let current = serde_json::to_value(&plan)?;
        assert_eq!(current["schema"], MAINTENANCE_PLAN_SCHEMA);
        assert_eq!(
            current["records"][0]["version"]["kind"],
            "canonical_repository"
        );

        let mut removed_contract_version = current.clone();
        removed_contract_version["policy"]["contract_version"] =
            serde_json::json!("maintenance-plan/2");
        assert!(
            parse_maintenance_plan(&serde_json::to_string(&removed_contract_version)?).is_err(),
            "removed maintenance contract-version fields must be rejected"
        );

        let mut removed_top_level = current.clone();
        removed_top_level["contract_version"] = serde_json::json!("maintenance-plan/1");
        assert!(
            parse_maintenance_plan(&serde_json::to_string(&removed_top_level)?).is_err(),
            "removed top-level contract-version fields must be rejected"
        );

        let mut missing_policy_version = current.clone();
        missing_policy_version["policy"]
            .as_object_mut()
            .expect("policy object")
            .remove("policy_version");
        assert!(
            parse_maintenance_plan(&serde_json::to_string(&missing_policy_version)?).is_err(),
            "the current policy version must remain required"
        );

        let mut missing_record_ids = current.clone();
        missing_record_ids["request"]
            .as_object_mut()
            .expect("request object")
            .remove("record_ids");
        assert!(
            parse_maintenance_plan(&serde_json::to_string(&missing_record_ids)?).is_err(),
            "current maintenance requests must not infer missing record IDs"
        );

        let mut removed_record_shape = current;
        let version = removed_record_shape["records"][0]["version"].clone();
        removed_record_shape["records"][0]["source_path"] = version["source_path"].clone();
        removed_record_shape["records"][0]["revision"] = version["revision"].clone();
        removed_record_shape["records"][0]
            .as_object_mut()
            .expect("record object")
            .remove("version");
        assert!(
            parse_maintenance_plan(&serde_json::to_string(&removed_record_shape)?).is_err(),
            "removed source_path/revision fields must not be accepted"
        );
        Ok(())
    }

    #[test]
    fn contradiction_detector_is_conservative_about_scope_conditions_and_time() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write(&record(
            "positive",
            "Auth policy",
            "Authentication is required.",
        ))?;
        fixture.write(&record(
            "negative",
            "Auth policy",
            "Authentication is not required.",
        ))?;
        let mut other_scope = record(
            "other-scope",
            "Auth policy",
            "Authentication is not required.",
        );
        other_scope.draft.scope_id = Some("other".to_owned());
        fixture.write(&other_scope)?;
        fixture.write(&record(
            "conditional",
            "Auth policy",
            "If offline, authentication is not required.",
        ))?;
        fixture.write(&record(
            "temporal",
            "Auth policy",
            "As of 2025 authentication is not required.",
        ))?;
        fixture.write(&record(
            "different-title",
            "Different policy",
            "Authentication is not required.",
        ))?;
        let temporal_old = record(
            "temporal-update-old",
            "Cache policy",
            "Caching is required.",
        );
        fixture.write(&temporal_old)?;
        let mut temporal_new = record(
            "temporal-update-new",
            "Cache policy",
            "Caching is not required.",
        );
        temporal_new.supersedes_id = Some(temporal_old.concept_id.clone());
        fixture.write(&temporal_new)?;
        fixture.write(&record(
            "conditional-where-positive",
            "Availability condition",
            "Access is required where TLS is enabled.",
        ))?;
        fixture.write(&record(
            "conditional-where-negative",
            "Availability condition",
            "Access is required where TLS is not enabled.",
        ))?;
        fixture.write(&record(
            "boolean-word-true",
            "Boolean vocabulary",
            "The word true is allowed.",
        ))?;
        fixture.write(&record(
            "boolean-word-false",
            "Boolean vocabulary",
            "The word false is allowed.",
        ))?;
        let plan = fixture.plan("2026-07-18T12:00:00Z", Vec::new())?;
        assert_eq!(
            finding_count(&plan, MaintenanceFindingKind::HighConfidenceContradiction),
            1
        );
        let contradiction = plan
            .findings
            .iter()
            .find(|finding| finding.kind == MaintenanceFindingKind::HighConfidenceContradiction)
            .expect("one contradiction");
        assert_eq!(contradiction.record_ids, ["negative", "positive"]);
        assert!(contradiction.proposed_action_ids.is_empty());
        Ok(())
    }

    #[test]
    fn contradiction_detector_aggregates_full_sets_and_limits_boolean_polarity() -> Result<()> {
        let fixture = Fixture::new()?;
        for id in ["positive-a", "positive-b"] {
            fixture.write(&record(id, "Auth policy", "Authentication is required."))?;
        }
        fixture.write(&record(
            "negative",
            "Auth policy",
            "Authentication is not required.",
        ))?;
        fixture.write(&record(
            "boolean-true",
            "Feature flag",
            "The feature flag is true.",
        ))?;
        fixture.write(&record(
            "boolean-false",
            "Feature flag",
            "The feature flag is false.",
        ))?;

        let plan = fixture.plan("2026-07-18T12:00:00Z", Vec::new())?;
        let contradictions = plan
            .findings
            .iter()
            .filter(|finding| finding.kind == MaintenanceFindingKind::HighConfidenceContradiction)
            .collect::<Vec<_>>();
        assert_eq!(contradictions.len(), 2);
        assert!(
            contradictions
                .iter()
                .any(|finding| { finding.record_ids == ["negative", "positive-a", "positive-b"] })
        );
        assert!(
            contradictions
                .iter()
                .any(|finding| { finding.record_ids == ["boolean-false", "boolean-true"] })
        );
        Ok(())
    }

    #[test]
    fn episodic_occurrence_prevents_false_duplicate_reports() -> Result<()> {
        let fixture = Fixture::new()?;
        for (id, occurred_at) in [
            ("episode-a", "2026-07-10T00:00:00Z"),
            ("episode-b", "2026-07-11T00:00:00Z"),
        ] {
            let mut episode = record(id, "Deploy report", "Deployment completed.");
            episode.draft.memory_type = MemoryType::Episode;
            episode.draft.lane = MemoryLane::Episodic;
            episode.created = occurred_at.to_owned();
            episode.retention.occurred_at = Some(occurred_at.to_owned());
            fixture.write(&episode)?;
        }
        let plan = fixture.plan("2026-07-18T12:00:00Z", Vec::new())?;
        assert_eq!(
            finding_count(&plan, MaintenanceFindingKind::ExactDuplicate),
            0
        );
        assert_eq!(
            finding_count(&plan, MaintenanceFindingKind::HighConfidenceContradiction),
            0
        );
        Ok(())
    }

    #[test]
    fn staleness_and_expiry_are_report_only_and_bound_validity() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut stale = record("stale-record", "Old fact", "The policy is stable.");
        stale.created = "2026-01-01T00:00:00Z".to_owned();
        fixture.write(&stale)?;
        let mut expired = record("expired-record", "Lease", "The lease is active.");
        expired.retention = retention(Some("2026-07-18T13:00:00Z"));
        fixture.write(&expired)?;
        let before_boundary = fixture.plan("2026-07-18T12:00:00Z", Vec::new())?;
        assert_eq!(
            finding_count(&before_boundary, MaintenanceFindingKind::Stale),
            1
        );
        assert_eq!(
            finding_count(&before_boundary, MaintenanceFindingKind::Expired),
            0
        );
        assert_eq!(before_boundary.not_after, "2026-07-18T13:00:00Z");

        let after_boundary = fixture.plan("2026-07-18T13:00:00Z", Vec::new())?;
        assert_eq!(
            finding_count(&after_boundary, MaintenanceFindingKind::Expired),
            1
        );
        assert!(after_boundary.action_groups.iter().all(|group| {
            group.actions.iter().all(|action| {
                !matches!(
                    action.class,
                    MaintenanceActionClass::SuppressUnresolvedConflict
                        | MaintenanceActionClass::OwnerConsolidateExactDuplicates
                        | MaintenanceActionClass::OwnerCreateRenewalSuccessor
                )
            })
        }));
        Ok(())
    }

    #[test]
    fn explicit_expired_status_is_reported_without_a_retention_boundary() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut expired = record(
            "lifecycle-expired",
            "Retired policy",
            "The retired policy is archived.",
        );
        expired.status = MemoryStatus::Expired;
        fixture.write(&expired)?;

        let plan = fixture.plan("2026-07-18T12:00:00Z", Vec::new())?;
        let finding = plan
            .findings
            .iter()
            .find(|finding| finding.kind == MaintenanceFindingKind::Expired)
            .expect("explicit expired lifecycle status must be reported");
        assert_eq!(finding.record_ids, ["lifecycle-expired"]);
        assert!(
            finding
                .evidence
                .iter()
                .any(|evidence| evidence.code == "lifecycle_expired")
        );
        assert!(finding.proposed_action_ids.is_empty());
        Ok(())
    }

    #[test]
    fn renewal_requires_fresh_distinct_accepted_evidence_without_retention_mutation() -> Result<()>
    {
        let fixture = Fixture::new()?;
        let mut predecessor = record("renewal-old", "Policy", "Authentication is required.");
        predecessor.created = "2026-01-01T00:00:00Z".to_owned();
        predecessor.retention = retention(Some("2026-06-01T00:00:00Z"));
        fixture.write(&predecessor)?;
        let mut evidence = record("renewal-evidence", "Policy", "Authentication is required.");
        evidence.created = "2026-07-01T00:00:00Z".to_owned();
        evidence.capture = Some(capture("2026-07-01T00:00:00Z", '9'));
        fixture.write(&evidence)?;
        let before = snapshot_tree(&fixture.paths.memory_dir)?;

        let plan = fixture.plan("2026-07-18T12:00:00Z", Vec::new())?;
        assert_eq!(
            finding_count(&plan, MaintenanceFindingKind::RenewalCandidate),
            1
        );
        assert!(plan.action_groups[0].actions.iter().any(|action| {
            action.class == MaintenanceActionClass::CreateRenewalSuccessor
                && action.predecessor_record_id.as_deref() == Some("renewal-old")
                && action.evidence_record_id.as_deref() == Some("renewal-evidence")
        }));
        assert_eq!(snapshot_tree(&fixture.paths.memory_dir)?, before);
        Ok(())
    }

    #[test]
    fn renewal_rejects_future_review_and_same_origin_replay() -> Result<()> {
        let future_fixture = Fixture::new()?;
        let mut predecessor = record("future-old", "Policy", "Authentication is required.");
        predecessor.created = "2026-01-01T00:00:00Z".to_owned();
        predecessor.retention = retention(Some("2026-06-01T00:00:00Z"));
        future_fixture.write(&predecessor)?;
        let mut future_evidence =
            record("future-evidence", "Policy", "Authentication is required.");
        future_evidence.capture = Some(capture("2026-07-19T00:00:00Z", '8'));
        future_fixture.write(&future_evidence)?;
        let plan = future_fixture.plan("2026-07-18T12:00:00Z", Vec::new())?;
        assert_eq!(
            finding_count(&plan, MaintenanceFindingKind::RenewalCandidate),
            0
        );

        let replay_fixture = Fixture::new()?;
        let mut replay_predecessor = record("replay-old", "Policy", "Authentication is required.");
        replay_predecessor.created = "2026-01-01T00:00:00Z".to_owned();
        replay_predecessor.retention = retention(Some("2026-06-01T00:00:00Z"));
        replay_fixture.write(&replay_predecessor)?;
        let mut replay_evidence =
            record("replay-evidence", "Policy", "Authentication is required.");
        replay_evidence.origin = replay_predecessor.origin.clone();
        replay_evidence.capture = Some(capture("2026-07-01T00:00:00Z", '7'));
        replay_fixture.write(&replay_evidence)?;
        let plan = replay_fixture.plan("2026-07-18T12:00:00Z", Vec::new())?;
        assert_eq!(
            finding_count(&plan, MaintenanceFindingKind::RenewalCandidate),
            0
        );
        Ok(())
    }

    #[test]
    fn episodic_reports_are_not_renewal_candidates() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut predecessor = record("episode-old", "Deploy report", "Deployment completed.");
        predecessor.draft.memory_type = MemoryType::Episode;
        predecessor.draft.lane = MemoryLane::Episodic;
        predecessor.created = "2026-01-01T00:00:00Z".to_owned();
        predecessor.retention.occurred_at = Some(predecessor.created.clone());
        fixture.write(&predecessor)?;

        let mut current = record("episode-current", "Deploy report", "Deployment completed.");
        current.draft.memory_type = MemoryType::Episode;
        current.draft.lane = MemoryLane::Episodic;
        current.created = "2026-07-01T00:00:00Z".to_owned();
        current.retention.occurred_at = Some(current.created.clone());
        current.capture = Some(capture("2026-07-01T00:00:00Z", '6'));
        fixture.write(&current)?;

        let plan = fixture.plan("2026-07-18T12:00:00Z", Vec::new())?;
        assert_eq!(
            finding_count(&plan, MaintenanceFindingKind::RenewalCandidate),
            0
        );
        Ok(())
    }

    #[test]
    fn revalidation_types_target_neighborhood_and_validity_changes() -> Result<()> {
        let fixture = Fixture::new()?;
        let target = record("target", "Target", "Target is current.");
        let neighbor = record("neighbor", "Neighbor", "Neighbor is current.");
        fixture.write(&target)?;
        fixture.write(&neighbor)?;
        let plan = fixture.plan("2026-07-18T12:00:00Z", vec!["target".to_owned()])?;
        assert_eq!(
            revalidate_maintenance_plan_at(
                &fixture.paths,
                &plan,
                parse_timestamp("2026-07-18T12:30:00Z", "test")?,
            )?,
            MaintenanceRevalidation::Current {
                plan_id: plan.plan_id.clone()
            }
        );

        let mut changed_neighbor = neighbor.clone();
        changed_neighbor.draft.body = "Neighbor is changed.".to_owned();
        fixture.write(&changed_neighbor)?;
        let stale = revalidate_maintenance_plan_at(
            &fixture.paths,
            &plan,
            parse_timestamp("2026-07-18T12:30:00Z", "test")?,
        )?;
        assert!(
            matches!(stale, MaintenanceRevalidation::Stale { ref reasons, .. }
            if reasons.contains(&MaintenanceStaleReason::ComparisonNeighborhoodChanged))
        );

        fixture.write(&neighbor)?;
        let mut changed_target = target;
        changed_target.draft.body = "Target is changed.".to_owned();
        fixture.write(&changed_target)?;
        let stale = revalidate_maintenance_plan_at(
            &fixture.paths,
            &plan,
            parse_timestamp("2026-07-18T12:30:00Z", "test")?,
        )?;
        assert!(
            matches!(stale, MaintenanceRevalidation::Stale { ref reasons, .. }
            if reasons.contains(&MaintenanceStaleReason::TargetChanged))
        );

        fs::remove_file(fixture.paths.records_dir().join("target.md"))?;
        let deleted = revalidate_maintenance_plan_at(
            &fixture.paths,
            &plan,
            parse_timestamp("2026-07-18T12:30:00Z", "test")?,
        )?;
        assert!(
            matches!(deleted, MaintenanceRevalidation::Stale { ref reasons, .. }
            if reasons.contains(&MaintenanceStaleReason::TargetChanged)
                && !reasons.contains(&MaintenanceStaleReason::SourceChanged))
        );

        let expired = revalidate_maintenance_plan_at(
            &fixture.paths,
            &plan,
            parse_timestamp(&plan.not_after, "test")?,
        )?;
        assert!(
            matches!(expired, MaintenanceRevalidation::Stale { ref reasons, .. }
            if reasons.contains(&MaintenanceStaleReason::PlanExpired))
        );
        Ok(())
    }

    #[test]
    fn strict_parser_rejects_unknown_fields_and_identity_tampering() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write(&record("tamper", "Tamper", "Tamper is rejected."))?;
        let plan = fixture.plan("2026-07-18T12:00:00Z", Vec::new())?;
        let mut value = serde_json::to_value(&plan)?;
        value
            .as_object_mut()
            .expect("plan object")
            .insert("unknown".to_owned(), serde_json::Value::Bool(true));
        assert!(parse_maintenance_plan(&serde_json::to_string(&value)?).is_err());

        let mut nested_unknown = serde_json::to_value(&plan)?;
        nested_unknown["records"][0]["version"]["revision"]["unknown"] =
            serde_json::Value::Bool(true);
        assert!(
            parse_maintenance_plan(&serde_json::to_string(&nested_unknown)?).is_err(),
            "nested revision fields must remain strict"
        );

        let mut tampered = plan.clone();
        tampered.summary.records += 1;
        assert!(tampered.validate().is_err());

        let mut stale_precondition = plan.clone();
        let mut stale_version = plan.records[0].version.clone();
        let MaintenanceRecordVersion::CanonicalRepository { revision, .. } = &mut stale_version
        else {
            panic!("repository fixture must use a canonical version")
        };
        revision.revision_hash = identity_fixture('9');
        stale_precondition
            .preconditions
            .target_versions
            .insert("tamper".to_owned(), stale_version);
        stale_precondition.plan_id = maintenance_plan_id(&stale_precondition)?;
        assert!(stale_precondition.validate().is_err());

        let mut noncanonical_time = plan;
        noncanonical_time.evaluated_at = " 2026-07-18T12:00:00Z".to_owned();
        noncanonical_time.request.evaluated_at = Some(noncanonical_time.evaluated_at.clone());
        noncanonical_time.plan_id = maintenance_plan_id(&noncanonical_time)?;
        assert!(noncanonical_time.validate().is_err());
        Ok(())
    }

    #[test]
    fn future_records_are_bound_but_excluded_from_historical_evaluation() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut future = record("future-record", "Future", "Future is available.");
        future.created = "2026-07-19T00:00:00Z".to_owned();
        future.retention = retention(Some("2026-07-19T06:00:00Z"));
        fixture.write(&future)?;

        let plan = fixture.plan("2026-07-18T12:00:00Z", Vec::new())?;
        assert_eq!(plan.records.len(), 1);
        assert!(!plan.records[0].current_assertion);
        assert!(plan.findings.is_empty());
        assert_eq!(plan.not_after, "2026-07-19T12:00:00Z");
        assert!(plan.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "records_after_evaluation_excluded" && diagnostic.count == 1
        }));
        Ok(())
    }

    #[test]
    fn extreme_time_and_oversized_source_fail_closed() -> Result<()> {
        let fixture = Fixture::new()?;
        let error = fixture
            .plan("9999-12-31T23:59:59Z", Vec::new())
            .expect_err("validity arithmetic must not overflow");
        assert!(
            format!("{error:#}").contains("cannot represent the validity window"),
            "{error:#}"
        );

        fs::write(
            fixture.paths.records_dir().join("oversized.md"),
            vec![b'x'; MAINTENANCE_MAX_INPUT_FILE_BYTES + 1],
        )?;
        let error = fixture
            .plan("2026-07-18T12:00:00Z", Vec::new())
            .expect_err("oversized source must fail before parsing");
        assert!(format!("{error:#}").contains("maintenance file limit"));
        Ok(())
    }

    #[test]
    fn selector_limit_applies_to_normalized_unique_ids() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write(&record(
            "selector-target",
            "Selector target",
            "Selector target is current.",
        ))?;

        let duplicate_selectors =
            vec!["selector-target".to_owned(); MAINTENANCE_MAX_ADMITTED_RECORDS + 17];
        let plan = fixture.plan("2026-07-18T12:00:00Z", duplicate_selectors)?;
        assert_eq!(plan.request.record_ids, ["selector-target"]);
        assert_eq!(plan.scope.target_record_ids(), ["selector-target"]);

        let unique_selectors = (0..=MAINTENANCE_MAX_ADMITTED_RECORDS)
            .map(|index| format!("selector-{index:03}"))
            .collect::<Vec<_>>();
        let error = fixture
            .plan("2026-07-18T12:00:00Z", unique_selectors)
            .expect_err("more than the admitted number of unique selectors must fail");
        assert!(
            format!("{error:#}").contains("admitted-record selector limit"),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn admitted_record_and_pair_work_limits_are_hard() -> Result<()> {
        let fixture = Fixture::new()?;
        for index in 0..=MAINTENANCE_MAX_ADMITTED_RECORDS {
            fixture.write(&record(
                &format!("bounded-{index:03}"),
                "Bounded",
                &format!("Record {index} is bounded."),
            ))?;
        }
        let error = fixture
            .plan("2026-07-18T12:00:00Z", Vec::new())
            .expect_err("admission subprocess work must be bounded");
        assert!(format!("{error:#}").contains("admitted-record limit"));

        let mut comparisons = MAINTENANCE_MAX_PAIR_COMPARISONS;
        let error = consume_pair_work(&mut comparisons, None)
            .expect_err("pair-comparison work must be bounded");
        assert!(format!("{error:#}").contains("pair-comparison work limit"));
        Ok(())
    }

    fn snapshot_tree(root: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
        fn visit(
            root: &Path,
            directory: &Path,
            output: &mut BTreeMap<String, Vec<u8>>,
        ) -> Result<()> {
            if !directory.exists() {
                return Ok(());
            }
            let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    visit(root, &path, output)?;
                } else {
                    output.insert(
                        path.strip_prefix(root)?
                            .to_string_lossy()
                            .replace('\\', "/"),
                        fs::read(path)?,
                    );
                }
            }
            Ok(())
        }
        let mut output = BTreeMap::new();
        visit(root, root, &mut output)?;
        Ok(output)
    }
}
