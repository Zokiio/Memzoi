mod detectors;
mod diagnostics;
mod policy;
mod projection;

pub use diagnostics::{
    RepositoryWriteBlocked, RepositoryWriteSafetyFinding, RepositoryWriteSafetyReasonCode,
    RepositoryWriteSafetyReport, SafetyFieldLocation,
};
pub use policy::{
    AuthorizationProof, FreshnessCheck, ProvenanceAssessment, RepositoryContentClass,
    RepositoryScope, RepositoryWriteRequest, RepositoryWriteRoute, SafetyField, SafetyFieldKind,
};
pub use projection::RepositoryProjection;

use std::fmt;

use serde::{Deserialize, Serialize};

pub const REPOSITORY_WRITE_SAFETY_SCHEMA: &str = "memzoi/repository-write-safety-v1";
pub const REPOSITORY_WRITE_SAFETY_VERSION: &str = "1";
pub const REPOSITORY_WRITE_DETECTOR_POLICY_VERSION: &str = "1";

/// Capability minted only after the shared repository-write policy authorizes an exact batch.
///
/// External callers cannot construct or mutate the capability:
///
/// ```compile_fail
/// use memzoi_core::{AuthorizedRepositoryWriteBatch, RepositoryWriteRoute};
///
/// let _forged = AuthorizedRepositoryWriteBatch {
///     contract_version: "1",
///     detector_policy_version: "1",
///     route: RepositoryWriteRoute::Materialization,
///     project_digest: [0; 32],
///     authorization_digest: [0; 32],
///     projection_digest: [0; 32],
/// };
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct AuthorizedRepositoryWriteBatch {
    contract_version: &'static str,
    detector_policy_version: &'static str,
    route: RepositoryWriteRoute,
    project_digest: [u8; 32],
    policy_context_digest: [u8; 32],
    authorization_digest: [u8; 32],
    projection_digest: [u8; 32],
}

impl fmt::Debug for AuthorizedRepositoryWriteBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizedRepositoryWriteBatch")
            .field("contract_version", &self.contract_version)
            .field("detector_policy_version", &self.detector_policy_version)
            .field("route", &self.route)
            .field("authorization_digest", &self.digest())
            .finish_non_exhaustive()
    }
}

impl AuthorizedRepositoryWriteBatch {
    pub fn digest(&self) -> String {
        hex_digest(&self.authorization_digest)
    }

    pub fn route(&self) -> RepositoryWriteRoute {
        self.route
    }

    pub(crate) fn authorizes(
        &self,
        expected_route: RepositoryWriteRoute,
        project_identity: &[u8],
        expected_policy_context_digest: &[u8; 32],
        projections: &[RepositoryProjection<'_>],
    ) -> bool {
        self.contract_version == REPOSITORY_WRITE_SAFETY_VERSION
            && self.detector_policy_version == REPOSITORY_WRITE_DETECTOR_POLICY_VERSION
            && self.route == expected_route
            && self.project_digest == projection::project_digest(project_identity)
            && self.policy_context_digest == *expected_policy_context_digest
            && self.projection_digest == projection::projection_digest(projections)
            && self.authorization_digest
                == projection::authorization_digest(
                    &self.project_digest,
                    &self.policy_context_digest,
                    &self.projection_digest,
                )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryWriteSafetyAssessment {
    pub report: RepositoryWriteSafetyReport,
}

impl RepositoryWriteSafetyAssessment {
    pub fn is_allowed(&self) -> bool {
        self.report.allowed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositoryWriteDecision {
    Allowed(AuthorizedRepositoryWriteBatch),
    Blocked(RepositoryWriteSafetyReport),
}

pub fn assess_repository_candidate(
    request: &RepositoryWriteRequest<'_>,
) -> RepositoryWriteSafetyAssessment {
    RepositoryWriteSafetyAssessment {
        report: policy::evaluate(request),
    }
}

pub fn authorize_repository_write(
    request: &RepositoryWriteRequest<'_>,
) -> Result<AuthorizedRepositoryWriteBatch, RepositoryWriteBlocked> {
    let report = policy::evaluate(request);
    if !report.allowed {
        return Err(RepositoryWriteBlocked::new(report));
    }
    let projection_digest = projection::projection_digest(&request.projections);
    let project_digest = projection::project_digest(request.scope.current_project_identity);
    let policy_context_digest = projection::policy_context_digest(request);
    let authorization_digest = projection::authorization_digest(
        &project_digest,
        &policy_context_digest,
        &projection_digest,
    );
    Ok(AuthorizedRepositoryWriteBatch {
        contract_version: REPOSITORY_WRITE_SAFETY_VERSION,
        detector_policy_version: REPOSITORY_WRITE_DETECTOR_POLICY_VERSION,
        route: request.route,
        project_digest,
        policy_context_digest,
        authorization_digest,
        projection_digest,
    })
}

pub(crate) fn repository_write_policy_context_digest(
    request: &RepositoryWriteRequest<'_>,
) -> [u8; 32] {
    projection::policy_context_digest(request)
}

pub fn scan_repository_blob(
    project_identity: &[u8],
    repository_relative_path: &std::path::Path,
    bytes: &[u8],
) -> RepositoryWriteSafetyReport {
    scan_repository_blob_with_class(
        project_identity,
        repository_relative_path,
        bytes,
        RepositoryContentClass::GeneralRepoKnowledge,
        true,
    )
}

pub fn scan_managed_repository_blob(
    project_identity: &[u8],
    repository_relative_path: &std::path::Path,
    bytes: &[u8],
) -> RepositoryWriteSafetyReport {
    let (content_class, metadata_valid) =
        repository_blob_content_class(repository_relative_path, bytes);
    scan_repository_blob_with_class(
        project_identity,
        repository_relative_path,
        bytes,
        content_class,
        metadata_valid,
    )
}

fn scan_repository_blob_with_class(
    project_identity: &[u8],
    repository_relative_path: &std::path::Path,
    bytes: &[u8],
    content_class: RepositoryContentClass,
    metadata_valid: bool,
) -> RepositoryWriteSafetyReport {
    let projections = vec![RepositoryProjection {
        path: repository_relative_path,
        bytes,
        target_revision: None,
    }];
    let request = RepositoryWriteRequest {
        route: RepositoryWriteRoute::Maintenance,
        destination: crate::MemoryDestination::Repo,
        sensitivity: crate::OkfProposalSensitivity::RepoSafe,
        scope: RepositoryScope {
            kind: crate::ScopeKind::Repo,
            id: None,
            current_project_identity: project_identity,
            configured_project_id: None,
        },
        visibility: crate::Visibility::Repo,
        authorization: AuthorizationProof::ExplicitCommand {
            operation: "safety_scan",
        },
        freshness: Vec::new(),
        provenance: ProvenanceAssessment {
            present: metadata_valid,
            evidence_valid: metadata_valid,
            content_class,
            source_identity: metadata_valid.then_some("repository_blob"),
        },
        fields: Vec::new(),
        projections,
    };
    policy::evaluate(&request)
}

fn repository_blob_content_class(
    repository_relative_path: &std::path::Path,
    bytes: &[u8],
) -> (RepositoryContentClass, bool) {
    let Ok(markdown) = std::str::from_utf8(bytes) else {
        return (RepositoryContentClass::Unknown, false);
    };
    let proposals_root = std::path::Path::new(".memzoi/proposals");
    if repository_relative_path.starts_with(proposals_root) {
        return match crate::okf::parse_okf_proposal_markdown(
            proposals_root,
            repository_relative_path,
            markdown,
        ) {
            Ok(Some(proposal)) => (proposal.content_class, true),
            _ => (RepositoryContentClass::Unknown, false),
        };
    }
    for records_root in [
        std::path::Path::new(".memzoi/records"),
        std::path::Path::new(".memzoi/memory"),
    ] {
        if repository_relative_path.starts_with(records_root) {
            return match crate::okf::parse_okf_record_markdown(
                records_root,
                repository_relative_path,
                markdown,
            ) {
                Ok(Some(record)) => (record.draft.content_class, true),
                _ => (RepositoryContentClass::Unknown, false),
            };
        }
    }
    (RepositoryContentClass::Unknown, false)
}

fn hex_digest(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn projection_path_is_safe(path: &std::path::Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}
