use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    CanonicalLifecycleProjection, CanonicalRevision, ExpectedPriorRevision, MaintenanceAction,
    MaintenanceActionClass, MaintenanceActionGroupKind, MaintenanceAuthorityMode, MaintenancePlan,
    MaintenanceRecordVersion, MaintenanceScope, MaintenanceScopeBinding, MaterializationAction,
    MaterializationAuthorizationCapability, MaterializationCounterpartRelationship,
    MaterializationOutputOutcome, MaterializationOutputRole, MemoryStatus,
    REPOSITORY_WRITE_SAFETY_SCHEMA, RecordLineage,
    maintenance::validate_current_maintenance_snapshots, validate_canonical_record_id,
    validate_materialization_identity, validate_repository_relative_path,
};

pub const REPOSITORY_MAINTENANCE_MATERIALIZATION_REQUEST_SCHEMA: &str =
    "memzoi/repository-maintenance-materialization-request";
pub const REPOSITORY_MAINTENANCE_MATERIALIZATION_RESULT_SCHEMA: &str =
    "memzoi/repository-maintenance-materialization-result";
pub const REPOSITORY_MAINTENANCE_MATERIALIZATION_METADATA_SCHEMA: &str =
    "memzoi/repository-maintenance-materialization";
pub const REPOSITORY_MAINTENANCE_MATERIALIZATION_JOURNAL_SCHEMA: &str =
    "memzoi/repository-maintenance-materialization-journal";

const SELECTION_ID_DOMAIN: &str = "memzoi.repository-maintenance.selection-id";
const DECISION_ID_DOMAIN: &str = "memzoi.repository-maintenance.decision-id";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryMaintenanceMaterializationRequest {
    pub schema: String,
    pub plan_id: String,
    pub selected_action_ids: Vec<String>,
    pub decision_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedRepositoryMaintenanceSelection {
    pub plan_id: String,
    pub selection_id: String,
    pub selected_actions: Vec<MaintenanceAction>,
    pub output_record_ids: Vec<String>,
    pub comparison_record_ids: Vec<String>,
}

pub fn validate_repository_maintenance_selection(
    plan: &MaintenancePlan,
    request: &RepositoryMaintenanceMaterializationRequest,
) -> Result<ValidatedRepositoryMaintenanceSelection> {
    plan.validate()?;
    validate_current_maintenance_snapshots(plan)?;
    request.validate()?;
    if request.plan_id != plan.plan_id {
        bail!("maintenance materialization request does not match its plan");
    }
    if !matches!(plan.scope, MaintenanceScope::Repository { .. })
        || !matches!(
            plan.preconditions.scope,
            MaintenanceScopeBinding::Repository { .. }
        )
    {
        bail!("maintenance materialization requires repository scope and preconditions");
    }
    if plan.authority.mode != MaintenanceAuthorityMode::ReportOnly {
        bail!("maintenance materialization requires report-only authority");
    }
    let group = plan
        .action_groups
        .iter()
        .find(|group| group.kind == MaintenanceActionGroupKind::RepositoryMaterialization)
        .context("maintenance plan has no repository-materialization group")?;
    let mut selected_actions = Vec::with_capacity(request.selected_action_ids.len());
    for selected_id in &request.selected_action_ids {
        let action = group
            .actions
            .binary_search_by_key(&selected_id.as_str(), |action| action.action_id.as_str())
            .ok()
            .map(|index| group.actions[index].clone())
            .with_context(|| {
                format!("selected action {selected_id} is not a repository-materialization action")
            })?;
        if !matches!(
            action.class,
            MaintenanceActionClass::ConsolidateExactDuplicates
                | MaintenanceActionClass::CreateRenewalSuccessor
        ) {
            bail!("selected maintenance action class is unsupported");
        }
        selected_actions.push(action);
    }

    let records = plan
        .records
        .iter()
        .map(|record| (record.record_id.as_str(), record))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut occupied = BTreeSet::new();
    let mut outputs = BTreeSet::new();
    let mut comparisons = BTreeSet::new();
    for action in &selected_actions {
        for record_id in &action.record_ids {
            if !occupied.insert(record_id.clone()) {
                bail!("selected maintenance actions share a mutation or comparison record");
            }
            let snapshot = records
                .get(record_id.as_str())
                .context("selected maintenance action references an unknown plan record")?;
            if action.preconditions.record_versions.get(record_id) != Some(&snapshot.version) {
                bail!("selected maintenance action preconditions do not match plan snapshots");
            }
            let MaintenanceRecordVersion::CanonicalRepository {
                source_path,
                revision: _,
            } = &snapshot.version
            else {
                bail!("selected maintenance records must use canonical repository revisions");
            };
            let expected = format!(".memzoi/records/{record_id}.md");
            if source_path != &expected {
                bail!("selected maintenance record does not use its canonical path");
            }
        }
        match action.class {
            MaintenanceActionClass::ConsolidateExactDuplicates => {
                let keeper = action
                    .keeper_record_id
                    .as_ref()
                    .context("duplicate consolidation has no keeper")?;
                comparisons.insert(keeper.clone());
                for record_id in &action.record_ids {
                    if record_id != keeper && !outputs.insert(record_id.clone()) {
                        bail!("selected maintenance actions produce a duplicate output path");
                    }
                }
            }
            MaintenanceActionClass::CreateRenewalSuccessor => {
                for record_id in [
                    action
                        .predecessor_record_id
                        .as_ref()
                        .context("renewal action has no predecessor")?,
                    action
                        .evidence_record_id
                        .as_ref()
                        .context("renewal action has no evidence record")?,
                ] {
                    if !outputs.insert(record_id.clone()) {
                        bail!("selected maintenance actions produce a duplicate output path");
                    }
                }
            }
            _ => bail!("selected maintenance action class is unsupported"),
        }
    }
    if outputs
        .iter()
        .any(|record_id| comparisons.contains(record_id))
    {
        bail!("selected maintenance comparison records cannot also be outputs");
    }
    let selection_id =
        repository_maintenance_selection_id(&request.schema, &plan.plan_id, &selected_actions)?;
    Ok(ValidatedRepositoryMaintenanceSelection {
        plan_id: plan.plan_id.clone(),
        selection_id,
        selected_actions,
        output_record_ids: outputs.into_iter().collect(),
        comparison_record_ids: comparisons.into_iter().collect(),
    })
}

impl RepositoryMaintenanceMaterializationRequest {
    pub fn validate(&self) -> Result<()> {
        if self.schema != REPOSITORY_MAINTENANCE_MATERIALIZATION_REQUEST_SCHEMA {
            bail!("unsupported repository maintenance materialization request schema");
        }
        validate_materialization_identity(&self.plan_id, "maintenance plan_id")?;
        if self.selected_action_ids.is_empty() {
            bail!("repository maintenance materialization requires at least one action");
        }
        validate_sorted_unique_action_ids(&self.selected_action_ids)?;
        validate_canonical_utc_timestamp(&self.decision_at, "maintenance decision_at")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryMaintenanceOutputIntent {
    pub action_id: String,
    pub path: String,
    pub record_id: String,
    pub action: MaterializationAction,
    pub role: MaterializationOutputRole,
    pub expected_prior_revision: ExpectedPriorRevision,
    pub intended_semantic_revision: CanonicalRevision,
    pub reason: String,
}

impl RepositoryMaintenanceOutputIntent {
    pub fn validate(&self) -> Result<()> {
        validate_materialization_identity(&self.action_id, "maintenance action_id")?;
        validate_repository_relative_path(&self.path)?;
        validate_canonical_record_id(&self.record_id)?;
        self.expected_prior_revision.validate()?;
        self.intended_semantic_revision.validate()?;
        validate_reason(&self.reason)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryMaintenanceDecisionBinding {
    pub policy_version: String,
    pub policy_digest: String,
    pub safety_contract: String,
    pub authorization_capability: MaterializationAuthorizationCapability,
    pub outputs: Vec<RepositoryMaintenanceOutputIntent>,
    pub decision_at: String,
}

impl RepositoryMaintenanceDecisionBinding {
    pub fn validate(&self) -> Result<()> {
        validate_trimmed(&self.policy_version, "maintenance policy_version")?;
        validate_materialization_identity(&self.policy_digest, "maintenance policy_digest")?;
        if self.safety_contract != REPOSITORY_WRITE_SAFETY_SCHEMA {
            bail!("maintenance decision uses an unsupported safety contract");
        }
        if self.authorization_capability != MaterializationAuthorizationCapability::ExplicitCli {
            bail!("maintenance decisions require explicit CLI authorization");
        }
        if self.outputs.is_empty() {
            bail!("maintenance decision must contain at least one output");
        }
        validate_ordered_outputs(&self.outputs)?;
        validate_canonical_utc_timestamp(&self.decision_at, "maintenance decision_at")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryReviewCommand {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryMaintenanceMaterializationOutputResult {
    pub action_id: String,
    pub path: String,
    pub record_id: String,
    pub action: MaterializationAction,
    pub role: MaterializationOutputRole,
    pub semantic_revision: CanonicalRevision,
    pub outcome: MaterializationOutputOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryMaintenanceMaterializationResult {
    pub schema: String,
    pub plan_id: String,
    pub selection_id: String,
    pub decision_id: String,
    pub decision_at: String,
    pub selected_actions: Vec<MaintenanceAction>,
    pub outputs: Vec<RepositoryMaintenanceMaterializationOutputResult>,
    pub review_commands: Vec<RepositoryReviewCommand>,
}

impl RepositoryMaintenanceMaterializationResult {
    pub fn validate(&self) -> Result<()> {
        if self.schema != REPOSITORY_MAINTENANCE_MATERIALIZATION_RESULT_SCHEMA {
            bail!("unsupported repository maintenance materialization result schema");
        }
        for (value, label) in [
            (&self.plan_id, "maintenance plan_id"),
            (&self.selection_id, "maintenance selection_id"),
            (&self.decision_id, "maintenance decision_id"),
        ] {
            validate_materialization_identity(value, label)?;
        }
        validate_canonical_utc_timestamp(&self.decision_at, "maintenance decision_at")?;
        if self.selected_actions.is_empty() || self.outputs.is_empty() {
            bail!("maintenance materialization result cannot be empty");
        }
        let mut paths = BTreeSet::new();
        for output in &self.outputs {
            validate_materialization_identity(&output.action_id, "maintenance action_id")?;
            validate_repository_relative_path(&output.path)?;
            validate_canonical_record_id(&output.record_id)?;
            output.semantic_revision.validate()?;
            if !paths.insert(output.path.as_str()) {
                bail!("maintenance result contains duplicate output paths");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceCounterpart {
    pub record_id: String,
    pub expected_prior_revision: CanonicalRevision,
    pub relationship: MaterializationCounterpartRelationship,
}

impl MaintenanceCounterpart {
    fn validate(&self) -> Result<()> {
        validate_canonical_record_id(&self.record_id)?;
        self.expected_prior_revision.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriorCanonicalLifecycleProjection {
    pub status: MemoryStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineage: Option<RecordLineage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<MaterializationAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterpart_record_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterpart_expected_revision: Option<CanonicalRevision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterpart_relationship: Option<MaterializationCounterpartRelationship>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PriorCanonicalLifecycleProjection {
    pub fn lifecycle_projection(&self) -> CanonicalLifecycleProjection {
        CanonicalLifecycleProjection {
            action: self.action,
            target_expected_revision: self.counterpart_expected_revision.clone(),
            counterpart_record_id: self.counterpart_record_id.clone(),
            counterpart_relationship: self.counterpart_relationship,
            reason: self.reason.clone(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(record_id) = self.supersedes_id.as_deref() {
            validate_canonical_record_id(record_id)?;
        }
        if let Some(lineage) = self.lineage.as_ref() {
            lineage.validate()?;
        }
        self.lifecycle_projection().validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryMaintenanceMaterializationMetadata {
    pub schema: String,
    pub action_id: String,
    pub action: MaterializationAction,
    pub role: MaterializationOutputRole,
    pub plan_id: String,
    pub selection_id: String,
    pub decision_id: String,
    pub decision_at: String,
    pub policy_version: String,
    pub policy_digest: String,
    pub safety_contract: String,
    pub authorization_capability: MaterializationAuthorizationCapability,
    pub expected_prior_revision: ExpectedPriorRevision,
    pub intended_semantic_revision: CanonicalRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterpart: Option<MaintenanceCounterpart>,
    pub reason: String,
    pub prior_lifecycle: PriorCanonicalLifecycleProjection,
}

impl RepositoryMaintenanceMaterializationMetadata {
    pub fn validate(&self) -> Result<()> {
        if self.schema != REPOSITORY_MAINTENANCE_MATERIALIZATION_METADATA_SCHEMA {
            bail!("unsupported repository maintenance materialization metadata schema");
        }
        for (value, label) in [
            (&self.action_id, "maintenance action_id"),
            (&self.plan_id, "maintenance plan_id"),
            (&self.selection_id, "maintenance selection_id"),
            (&self.decision_id, "maintenance decision_id"),
            (&self.policy_digest, "maintenance policy_digest"),
        ] {
            validate_materialization_identity(value, label)?;
        }
        validate_canonical_utc_timestamp(&self.decision_at, "maintenance decision_at")?;
        validate_trimmed(&self.policy_version, "maintenance policy_version")?;
        if self.safety_contract != REPOSITORY_WRITE_SAFETY_SCHEMA {
            bail!("maintenance metadata uses an unsupported safety contract");
        }
        if self.authorization_capability != MaterializationAuthorizationCapability::ExplicitCli {
            bail!("maintenance metadata requires explicit CLI authorization");
        }
        self.expected_prior_revision.validate()?;
        self.intended_semantic_revision.validate()?;
        validate_reason(&self.reason)?;
        self.prior_lifecycle.validate()?;
        validate_counterpart_shape(self.action, self.role, self.counterpart.as_ref())
    }

    pub fn lifecycle_projection(&self) -> CanonicalLifecycleProjection {
        CanonicalLifecycleProjection {
            action: Some(self.action),
            target_expected_revision: self
                .counterpart
                .as_ref()
                .map(|counterpart| counterpart.expected_prior_revision.clone()),
            counterpart_record_id: self
                .counterpart
                .as_ref()
                .map(|counterpart| counterpart.record_id.clone()),
            counterpart_relationship: self
                .counterpart
                .as_ref()
                .map(|counterpart| counterpart.relationship),
            reason: matches!(
                self.action,
                MaterializationAction::Supersede | MaterializationAction::Tombstone
            )
            .then(|| self.reason.clone()),
        }
    }
}

pub fn repository_maintenance_selection_id(
    schema: &str,
    plan_id: &str,
    selected_actions: &[MaintenanceAction],
) -> Result<String> {
    if schema != REPOSITORY_MAINTENANCE_MATERIALIZATION_REQUEST_SCHEMA {
        bail!("unsupported repository maintenance materialization request schema");
    }
    validate_materialization_identity(plan_id, "maintenance plan_id")?;
    if selected_actions.is_empty()
        || !selected_actions
            .windows(2)
            .all(|window| window[0].action_id < window[1].action_id)
    {
        bail!("selected maintenance actions must be sorted and unique");
    }
    domain_identity(
        SELECTION_ID_DOMAIN,
        &serde_json::json!({
            "schema": schema,
            "plan_id": plan_id,
            "selected_actions": selected_actions,
        }),
    )
}

pub fn repository_maintenance_decision_id(
    selection_id: &str,
    binding: &RepositoryMaintenanceDecisionBinding,
) -> Result<String> {
    validate_materialization_identity(selection_id, "maintenance selection_id")?;
    binding.validate()?;
    domain_identity(
        DECISION_ID_DOMAIN,
        &serde_json::json!({
            "selection_id": selection_id,
            "binding": binding,
        }),
    )
}

fn validate_counterpart_shape(
    action: MaterializationAction,
    role: MaterializationOutputRole,
    counterpart: Option<&MaintenanceCounterpart>,
) -> Result<()> {
    let expected = match (action, role) {
        (MaterializationAction::Supersede, MaterializationOutputRole::CanonicalRecord) => {
            Some(MaterializationCounterpartRelationship::Supersedes)
        }
        (MaterializationAction::Supersede, MaterializationOutputRole::LifecycleCounterpart) => {
            Some(MaterializationCounterpartRelationship::SupersededBy)
        }
        (MaterializationAction::Tombstone, MaterializationOutputRole::LifecycleCounterpart) => {
            Some(MaterializationCounterpartRelationship::Tombstones)
        }
        (MaterializationAction::Create | MaterializationAction::Update, _) => None,
        _ => bail!("unsupported maintenance action and output-role combination"),
    };
    match (expected, counterpart) {
        (None, None) => Ok(()),
        (Some(expected), Some(counterpart)) => {
            counterpart.validate()?;
            if counterpart.relationship != expected {
                bail!("maintenance counterpart relationship does not match action and role");
            }
            Ok(())
        }
        (None, Some(_)) => bail!("maintenance output cannot contain a counterpart"),
        (Some(_), None) => bail!("maintenance output requires a counterpart"),
    }
}

fn validate_sorted_unique_action_ids(ids: &[String]) -> Result<()> {
    for id in ids {
        validate_materialization_identity(id, "maintenance action_id")?;
    }
    if !ids.windows(2).all(|window| window[0] < window[1]) {
        bail!("maintenance action IDs must be strictly sorted and unique");
    }
    Ok(())
}

fn validate_ordered_outputs(outputs: &[RepositoryMaintenanceOutputIntent]) -> Result<()> {
    for output in outputs {
        output.validate()?;
    }
    let keys: Vec<_> = outputs
        .iter()
        .map(|output| {
            (
                output.path.as_str(),
                role_rank(output.role),
                output.action_id.as_str(),
            )
        })
        .collect();
    if !keys.windows(2).all(|window| window[0] < window[1]) {
        bail!("maintenance outputs must use canonical ordering and unique paths");
    }
    if outputs
        .windows(2)
        .any(|window| window[0].path == window[1].path)
    {
        bail!("maintenance outputs cannot share a path");
    }
    Ok(())
}

pub(crate) fn role_rank(role: MaterializationOutputRole) -> u8 {
    match role {
        MaterializationOutputRole::CanonicalRecord => 0,
        MaterializationOutputRole::LifecycleCounterpart => 1,
    }
}

fn validate_reason(reason: &str) -> Result<()> {
    validate_trimmed(reason, "maintenance reason")?;
    if reason.len() > crate::MAX_MATERIALIZATION_REASON_BYTES || reason.contains(['\n', '\r']) {
        bail!("maintenance reason is not a bounded single line");
    }
    Ok(())
}

fn validate_trimmed(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value != value.trim() {
        bail!("{label} must be a non-empty trimmed string");
    }
    Ok(())
}

pub(crate) fn validate_canonical_utc_timestamp(value: &str, label: &str) -> Result<()> {
    if value != value.trim() || !value.ends_with('Z') {
        bail!("{label} must use canonical UTC RFC 3339 syntax");
    }
    let parsed = OffsetDateTime::parse(value, &Rfc3339)
        .with_context(|| format!("{label} must be an RFC 3339 timestamp"))?;
    let canonical = parsed
        .to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .with_context(|| format!("failed to canonicalize {label}"))?;
    if canonical != value {
        bail!("{label} must use canonical UTC RFC 3339 syntax");
    }
    Ok(())
}

fn domain_identity(domain: &str, value: &impl Serialize) -> Result<String> {
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

    #[test]
    fn request_requires_sorted_unique_actions_and_canonical_utc() {
        let identity = |byte: char| format!("blake3:{}", byte.to_string().repeat(64));
        let request = RepositoryMaintenanceMaterializationRequest {
            schema: REPOSITORY_MAINTENANCE_MATERIALIZATION_REQUEST_SCHEMA.to_owned(),
            plan_id: identity('a'),
            selected_action_ids: vec![identity('b'), identity('c')],
            decision_at: "2026-07-20T10:00:00Z".to_owned(),
        };
        request.validate().expect("current request should validate");

        let mut duplicate = request.clone();
        duplicate.selected_action_ids[1] = duplicate.selected_action_ids[0].clone();
        assert!(duplicate.validate().is_err());

        let mut offset = request;
        offset.decision_at = "2026-07-20T12:00:00+02:00".to_owned();
        assert!(offset.validate().is_err());
    }
}
