use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use super::{REPOSITORY_WRITE_SAFETY_SCHEMA, RepositoryWriteRoute};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryWriteSafetyReasonCode {
    DestinationNotRepo,
    SensitivityNotRepoSafe,
    ScopeNotRepository,
    ScopeProjectMismatch,
    VisibilityNotRepositorySafe,
    AuthorizationMissing,
    ProvenanceMissing,
    EvidenceInvalid,
    StaleSafetyDecision,
    CredentialToken,
    PrivateKey,
    AuthorizationHeader,
    CredentialedUrl,
    ConnectionString,
    SessionToken,
    CloudCredential,
    EnvironmentSecret,
    HighEntropyValue,
    RawTranscript,
    PrivatePersonalData,
    ActivityHistory,
    PrivateEndpoint,
    UndisclosedVulnerability,
    PrivateEvidenceUnminimized,
    TemporaryTaskState,
    LocalOnlyState,
    UnknownContentClass,
    InvalidEncoding,
    CandidateTooLarge,
    UnsafeOutputPath,
}

impl RepositoryWriteSafetyReasonCode {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::DestinationNotRepo => "destination_not_repo",
            Self::SensitivityNotRepoSafe => "sensitivity_not_repo_safe",
            Self::ScopeNotRepository => "scope_not_repository",
            Self::ScopeProjectMismatch => "scope_project_mismatch",
            Self::VisibilityNotRepositorySafe => "visibility_not_repository_safe",
            Self::AuthorizationMissing => "authorization_missing",
            Self::ProvenanceMissing => "provenance_missing",
            Self::EvidenceInvalid => "evidence_invalid",
            Self::StaleSafetyDecision => "stale_safety_decision",
            Self::CredentialToken => "credential_token",
            Self::PrivateKey => "private_key",
            Self::AuthorizationHeader => "authorization_header",
            Self::CredentialedUrl => "credentialed_url",
            Self::ConnectionString => "connection_string",
            Self::SessionToken => "session_token",
            Self::CloudCredential => "cloud_credential",
            Self::EnvironmentSecret => "environment_secret",
            Self::HighEntropyValue => "high_entropy_value",
            Self::RawTranscript => "raw_transcript",
            Self::PrivatePersonalData => "private_personal_data",
            Self::ActivityHistory => "activity_history",
            Self::PrivateEndpoint => "private_endpoint",
            Self::UndisclosedVulnerability => "undisclosed_vulnerability",
            Self::PrivateEvidenceUnminimized => "private_evidence_unminimized",
            Self::TemporaryTaskState => "temporary_task_state",
            Self::LocalOnlyState => "local_only_state",
            Self::UnknownContentClass => "unknown_content_class",
            Self::InvalidEncoding => "invalid_encoding",
            Self::CandidateTooLarge => "candidate_too_large",
            Self::UnsafeOutputPath => "unsafe_output_path",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SafetyFieldLocation(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryWriteSafetyFinding {
    pub code: RepositoryWriteSafetyReasonCode,
    pub field: SafetyFieldLocation,
    pub detector: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryWriteSafetyReport {
    pub schema: String,
    pub route: RepositoryWriteRoute,
    pub allowed: bool,
    pub candidate_fingerprint: String,
    pub findings: Vec<RepositoryWriteSafetyFinding>,
}

impl RepositoryWriteSafetyReport {
    pub(crate) fn new(
        route: RepositoryWriteRoute,
        candidate_fingerprint: String,
        mut findings: Vec<RepositoryWriteSafetyFinding>,
    ) -> Self {
        findings.sort_by(|left, right| {
            (
                &left.field,
                left.code.as_str(),
                &left.detector,
                &left.fingerprint,
            )
                .cmp(&(
                    &right.field,
                    right.code.as_str(),
                    &right.detector,
                    &right.fingerprint,
                ))
        });
        findings.dedup();
        Self {
            schema: REPOSITORY_WRITE_SAFETY_SCHEMA.to_owned(),
            route,
            allowed: findings.is_empty(),
            candidate_fingerprint,
            findings,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryWriteBlocked {
    report: Box<RepositoryWriteSafetyReport>,
}

impl RepositoryWriteBlocked {
    pub(crate) fn new(report: RepositoryWriteSafetyReport) -> Self {
        Self {
            report: Box::new(report),
        }
    }

    pub fn report(&self) -> &RepositoryWriteSafetyReport {
        &self.report
    }

    pub fn into_report(self) -> RepositoryWriteSafetyReport {
        *self.report
    }
}

impl fmt::Display for RepositoryWriteBlocked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "repository write blocked for route {} (candidate {}, {} finding(s))",
            self.report.route.as_str(),
            self.report.candidate_fingerprint,
            self.report.findings.len()
        )
    }
}

impl Error for RepositoryWriteBlocked {}
