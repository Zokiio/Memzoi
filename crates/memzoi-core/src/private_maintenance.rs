use serde::{Deserialize, Serialize};

pub const PRIVATE_MAINTENANCE_GRANT_SCHEMA: &str = "memzoi/private-maintenance-grant";
pub const PRIVATE_MAINTENANCE_RESULT_SCHEMA: &str = "memzoi/private-maintenance-result";
pub const PRIVATE_MAINTENANCE_INSPECTION_SCHEMA: &str = "memzoi/private-maintenance-inspection";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateMaintenanceGrantState {
    Active,
    Revoked,
}

impl PrivateMaintenanceGrantState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateMaintenanceProjectionState {
    Disabled,
    Current,
    Dirty,
    Blocked,
}

impl PrivateMaintenanceProjectionState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Current => "current",
            Self::Dirty => "dirty",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateMaintenanceGrant {
    pub schema: String,
    pub grant_id: String,
    pub grant_fingerprint: String,
    pub state: PrivateMaintenanceGrantState,
    pub policy_version: String,
    pub authorized_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateMaintenanceProjectionInspection {
    pub state: PrivateMaintenanceProjectionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    pub authoritative_generation: u64,
    pub policy_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detector_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    pub member_count: usize,
    pub edge_count: usize,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateMaintenanceInspection {
    pub schema: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_grant: Option<PrivateMaintenanceGrant>,
    pub projection: PrivateMaintenanceProjectionInspection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateMaintenanceOutcome {
    Enabled,
    Disabled,
    Reconciled,
    Unchanged,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateMaintenanceResult {
    pub schema: String,
    pub outcome: PrivateMaintenanceOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant: Option<PrivateMaintenanceGrant>,
    pub projection: PrivateMaintenanceProjectionInspection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateConflictParticipation {
    pub conflict_id: String,
    pub other_member_ids: Vec<String>,
    pub reason_code: String,
    pub resolution_state: String,
    pub recall_effect: String,
    pub detector_version: String,
    pub policy_version: String,
}
