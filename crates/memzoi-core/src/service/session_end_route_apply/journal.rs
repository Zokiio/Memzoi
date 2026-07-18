use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{MemoryPaths, OriginIdentity, OriginOutcomeKind};

use super::super::{
    repository_mutation::repository_transaction_root,
    safe_files::{ensure_safe_directory, ensure_safe_existing_file, sync_directory},
};
use super::CheckpointPromotion;

const JOURNAL_SCHEMA: &str = "memzoi/session-end-origin-journal";
const JOURNAL_PREFIX: &str = ".session-end-origin-";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionEndOriginJournal {
    pub schema: String,
    pub operation_id: String,
    pub actor: String,
    pub checkpoint_id: String,
    pub expected_version: String,
    pub evaluated_at: String,
    pub identity: OriginIdentity,
    pub input_fingerprint: String,
    pub intended_outcome: OriginOutcomeKind,
    pub artifacts: Vec<SessionEndOriginArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionEndOriginArtifact {
    pub proposal_id: String,
    pub relative_path: PathBuf,
    pub digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArtifactState {
    NoneInstalled,
    AllInstalled,
}

impl SessionEndOriginJournal {
    pub(super) fn validate(&self, paths: &MemoryPaths) -> Result<()> {
        ensure!(
            self.schema == JOURNAL_SCHEMA,
            "unsupported session-end origin journal schema {:?}",
            self.schema
        );
        ensure!(
            self.identity.repository_key == paths.repository_key(),
            "session-end origin journal belongs to a different repository authority"
        );
        ensure!(
            !self.operation_id.trim().is_empty(),
            "journal operation_id is empty"
        );
        ensure!(!self.actor.trim().is_empty(), "journal actor is empty");
        ensure!(
            !self.checkpoint_id.trim().is_empty(),
            "journal checkpoint_id is empty"
        );
        ensure!(
            !self.expected_version.trim().is_empty(),
            "journal expected_version is empty"
        );
        OffsetDateTime::parse(&self.evaluated_at, &Rfc3339)
            .context("journal evaluated_at is not RFC3339")?;
        ensure!(
            !self.artifacts.is_empty(),
            "journal has no repository artifacts"
        );
        ensure!(
            self.intended_outcome == OriginOutcomeKind::Created,
            "session-end repository journal has an unsupported intended outcome"
        );
        for artifact in &self.artifacts {
            ensure!(
                !artifact.proposal_id.trim().is_empty(),
                "journal proposal_id is empty"
            );
            ensure_digest(&artifact.digest)?;
            validate_relative_path(&artifact.relative_path)?;
            let absolute = paths.project_root.join(&artifact.relative_path);
            ensure!(
                absolute.starts_with(paths.proposals_dir()),
                "session-end journal artifact is outside the proposal root: {}",
                artifact.relative_path.display()
            );
        }
        Ok(())
    }
}

pub(super) fn journal_for(
    paths: &MemoryPaths,
    checkpoint: &CheckpointPromotion,
    actor: &str,
    evaluated_at: &str,
    artifacts: Vec<SessionEndOriginArtifact>,
) -> Result<SessionEndOriginJournal> {
    let journal = SessionEndOriginJournal {
        schema: JOURNAL_SCHEMA.to_owned(),
        operation_id: checkpoint.operation_id.clone(),
        actor: actor.to_owned(),
        checkpoint_id: checkpoint.checkpoint_id.clone(),
        expected_version: checkpoint.expected_version.clone(),
        evaluated_at: evaluated_at.to_owned(),
        identity: checkpoint.identity.clone(),
        input_fingerprint: checkpoint.fingerprint.clone(),
        intended_outcome: OriginOutcomeKind::Created,
        artifacts,
    };
    journal.validate(paths)?;
    Ok(journal)
}

pub(super) fn journal_path(paths: &MemoryPaths, identity: &OriginIdentity) -> PathBuf {
    let digest = blake3::hash(identity.origin_key.as_bytes()).to_hex();
    repository_transaction_root(paths).join(format!("{JOURNAL_PREFIX}{digest}.json"))
}

pub(super) fn write_journal(paths: &MemoryPaths, journal: &SessionEndOriginJournal) -> Result<()> {
    journal.validate(paths)?;
    let root = repository_transaction_root(paths);
    ensure_safe_directory(
        &paths.runtime_dir,
        &root,
        true,
        "session-end origin journal root",
    )?;
    let destination = journal_path(paths, &journal.identity);
    if destination.exists() {
        bail!(
            "session-end origin journal already exists: {}",
            destination.display()
        );
    }
    let temporary = root.join(format!(".{JOURNAL_PREFIX}{}.tmp", Uuid::now_v7()));
    let bytes = serde_json::to_vec_pretty(journal)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(|| {
            format!(
                "failed to create session-end journal {}",
                temporary.display()
            )
        })?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, &destination).with_context(|| {
        format!(
            "failed to install session-end origin journal {}",
            destination.display()
        )
    })?;
    sync_directory(&root)?;
    Ok(())
}

pub(super) fn remove_journal(paths: &MemoryPaths, journal: &SessionEndOriginJournal) -> Result<()> {
    let path = journal_path(paths, &journal.identity);
    match fs::remove_file(&path) {
        Ok(()) => sync_directory(
            path.parent()
                .context("session-end origin journal has no parent")?,
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove session-end journal {}", path.display())),
    }
}

pub(super) fn read_pending(paths: &MemoryPaths) -> Result<Vec<SessionEndOriginJournal>> {
    let root = repository_transaction_root(paths);
    match fs::read_dir(&root) {
        Ok(entries) => {
            let mut journals = Vec::new();
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with(JOURNAL_PREFIX) || !name.ends_with(".json") {
                    continue;
                }
                let path = entry.path();
                let bytes = fs::read(&path).with_context(|| {
                    format!("failed to read session-end journal {}", path.display())
                })?;
                let journal = serde_json::from_slice::<SessionEndOriginJournal>(&bytes)
                    .with_context(|| {
                        format!("failed to parse session-end journal {}", path.display())
                    })?;
                journal.validate(paths)?;
                ensure!(
                    journal_path(paths, &journal.identity) == path,
                    "session-end origin journal path does not match its identity"
                );
                journals.push(journal);
            }
            journals
                .sort_by(|left, right| left.identity.origin_key.cmp(&right.identity.origin_key));
            Ok(journals)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to read session-end journal root {}", root.display())),
    }
}

pub(super) fn artifact_state(
    paths: &MemoryPaths,
    journal: &SessionEndOriginJournal,
) -> Result<ArtifactState> {
    let mut installed = 0usize;
    for artifact in &journal.artifacts {
        let path = paths.project_root.join(&artifact.relative_path);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                ensure_safe_existing_file(
                    &paths.project_root,
                    &paths.proposals_dir(),
                    &path,
                    "session-end recovery artifact",
                )?;
                let digest = blake3::hash(&fs::read(&path)?).to_hex().to_string();
                ensure!(
                    digest == artifact.digest,
                    "session-end recovery artifact changed: {}",
                    artifact.relative_path.display()
                );
                installed += 1;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    match installed {
        0 => Ok(ArtifactState::NoneInstalled),
        count if count == journal.artifacts.len() => Ok(ArtifactState::AllInstalled),
        count => bail!(
            "session-end origin recovery found a partial artifact batch ({count}/{})",
            journal.artifacts.len()
        ),
    }
}

fn validate_relative_path(path: &Path) -> Result<()> {
    ensure!(path.is_relative(), "journal artifact path must be relative");
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "journal artifact path contains traversal or an unsafe component"
    );
    Ok(())
}

fn ensure_digest(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "journal artifact digest must be lowercase hexadecimal"
    );
    Ok(())
}
