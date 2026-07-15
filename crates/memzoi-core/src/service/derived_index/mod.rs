use std::path::PathBuf;

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::{MemoryPaths, MemoryRecord, okf};

mod drift;
mod rebuild;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildResult {
    pub records_root: PathBuf,
    pub db_path: PathBuf,
    pub record_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoIndexDrift {
    pub missing_from_index: Vec<String>,
    pub stale_in_index: Vec<String>,
    pub changed_in_index: Vec<String>,
    pub fts_out_of_sync: bool,
}

impl RepoIndexDrift {
    pub fn is_current(&self) -> bool {
        self.missing_from_index.is_empty()
            && self.stale_in_index.is_empty()
            && self.changed_in_index.is_empty()
            && !self.fts_out_of_sync
    }
}

pub(super) fn inspect(paths: &MemoryPaths, conn: &Connection) -> Result<RepoIndexDrift> {
    drift::inspect(paths, conn)
}

pub(super) fn rebuild(paths: MemoryPaths) -> Result<RebuildResult> {
    rebuild::rebuild(paths)
}

pub(super) fn record_matches(canonical: &okf::OkfRecordFile, indexed: &MemoryRecord) -> bool {
    drift::repo_record_matches(canonical, indexed)
}

pub(super) fn fts_is_current(conn: &Connection) -> Result<bool> {
    drift::fts_content_index_is_current(conn)
}
