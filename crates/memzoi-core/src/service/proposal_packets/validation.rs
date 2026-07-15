use super::inventory::{
    db_proposal_identity_tokens, require_clean_file_proposal_inventory,
    scan_file_proposal_inventory,
};
use super::transaction::{rebuild_fts_content_index, validate_resolution_actor};
use super::*;

impl MemoryService {
    pub fn validate_file_proposal(&self, proposal: &OkfProposalFile) -> Result<()> {
        let inventory = scan_file_proposal_inventory(&self.paths)?;
        require_clean_file_proposal_inventory(&inventory)?;
        self.build_file_proposal_apply_plan(proposal, &expiry::format_timestamp(self.now())?)?;
        Ok(())
    }

    pub fn file_proposal_inventory(&self) -> Result<FileProposalInventory> {
        scan_file_proposal_inventory(&self.paths)
    }

    pub fn validate_file_proposal_inventory(&self) -> Result<FileProposalInventory> {
        let mut inventory = scan_file_proposal_inventory(&self.paths)?;
        let resolved_at = expiry::format_timestamp(self.now())?;
        let mut valid = Vec::with_capacity(inventory.pending.len());
        for entry in std::mem::take(&mut inventory.pending) {
            if inventory
                .errors
                .iter()
                .any(|error| error.display_path == entry.display_path)
            {
                continue;
            }
            match self.build_file_proposal_apply_plan(&entry.proposal, &resolved_at) {
                Ok(_) => valid.push(entry),
                Err(error) => {
                    inventory.errors.push(FileProposalInventoryError {
                        display_path: entry.display_path,
                        error: error.to_string(),
                    });
                }
            }
        }
        inventory.pending = valid;
        inventory
            .errors
            .sort_by(|left, right| left.display_path.cmp(&right.display_path));
        Ok(inventory)
    }

    pub fn replay_file_proposal(
        &self,
        proposal_identity: &str,
        requested_outcome: OkfProposalOutcome,
        actor: &str,
    ) -> Result<FileProposalResolutionResult> {
        validate_resolution_actor(actor)?;
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        let inventory = scan_file_proposal_inventory(&self.paths)?;
        require_clean_file_proposal_inventory(&inventory)?;

        let matches = inventory
            .resolved
            .iter()
            .filter(|entry| okf::okf_proposal_matches_identity(&entry.proposal, proposal_identity))
            .collect::<Vec<_>>();
        let entry = match matches.as_slice() {
            [] => bail!("proposal file not found: {proposal_identity}"),
            [entry] => *entry,
            _ => bail!(
                "proposal file identity {proposal_identity:?} matched multiple resolved packets"
            ),
        };
        let resolution = entry
            .proposal
            .resolution
            .clone()
            .context("resolved proposal is missing resolution metadata")?;
        if resolution.outcome != requested_outcome {
            bail!(
                "proposal file {} is already resolved as {}; cannot resolve as {}",
                entry.proposal.id,
                resolution.outcome.as_str(),
                requested_outcome.as_str()
            );
        }

        if requested_outcome == OkfProposalOutcome::Rejected {
            return Ok(FileProposalResolutionResult {
                proposal: entry.proposal.clone(),
                resolution,
                resolved_path: entry.actual_path.clone(),
                record: None,
                record_path: None,
                already_resolved: true,
                runtime_index_updated: false,
            });
        }

        let (canonical_records, primary_index) =
            self.validate_resolved_apply_canonical_truth(&entry.proposal, &resolution)?;
        let relational_drift = canonical_records
            .iter()
            .map(|canonical| self.indexed_record_matches_canonical(canonical))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .any(|matches| !matches);
        let fts_out_of_sync = !derived_index::fts_is_current(&self.conn)?;
        let runtime_index_updated = relational_drift || fts_out_of_sync;
        if runtime_index_updated {
            let tx = self.conn.unchecked_transaction()?;
            if relational_drift {
                okf::import_okf_records(&tx, &canonical_records)?;
            }
            rebuild_fts_content_index(&tx)?;
            append_event(
                &tx,
                AppendEvent {
                    event_type: "proposal_file.index_repaired".to_owned(),
                    actor: actor.trim().to_owned(),
                    payload: json!({
                        "proposal_id": entry.proposal.id,
                        "file_id": entry.proposal.file_id,
                        "record_ids": canonical_records
                            .iter()
                            .map(|record| record.concept_id.as_str())
                            .collect::<Vec<_>>(),
                        "resolved_path": entry.display_path,
                        "relational_drift": relational_drift,
                        "fts_out_of_sync": fts_out_of_sync,
                    }),
                    record_id: resolution.record_id.clone(),
                    proposal_id: Some(entry.proposal.id.clone()),
                },
            )?;
            tx.commit()?;
        }

        let canonical = &canonical_records[primary_index];
        let record = okf::project_okf_record(canonical);
        let record_path = self.canonical_record_path(&record.id)?;
        Ok(FileProposalResolutionResult {
            proposal: entry.proposal.clone(),
            resolution,
            resolved_path: entry.actual_path.clone(),
            record: Some(record),
            record_path: Some(record_path),
            already_resolved: true,
            runtime_index_updated,
        })
    }

    pub(super) fn validate_fresh_file_proposal_identity(
        &self,
        proposal: &OkfProposalFile,
    ) -> Result<()> {
        let inventory = scan_file_proposal_inventory(&self.paths)?;
        require_clean_file_proposal_inventory(&inventory)?;
        if inventory
            .resolved
            .iter()
            .any(|entry| okf::okf_proposals_share_identity(proposal, &entry.proposal))
        {
            let token = okf::proposal_identity_tokens(proposal)
                .into_iter()
                .next()
                .unwrap_or_else(|| "redacted-identity-unavailable".to_owned());
            bail!("pending proposal identity token {token} is already resolved");
        }
        let proposal_tokens = okf::proposal_identity_tokens(proposal);
        if let Some(token) = db_proposal_identity_tokens(&self.conn)?
            .intersection(&proposal_tokens)
            .next()
        {
            bail!("file proposal identity token {token} conflicts with a database proposal");
        }
        Ok(())
    }

    pub(super) fn prepare_resolution_destination(&self, path: &Path) -> Result<()> {
        let proposals_root = self.paths.proposals_dir();
        ensure_safe_path_parent(
            &self.paths.project_root,
            &proposals_root,
            path,
            true,
            "resolved proposal packet",
        )?;
        ensure_path_absent(path, "resolved proposal packet")
    }

    fn validate_resolved_apply_canonical_truth(
        &self,
        proposal: &OkfProposalFile,
        resolution: &OkfProposalResolution,
    ) -> Result<(Vec<okf::OkfRecordFile>, usize)> {
        let mut pending_shape = proposal.clone();
        pending_shape.status = crate::OkfProposalStatus::Proposed;
        pending_shape.resolution = None;
        okf::validate_repo_apply_proposal(&pending_shape)?;
        let record_id = resolution
            .record_id
            .as_deref()
            .context("applied resolution is missing record_id")?;

        match proposal.proposal.action {
            OkfProposalAction::Create => {
                if resolution.target_id.is_some() {
                    bail!(
                        "resolved create proposal {} unexpectedly names a target",
                        proposal.id
                    );
                }
                let expected = okf::project_okf_create_proposal(&pending_shape)?;
                if expected.id != record_id {
                    bail!(
                        "resolved proposal {} record lineage drift: expected {}, found {}",
                        proposal.id,
                        expected.id,
                        record_id
                    );
                }
                let (canonical, path, markdown) = self.load_canonical_record(record_id)?;
                ensure_expected_canonical_bytes(
                    proposal,
                    &expected,
                    &proposal.tags,
                    &proposal.applies_to,
                    &path,
                    &markdown,
                )?;
                Ok((vec![canonical], 0))
            }
            OkfProposalAction::Supersede => {
                let target_id = proposal
                    .supersedes
                    .first()
                    .context("resolved supersede proposal is missing target")?;
                if proposal.supersedes.len() != 1
                    || resolution.target_id.as_deref() != Some(target_id.as_str())
                {
                    bail!(
                        "resolved supersede proposal {} has inconsistent target lineage",
                        proposal.id
                    );
                }
                let expected = okf::project_okf_supersede_proposal(&pending_shape, target_id)?;
                if expected.id != record_id {
                    bail!(
                        "resolved proposal {} record lineage drift: expected {}, found {}",
                        proposal.id,
                        expected.id,
                        record_id
                    );
                }
                let (target, _, _) = self.load_canonical_record(target_id)?;
                if target.status != MemoryStatus::Superseded
                    || target.draft.scope_kind != proposal.scope_kind
                    || target.draft.scope_id != proposal.scope_id
                    || target.updated.as_deref() != Some(resolution.resolved_at.as_str())
                {
                    bail!(
                        "resolved supersede proposal {} has canonical target drift at {}",
                        proposal.id,
                        target_id
                    );
                }
                let (replacement, path, markdown) = self.load_canonical_record(record_id)?;
                ensure_expected_canonical_bytes(
                    proposal,
                    &expected,
                    &proposal.tags,
                    &proposal.applies_to,
                    &path,
                    &markdown,
                )?;
                Ok((vec![target, replacement], 1))
            }
            OkfProposalAction::Tombstone => {
                let target_id = proposal
                    .proposal
                    .target
                    .as_deref()
                    .context("resolved tombstone proposal is missing target")?;
                if resolution.target_id.as_deref() != Some(target_id) || record_id != target_id {
                    bail!(
                        "resolved tombstone proposal {} has inconsistent target lineage",
                        proposal.id
                    );
                }
                let (target, _, _) = self.load_canonical_record(target_id)?;
                if target.status != MemoryStatus::Tombstoned
                    || target.draft.scope_kind != proposal.scope_kind
                    || target.draft.scope_id != proposal.scope_id
                    || target.updated.as_deref() != Some(resolution.resolved_at.as_str())
                {
                    bail!(
                        "resolved tombstone proposal {} has canonical target drift at {}",
                        proposal.id,
                        target_id
                    );
                }
                Ok((vec![target], 0))
            }
        }
    }

    fn load_canonical_record(
        &self,
        record_id: &str,
    ) -> Result<(okf::OkfRecordFile, PathBuf, String)> {
        let path = self.canonical_record_path(record_id)?;
        ensure_regular_file(&path, "canonical memory record").with_context(|| {
            format!("resolved proposal canonical drift: record {record_id} is missing or unsafe")
        })?;
        let markdown = fs::read_to_string(&path).with_context(|| {
            format!("failed to read canonical memory record {}", path.display())
        })?;
        let record = okf::parse_okf_record_markdown(self.paths.records_dir(), &path, &markdown)?
            .with_context(|| {
                format!("resolved proposal canonical drift: record {record_id} was ignored")
            })?;
        if record.concept_id != record_id {
            bail!(
                "resolved proposal canonical drift: expected record {record_id}, found {}",
                record.concept_id
            );
        }
        Ok((record, path, markdown))
    }

    fn canonical_record_path(&self, record_id: &str) -> Result<PathBuf> {
        okf::validate_concept_id(record_id)
            .with_context(|| format!("invalid canonical record lineage id {record_id:?}"))?;
        let records_root = self.paths.records_dir();
        let path = records_root.join(format!("{record_id}.md"));
        ensure_safe_path_parent(
            &self.paths.project_root,
            &records_root,
            &path,
            false,
            "canonical memory record",
        )
        .with_context(|| {
            format!(
                "failed to inspect canonical memory record {}",
                path.display()
            )
        })?;
        Ok(path)
    }

    fn indexed_record_matches_canonical(&self, canonical: &okf::OkfRecordFile) -> Result<bool> {
        let Some(indexed) = RuntimeRecords::new(&self.conn).get(&canonical.concept_id)? else {
            return Ok(false);
        };
        if !derived_index::record_matches(canonical, &indexed) {
            return Ok(false);
        }

        let expected_tags = canonical
            .draft
            .tags
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if RuntimeRecords::new(&self.conn).tags(&canonical.concept_id)? != expected_tags {
            return Ok(false);
        }
        let mut expected_paths = canonical.applies_to.clone();
        expected_paths.sort();
        let actual_paths = search::load_paths(&self.conn, &canonical.concept_id)?
            .into_iter()
            .map(|path| path.path)
            .collect::<Vec<_>>();
        Ok(actual_paths == expected_paths)
    }

    pub(super) fn build_file_proposal_apply_plan(
        &self,
        proposal: &OkfProposalFile,
        resolved_at: &str,
    ) -> Result<FileProposalApplyPlan> {
        okf::validate_repo_apply_proposal(proposal)?;
        match proposal.proposal.action {
            OkfProposalAction::Create => {
                if proposal.proposal.target.is_some() || !proposal.supersedes.is_empty() {
                    bail!("OKF create proposals cannot name a target or supersedes record");
                }
                let record = okf::project_okf_create_proposal(proposal)?;
                let write = prepare_canonical_file_write(
                    &self.paths,
                    record.clone(),
                    proposal.tags.clone(),
                    proposal.applies_to.clone(),
                    FileWriteMode::CreateNew,
                )?;
                Ok(FileProposalApplyPlan {
                    record_path: write.path.clone(),
                    writes: vec![write],
                    record,
                    target_id: None,
                })
            }
            OkfProposalAction::Supersede => {
                require_action_reason(proposal, "supersede")?;
                if proposal.supersedes.len() != 1 || proposal.proposal.target.is_some() {
                    bail!(
                        "OKF supersede proposals must include exactly one supersedes target and no proposal.target"
                    );
                }
                let target_id = proposal.supersedes[0].clone();
                let target = self.load_file_proposal_target(proposal, &target_id)?;
                let mut previous = okf::project_okf_record(&target);
                previous.status = MemoryStatus::Superseded;
                previous.updated_at = resolved_at.to_owned();
                let replacement = okf::project_okf_supersede_proposal(proposal, &target_id)?;
                if replacement.id == target_id {
                    bail!(
                        "supersede replacement record id {} collides with its target; use a distinct title or proposal file id",
                        replacement.id
                    );
                }
                let previous_write = prepare_canonical_file_write(
                    &self.paths,
                    previous,
                    target.draft.tags.clone(),
                    target.applies_to.clone(),
                    FileWriteMode::Overwrite,
                )?;
                let replacement_write = prepare_canonical_file_write(
                    &self.paths,
                    replacement.clone(),
                    proposal.tags.clone(),
                    proposal.applies_to.clone(),
                    FileWriteMode::CreateNew,
                )?;
                Ok(FileProposalApplyPlan {
                    record_path: replacement_write.path.clone(),
                    writes: vec![previous_write, replacement_write],
                    record: replacement,
                    target_id: Some(target_id),
                })
            }
            OkfProposalAction::Tombstone => {
                require_action_reason(proposal, "tombstone")?;
                if !proposal.supersedes.is_empty() {
                    bail!("OKF tombstone proposals cannot include supersedes records");
                }
                let target_id = proposal
                    .proposal
                    .target
                    .as_deref()
                    .context("OKF tombstone proposals must include exactly one proposal.target")?
                    .to_owned();
                let target = self.load_file_proposal_target(proposal, &target_id)?;
                let mut tombstoned = okf::project_okf_record(&target);
                tombstoned.status = MemoryStatus::Tombstoned;
                tombstoned.updated_at = resolved_at.to_owned();
                let write = prepare_canonical_file_write(
                    &self.paths,
                    tombstoned.clone(),
                    target.draft.tags.clone(),
                    target.applies_to.clone(),
                    FileWriteMode::Overwrite,
                )?;
                Ok(FileProposalApplyPlan {
                    record_path: write.path.clone(),
                    writes: vec![write],
                    record: tombstoned,
                    target_id: Some(target_id),
                })
            }
        }
    }

    fn load_file_proposal_target(
        &self,
        proposal: &OkfProposalFile,
        target_id: &str,
    ) -> Result<okf::OkfRecordFile> {
        ensure_safe_directory(
            &self.paths.project_root,
            &self.paths.records_dir(),
            false,
            "canonical record root",
        )?;
        let records = okf::read_okf_record_files(self.paths.records_dir())?;
        let target = records
            .into_iter()
            .find(|record| record.concept_id == target_id)
            .with_context(|| format!("proposal target does not exist: {target_id}"))?;
        if target.status != MemoryStatus::Active {
            bail!(
                "proposal target {target_id} is inactive with status {}",
                target.status.as_str()
            );
        }
        if target.draft.scope_kind != proposal.scope_kind
            || target.draft.scope_id != proposal.scope_id
        {
            bail!(
                "proposal target {target_id} is cross-scope: target={}:{}, proposal={}:{}",
                target.draft.scope_kind.as_str(),
                target.draft.scope_id.as_deref().unwrap_or("-"),
                proposal.scope_kind.as_str(),
                proposal.scope_id.as_deref().unwrap_or("-")
            );
        }
        let target_updated = target.updated.as_deref().unwrap_or(&target.created);
        if parse_orderable_timestamp(target_updated, "target updated")?
            > parse_orderable_timestamp(&proposal.proposal.proposed_at, "proposal.proposed_at")?
        {
            bail!(
                "proposal target {target_id} is stale: target updated at {target_updated} after proposal at {}",
                proposal.proposal.proposed_at
            );
        }
        Ok(target)
    }
}

fn require_action_reason(proposal: &OkfProposalFile, action: &str) -> Result<()> {
    if proposal
        .proposal
        .reason
        .as_deref()
        .is_none_or(|reason| reason.trim().is_empty())
    {
        bail!("OKF {action} proposals must include proposal.reason");
    }
    Ok(())
}

fn parse_orderable_timestamp(value: &str, label: &str) -> Result<OffsetDateTime> {
    if let Ok(timestamp) = OffsetDateTime::parse(value, &Rfc3339) {
        return Ok(timestamp);
    }
    if value.len() == 10 {
        let format = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")?;
        if let Ok(date) = Date::parse(value, &format) {
            return Ok(date.with_time(Time::MIDNIGHT).assume_utc());
        }
    }
    bail!("{label} must be an RFC 3339 timestamp or YYYY-MM-DD date: {value:?}")
}

fn ensure_expected_canonical_bytes(
    proposal: &OkfProposalFile,
    expected: &MemoryRecord,
    tags: &[String],
    applies_to: &[String],
    path: &Path,
    actual_markdown: &str,
) -> Result<()> {
    let expected_markdown = okf::render_memory_record_markdown(expected, tags, applies_to);
    if actual_markdown != expected_markdown {
        bail!(
            "resolved proposal {} canonical byte drift at {}",
            proposal.id,
            path.display()
        );
    }
    Ok(())
}
