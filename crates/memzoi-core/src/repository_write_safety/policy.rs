use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::{MemoryDestination, OkfProposalSensitivity, ScopeKind, Visibility};

use super::{
    RepositoryProjection,
    detectors::{MAX_AGGREGATE_BYTES, MAX_FIELD_BYTES, scan_value},
    diagnostics::{
        RepositoryWriteSafetyFinding, RepositoryWriteSafetyReasonCode, RepositoryWriteSafetyReport,
        SafetyFieldLocation,
    },
    projection::{candidate_fingerprint, finding_fingerprint},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryWriteRoute {
    DatabaseProposalApply,
    FileProposalCreate,
    FileProposalApply,
    FileProposalRejectReceipt,
    CaptureApply,
    ImportApply,
    SessionEndPromotion,
    Supersede,
    Tombstone,
    Maintenance,
    Migration,
    Recovery,
    Materialization,
    ProviderImport,
}

impl RepositoryWriteRoute {
    pub const ALL: [Self; 14] = [
        Self::DatabaseProposalApply,
        Self::FileProposalCreate,
        Self::FileProposalApply,
        Self::FileProposalRejectReceipt,
        Self::CaptureApply,
        Self::ImportApply,
        Self::SessionEndPromotion,
        Self::Supersede,
        Self::Tombstone,
        Self::Maintenance,
        Self::Migration,
        Self::Recovery,
        Self::Materialization,
        Self::ProviderImport,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DatabaseProposalApply => "database_proposal_apply",
            Self::FileProposalCreate => "file_proposal_create",
            Self::FileProposalApply => "file_proposal_apply",
            Self::FileProposalRejectReceipt => "file_proposal_reject_receipt",
            Self::CaptureApply => "capture_apply",
            Self::ImportApply => "import_apply",
            Self::SessionEndPromotion => "session_end_promotion",
            Self::Supersede => "supersede",
            Self::Tombstone => "tombstone",
            Self::Maintenance => "maintenance",
            Self::Migration => "migration",
            Self::Recovery => "recovery",
            Self::Materialization => "materialization",
            Self::ProviderImport => "provider_import",
        }
    }
}

impl fmt::Display for RepositoryWriteRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy)]
pub struct RepositoryScope<'a> {
    pub kind: ScopeKind,
    pub id: Option<&'a str>,
    pub current_project_identity: &'a [u8],
    pub configured_project_id: Option<&'a str>,
}

impl fmt::Debug for RepositoryScope<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepositoryScope")
            .field("kind", &self.kind)
            .field("id", &self.id.map(|_| "<redacted>"))
            .field("current_project_identity", &"<redacted>")
            .field(
                "configured_project_id",
                &self.configured_project_id.map(|_| "<redacted>"),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryContentClass {
    GeneralRepoKnowledge,
    RawTranscript,
    PrivatePersonalData,
    ScreenOrActivityHistory,
    PrivateEndpoint,
    UndisclosedVulnerability,
    UnminimizedPrivateEvidence,
    TemporaryTaskState,
    LocalOnlyState,
    #[default]
    Unknown,
}

impl RepositoryContentClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GeneralRepoKnowledge => "general_repo_knowledge",
            Self::RawTranscript => "raw_transcript",
            Self::PrivatePersonalData => "private_personal_data",
            Self::ScreenOrActivityHistory => "screen_or_activity_history",
            Self::PrivateEndpoint => "private_endpoint",
            Self::UndisclosedVulnerability => "undisclosed_vulnerability",
            Self::UnminimizedPrivateEvidence => "unminimized_private_evidence",
            Self::TemporaryTaskState => "temporary_task_state",
            Self::LocalOnlyState => "local_only_state",
            Self::Unknown => "unknown",
        }
    }
}

impl FromStr for RepositoryContentClass {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "general_repo_knowledge" | "general-repo-knowledge" => Ok(Self::GeneralRepoKnowledge),
            "raw_transcript" | "raw-transcript" => Ok(Self::RawTranscript),
            "private_personal_data" | "private-personal-data" => Ok(Self::PrivatePersonalData),
            "screen_or_activity_history" | "screen-or-activity-history" => {
                Ok(Self::ScreenOrActivityHistory)
            }
            "private_endpoint" | "private-endpoint" => Ok(Self::PrivateEndpoint),
            "undisclosed_vulnerability" | "undisclosed-vulnerability" => {
                Ok(Self::UndisclosedVulnerability)
            }
            "unminimized_private_evidence" | "unminimized-private-evidence" => {
                Ok(Self::UnminimizedPrivateEvidence)
            }
            "temporary_task_state" | "temporary-task-state" => Ok(Self::TemporaryTaskState),
            "local_only_state" | "local-only-state" => Ok(Self::LocalOnlyState),
            "unknown" => Ok(Self::Unknown),
            other => Err(format!(
                "invalid repository content class {other:?}; expected general_repo_knowledge, raw_transcript, private_personal_data, screen_or_activity_history, private_endpoint, undisclosed_vulnerability, unminimized_private_evidence, temporary_task_state, local_only_state, or unknown"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProvenanceAssessment<'a> {
    pub present: bool,
    pub evidence_valid: bool,
    pub content_class: RepositoryContentClass,
    pub source_identity: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct FreshnessCheck<'a> {
    pub name: &'a str,
    pub expected: &'a str,
    pub current: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub enum AuthorizationProof<'a> {
    Missing,
    ApprovedDatabaseProposal {
        proposal_id: &'a str,
    },
    ExplicitCommand {
        operation: &'a str,
    },
    CaptureReview {
        plan_id: &'a str,
        review_id: &'a str,
    },
    ImportPlan {
        plan_id: &'a str,
    },
    LifecycleOperation {
        target_id: &'a str,
    },
    PinnedMaterialization {
        decision_id: &'a str,
    },
    Recovery {
        authorization_digest: &'a str,
    },
    Maintenance {
        operation_id: &'a str,
    },
    Migration {
        migration_id: &'a str,
    },
    ProviderImport {
        import_id: &'a str,
    },
}

impl AuthorizationProof<'_> {
    pub(crate) fn is_present(self) -> bool {
        !matches!(self, Self::Missing)
    }

    pub(crate) fn stable_bytes(self) -> String {
        match self {
            Self::Missing => "missing".to_owned(),
            Self::ApprovedDatabaseProposal { proposal_id } => format!("db:{proposal_id}"),
            Self::ExplicitCommand { operation } => format!("command:{operation}"),
            Self::CaptureReview { plan_id, review_id } => {
                format!("capture:{plan_id}:{review_id}")
            }
            Self::ImportPlan { plan_id } => format!("import:{plan_id}"),
            Self::LifecycleOperation { target_id } => format!("lifecycle:{target_id}"),
            Self::PinnedMaterialization { decision_id } => {
                format!("materialization:{decision_id}")
            }
            Self::Recovery {
                authorization_digest,
            } => format!("recovery:{authorization_digest}"),
            Self::Maintenance { operation_id } => format!("maintenance:{operation_id}"),
            Self::Migration { migration_id } => format!("migration:{migration_id}"),
            Self::ProviderImport { import_id } => format!("provider:{import_id}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyFieldKind {
    Text,
    Identifier,
    Path,
    SourceReference,
    Evidence,
    Reason,
    RenderedProjection,
    TypedDigest,
    Uuid,
    CommitHash,
}

impl SafetyFieldKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Identifier => "identifier",
            Self::Path => "path",
            Self::SourceReference => "source_reference",
            Self::Evidence => "evidence",
            Self::Reason => "reason",
            Self::RenderedProjection => "rendered_projection",
            Self::TypedDigest => "typed_digest",
            Self::Uuid => "uuid",
            Self::CommitHash => "commit_hash",
        }
    }

    pub(crate) const fn entropy_exempt(self) -> bool {
        matches!(self, Self::TypedDigest | Self::Uuid | Self::CommitHash)
    }
}

#[derive(Clone, Copy)]
pub struct SafetyField<'a> {
    pub location: &'a str,
    pub kind: SafetyFieldKind,
    pub value: &'a [u8],
}

impl fmt::Debug for SafetyField<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SafetyField")
            .field("location", &self.location)
            .field("kind", &self.kind)
            .field(
                "value",
                &format_args!("<redacted:{} bytes>", self.value.len()),
            )
            .finish()
    }
}

pub struct RepositoryWriteRequest<'a> {
    pub route: RepositoryWriteRoute,
    pub destination: MemoryDestination,
    pub sensitivity: OkfProposalSensitivity,
    pub scope: RepositoryScope<'a>,
    pub visibility: Visibility,
    pub authorization: AuthorizationProof<'a>,
    pub freshness: Vec<FreshnessCheck<'a>>,
    pub provenance: ProvenanceAssessment<'a>,
    pub fields: Vec<SafetyField<'a>>,
    pub projections: Vec<RepositoryProjection<'a>>,
}

impl fmt::Debug for RepositoryWriteRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepositoryWriteRequest")
            .field("route", &self.route)
            .field("destination", &self.destination)
            .field("sensitivity", &self.sensitivity)
            .field("scope", &self.scope)
            .field("visibility", &self.visibility)
            .field("freshness_count", &self.freshness.len())
            .field("provenance", &self.provenance)
            .field("field_count", &self.fields.len())
            .field("projection_count", &self.projections.len())
            .finish_non_exhaustive()
    }
}

pub(crate) fn evaluate(request: &RepositoryWriteRequest<'_>) -> RepositoryWriteSafetyReport {
    let mut findings = Vec::new();

    if request.destination != MemoryDestination::Repo {
        push_finding(
            &mut findings,
            RepositoryWriteSafetyReasonCode::DestinationNotRepo,
            "destination",
            "metadata",
            request.destination.as_str().as_bytes(),
        );
    }
    if request.sensitivity != OkfProposalSensitivity::RepoSafe {
        push_finding(
            &mut findings,
            RepositoryWriteSafetyReasonCode::SensitivityNotRepoSafe,
            "sensitivity",
            "metadata",
            request.sensitivity.as_str().as_bytes(),
        );
    }
    match request.scope.kind {
        ScopeKind::Repo => {}
        ScopeKind::Project => {
            if request.scope.id.is_none()
                || request.scope.configured_project_id.is_none()
                || request.scope.id != request.scope.configured_project_id
            {
                push_finding(
                    &mut findings,
                    RepositoryWriteSafetyReasonCode::ScopeProjectMismatch,
                    "scope",
                    "metadata",
                    request.scope.id.unwrap_or("<missing>").as_bytes(),
                );
            }
        }
        ScopeKind::Team | ScopeKind::Org => push_finding(
            &mut findings,
            RepositoryWriteSafetyReasonCode::ScopeNotRepository,
            "scope",
            "metadata",
            request.scope.kind.as_str().as_bytes(),
        ),
        _ => push_finding(
            &mut findings,
            RepositoryWriteSafetyReasonCode::ScopeNotRepository,
            "scope",
            "metadata",
            request.scope.kind.as_str().as_bytes(),
        ),
    }
    if !matches!(request.visibility, Visibility::Repo | Visibility::Public) {
        push_finding(
            &mut findings,
            RepositoryWriteSafetyReasonCode::VisibilityNotRepositorySafe,
            "visibility",
            "metadata",
            request.visibility.as_str().as_bytes(),
        );
    }
    if !request.authorization.is_present() {
        push_finding(
            &mut findings,
            RepositoryWriteSafetyReasonCode::AuthorizationMissing,
            "authorization",
            "metadata",
            b"missing",
        );
    }
    if !request.provenance.present {
        push_finding(
            &mut findings,
            RepositoryWriteSafetyReasonCode::ProvenanceMissing,
            "provenance",
            "metadata",
            b"missing",
        );
    }
    if !request.provenance.evidence_valid {
        push_finding(
            &mut findings,
            RepositoryWriteSafetyReasonCode::EvidenceInvalid,
            "provenance.evidence",
            "metadata",
            b"invalid",
        );
    }
    if request.provenance.content_class != RepositoryContentClass::GeneralRepoKnowledge {
        let code = match request.provenance.content_class {
            RepositoryContentClass::RawTranscript => RepositoryWriteSafetyReasonCode::RawTranscript,
            RepositoryContentClass::PrivatePersonalData => {
                RepositoryWriteSafetyReasonCode::PrivatePersonalData
            }
            RepositoryContentClass::ScreenOrActivityHistory => {
                RepositoryWriteSafetyReasonCode::ActivityHistory
            }
            RepositoryContentClass::PrivateEndpoint => {
                RepositoryWriteSafetyReasonCode::PrivateEndpoint
            }
            RepositoryContentClass::UndisclosedVulnerability => {
                RepositoryWriteSafetyReasonCode::UndisclosedVulnerability
            }
            RepositoryContentClass::UnminimizedPrivateEvidence => {
                RepositoryWriteSafetyReasonCode::PrivateEvidenceUnminimized
            }
            RepositoryContentClass::TemporaryTaskState => {
                RepositoryWriteSafetyReasonCode::TemporaryTaskState
            }
            RepositoryContentClass::LocalOnlyState => {
                RepositoryWriteSafetyReasonCode::LocalOnlyState
            }
            RepositoryContentClass::Unknown => RepositoryWriteSafetyReasonCode::UnknownContentClass,
            RepositoryContentClass::GeneralRepoKnowledge => unreachable!(),
        };
        push_finding(
            &mut findings,
            code,
            "provenance.content_class",
            "contextual_policy",
            b"blocked",
        );
    }
    for check in &request.freshness {
        if check.expected != check.current {
            push_finding(
                &mut findings,
                RepositoryWriteSafetyReasonCode::StaleSafetyDecision,
                check.name,
                "freshness",
                check.current.as_bytes(),
            );
        }
    }
    let aggregate_size = request
        .fields
        .iter()
        .map(|field| field.value.len())
        .chain(
            request
                .projections
                .iter()
                .map(|projection| projection.bytes.len()),
        )
        .sum::<usize>();
    if aggregate_size > MAX_AGGREGATE_BYTES {
        push_finding(
            &mut findings,
            RepositoryWriteSafetyReasonCode::CandidateTooLarge,
            "candidate",
            "size_limit",
            &aggregate_size.to_le_bytes(),
        );
    }
    for field in &request.fields {
        if field.value.len() > MAX_FIELD_BYTES {
            push_finding(
                &mut findings,
                RepositoryWriteSafetyReasonCode::CandidateTooLarge,
                field.location,
                "size_limit",
                &field.value.len().to_le_bytes(),
            );
            continue;
        }
        scan_value(field.location, field.kind, field.value, &mut findings);
    }
    for (index, projection) in request.projections.iter().enumerate() {
        let location = format!("projection[{index}]");
        let path_bytes = projection.path.as_os_str().as_encoded_bytes();
        if !super::projection_path_is_safe(projection.path) {
            push_finding(
                &mut findings,
                RepositoryWriteSafetyReasonCode::UnsafeOutputPath,
                &format!("{location}.path"),
                "path_policy",
                path_bytes,
            );
        }
        scan_value(
            &format!("{location}.path"),
            SafetyFieldKind::Path,
            path_bytes,
            &mut findings,
        );
        if projection.bytes.len() > MAX_FIELD_BYTES {
            push_finding(
                &mut findings,
                RepositoryWriteSafetyReasonCode::CandidateTooLarge,
                &format!("{location}.bytes"),
                "size_limit",
                &projection.bytes.len().to_le_bytes(),
            );
        } else {
            scan_value(
                &format!("{location}.bytes"),
                SafetyFieldKind::RenderedProjection,
                projection.bytes,
                &mut findings,
            );
        }
    }

    RepositoryWriteSafetyReport::new(request.route, candidate_fingerprint(request), findings)
}

fn push_finding(
    findings: &mut Vec<RepositoryWriteSafetyFinding>,
    code: RepositoryWriteSafetyReasonCode,
    field: &str,
    detector: &str,
    value: &[u8],
) {
    findings.push(RepositoryWriteSafetyFinding {
        fingerprint: finding_fingerprint(code.as_str(), field, value),
        code,
        field: SafetyFieldLocation(field.to_owned()),
        detector: detector.to_owned(),
    });
}
