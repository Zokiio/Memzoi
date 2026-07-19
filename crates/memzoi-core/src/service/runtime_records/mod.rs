use std::collections::BTreeSet;

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    CaptureCandidate, CaptureProvenance, MemoryDestination, MemoryLane, MemoryPaths, MemoryRecord,
    MemoryType, okf,
};

use super::safe_files::ensure_safe_directory;

mod lifecycle;
mod preservation;
mod private_lifecycle_storage;
mod query;
mod write;

pub(super) use self::lifecycle::CheckpointLifecycleMutation;
pub(super) use self::preservation::RuntimeRecordSnapshot;
pub(in crate::service) use self::private_lifecycle_storage::{
    OwnerActionGrantRow, OwnerActionGrantState, PrivateLifecycleApplicationRow,
    PrivateLifecycleRelation, PrivateLifecycleRelationKind, PrivateLifecycleState,
    PrivateLifecycleStorage, RevokeGrantOutcome, lifecycle_generation, set_lifecycle_generation,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalMemoryInput {
    pub memory_type: MemoryType,
    pub lane: MemoryLane,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointInput {
    pub task: String,
    pub note: String,
}

/// Idempotent creation of a new, unrelated checkpoint generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateCheckpointCommand {
    pub operation_id: String,
    pub input: CheckpointInput,
}

/// Idempotent continuation of an open checkpoint lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinueCheckpointCommand {
    pub operation_id: String,
    pub checkpoint_id: String,
    pub expected_version: String,
}

/// Idempotent terminal closure of a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseCheckpointCommand {
    pub operation_id: String,
    pub checkpoint_id: String,
    pub expected_version: String,
}

/// Idempotent creation of a successor after its predecessor has become terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateCheckpointSuccessorCommand {
    pub operation_id: String,
    pub predecessor_id: String,
    pub expected_predecessor_version: String,
    pub input: CheckpointInput,
}

/// Stable, content-free command result suitable for orchestrator retries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointCommandResult {
    pub operation_id: String,
    pub checkpoint_id: String,
    pub record_version: String,
    pub lifecycle_event_id: Option<String>,
    pub applied: bool,
    pub replayed: bool,
}

pub(super) struct RuntimeRecords<'a> {
    conn: &'a Connection,
}

impl<'a> RuntimeRecords<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    #[cfg(test)]
    pub(super) fn create_local(
        &self,
        actor: &str,
        input: &LocalMemoryInput,
        now: &str,
    ) -> Result<MemoryRecord> {
        write::create_local_memory(self.conn, actor, input, now)
    }

    pub(super) fn create_local_avoiding(
        &self,
        actor: &str,
        input: &LocalMemoryInput,
        now: &str,
        reserved_ids: &BTreeSet<String>,
    ) -> Result<MemoryRecord> {
        write::create_local_memory_avoiding(self.conn, actor, input, now, reserved_ids)
    }

    pub(super) fn create_local_with_metadata_avoiding(
        &self,
        actor: &str,
        input: &LocalMemoryInput,
        now: &str,
        origin: crate::OriginDescriptor,
        lineage: Option<crate::RecordLineage>,
        reserved_ids: &BTreeSet<String>,
    ) -> Result<MemoryRecord> {
        write::create_local_memory_with_metadata_avoiding(
            self.conn,
            actor,
            input,
            now,
            origin,
            lineage,
            reserved_ids,
        )
    }

    pub(super) fn create_local_with_id_for_trusted_recall_eval(
        &self,
        actor: &str,
        id: &str,
        input: &LocalMemoryInput,
        now: &str,
    ) -> Result<MemoryRecord> {
        write::create_local_memory_with_id_for_trusted_recall_eval(self.conn, actor, id, input, now)
    }

    pub(super) fn create_checkpoint_avoiding(
        &self,
        actor: &str,
        input: &CheckpointInput,
        now: &str,
        reserved_ids: &BTreeSet<String>,
    ) -> Result<MemoryRecord> {
        write::create_checkpoint_avoiding(self.conn, actor, input, now, reserved_ids)
    }

    pub(super) fn create_checkpoint_with_metadata_avoiding(
        &self,
        actor: &str,
        input: &CheckpointInput,
        now: &str,
        origin: crate::OriginDescriptor,
        lineage: Option<crate::RecordLineage>,
        reserved_ids: &BTreeSet<String>,
    ) -> Result<MemoryRecord> {
        write::create_checkpoint_with_metadata_avoiding(
            self.conn,
            actor,
            input,
            now,
            origin,
            lineage,
            reserved_ids,
        )
    }

    pub(super) fn create_checkpoint_with_id_for_trusted_recall_eval(
        &self,
        actor: &str,
        id: &str,
        input: &CheckpointInput,
        now: &str,
    ) -> Result<MemoryRecord> {
        write::create_checkpoint_with_id_for_trusted_recall_eval(self.conn, actor, id, input, now)
    }

    pub(super) fn create_capture(
        &self,
        actor: &str,
        candidate: &CaptureCandidate,
        destination: MemoryDestination,
        now: &str,
        provenance: CaptureProvenance,
        reserved_ids: &BTreeSet<String>,
    ) -> Result<MemoryRecord> {
        write::create_capture(
            self.conn,
            actor,
            candidate,
            destination,
            now,
            provenance,
            reserved_ids,
        )
    }

    pub(super) fn get(&self, record_id: &str) -> Result<Option<MemoryRecord>> {
        query::record_by_id(self.conn, record_id)
    }

    pub(super) fn active_for_destination(
        &self,
        destination: MemoryDestination,
        now: OffsetDateTime,
    ) -> Result<Vec<MemoryRecord>> {
        query::active_records_for_destination(self.conn, destination, now)
    }

    pub(super) fn indexed_active_for_destination(
        &self,
        destination: MemoryDestination,
    ) -> Result<Vec<MemoryRecord>> {
        query::indexed_active_records_for_destination(self.conn, destination)
    }

    pub(super) fn indexed_non_runtime_record_ids(&self) -> Result<Vec<String>> {
        query::indexed_non_runtime_record_ids(self.conn)
    }

    pub(super) fn active_checkpoints(&self, now: OffsetDateTime) -> Result<Vec<MemoryRecord>> {
        query::active_checkpoint_records(self.conn, now)
    }

    pub(super) fn checkpoint(
        &self,
        record_id: &str,
        now: OffsetDateTime,
    ) -> Result<Option<MemoryRecord>> {
        query::checkpoint_record(self.conn, record_id, now)
    }

    pub(super) fn checkpoint_for_lifecycle(&self, record_id: &str) -> Result<MemoryRecord> {
        lifecycle::checkpoint_for_lifecycle(self.conn, record_id)
    }

    pub(super) fn private_record_version(&self, record_id: &str) -> Result<String> {
        private_lifecycle_storage::private_record_version(self.conn, record_id)
    }

    pub(super) fn ensure_private_record_version(
        &self,
        record_id: &str,
        expected_version: &str,
    ) -> Result<()> {
        private_lifecycle_storage::ensure_private_record_version(
            self.conn,
            record_id,
            expected_version,
        )
    }

    #[allow(dead_code)]
    pub(super) fn rotate_private_record_version(
        &self,
        record_id: &str,
        updated_at: &str,
    ) -> Result<String> {
        private_lifecycle_storage::rotate_private_record_version(self.conn, record_id, updated_at)
    }

    pub(super) fn checkpoint_record_version(&self, record_id: &str) -> Result<String> {
        lifecycle::checkpoint_record_version(self.conn, record_id)
    }

    pub(super) fn ensure_successor_predecessor(
        &self,
        record: &MemoryRecord,
        expected_version: &str,
        now: OffsetDateTime,
    ) -> Result<()> {
        lifecycle::ensure_successor_predecessor(self.conn, record, expected_version, now)
    }

    pub(super) fn continue_checkpoint(
        &self,
        actor: &str,
        command: &ContinueCheckpointCommand,
        now: OffsetDateTime,
        timestamp: &str,
    ) -> Result<CheckpointLifecycleMutation> {
        lifecycle::continue_checkpoint(self.conn, actor, command, now, timestamp)
    }

    pub(super) fn close_checkpoint(
        &self,
        actor: &str,
        command: &CloseCheckpointCommand,
        now: OffsetDateTime,
        timestamp: &str,
    ) -> Result<CheckpointLifecycleMutation> {
        lifecycle::close_checkpoint(self.conn, actor, command, now, timestamp)
    }

    pub(super) fn records_for_preservation(&self) -> Result<Vec<MemoryRecord>> {
        query::records_for_runtime_preservation(self.conn)
    }

    pub(super) fn snapshots(&self) -> Result<Vec<RuntimeRecordSnapshot>> {
        preservation::runtime_record_snapshots(self.conn)
    }

    pub(super) fn snapshots_for_ids(
        &self,
        record_ids: &BTreeSet<String>,
    ) -> Result<Vec<RuntimeRecordSnapshot>> {
        preservation::runtime_record_snapshots_for_ids(self.conn, record_ids)
    }

    pub(super) fn tags(&self, record_id: &str) -> Result<Vec<String>> {
        preservation::record_tags(self.conn, record_id)
    }

    pub(super) fn restore_snapshots(&self, records: &[RuntimeRecordSnapshot]) -> Result<()> {
        preservation::restore_runtime_record_snapshots(self.conn, records)
    }

    #[cfg(test)]
    pub(super) fn insert_for_test(&self, record: &MemoryRecord) -> Result<()> {
        write::insert_memory_record_row(self.conn, record, write::InsertMode::Create).map(|_| ())
    }
}

pub(super) fn reserved_runtime_record_ids(
    paths: &MemoryPaths,
    index: &Connection,
) -> Result<BTreeSet<String>> {
    ensure_safe_directory(
        &paths.project_root,
        &paths.records_dir(),
        false,
        "canonical record root",
    )?;
    let mut reserved_ids = RuntimeRecords::new(index)
        .indexed_non_runtime_record_ids()?
        .into_iter()
        .collect::<BTreeSet<_>>();
    reserved_ids.extend(
        okf::read_okf_record_snapshots(paths.records_dir())?
            .into_iter()
            .map(|snapshot| snapshot.record.concept_id),
    );
    Ok(reserved_ids)
}
