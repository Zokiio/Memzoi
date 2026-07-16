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

    pub(super) fn create_checkpoint_avoiding(
        &self,
        actor: &str,
        input: &CheckpointInput,
        now: &str,
        reserved_ids: &BTreeSet<String>,
    ) -> Result<MemoryRecord> {
        write::create_checkpoint_avoiding(self.conn, actor, input, now, reserved_ids)
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

    pub(super) fn replace_snapshot_exact(&self, snapshot: &RuntimeRecordSnapshot) -> Result<()> {
        preservation::replace_runtime_record_snapshot_exact(self.conn, snapshot)
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
