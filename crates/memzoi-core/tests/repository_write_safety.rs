use std::path::Path;

use memzoi_core::{
    AuthorizationProof, MemoryDestination, OkfProposalSensitivity, ProvenanceAssessment,
    REPOSITORY_WRITE_DETECTOR_POLICY_VERSION, REPOSITORY_WRITE_SAFETY_SCHEMA,
    RepositoryContentClass, RepositoryProjection, RepositoryScope, RepositoryWriteRequest,
    RepositoryWriteRoute, SafetyField, SafetyFieldKind, ScopeKind, Visibility,
    authorize_repository_write, scan_repository_blob,
};

fn safe_request<'a>(
    project: &'a [u8],
    value: &'a [u8],
    path: &'a Path,
) -> RepositoryWriteRequest<'a> {
    RepositoryWriteRequest {
        route: RepositoryWriteRoute::Materialization,
        destination: MemoryDestination::Repo,
        sensitivity: OkfProposalSensitivity::RepoSafe,
        scope: RepositoryScope {
            kind: ScopeKind::Repo,
            id: None,
            current_project_identity: project,
            configured_project_id: None,
        },
        visibility: Visibility::Repo,
        authorization: AuthorizationProof::PinnedMaterialization {
            decision_id: "decision-1",
        },
        freshness: Vec::new(),
        provenance: ProvenanceAssessment {
            present: true,
            evidence_valid: true,
            content_class: RepositoryContentClass::GeneralRepoKnowledge,
            source_identity: Some("test"),
        },
        fields: vec![SafetyField {
            location: "candidate.body",
            kind: SafetyFieldKind::Text,
            value,
        }],
        projections: vec![RepositoryProjection {
            path,
            bytes: value,
            target_revision: None,
        }],
    }
}

#[test]
fn prohibited_content_reports_are_stable_and_redacted() {
    let fixtures: &[(&str, &[u8])] = &[
        ("private_key", b"-----BEGIN PRIVATE KEY-----\nSENTINEL\n"),
        (
            "encrypted_pkcs8_private_key",
            b"-----BEGIN ENCRYPTED PRIVATE KEY-----\nYWJj\n-----END ENCRYPTED PRIVATE KEY-----\n",
        ),
        (
            "rsa_private_key",
            b"-----BEGIN RSA PRIVATE KEY-----\nYWJj\n-----END RSA PRIVATE KEY-----\n",
        ),
        (
            "ec_private_key",
            b"-----BEGIN EC PRIVATE KEY-----\nYWJj\n-----END EC PRIVATE KEY-----\n",
        ),
        (
            "openssh_private_key",
            b"-----BEGIN OPENSSH PRIVATE KEY-----\nYWJj\n-----END OPENSSH PRIVATE KEY-----\n",
        ),
        (
            "dsa_private_key",
            b"-----BEGIN DSA PRIVATE KEY-----\nYWJj\n-----END DSA PRIVATE KEY-----\n",
        ),
        ("authorization", b"Authorization: Bearer SECRET-SENTINEL-123456"),
        ("credentialed_url", b"https://user:SECRET-SENTINEL@example.test/path"),
        ("connection", b"postgres://user:SECRET-SENTINEL@db.test/main"),
        ("environment", b"SERVICE_API_KEY=SECRET-SENTINEL-123456789"),
        ("jwt", b"eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJTRUNSRVQtU0VOVElORUwiLCJleHAiOjk5OTk5OTk5OTl9.signatureSECRET"),
    ];
    for (name, bytes) in fixtures {
        let report =
            scan_repository_blob(b"project", Path::new(".memzoi/records/candidate.md"), bytes);
        assert!(!report.allowed, "fixture {name} should be blocked");
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("SECRET-SENTINEL"), "fixture {name} leaked");
        assert_eq!(report.schema, REPOSITORY_WRITE_SAFETY_SCHEMA);
        assert_eq!(
            report.detector_policy_version,
            REPOSITORY_WRITE_DETECTOR_POLICY_VERSION
        );
    }
}

#[test]
fn invalid_encoding_and_unsafe_paths_fail_closed() {
    let invalid = scan_repository_blob(
        b"project",
        Path::new(".memzoi/records/candidate.md"),
        &[0xff, 0xfe],
    );
    assert!(!invalid.allowed);
    let traversal = scan_repository_blob(b"project", Path::new("../candidate.md"), b"safe");
    assert!(!traversal.allowed);
}

#[test]
fn capability_changes_when_any_output_byte_changes() {
    let path = Path::new(".memzoi/records/candidate.md");
    let request = safe_request(b"project", b"safe knowledge", path);
    let first = authorize_repository_write(&request).unwrap();
    let changed = safe_request(b"project", b"safe knowledge changed", path);
    let second = authorize_repository_write(&changed).unwrap();
    assert_ne!(first.digest(), second.digest());
}
