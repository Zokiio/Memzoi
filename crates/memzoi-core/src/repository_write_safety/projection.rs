use std::path::Path;

use blake3::Hasher;

use super::{
    REPOSITORY_WRITE_DETECTOR_POLICY_VERSION, REPOSITORY_WRITE_SAFETY_VERSION,
    RepositoryWriteRequest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepositoryProjectionPurpose {
    Write,
    Existing,
}

impl RepositoryProjectionPurpose {
    pub(crate) fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Write => b"write",
            Self::Existing => b"existing",
        }
    }
}

#[derive(Clone, Copy)]
pub struct RepositoryProjection<'a> {
    pub path: &'a Path,
    pub bytes: &'a [u8],
    pub target_revision: Option<&'a str>,
    pub purpose: RepositoryProjectionPurpose,
}

impl std::fmt::Debug for RepositoryProjection<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RepositoryProjection")
            .field("path", &self.path)
            .field(
                "bytes",
                &format_args!("<redacted:{} bytes>", self.bytes.len()),
            )
            .field("target_revision", &self.target_revision)
            .field("purpose", &self.purpose)
            .finish()
    }
}

pub(crate) fn project_digest(identity: &[u8]) -> [u8; 32] {
    domain_hash(b"memzoi.repository-write.project.v1", [identity])
}

pub(crate) fn projection_digest(projections: &[RepositoryProjection<'_>]) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(b"memzoi.repository-write.projections.v1\0");
    put_usize(&mut hasher, projections.len());
    for projection in projections {
        put_bytes(&mut hasher, projection.path.as_os_str().as_encoded_bytes());
        put_bytes(&mut hasher, projection.bytes);
        put_optional(&mut hasher, projection.target_revision.map(str::as_bytes));
        put_bytes(&mut hasher, projection.purpose.as_bytes());
    }
    *hasher.finalize().as_bytes()
}

pub(crate) fn authorization_digest(
    project_digest: &[u8; 32],
    policy_context_digest: &[u8; 32],
    projection_digest: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(b"memzoi.repository-write.authorization.v1\0");
    put_bytes(&mut hasher, REPOSITORY_WRITE_SAFETY_VERSION.as_bytes());
    put_bytes(
        &mut hasher,
        REPOSITORY_WRITE_DETECTOR_POLICY_VERSION.as_bytes(),
    );
    put_bytes(&mut hasher, project_digest);
    put_bytes(&mut hasher, policy_context_digest);
    put_bytes(&mut hasher, projection_digest);
    *hasher.finalize().as_bytes()
}

pub(crate) fn policy_context_digest(request: &RepositoryWriteRequest<'_>) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(b"memzoi.repository-write.policy-context.v1\0");
    put_bytes(&mut hasher, request.route.as_str().as_bytes());
    put_bytes(&mut hasher, request.destination.as_str().as_bytes());
    put_bytes(&mut hasher, request.sensitivity.as_str().as_bytes());
    put_bytes(&mut hasher, request.scope.kind.as_str().as_bytes());
    put_optional(&mut hasher, request.scope.id.map(str::as_bytes));
    put_bytes(&mut hasher, request.scope.current_project_identity);
    put_optional(
        &mut hasher,
        request.scope.configured_project_id.map(str::as_bytes),
    );
    put_bytes(&mut hasher, request.visibility.as_str().as_bytes());
    put_bytes(&mut hasher, request.authorization.stable_bytes().as_bytes());
    put_usize(&mut hasher, request.freshness.len());
    for freshness in &request.freshness {
        put_bytes(&mut hasher, freshness.name.as_bytes());
        put_bytes(&mut hasher, freshness.expected.as_bytes());
        put_bytes(&mut hasher, freshness.current.as_bytes());
    }
    hasher.update(&[u8::from(request.provenance.present)]);
    hasher.update(&[u8::from(request.provenance.evidence_valid)]);
    put_bytes(
        &mut hasher,
        request.provenance.content_class.as_str().as_bytes(),
    );
    put_optional(
        &mut hasher,
        request.provenance.source_identity.map(str::as_bytes),
    );
    put_usize(&mut hasher, request.fields.len());
    for field in &request.fields {
        put_bytes(&mut hasher, field.location.as_bytes());
        put_bytes(&mut hasher, field.kind.as_str().as_bytes());
        put_bytes(&mut hasher, field.value);
    }
    *hasher.finalize().as_bytes()
}

pub(crate) fn candidate_fingerprint(request: &RepositoryWriteRequest<'_>) -> String {
    let mut hasher = Hasher::new();
    hasher.update(b"memzoi.repository-write.candidate.v1\0");
    for field in &request.fields {
        put_bytes(&mut hasher, field.location.as_bytes());
        put_bytes(&mut hasher, field.kind.as_str().as_bytes());
        put_bytes(&mut hasher, field.value);
    }
    for projection in &request.projections {
        put_bytes(&mut hasher, projection.path.as_os_str().as_encoded_bytes());
        put_bytes(&mut hasher, projection.bytes);
        put_optional(&mut hasher, projection.target_revision.map(str::as_bytes));
        put_bytes(&mut hasher, projection.purpose.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

pub(crate) fn finding_fingerprint(code: &str, field: &str, value: &[u8]) -> String {
    let mut hasher = Hasher::new();
    hasher.update(b"memzoi.repository-write.finding.v1\0");
    put_bytes(&mut hasher, code.as_bytes());
    put_bytes(&mut hasher, field.as_bytes());
    put_bytes(&mut hasher, blake3::hash(value).as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn domain_hash<'a>(domain: &[u8], values: impl IntoIterator<Item = &'a [u8]>) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(domain);
    hasher.update(b"\0");
    for value in values {
        put_bytes(&mut hasher, value);
    }
    *hasher.finalize().as_bytes()
}

fn put_optional(hasher: &mut Hasher, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            put_bytes(hasher, value);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn put_bytes(hasher: &mut Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn put_usize(hasher: &mut Hasher, value: usize) {
    hasher.update(&(value as u64).to_le_bytes());
}
