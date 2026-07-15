use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    CaptureCandidate, CaptureProvenance, MemoryDestination, MemoryLane, MemoryRecord, MemoryType,
};

mod preservation;
mod query;
mod write;

pub(super) use self::preservation::RuntimeRecordSnapshot;

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

pub(super) struct RuntimeRecords<'a> {
    conn: &'a Connection,
}

impl<'a> RuntimeRecords<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub(super) fn create_local(
        &self,
        actor: &str,
        input: &LocalMemoryInput,
        now: &str,
    ) -> Result<MemoryRecord> {
        write::create_local_memory(self.conn, actor, input, now)
    }

    pub(super) fn create_checkpoint(
        &self,
        actor: &str,
        input: &CheckpointInput,
        now: &str,
    ) -> Result<MemoryRecord> {
        write::create_checkpoint(self.conn, actor, input, now)
    }

    pub(super) fn create_capture(
        &self,
        actor: &str,
        candidate: &CaptureCandidate,
        destination: MemoryDestination,
        now: &str,
        provenance: CaptureProvenance,
    ) -> Result<MemoryRecord> {
        write::create_capture(self.conn, actor, candidate, destination, now, provenance)
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

    pub(super) fn records_for_preservation(&self) -> Result<Vec<MemoryRecord>> {
        query::records_for_runtime_preservation(self.conn)
    }

    pub(super) fn snapshots(&self) -> Result<Vec<RuntimeRecordSnapshot>> {
        preservation::runtime_record_snapshots(self.conn)
    }

    pub(super) fn tags(&self, record_id: &str) -> Result<Vec<String>> {
        preservation::record_tags(self.conn, record_id)
    }

    pub(super) fn restore_snapshots(&self, records: &[RuntimeRecordSnapshot]) -> Result<()> {
        preservation::restore_runtime_record_snapshots(self.conn, records)
    }

    #[cfg(test)]
    pub(super) fn insert_for_test(&self, record: &MemoryRecord) -> Result<()> {
        write::insert_memory_record_row(self.conn, record, write::InsertMode::Create)
    }
}
