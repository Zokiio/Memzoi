use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::memory_policy::MemoryPlane;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    Fact,
    Preference,
    Decision,
    Procedure,
    Episode,
    Relationship,
    Warning,
    FailedAttempt,
    Risk,
    InstructionProjection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLane {
    Session,
    #[default]
    Semantic,
    Episodic,
    Procedural,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryDestination {
    Repo,
    Local,
    Session,
    Discard,
    NeedsReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Proposed,
    Active,
    Rejected,
    Superseded,
    Expired,
    Tombstoned,
    Redacted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Personal,
    Repo,
    Project,
    Team,
    Org,
    Agent,
    ImportedUntrusted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Private,
    Repo,
    Team,
    Org,
}

/// Export boundary for an event payload. This is explicit because some read
/// audit events carry private data without being attached to one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEventDataClass {
    Repository,
    Private,
}

impl MemoryType {
    pub fn as_str(self) -> &'static str {
        enum_to_str(self)
    }
}

impl MemoryLane {
    pub fn as_str(self) -> &'static str {
        lane_to_str(self)
    }
}

impl MemoryDestination {
    pub fn as_str(self) -> &'static str {
        destination_to_str(self)
    }
}

impl MemoryStatus {
    pub fn as_str(self) -> &'static str {
        status_to_str(self)
    }
}

impl ScopeKind {
    pub fn as_str(self) -> &'static str {
        scope_to_str(self)
    }
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        visibility_to_str(self)
    }
}

impl MemoryEventDataClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repository => "repository",
            Self::Private => "private",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub destination: MemoryDestination,
    pub scope_kind: ScopeKind,
    pub scope_id: Option<String>,
    pub visibility: Visibility,
    pub title: String,
    pub body: String,
    pub status: MemoryStatus,
    pub confidence: f64,
    pub source_kind: Option<String>,
    pub source_ref: Option<String>,
    /// Review packet that approved this canonical record, kept separate from evidence provenance.
    pub proposal_id: Option<String>,
    /// Versioned capture evidence and review lineage, when this record originated from capture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<crate::CaptureProvenance>,
    pub content_hash: String,
    pub created_at: String,
    pub updated_at: String,
    pub supersedes_id: Option<String>,
    pub retention: crate::RetentionFacts,
    pub origin: crate::OriginDescriptor,
    pub lineage: Option<crate::RecordLineage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryProposal {
    pub id: String,
    pub operation: String,
    pub payload: Value,
    pub status: String,
    pub actor: String,
    pub validation: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryEvent {
    pub id: String,
    pub event_type: String,
    pub actor: String,
    pub data_class: MemoryEventDataClass,
    pub payload: Value,
    pub record_id: Option<String>,
    pub proposal_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryPath {
    pub path: String,
    pub symbol: Option<String>,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryCitation {
    pub record_id: String,
    pub memory_type: MemoryType,
    pub scope_kind: ScopeKind,
    pub provenance: MemoryPlane,
    pub destination: MemoryDestination,
    pub visibility: Visibility,
    pub source_kind: Option<String>,
    pub source_ref: Option<String>,
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<crate::CaptureProvenance>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchRanking {
    pub score: f64,
    pub signals: SearchRankingSignals,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchRankingSignals {
    pub fts_match: bool,
    pub fts_score: f64,
    pub path_score: i64,
    pub type_priority: i64,
    pub lane_priority: i64,
    pub destination_priority: i64,
    pub confidence: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub record: MemoryRecord,
    pub score: f64,
    pub snippet: Option<String>,
    pub rationale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranking: Option<SearchRanking>,
    pub paths: Vec<MemoryPath>,
    pub citations: Vec<MemoryCitation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPackBudget {
    pub requested: Option<usize>,
    pub effective: usize,
    pub estimated_used: usize,
    pub estimate_unit: String,
    pub candidate_records: usize,
    pub selected_records: usize,
    pub rendered_words: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPackPolicy {
    pub include_local: bool,
    pub include_session: bool,
    pub requested_destinations: Vec<MemoryDestination>,
    pub included_destinations: Vec<MemoryDestination>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextPackIncludedItem {
    pub record_id: String,
    pub title: String,
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub scope_kind: ScopeKind,
    pub path: Option<String>,
    pub citation: MemoryCitation,
    pub provenance: MemoryPlane,
    pub destination: MemoryDestination,
    pub score: f64,
    pub rationale: Option<String>,
    pub estimated_size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPackOmittedItem {
    pub record_id: String,
    pub title: String,
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub destination: MemoryDestination,
    pub estimated_size: usize,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPackWarning {
    pub code: String,
    pub provenance: MemoryPlane,
    pub destination: MemoryDestination,
    pub matching_count: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextPack {
    pub id: String,
    pub task: String,
    pub prompt: String,
    pub records: Vec<SearchResult>,
    pub citations: Vec<MemoryCitation>,
    pub token_budget: Option<usize>,
    pub policy: ContextPackPolicy,
    pub budget: ContextPackBudget,
    pub included: Vec<ContextPackIncludedItem>,
    pub omitted: Vec<ContextPackOmittedItem>,
    pub warnings: Vec<ContextPackWarning>,
    pub next_queries: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalInboxSummary {
    pub source: String,
    pub open_total: usize,
    pub pending: usize,
    pub validated: usize,
    pub approved: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffPack {
    pub id: String,
    pub task: String,
    pub path_prefix: Option<String>,
    pub token_budget: Option<usize>,
    pub include_local: bool,
    pub include_session: bool,
    pub proposal_inbox: ProposalInboxSummary,
    pub context: ContextPack,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrecheckWarning {
    pub id: String,
    pub record_id: String,
    pub message: String,
    pub severity: String,
    pub citations: Vec<MemoryCitation>,
    pub suggested_next_step: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryDestinationClassification {
    pub destination: MemoryDestination,
    pub reason: String,
}

impl fmt::Display for MemoryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(enum_to_str(*self))
    }
}

impl fmt::Display for MemoryLane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(lane_to_str(*self))
    }
}

impl fmt::Display for MemoryDestination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(destination_to_str(*self))
    }
}

impl fmt::Display for MemoryStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(status_to_str(*self))
    }
}

impl fmt::Display for ScopeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(scope_to_str(*self))
    }
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(visibility_to_str(*self))
    }
}

impl FromStr for MemoryType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "fact" => Ok(Self::Fact),
            "preference" => Ok(Self::Preference),
            "decision" => Ok(Self::Decision),
            "procedure" => Ok(Self::Procedure),
            "episode" => Ok(Self::Episode),
            "relationship" => Ok(Self::Relationship),
            "warning" => Ok(Self::Warning),
            "failed_attempt" => Ok(Self::FailedAttempt),
            "risk" => Ok(Self::Risk),
            "instruction_projection" => Ok(Self::InstructionProjection),
            other => Err(format!("unknown memory type {other:?}")),
        }
    }
}

impl FromStr for MemoryLane {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "session" => Ok(Self::Session),
            "semantic" => Ok(Self::Semantic),
            "episodic" => Ok(Self::Episodic),
            "procedural" => Ok(Self::Procedural),
            other => Err(format!("unknown memory lane {other:?}")),
        }
    }
}

impl FromStr for MemoryDestination {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "repo" => Ok(Self::Repo),
            "local" => Ok(Self::Local),
            "session" => Ok(Self::Session),
            "discard" => Ok(Self::Discard),
            "needs_review" => Ok(Self::NeedsReview),
            other => Err(format!("unknown memory destination {other:?}")),
        }
    }
}

impl FromStr for MemoryStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "proposed" => Ok(Self::Proposed),
            "active" => Ok(Self::Active),
            "rejected" => Ok(Self::Rejected),
            "superseded" => Ok(Self::Superseded),
            "expired" => Ok(Self::Expired),
            "tombstoned" => Ok(Self::Tombstoned),
            "redacted" => Ok(Self::Redacted),
            other => Err(format!("unknown memory status {other:?}")),
        }
    }
}

impl FromStr for ScopeKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "personal" => Ok(Self::Personal),
            "repo" => Ok(Self::Repo),
            "project" => Ok(Self::Project),
            "team" => Ok(Self::Team),
            "org" => Ok(Self::Org),
            "agent" => Ok(Self::Agent),
            "imported_untrusted" => Ok(Self::ImportedUntrusted),
            other => Err(format!("unknown scope kind {other:?}")),
        }
    }
}

impl FromStr for Visibility {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "public" => Ok(Self::Public),
            "private" => Ok(Self::Private),
            "repo" => Ok(Self::Repo),
            "team" => Ok(Self::Team),
            "org" => Ok(Self::Org),
            other => Err(format!("unknown visibility {other:?}")),
        }
    }
}

impl FromStr for MemoryEventDataClass {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "repository" => Ok(Self::Repository),
            "private" => Ok(Self::Private),
            other => Err(format!("unknown memory event data class {other:?}")),
        }
    }
}

pub fn enum_to_str(value: MemoryType) -> &'static str {
    match value {
        MemoryType::Fact => "fact",
        MemoryType::Preference => "preference",
        MemoryType::Decision => "decision",
        MemoryType::Procedure => "procedure",
        MemoryType::Episode => "episode",
        MemoryType::Relationship => "relationship",
        MemoryType::Warning => "warning",
        MemoryType::FailedAttempt => "failed_attempt",
        MemoryType::Risk => "risk",
        MemoryType::InstructionProjection => "instruction_projection",
    }
}

pub fn lane_to_str(value: MemoryLane) -> &'static str {
    match value {
        MemoryLane::Session => "session",
        MemoryLane::Semantic => "semantic",
        MemoryLane::Episodic => "episodic",
        MemoryLane::Procedural => "procedural",
    }
}

pub fn destination_to_str(value: MemoryDestination) -> &'static str {
    match value {
        MemoryDestination::Repo => "repo",
        MemoryDestination::Local => "local",
        MemoryDestination::Session => "session",
        MemoryDestination::Discard => "discard",
        MemoryDestination::NeedsReview => "needs_review",
    }
}

pub fn status_to_str(value: MemoryStatus) -> &'static str {
    match value {
        MemoryStatus::Proposed => "proposed",
        MemoryStatus::Active => "active",
        MemoryStatus::Rejected => "rejected",
        MemoryStatus::Superseded => "superseded",
        MemoryStatus::Expired => "expired",
        MemoryStatus::Tombstoned => "tombstoned",
        MemoryStatus::Redacted => "redacted",
    }
}

pub fn scope_to_str(value: ScopeKind) -> &'static str {
    match value {
        ScopeKind::Personal => "personal",
        ScopeKind::Repo => "repo",
        ScopeKind::Project => "project",
        ScopeKind::Team => "team",
        ScopeKind::Org => "org",
        ScopeKind::Agent => "agent",
        ScopeKind::ImportedUntrusted => "imported_untrusted",
    }
}

pub fn visibility_to_str(value: Visibility) -> &'static str {
    match value {
        Visibility::Public => "public",
        Visibility::Private => "private",
        Visibility::Repo => "repo",
        Visibility::Team => "team",
        Visibility::Org => "org",
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use serde::{Serialize, de::DeserializeOwned};

    use crate::models::{
        MemoryDestination, MemoryDestinationClassification, MemoryEventDataClass, MemoryLane,
        MemoryStatus, MemoryType, ScopeKind, Visibility,
    };

    #[test]
    fn memory_model_enums_round_trip_as_stable_schema_strings() -> anyhow::Result<()> {
        assert_json_string_round_trip(MemoryType::Fact, "fact")?;
        assert_json_string_round_trip(MemoryType::Preference, "preference")?;
        assert_json_string_round_trip(MemoryType::Decision, "decision")?;
        assert_json_string_round_trip(MemoryType::Procedure, "procedure")?;
        assert_json_string_round_trip(MemoryType::Episode, "episode")?;
        assert_json_string_round_trip(MemoryType::Relationship, "relationship")?;
        assert_json_string_round_trip(MemoryType::Warning, "warning")?;
        assert_json_string_round_trip(MemoryType::FailedAttempt, "failed_attempt")?;
        assert_json_string_round_trip(MemoryType::Risk, "risk")?;
        assert_json_string_round_trip(MemoryType::InstructionProjection, "instruction_projection")?;

        assert_json_string_round_trip(MemoryLane::Session, "session")?;
        assert_json_string_round_trip(MemoryLane::Semantic, "semantic")?;
        assert_json_string_round_trip(MemoryLane::Episodic, "episodic")?;
        assert_json_string_round_trip(MemoryLane::Procedural, "procedural")?;

        assert_json_string_round_trip(MemoryDestination::Repo, "repo")?;
        assert_json_string_round_trip(MemoryDestination::Local, "local")?;
        assert_json_string_round_trip(MemoryDestination::Session, "session")?;
        assert_json_string_round_trip(MemoryDestination::Discard, "discard")?;
        assert_json_string_round_trip(MemoryDestination::NeedsReview, "needs_review")?;

        assert_json_string_round_trip(MemoryStatus::Proposed, "proposed")?;
        assert_json_string_round_trip(MemoryStatus::Active, "active")?;
        assert_json_string_round_trip(MemoryStatus::Rejected, "rejected")?;
        assert_json_string_round_trip(MemoryStatus::Superseded, "superseded")?;
        assert_json_string_round_trip(MemoryStatus::Expired, "expired")?;
        assert_json_string_round_trip(MemoryStatus::Tombstoned, "tombstoned")?;
        assert_json_string_round_trip(MemoryStatus::Redacted, "redacted")?;

        assert_json_string_round_trip(ScopeKind::Personal, "personal")?;
        assert_json_string_round_trip(ScopeKind::Repo, "repo")?;
        assert_json_string_round_trip(ScopeKind::Project, "project")?;
        assert_json_string_round_trip(ScopeKind::Team, "team")?;
        assert_json_string_round_trip(ScopeKind::Org, "org")?;
        assert_json_string_round_trip(ScopeKind::Agent, "agent")?;
        assert_json_string_round_trip(ScopeKind::ImportedUntrusted, "imported_untrusted")?;

        assert_json_string_round_trip(Visibility::Public, "public")?;
        assert_json_string_round_trip(Visibility::Private, "private")?;
        assert_json_string_round_trip(Visibility::Repo, "repo")?;
        assert_json_string_round_trip(Visibility::Team, "team")?;
        assert_json_string_round_trip(Visibility::Org, "org")?;

        assert_json_string_round_trip(MemoryEventDataClass::Repository, "repository")?;
        assert_json_string_round_trip(MemoryEventDataClass::Private, "private")?;

        Ok(())
    }

    #[test]
    fn memory_model_enums_reject_unknown_schema_strings() {
        assert_invalid_json_string::<MemoryType>("note");
        assert_invalid_json_string::<MemoryLane>("memoir");
        assert_invalid_json_string::<MemoryDestination>("cloud");
        assert_invalid_json_string::<MemoryStatus>("archived");
        assert_invalid_json_string::<ScopeKind>("workspace");
        assert_invalid_json_string::<Visibility>("friends");
        assert_invalid_json_string::<MemoryEventDataClass>("public");
    }

    #[test]
    fn memory_destination_classification_serializes_as_json() -> anyhow::Result<()> {
        let classification = MemoryDestinationClassification {
            destination: MemoryDestination::NeedsReview,
            reason: "sensitivity boundary is unclear".to_owned(),
        };

        let json = serde_json::to_value(&classification)?;
        assert_eq!(
            json,
            serde_json::json!({
                "destination": "needs_review",
                "reason": "sensitivity boundary is unclear"
            })
        );

        let decoded: MemoryDestinationClassification = serde_json::from_value(json)?;
        assert_eq!(decoded, classification);
        Ok(())
    }

    fn assert_json_string_round_trip<T>(value: T, encoded: &str) -> anyhow::Result<()>
    where
        T: Clone + Debug + Eq + Serialize + DeserializeOwned,
    {
        let json = serde_json::to_string(&value)?;
        assert_eq!(json, format!("\"{encoded}\""));

        let decoded: T = serde_json::from_str(&json)?;
        assert_eq!(decoded, value);
        Ok(())
    }

    fn assert_invalid_json_string<T>(encoded: &str)
    where
        T: Debug + DeserializeOwned,
    {
        let json = format!("\"{encoded}\"");
        let error = serde_json::from_str::<T>(&json).expect_err("invalid enum string must fail");
        let message = error.to_string();
        assert!(
            message.contains(encoded),
            "error should name the rejected value {encoded:?}, got {message:?}"
        );
    }
}
