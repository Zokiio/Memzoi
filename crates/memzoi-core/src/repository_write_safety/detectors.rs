use std::{collections::BTreeMap, sync::LazyLock};

use regex::Regex;
use url::Url;

use super::{
    SafetyFieldKind,
    diagnostics::{
        RepositoryWriteSafetyFinding, RepositoryWriteSafetyReasonCode, SafetyFieldLocation,
    },
    projection::finding_fingerprint,
};

pub(crate) const MAX_FIELD_BYTES: usize = 512 * 1024;
pub(crate) const MAX_AGGREGATE_BYTES: usize = 2 * 1024 * 1024;
const MIN_ENTROPY_TOKEN: usize = 32;
const MAX_ENTROPY_TOKEN: usize = 256;
const ENTROPY_THRESHOLD: f64 = 4.5;

struct PatternDetector {
    name: &'static str,
    code: RepositoryWriteSafetyReasonCode,
    regex: Regex,
}

static PATTERNS: LazyLock<Vec<PatternDetector>> = LazyLock::new(|| {
    [
        (
            "private_key",
            RepositoryWriteSafetyReasonCode::PrivateKey,
            r"(?i)-----BEGIN (?:(?:ENCRYPTED |RSA |EC |OPENSSH |DSA )?PRIVATE KEY|PGP PRIVATE KEY(?: BLOCK)?)-----",
        ),
        (
            "authorization_header",
            RepositoryWriteSafetyReasonCode::AuthorizationHeader,
            r"(?im)^\s*(?:proxy-)?authorization\s*:\s*(?:bearer|basic)\s+\S+",
        ),
        (
            "known_credential_prefix",
            RepositoryWriteSafetyReasonCode::CredentialToken,
            r"(?i)(?:AKIA|ASIA)[A-Z0-9]{16}|gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}|xox[baprs]-[A-Za-z0-9-]{10,}|sk_(?:live|test)_[A-Za-z0-9]{16,}|npm_[A-Za-z0-9]{20,}|AIza[0-9A-Za-z_-]{30,}",
        ),
        (
            "jwt",
            RepositoryWriteSafetyReasonCode::SessionToken,
            r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        ),
        (
            "cookie_or_session",
            RepositoryWriteSafetyReasonCode::SessionToken,
            r"(?i)(?:set-cookie|cookie)\s*:\s*[^\r\n]+|(?:session(?:id)?|csrf(?:_token)?|auth(?:_token)?)\s*[=:]\s*[^\s,;]{12,}",
        ),
        (
            "connection_string",
            RepositoryWriteSafetyReasonCode::ConnectionString,
            r"(?i)(?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis|amqps?|mssql)://[^\s]+",
        ),
        (
            "environment_secret",
            RepositoryWriteSafetyReasonCode::EnvironmentSecret,
            r"(?im)^\s*(?:export\s+)?(?:SECRET|TOKEN|PASSWORD|PASSWD|API_KEY|PRIVATE_KEY|ACCESS_KEY|[A-Z][A-Z0-9_]*(?:SECRET|TOKEN|PASSWORD|PASSWD|API_KEY|PRIVATE_KEY|ACCESS_KEY)[A-Z0-9_]*)\s*=\s*\S+",
        ),
        (
            "cloud_service_account",
            RepositoryWriteSafetyReasonCode::CloudCredential,
            r#"(?i)\"type\"\s*:\s*\"service_account\"|\"private_key_id\"\s*:|AccountKey=[^;\s]+|SharedAccessSignature\s+sr="#,
        ),
    ]
    .into_iter()
    .map(|(name, code, pattern)| PatternDetector {
        name,
        code,
        regex: Regex::new(pattern).expect("repository safety detector regex is valid"),
    })
    .collect()
});

static URL_CANDIDATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[a-z][a-z0-9+.-]{1,15}://[^\s<>]+").expect("credentialed URL regex is valid")
});

static TOKEN_CANDIDATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9_+/=-]{32,256}").expect("entropy token regex is valid")
});

pub(crate) fn scan_value(
    location: &str,
    kind: SafetyFieldKind,
    value: &[u8],
    findings: &mut Vec<RepositoryWriteSafetyFinding>,
) {
    let Ok(text) = std::str::from_utf8(value) else {
        push(
            findings,
            RepositoryWriteSafetyReasonCode::InvalidEncoding,
            location,
            "utf8",
            value,
        );
        return;
    };
    for detector in PATTERNS.iter() {
        if detector.regex.is_match(text) {
            push(
                findings,
                detector.code.clone(),
                location,
                detector.name,
                value,
            );
        }
    }
    if contains_credentialed_url(text) {
        push(
            findings,
            RepositoryWriteSafetyReasonCode::CredentialedUrl,
            location,
            "credentialed_url",
            value,
        );
    }
    if !kind.entropy_exempt() && contains_high_entropy_token(text) {
        push(
            findings,
            RepositoryWriteSafetyReasonCode::HighEntropyValue,
            location,
            "bounded_entropy",
            value,
        );
    }
}

fn contains_credentialed_url(text: &str) -> bool {
    URL_CANDIDATE.find_iter(text).any(|candidate| {
        let candidate = candidate
            .as_str()
            .trim_end_matches(['.', ',', ')', ']', '}']);
        Url::parse(candidate).is_ok_and(|url| {
            !url.username().is_empty()
                || url.password().is_some()
                || url
                    .query_pairs()
                    .any(|(key, value)| is_sensitive_name(&key) && !value.trim().is_empty())
        })
    })
}

fn is_sensitive_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "passwd",
        "api_key",
        "apikey",
        "access_key",
    ]
    .iter()
    .any(|needle| name == *needle || name.ends_with(needle))
}

fn contains_high_entropy_token(text: &str) -> bool {
    TOKEN_CANDIDATE.find_iter(text).any(|candidate| {
        let token = candidate.as_str().trim_matches('=');
        if token.len() < MIN_ENTROPY_TOKEN || token.len() > MAX_ENTROPY_TOKEN {
            return false;
        }
        if token.bytes().all(|byte| byte.is_ascii_hexdigit()) || looks_like_placeholder(token) {
            return false;
        }
        let classes = [
            token.bytes().any(|byte| byte.is_ascii_lowercase()),
            token.bytes().any(|byte| byte.is_ascii_uppercase()),
            token.bytes().any(|byte| byte.is_ascii_digit()),
            token
                .bytes()
                .any(|byte| matches!(byte, b'_' | b'-' | b'+' | b'/')),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        classes >= 3 && shannon_entropy(token.as_bytes()) >= ENTROPY_THRESHOLD
    })
}

fn looks_like_placeholder(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    [
        "example",
        "placeholder",
        "replace",
        "redacted",
        "xxxxxxxx",
        "your_token",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn shannon_entropy(bytes: &[u8]) -> f64 {
    let mut counts = BTreeMap::new();
    for byte in bytes {
        *counts.entry(*byte).or_insert(0usize) += 1;
    }
    let length = bytes.len() as f64;
    counts
        .values()
        .map(|count| {
            let probability = *count as f64 / length;
            -probability * probability.log2()
        })
        .sum()
}

fn push(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detectors_find_credentials_without_retaining_them() {
        let sentinel = "ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789";
        let mut findings = Vec::new();
        scan_value(
            "candidate[0].body",
            SafetyFieldKind::Text,
            sentinel.as_bytes(),
            &mut findings,
        );
        assert!(!findings.is_empty());
        assert!(!format!("{findings:?}").contains(sentinel));
        assert!(!serde_json::to_string(&findings).unwrap().contains(sentinel));
    }

    #[test]
    fn safe_typed_identifiers_are_not_entropy_findings() {
        let mut findings = Vec::new();
        scan_value(
            "content_hash",
            SafetyFieldKind::TypedDigest,
            b"c2d84e7f95cbca99069f5fd1be2f8170a6eab226f311923a48f86087ad9e05db",
            &mut findings,
        );
        assert!(findings.is_empty());
    }
}
