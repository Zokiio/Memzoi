use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Number, Value};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub const PRIVATE_LIFECYCLE_REQUEST_SCHEMA: &str = "memzoi/private-lifecycle-request";
pub const PRIVATE_LIFECYCLE_GRANT_SCHEMA: &str = "memzoi/private-lifecycle-grant";
pub const PRIVATE_LIFECYCLE_RESULT_SCHEMA: &str = "memzoi/private-lifecycle-result";
pub const PRIVATE_LIFECYCLE_RECORD_INSPECTION_SCHEMA: &str =
    "memzoi/private-lifecycle-record-inspection";
pub const PRIVATE_LIFECYCLE_POLICY_VERSION: &str = "private-lifecycle-policy/1";
pub const PRIVATE_LIFECYCLE_MAX_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;
pub const PRIVATE_LIFECYCLE_MAX_ACTIONS: usize = 64;
pub const PRIVATE_LIFECYCLE_MAX_TARGETS: usize = 256;
pub const PRIVATE_LIFECYCLE_MAX_REASON_CODE_BYTES: usize = 128;
const PRIVATE_LIFECYCLE_MAX_OPERATION_ID_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrivateLifecycleSource {
    Direct,
    MaintenancePlan {
        plan_id: String,
        selected_action_ids: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateCorrectionReplacement {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrivateLifecycleAction {
    ExtendAutomaticRecall {
        record_id: String,
        expected_version: String,
        recall_until: String,
    },
    ExtendValidity {
        record_id: String,
        expected_version: String,
        validity_until: String,
    },
    RetainUntil {
        record_id: String,
        expected_version: String,
        retain_until: String,
    },
    Pin {
        record_id: String,
        expected_version: String,
    },
    Unpin {
        record_id: String,
        expected_version: String,
    },
    RenewFromEvidence {
        predecessor_id: String,
        expected_predecessor_version: String,
        evidence_record_id: String,
        expected_evidence_version: String,
    },
    Correct {
        record_id: String,
        expected_version: String,
        replacement: PrivateCorrectionReplacement,
        reason_code: String,
        evidence_versions: BTreeMap<String, String>,
    },
    Supersede {
        record_id: String,
        expected_version: String,
        successor_record_id: String,
        expected_successor_version: String,
        reason_code: String,
    },
    Consolidate {
        record_ids: Vec<String>,
        expected_versions: BTreeMap<String, String>,
        keeper_record_id: String,
    },
    ResolveContradiction {
        record_ids: Vec<String>,
        expected_versions: BTreeMap<String, String>,
        winner_record_id: String,
    },
    Quarantine {
        record_id: String,
        expected_version: String,
        reason_code: String,
    },
    ReleaseQuarantine {
        record_id: String,
        expected_version: String,
    },
}

impl PrivateLifecycleAction {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ExtendAutomaticRecall { .. } => "extend_automatic_recall",
            Self::ExtendValidity { .. } => "extend_validity",
            Self::RetainUntil { .. } => "retain_until",
            Self::Pin { .. } => "pin",
            Self::Unpin { .. } => "unpin",
            Self::RenewFromEvidence { .. } => "renew_from_evidence",
            Self::Correct { .. } => "correct",
            Self::Supersede { .. } => "supersede",
            Self::Consolidate { .. } => "consolidate",
            Self::ResolveContradiction { .. } => "resolve_contradiction",
            Self::Quarantine { .. } => "quarantine",
            Self::ReleaseQuarantine { .. } => "release_quarantine",
        }
    }

    /// Records whose lifecycle, content, status, or relations can change when
    /// this action is applied. Evidence-only references are intentionally not
    /// returned here.
    pub fn mutation_targets(&self) -> Vec<&str> {
        match self {
            Self::ExtendAutomaticRecall { record_id, .. }
            | Self::ExtendValidity { record_id, .. }
            | Self::RetainUntil { record_id, .. }
            | Self::Pin { record_id, .. }
            | Self::Unpin { record_id, .. }
            | Self::Quarantine { record_id, .. }
            | Self::ReleaseQuarantine { record_id, .. } => vec![record_id],
            Self::RenewFromEvidence {
                predecessor_id,
                evidence_record_id,
                ..
            } => vec![predecessor_id, evidence_record_id],
            Self::Correct { record_id, .. } => vec![record_id],
            Self::Supersede {
                record_id,
                successor_record_id,
                ..
            } => vec![record_id, successor_record_id],
            Self::Consolidate { record_ids, .. }
            | Self::ResolveContradiction { record_ids, .. } => {
                record_ids.iter().map(String::as_str).collect()
            }
        }
    }

    /// All exact versions bound by this action, including evidence-only
    /// references that cannot be allowed to change between authorization and
    /// application.
    pub fn expected_versions(&self) -> BTreeMap<&str, &str> {
        match self {
            Self::ExtendAutomaticRecall {
                record_id,
                expected_version,
                ..
            }
            | Self::ExtendValidity {
                record_id,
                expected_version,
                ..
            }
            | Self::RetainUntil {
                record_id,
                expected_version,
                ..
            }
            | Self::Pin {
                record_id,
                expected_version,
            }
            | Self::Unpin {
                record_id,
                expected_version,
            }
            | Self::Quarantine {
                record_id,
                expected_version,
                ..
            }
            | Self::ReleaseQuarantine {
                record_id,
                expected_version,
            } => BTreeMap::from([(record_id.as_str(), expected_version.as_str())]),
            Self::RenewFromEvidence {
                predecessor_id,
                expected_predecessor_version,
                evidence_record_id,
                expected_evidence_version,
            } => BTreeMap::from([
                (
                    predecessor_id.as_str(),
                    expected_predecessor_version.as_str(),
                ),
                (
                    evidence_record_id.as_str(),
                    expected_evidence_version.as_str(),
                ),
            ]),
            Self::Correct {
                record_id,
                expected_version,
                evidence_versions,
                ..
            } => {
                let mut versions =
                    BTreeMap::from([(record_id.as_str(), expected_version.as_str())]);
                versions.extend(
                    evidence_versions
                        .iter()
                        .map(|(id, version)| (id.as_str(), version.as_str())),
                );
                versions
            }
            Self::Supersede {
                record_id,
                expected_version,
                successor_record_id,
                expected_successor_version,
                ..
            } => BTreeMap::from([
                (record_id.as_str(), expected_version.as_str()),
                (
                    successor_record_id.as_str(),
                    expected_successor_version.as_str(),
                ),
            ]),
            Self::Consolidate {
                expected_versions, ..
            }
            | Self::ResolveContradiction {
                expected_versions, ..
            } => expected_versions
                .iter()
                .map(|(id, version)| (id.as_str(), version.as_str()))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateLifecycleRequest {
    pub schema: String,
    pub request_id: String,
    pub operation_id: String,
    pub source: PrivateLifecycleSource,
    pub actions: Vec<PrivateLifecycleAction>,
}

impl PrivateLifecycleRequest {
    pub fn with_computed_id(
        operation_id: impl Into<String>,
        source: PrivateLifecycleSource,
        actions: Vec<PrivateLifecycleAction>,
    ) -> Result<Self> {
        let mut request = Self {
            schema: PRIVATE_LIFECYCLE_REQUEST_SCHEMA.to_owned(),
            request_id: String::new(),
            operation_id: operation_id.into(),
            source,
            actions,
        };
        request.request_id = request.compute_request_id()?;
        request.validate()?;
        Ok(request)
    }

    pub fn compute_request_id(&self) -> Result<String> {
        #[derive(Serialize)]
        struct Identity<'a> {
            schema: &'a str,
            operation_id: &'a str,
            source: &'a PrivateLifecycleSource,
            actions: &'a [PrivateLifecycleAction],
        }

        let bytes = serde_json_canonicalizer::to_vec(&Identity {
            schema: &self.schema,
            operation_id: &self.operation_id,
            source: &self.source,
            actions: &self.actions,
        })
        .context("failed to canonicalize private lifecycle request identity")?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"memzoi/private-lifecycle-request-id/v1\0");
        hasher.update(&bytes);
        Ok(format!("blake3:{}", hasher.finalize().to_hex()))
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == PRIVATE_LIFECYCLE_REQUEST_SCHEMA,
            "unsupported private lifecycle request schema"
        );
        validate_operation_id(&self.operation_id)?;
        ensure!(
            (1..=PRIVATE_LIFECYCLE_MAX_ACTIONS).contains(&self.actions.len()),
            "private lifecycle request must contain between 1 and {PRIVATE_LIFECYCLE_MAX_ACTIONS} actions"
        );
        match &self.source {
            PrivateLifecycleSource::Direct => {}
            PrivateLifecycleSource::MaintenancePlan {
                plan_id,
                selected_action_ids,
            } => {
                validate_identifier(plan_id, "maintenance plan_id")?;
                ensure!(
                    !selected_action_ids.is_empty(),
                    "planned lifecycle request must select at least one action"
                );
                let selected = selected_action_ids.iter().collect::<BTreeSet<_>>();
                ensure!(
                    selected.len() == selected_action_ids.len(),
                    "planned lifecycle request contains duplicate selected_action_ids"
                );
                for action_id in selected_action_ids {
                    validate_identifier(action_id, "selected maintenance action_id")?;
                }
            }
        }

        let mut mutation_targets = BTreeSet::new();
        let mut evidence_only = BTreeSet::new();
        let mut bound_versions = BTreeMap::<String, String>::new();
        for action in &self.actions {
            validate_action(action)?;
            let action_targets = action
                .mutation_targets()
                .into_iter()
                .collect::<BTreeSet<_>>();
            for target in &action_targets {
                ensure!(
                    !evidence_only.contains(*target),
                    "private lifecycle mutation target {target} is also used as evidence"
                );
                ensure!(
                    mutation_targets.insert((*target).to_owned()),
                    "private lifecycle mutation target {target} participates in more than one action"
                );
                ensure!(
                    mutation_targets.len() <= PRIVATE_LIFECYCLE_MAX_TARGETS,
                    "private lifecycle request exceeds {PRIVATE_LIFECYCLE_MAX_TARGETS} distinct mutation targets"
                );
            }
            for (record_id, version) in action.expected_versions() {
                if let Some(prior) = bound_versions.insert(record_id.to_owned(), version.to_owned())
                {
                    ensure!(
                        prior == version,
                        "private record {record_id} has conflicting expected versions"
                    );
                }
                if !action_targets.contains(record_id) {
                    ensure!(
                        !mutation_targets.contains(record_id),
                        "private evidence record {record_id} is also a mutation target"
                    );
                    evidence_only.insert(record_id.to_owned());
                }
            }
        }
        let expected = self.compute_request_id()?;
        ensure!(
            self.request_id == expected,
            "private lifecycle request_id does not match its canonical contents"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerActionGrantState {
    Active,
    Consumed,
    Revoked,
}

impl OwnerActionGrantState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Consumed => "consumed",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateLifecycleGrant {
    pub schema: String,
    pub grant_id: String,
    pub request_id: String,
    pub operation_id: String,
    pub state: OwnerActionGrantState,
    pub authorized_at: String,
    pub expires_at: String,
    pub policy_version: String,
    pub source: PrivateLifecycleSource,
    pub action_kinds: Vec<String>,
    pub target_record_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumed_application_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateLifecycleRevokeOutcome {
    Revoked,
    AlreadyRevoked,
    AlreadyConsumed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateLifecycleRevokeResult {
    pub grant_id: String,
    pub outcome: PrivateLifecycleRevokeOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateLifecycleActionResult {
    pub kind: String,
    pub target_record_ids: Vec<String>,
    pub resulting_versions: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacement_record_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateLifecycleApplyResult {
    pub schema: String,
    pub application_id: String,
    pub operation_id: String,
    pub request_id: String,
    pub grant_id: String,
    pub applied_at: String,
    pub lifecycle_generation: u64,
    pub replayed: bool,
    pub actions: Vec<PrivateLifecycleActionResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateLifecycleRecordInspection {
    pub schema: String,
    pub record: crate::MemoryRecord,
    pub version: String,
    pub state: PrivateLifecycleStateSnapshot,
    pub base_eligibility: crate::CurrentAssertionDecision,
    pub effective_automatic_recall_eligibility: crate::CurrentAssertionDecision,
    pub conflicts: Vec<crate::PrivateConflictParticipation>,
    pub relations: Vec<PrivateLifecycleRelationSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateLifecycleStateSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_recall_until: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validity_until: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retain_until: Option<String>,
    pub pinned: bool,
    pub quarantined: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quarantine_reason_code: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateLifecycleRelationSnapshot {
    pub relation_id: String,
    pub kind: String,
    pub subject_record_id: String,
    pub related_record_id: String,
    pub application_id: String,
    pub created_at: String,
}

pub fn parse_private_lifecycle_request(input: &str) -> Result<PrivateLifecycleRequest> {
    let request: PrivateLifecycleRequest = parse_strict_lifecycle_artifact(input, "request")?;
    request.validate()?;
    Ok(request)
}

pub fn parse_strict_lifecycle_artifact<T>(input: &str, label: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    ensure!(
        !input.trim().is_empty(),
        "private lifecycle {label} cannot be empty"
    );
    ensure!(
        input.len() <= PRIVATE_LIFECYCLE_MAX_ARTIFACT_BYTES,
        "private lifecycle {label} exceeds the {} byte limit",
        PRIVATE_LIFECYCLE_MAX_ARTIFACT_BYTES
    );

    let value = if input.trim_start().starts_with(['{', '[']) {
        let mut deserializer = serde_json::Deserializer::from_str(input);
        let value = StrictValue::deserialize(&mut deserializer)
            .with_context(|| format!("failed to parse private lifecycle {label} as strict JSON"))?;
        deserializer
            .end()
            .with_context(|| format!("private lifecycle {label} contains trailing JSON data"))?;
        value.0
    } else {
        let mut documents = serde_yaml::Deserializer::from_str(input);
        let first = documents
            .next()
            .context("private lifecycle YAML artifact is empty")?;
        let value = StrictValue::deserialize(first)
            .with_context(|| format!("failed to parse private lifecycle {label} as strict YAML"))?;
        ensure!(
            documents.next().is_none(),
            "private lifecycle {label} contains trailing YAML documents"
        );
        value.0
    };

    serde_json::from_value(value)
        .with_context(|| format!("private lifecycle {label} has an invalid shape"))
}

fn validate_action(action: &PrivateLifecycleAction) -> Result<()> {
    for (record_id, version) in action.expected_versions() {
        validate_identifier(record_id, "private record_id")?;
        validate_version(version)?;
    }
    let targets = action.mutation_targets();
    let unique_targets = targets.iter().copied().collect::<BTreeSet<_>>();
    ensure!(
        targets.len() == unique_targets.len(),
        "{} contains duplicate record targets",
        action.kind()
    );

    match action {
        PrivateLifecycleAction::ExtendAutomaticRecall { recall_until, .. } => {
            parse_timestamp(recall_until, "recall_until")?;
        }
        PrivateLifecycleAction::ExtendValidity { validity_until, .. } => {
            parse_timestamp(validity_until, "validity_until")?;
        }
        PrivateLifecycleAction::RetainUntil { retain_until, .. } => {
            parse_timestamp(retain_until, "retain_until")?;
        }
        PrivateLifecycleAction::RenewFromEvidence {
            predecessor_id,
            evidence_record_id,
            ..
        } => ensure!(
            predecessor_id != evidence_record_id,
            "renewal evidence must be distinct from its predecessor"
        ),
        PrivateLifecycleAction::Correct {
            record_id,
            replacement,
            reason_code,
            evidence_versions,
            ..
        } => {
            ensure!(
                !replacement.title.trim().is_empty(),
                "correction title is required"
            );
            ensure!(
                !replacement.body.trim().is_empty(),
                "correction body is required"
            );
            validate_reason_code(reason_code)?;
            ensure!(
                !evidence_versions.is_empty(),
                "correction requires at least one exact versioned evidence record"
            );
            ensure!(
                !evidence_versions.contains_key(record_id),
                "correction target cannot also be correction evidence"
            );
        }
        PrivateLifecycleAction::Supersede {
            record_id,
            successor_record_id,
            reason_code,
            ..
        } => {
            ensure!(
                record_id != successor_record_id,
                "a record cannot supersede itself"
            );
            validate_reason_code(reason_code)?;
        }
        PrivateLifecycleAction::Consolidate {
            record_ids,
            expected_versions,
            keeper_record_id,
        } => validate_selection_set(
            "consolidation",
            record_ids,
            expected_versions,
            keeper_record_id,
        )?,
        PrivateLifecycleAction::ResolveContradiction {
            record_ids,
            expected_versions,
            winner_record_id,
        } => validate_selection_set(
            "contradiction resolution",
            record_ids,
            expected_versions,
            winner_record_id,
        )?,
        PrivateLifecycleAction::Quarantine { reason_code, .. } => {
            validate_reason_code(reason_code)?;
        }
        PrivateLifecycleAction::Pin { .. }
        | PrivateLifecycleAction::Unpin { .. }
        | PrivateLifecycleAction::ReleaseQuarantine { .. } => {}
    }
    Ok(())
}

fn validate_selection_set(
    label: &str,
    record_ids: &[String],
    expected_versions: &BTreeMap<String, String>,
    selected_record_id: &str,
) -> Result<()> {
    ensure!(
        record_ids.len() >= 2,
        "{label} requires at least two records"
    );
    let records = record_ids.iter().cloned().collect::<BTreeSet<_>>();
    ensure!(
        records.len() == record_ids.len(),
        "{label} contains duplicate record_ids"
    );
    let versioned = expected_versions.keys().cloned().collect::<BTreeSet<_>>();
    ensure!(
        records == versioned,
        "{label} expected_versions must match record_ids exactly"
    );
    ensure!(
        records.contains(selected_record_id),
        "{label} owner selection must be a member of the complete record set"
    );
    Ok(())
}

fn validate_operation_id(value: &str) -> Result<()> {
    validate_identifier(value, "operation_id")?;
    ensure!(
        value.len() <= PRIVATE_LIFECYCLE_MAX_OPERATION_ID_BYTES,
        "operation_id exceeds {PRIVATE_LIFECYCLE_MAX_OPERATION_ID_BYTES} UTF-8 bytes"
    );
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> Result<()> {
    ensure!(!value.trim().is_empty(), "{label} is required");
    ensure!(
        value == value.trim(),
        "{label} must not contain leading or trailing whitespace"
    );
    ensure!(
        !value.chars().any(char::is_control),
        "{label} must not contain control characters"
    );
    Ok(())
}

fn validate_version(value: &str) -> Result<()> {
    validate_identifier(value, "expected private record version")?;
    ensure!(
        value.len() == 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "expected private record version must be a 32-character lowercase hexadecimal token"
    );
    Ok(())
}

pub(crate) fn validate_reason_code(value: &str) -> Result<()> {
    validate_identifier(value, "reason_code")?;
    ensure!(
        value.len() <= PRIVATE_LIFECYCLE_MAX_REASON_CODE_BYTES,
        "reason_code exceeds {PRIVATE_LIFECYCLE_MAX_REASON_CODE_BYTES} UTF-8 bytes"
    );
    ensure!(
        value.bytes().all(|byte| byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || b"_-.:/".contains(&byte)),
        "reason_code must be machine-readable ASCII"
    );
    Ok(())
}

pub(crate) fn parse_timestamp(value: &str, label: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339)
        .with_context(|| format!("{label} must be an RFC 3339 timestamp"))
}

/// A serde value that rejects duplicate keys before typed deserialization.
/// This closes the usual JSON/YAML last-key-wins ambiguity for authority
/// artifacts.
struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> de::Visitor<'de> for StrictValueVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON/YAML value without duplicate mapping keys")
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        StrictValue::deserialize(deserializer)
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(StrictValue)
            .ok_or_else(|| E::custom("non-finite numbers are not allowed"))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut mapping: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        let mut values = serde_json::Map::new();
        while let Some(key) = mapping.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate mapping key: {key}")));
            }
            let value = mapping.next_value::<StrictValue>()?;
            values.insert(key, value.0);
        }
        Ok(StrictValue(Value::Object(values)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin_action(record_id: &str) -> PrivateLifecycleAction {
        PrivateLifecycleAction::Pin {
            record_id: record_id.to_owned(),
            expected_version: "0123456789abcdef0123456789abcdef".to_owned(),
        }
    }

    #[test]
    fn request_identity_is_recomputable() {
        let request = PrivateLifecycleRequest::with_computed_id(
            "owner-operation-1",
            PrivateLifecycleSource::Direct,
            vec![pin_action("local-remember-this")],
        )
        .expect("valid request");
        assert_eq!(request.request_id, request.compute_request_id().unwrap());
        assert!(request.request_id.starts_with("blake3:"));
    }

    #[test]
    fn request_rejects_overlapping_mutation_targets() {
        let error = PrivateLifecycleRequest::with_computed_id(
            "owner-operation-1",
            PrivateLifecycleSource::Direct,
            vec![
                pin_action("local-remember-this"),
                PrivateLifecycleAction::Quarantine {
                    record_id: "local-remember-this".to_owned(),
                    expected_version: "0123456789abcdef0123456789abcdef".to_owned(),
                    reason_code: "owner_request".to_owned(),
                },
            ],
        )
        .expect_err("overlap must fail");
        assert!(error.to_string().contains("participates in more than one"));
    }

    #[test]
    fn request_allows_shared_evidence_but_never_evidence_target_overlap() {
        let version = "0123456789abcdef0123456789abcdef";
        let correction = |record_id: &str| PrivateLifecycleAction::Correct {
            record_id: record_id.to_owned(),
            expected_version: version.to_owned(),
            replacement: PrivateCorrectionReplacement {
                title: "Corrected".to_owned(),
                body: "Corrected private claim.".to_owned(),
            },
            reason_code: "owner_correction".to_owned(),
            evidence_versions: BTreeMap::from([("local-evidence".to_owned(), version.to_owned())]),
        };
        PrivateLifecycleRequest::with_computed_id(
            "owner-operation-shared-evidence",
            PrivateLifecycleSource::Direct,
            vec![correction("local-a"), correction("local-b")],
        )
        .expect("disjoint corrections may reuse one exactly versioned evidence record");

        let error = PrivateLifecycleRequest::with_computed_id(
            "owner-operation-evidence-target-overlap",
            PrivateLifecycleSource::Direct,
            vec![correction("local-a"), pin_action("local-evidence")],
        )
        .expect_err("evidence must never also be mutated");
        assert!(error.to_string().contains("evidence"));
    }

    #[test]
    fn selection_requires_exact_versions_and_explicit_owner_winner() {
        let action = PrivateLifecycleAction::ResolveContradiction {
            record_ids: vec!["local-a".to_owned(), "local-b".to_owned()],
            expected_versions: BTreeMap::from([(
                "local-a".to_owned(),
                "0123456789abcdef0123456789abcdef".to_owned(),
            )]),
            winner_record_id: "local-b".to_owned(),
        };
        let mut request = PrivateLifecycleRequest {
            schema: PRIVATE_LIFECYCLE_REQUEST_SCHEMA.to_owned(),
            request_id: String::new(),
            operation_id: "owner-operation-1".to_owned(),
            source: PrivateLifecycleSource::Direct,
            actions: vec![action],
        };
        request.request_id = request.compute_request_id().unwrap();
        assert!(request.validate().is_err());
    }

    #[test]
    fn strict_parser_rejects_duplicate_json_keys() {
        let error = parse_private_lifecycle_request(
            r#"{
              "schema":"memzoi/private-lifecycle-request",
              "request_id":"first",
              "request_id":"second",
              "operation_id":"owner-operation-1",
              "source":{"kind":"direct"},
              "actions":[]
            }"#,
        )
        .expect_err("duplicate key must fail");
        assert!(error.to_string().contains("strict JSON"));
    }

    #[test]
    fn strict_parser_rejects_trailing_yaml_document() {
        let error = parse_strict_lifecycle_artifact::<Value>("a: 1\n---\nb: 2\n", "test")
            .expect_err("trailing YAML document must fail");
        assert!(error.to_string().contains("trailing YAML"));
    }
}
