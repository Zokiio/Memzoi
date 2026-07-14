use std::path::Path;

use memzoi_core::{
    AuthorizationProof, MemoryDestination, OkfProposalSensitivity, ProvenanceAssessment,
    REPOSITORY_WRITE_DETECTOR_POLICY_VERSION, REPOSITORY_WRITE_SAFETY_SCHEMA,
    RepositoryContentClass, RepositoryProjection, RepositoryScope, RepositoryWriteRequest,
    RepositoryWriteRoute, SafetyField, SafetyFieldKind, ScopeKind, Visibility,
    authorize_repository_write, scan_managed_repository_blob, scan_repository_blob,
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
        ("bare_password", b"PASSWORD=hunter2"),
        ("bare_token", b"TOKEN=abc"),
        ("bare_secret", b"SECRET=abc"),
        ("bare_api_key", b"API_KEY=abc"),
        ("jwt", b"eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJTRUNSRVQtU0VOVElORUwiLCJleHAiOjk5OTk5OTk5OTl9.signatureSECRET"),
    ];
    for (name, bytes) in fixtures {
        let markdown = canonical_record_markdown(
            std::str::from_utf8(bytes).unwrap(),
            Some("general_repo_knowledge"),
        );
        let report = scan_repository_blob(
            b"project",
            Path::new(".memzoi/records/candidate.md"),
            markdown.as_bytes(),
        );
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
fn managed_blob_classification_is_parsed_and_fails_closed() {
    let path = Path::new(".memzoi/records/candidate.md");
    let safe = canonical_record_markdown(
        "Durable repository knowledge.",
        Some("general_repo_knowledge"),
    );
    assert!(scan_managed_repository_blob(b"project", path, safe.as_bytes()).allowed);

    let team_scoped = safe.replace("scope: repo", "scope: team\nscope_id: platform");
    assert!(scan_managed_repository_blob(b"project", path, team_scoped.as_bytes()).allowed);
    let team_without_id = safe.replace("scope: repo", "scope: team");
    let team_without_id_report =
        scan_managed_repository_blob(b"project", path, team_without_id.as_bytes());
    assert!(!team_without_id_report.allowed);
    assert!(team_without_id_report.findings.iter().any(|finding| {
        finding.code == memzoi_core::RepositoryWriteSafetyReasonCode::ScopeNotRepository
    }));

    let missing = canonical_record_markdown("Unclassified repository knowledge.", None);
    let missing_report = scan_managed_repository_blob(b"project", path, missing.as_bytes());
    assert!(!missing_report.allowed);
    assert!(missing_report.findings.iter().any(|finding| {
        finding.code == memzoi_core::RepositoryWriteSafetyReasonCode::UnknownContentClass
    }));

    let prohibited =
        canonical_record_markdown("Lexically harmless transcript.", Some("raw_transcript"));
    let prohibited_report = scan_managed_repository_blob(b"project", path, prohibited.as_bytes());
    assert!(!prohibited_report.allowed);
    assert!(prohibited_report.findings.iter().any(|finding| {
        finding.code == memzoi_core::RepositoryWriteSafetyReasonCode::RawTranscript
    }));

    let private_record = safe
        .replace("scope: repo", "scope: personal")
        .replace("visibility: repo", "visibility: private");
    let private_report = scan_managed_repository_blob(b"project", path, private_record.as_bytes());
    assert!(!private_report.allowed);
    assert!(private_report.findings.iter().any(|finding| {
        finding.code == memzoi_core::RepositoryWriteSafetyReasonCode::ScopeNotRepository
    }));
    assert!(private_report.findings.iter().any(|finding| {
        finding.code == memzoi_core::RepositoryWriteSafetyReasonCode::VisibilityNotRepositorySafe
    }));

    let proposal_path = Path::new(".memzoi/proposals/pending/candidate.md");
    let secret_proposal = proposal_markdown("secret");
    let proposal_report =
        scan_managed_repository_blob(b"project", proposal_path, secret_proposal.as_bytes());
    assert!(!proposal_report.allowed);
    assert!(proposal_report.findings.iter().any(|finding| {
        finding.code == memzoi_core::RepositoryWriteSafetyReasonCode::SensitivityNotRepoSafe
    }));
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

#[test]
fn candidate_fingerprint_changes_with_target_revision() {
    let path = Path::new(".memzoi/records/candidate.md");
    let mut first = safe_request(b"project", b"safe knowledge", path);
    first.projections[0].target_revision = Some("revision-a");
    let mut second = safe_request(b"project", b"safe knowledge", path);
    second.projections[0].target_revision = Some("revision-b");
    let first = memzoi_core::assess_repository_candidate(&first);
    let second = memzoi_core::assess_repository_candidate(&second);
    assert_ne!(
        first.report.candidate_fingerprint,
        second.report.candidate_fingerprint
    );
}

fn canonical_record_markdown(body: &str, content_class: Option<&str>) -> String {
    let content_class = content_class
        .map(|value| format!("content_class: {value}\n"))
        .unwrap_or_default();
    format!(
        "---\ntype: fact\ntitle: Candidate\ntimestamp: 2026-07-14T00:00:00Z\nupdated: 2026-07-14T00:00:00Z\nstatus: active\nscope: repo\nvisibility: repo\n{content_class}confidence: 1\n---\n\n# Candidate\n\n{body}\n"
    )
}

fn proposal_markdown(sensitivity: &str) -> String {
    format!(
        "---\nid: mem_scan_candidate\nkind: proposal\nversion: okf/v0.1\nprofile: memzoi/v0\ntype: fact\nlane: semantic\ntitle: Candidate\ndescription: Candidate description.\nstatus: proposed\nproposal:\n  action: create\n  proposed_by: test\n  proposed_at: 2026-07-14T00:00:00Z\nscope:\n  kind: repo\n  paths: []\ntags: []\ntimestamp: 2026-07-14T00:00:00Z\ncreated_by: test\nsources: []\nsupersedes: []\nsensitivity: {sensitivity}\ncontent_class: general_repo_knowledge\n---\n\n# Candidate\n\nDurable repository knowledge.\n"
    )
}
