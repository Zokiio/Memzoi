use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Component, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    MemoryDestination, MemoryPaths, OriginIdentity, OriginOutcome, OriginOutcomeKind,
    OriginPreparation, OriginRoute, okf,
};

use super::{
    repository_mutation::repository_transaction_root,
    safe_files::{
        RepoLifecycleLock, ensure_safe_directory, ensure_safe_existing_file, sync_directory,
    },
};

const JOURNAL_SCHEMA: &str = "memzoi/import-origin-journal-v1";
const JOURNAL_PREFIX: &str = ".import-origin-";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImportOriginJournal {
    schema: String,
    pub(super) plan_id: String,
    pub(super) recorded_at: String,
    pub(super) entries: Vec<ImportOriginJournalEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImportOriginJournalEntry {
    pub(super) candidate_index: usize,
    pub(super) identity: OriginIdentity,
    pub(super) input_fingerprint: String,
    pub(super) proposal_id: String,
    pub(super) relative_path: PathBuf,
    pub(super) artifact_digest: String,
}

impl ImportOriginJournal {
    pub(super) fn new(
        paths: &MemoryPaths,
        plan_id: impl Into<String>,
        recorded_at: impl Into<String>,
        entries: Vec<ImportOriginJournalEntry>,
    ) -> Result<Self> {
        let journal = Self {
            schema: JOURNAL_SCHEMA.to_owned(),
            plan_id: plan_id.into(),
            recorded_at: recorded_at.into(),
            entries,
        };
        journal.validate(paths)?;
        Ok(journal)
    }

    fn validate(&self, paths: &MemoryPaths) -> Result<()> {
        ensure!(
            self.schema == JOURNAL_SCHEMA,
            "unsupported import origin journal schema {:?}",
            self.schema
        );
        ensure!(
            !self.plan_id.trim().is_empty(),
            "import journal plan_id is empty"
        );
        OffsetDateTime::parse(&self.recorded_at, &Rfc3339)
            .context("import journal recorded_at is not RFC3339")?;
        ensure!(
            !self.entries.is_empty(),
            "import origin journal has no artifacts"
        );
        for entry in &self.entries {
            entry.identity.validate()?;
            ensure!(
                entry.identity.repository_key == paths.repository_key(),
                "import origin journal belongs to a different repository authority"
            );
            ensure!(
                entry.identity.route == OriginRoute::Import,
                "import origin journal contains a non-import origin"
            );
            ensure_digest(&entry.input_fingerprint, "input fingerprint")?;
            ensure_digest(&entry.artifact_digest, "artifact digest")?;
            ensure!(
                !entry.proposal_id.trim().is_empty(),
                "import journal proposal_id is empty"
            );
            validate_relative_path(&entry.relative_path)?;
            let absolute = paths.project_root.join(&entry.relative_path);
            ensure!(
                absolute.starts_with(paths.proposals_dir().join("pending")),
                "import journal artifact is outside the pending proposal root"
            );
        }
        Ok(())
    }
}

impl ImportOriginJournalEntry {
    pub(super) fn new(
        paths: &MemoryPaths,
        candidate_index: usize,
        identity: OriginIdentity,
        input_fingerprint: String,
        proposal_id: String,
        path: &std::path::Path,
        markdown: &str,
    ) -> Result<Self> {
        let relative_path = path
            .strip_prefix(&paths.project_root)
            .context("import proposal is outside the project root")?
            .to_path_buf();
        Ok(Self {
            candidate_index,
            identity,
            input_fingerprint,
            proposal_id,
            relative_path,
            artifact_digest: blake3::hash(markdown.as_bytes()).to_hex().to_string(),
        })
    }
}

pub(super) fn write_journal(paths: &MemoryPaths, journal: &ImportOriginJournal) -> Result<()> {
    journal.validate(paths)?;
    let root = repository_transaction_root(paths);
    ensure_safe_directory(
        &paths.runtime_dir,
        &root,
        true,
        "import origin journal root",
    )?;
    let destination = journal_path(paths, &journal.plan_id);
    if destination.exists() {
        bail!(
            "import origin journal already exists for plan {}",
            journal.plan_id
        );
    }
    let temporary = root.join(format!(".{JOURNAL_PREFIX}{}.tmp", Uuid::now_v7()));
    let bytes = serde_json::to_vec_pretty(journal)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .context("failed to create staged import origin journal")?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(error).context("failed to persist import origin journal");
    }
    drop(file);
    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("failed to install import origin journal");
    }
    sync_directory(&root)?;
    Ok(())
}

pub(super) fn remove_journal(paths: &MemoryPaths, journal: &ImportOriginJournal) -> Result<()> {
    let path = journal_path(paths, &journal.plan_id);
    match fs::remove_file(&path) {
        Ok(()) => sync_directory(
            path.parent()
                .context("import origin journal has no parent directory")?,
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("failed to remove import origin journal"),
    }
}

pub(super) fn recover_pending(paths: &MemoryPaths, conn: &Connection) -> Result<usize> {
    let root = repository_transaction_root(paths);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).context("failed to scan import origin journals"),
    };
    let mut journals = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(JOURNAL_PREFIX) || !name.ends_with(".json") {
            continue;
        }
        let metadata = entry.metadata()?;
        ensure!(
            metadata.is_file(),
            "import origin journal must be a regular file"
        );
        let bytes = fs::read(entry.path())?;
        let journal: ImportOriginJournal =
            serde_json::from_slice(&bytes).context("failed to parse import origin journal")?;
        journal.validate(paths)?;
        ensure!(
            journal_path(paths, &journal.plan_id) == entry.path(),
            "import origin journal path does not match its plan identity"
        );
        journals.push(journal);
    }
    journals.sort_by(|left, right| left.plan_id.cmp(&right.plan_id));

    let mut recovered = 0usize;
    for journal in journals {
        let tx = conn.unchecked_transaction()?;
        for entry in &journal.entries {
            let absolute = paths.project_root.join(&entry.relative_path);
            match fs::symlink_metadata(&absolute) {
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("failed to inspect import artifact"),
                Ok(_) => {}
            }
            ensure_safe_existing_file(
                &paths.project_root,
                &paths.proposals_dir().join("pending"),
                &absolute,
                "import origin recovery artifact",
            )?;
            let markdown = fs::read_to_string(&absolute)
                .context("failed to read import origin recovery artifact")?;
            ensure!(
                blake3::hash(markdown.as_bytes()).to_hex().as_str() == entry.artifact_digest,
                "import recovery artifact digest changed: {}",
                entry.relative_path.display()
            );
            let parsed = okf::parse_okf_proposal_markdown(
                paths.proposals_dir().join("pending"),
                &absolute,
                &markdown,
            )?
            .context("import recovery artifact is not an OKF proposal")?;
            ensure!(
                parsed.id == entry.proposal_id,
                "import recovery proposal identity changed"
            );
            ensure!(
                OriginIdentity::new(paths.repository_key(), parsed.origin.clone())
                    == entry.identity,
                "import recovery artifact origin identity changed"
            );

            let expected = OriginOutcome::new(
                entry.identity.clone(),
                &entry.input_fingerprint,
                OriginOutcomeKind::Created,
                &journal.recorded_at,
            )
            .with_destination(MemoryDestination::Repo)
            .with_proposal_id(&entry.proposal_id);
            match crate::prepare_origin(
                &tx,
                &entry.identity,
                &entry.input_fingerprint,
                &journal.recorded_at,
            )? {
                OriginPreparation::Acquired | OriginPreparation::Pending(_) => {
                    let recorded = crate::finalize_origin(&tx, &expected)?;
                    ensure!(
                        recorded == expected,
                        "import recovery origin outcome changed"
                    );
                }
                OriginPreparation::Replay(recorded) => {
                    ensure!(
                        recorded == expected,
                        "import recovery replay outcome changed"
                    );
                }
            }
            recovered += 1;
        }
        tx.commit()?;
        remove_journal(paths, &journal)?;
    }
    Ok(recovered)
}

pub(super) fn recover_on_open(paths: &MemoryPaths, conn: &Connection) -> Result<()> {
    if !has_pending_journals(paths)? {
        return Ok(());
    }
    let _lifecycle_lock = RepoLifecycleLock::acquire(paths)?;
    recover_pending(paths, conn)
        .context("failed to recover interrupted import origin finalization")?;
    Ok(())
}

fn has_pending_journals(paths: &MemoryPaths) -> Result<bool> {
    let root = repository_transaction_root(paths);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("failed to scan import origin journals"),
    };
    for entry in entries {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(JOURNAL_PREFIX) && name.ends_with(".json") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn journal_path(paths: &MemoryPaths, plan_id: &str) -> PathBuf {
    let digest = blake3::hash(plan_id.as_bytes()).to_hex();
    repository_transaction_root(paths).join(format!("{JOURNAL_PREFIX}{digest}.json"))
}

fn ensure_digest(value: &str, label: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "import origin journal has an invalid {label}"
    );
    Ok(())
}

fn validate_relative_path(path: &std::path::Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty(),
        "import journal artifact path is empty"
    );
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!("import journal artifact path contains traversal");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        MemoryLane, MemoryType, OkfProposalSensitivity, OriginDescriptor, RepositoryContentClass,
        RetentionFacts, ScopeKind, retention::RETENTION_POLICY_VERSION,
    };

    #[test]
    fn recovery_finalizes_origin_when_exact_repository_artifact_exists() -> Result<()> {
        let project = tempfile::tempdir()?;
        let runtime = tempfile::tempdir()?;
        let paths = MemoryPaths::with_runtime_home(
            project.path().to_path_buf(),
            runtime.path().to_path_buf(),
        );
        fs::create_dir_all(&paths.runtime_dir)?;
        let pending = paths.proposals_dir().join("pending");
        fs::create_dir_all(&pending)?;
        let recorded_at = "2026-07-18T12:00:00Z";
        let descriptor = OriginDescriptor::new("import:event:recovery", OriginRoute::Import);
        let proposal = okf::plan_okf_create_proposal(
            &pending,
            &okf::OkfCreateProposalDraft {
                proposal_id: "mem_import_recovery".to_owned(),
                memory_type: MemoryType::Fact,
                lane: MemoryLane::Semantic,
                title: "Recovered import".to_owned(),
                body: "The artifact is the durable authority for origin recovery.".to_owned(),
                actor: "test".to_owned(),
                timestamp: recorded_at.to_owned(),
                reason: None,
                scope_kind: ScopeKind::Repo,
                scope_id: None,
                applies_to: Vec::new(),
                tags: Vec::new(),
                sources: Vec::new(),
                sensitivity: OkfProposalSensitivity::RepoSafe,
                content_class: RepositoryContentClass::GeneralRepoKnowledge,
                capture: None,
                retention: RetentionFacts {
                    policy_version: RETENTION_POLICY_VERSION.to_owned(),
                    occurred_at: None,
                    started_at: None,
                    last_continued_at: None,
                    closed_at: None,
                    explicit_expires_at: None,
                    episodic_extension: None,
                },
                origin: descriptor.clone(),
                lineage: None,
            },
        )?;
        fs::write(&proposal.path, &proposal.markdown)?;

        let identity = OriginIdentity::new(paths.repository_key(), descriptor);
        let fingerprint = crate::origin_input_fingerprint(
            OriginRoute::Import,
            &serde_json::json!({"fixture": "exact-artifact"}),
        )?;
        let entry = ImportOriginJournalEntry::new(
            &paths,
            0,
            identity.clone(),
            fingerprint.clone(),
            proposal.proposal_id.clone(),
            &proposal.path,
            &proposal.markdown,
        )?;
        let journal =
            ImportOriginJournal::new(&paths, "import-plan-recovery", recorded_at, vec![entry])?;
        write_journal(&paths, &journal)?;

        let conn = Connection::open_in_memory()?;
        crate::init_database(&conn)?;
        assert_eq!(recover_pending(&paths, &conn)?, 1);
        let expected = OriginOutcome::new(
            identity.clone(),
            &fingerprint,
            OriginOutcomeKind::Created,
            recorded_at,
        )
        .with_destination(MemoryDestination::Repo)
        .with_proposal_id(&proposal.proposal_id);
        assert_eq!(
            crate::lookup_origin(&conn, &identity, &fingerprint)?,
            crate::OriginLookup::Replay(expected)
        );
        assert_eq!(recover_pending(&paths, &conn)?, 0);
        Ok(())
    }
}
