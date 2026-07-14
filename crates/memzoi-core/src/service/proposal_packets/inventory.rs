use super::*;

fn scan_proposal_directory(
    paths: &MemoryPaths,
    root: &Path,
    expected_status: crate::OkfProposalStatus,
    expected_outcome: Option<OkfProposalOutcome>,
    label: &str,
    entries: &mut Vec<FileProposalInventoryEntry>,
    errors: &mut Vec<FileProposalInventoryError>,
) -> Result<()> {
    match fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect {label} {}", root.display()));
        }
    }
    if let Err(error) = ensure_safe_directory(&paths.project_root, root, false, label) {
        errors.push(FileProposalInventoryError {
            display_path: root.to_path_buf(),
            error: if expected_outcome.is_some() {
                format!("failed to inspect resolved proposal packet: {error}")
            } else {
                format!("failed to inspect pending proposal root: {error}")
            },
        });
        return Ok(());
    }

    let mut files = Vec::new();
    collect_safe_markdown_files(root, &mut files)?;
    files.sort();
    for actual_path in files {
        let fallback_display_path = okf::redacted_okf_proposal_path(root, &actual_path)
            .unwrap_or_else(|_| root.join("redacted-proposal.md"));
        if ensure_safe_existing_file(&paths.project_root, root, &actual_path, "proposal packet")
            .is_err()
        {
            errors.push(FileProposalInventoryError {
                display_path: fallback_display_path,
                error: "failed to inspect proposal packet".to_owned(),
            });
            continue;
        }
        let markdown = match fs::read_to_string(&actual_path) {
            Ok(markdown) => markdown,
            Err(_) => {
                errors.push(FileProposalInventoryError {
                    display_path: fallback_display_path,
                    error: "failed to read proposal during safety preflight".to_owned(),
                });
                continue;
            }
        };
        let preflight = match okf::preflight_okf_proposal_markdown(root, &actual_path, &markdown) {
            Ok(Some(preflight)) => preflight,
            Ok(None) => continue,
            Err(error) => {
                errors.push(FileProposalInventoryError {
                    display_path: fallback_display_path,
                    error: error.to_string(),
                });
                continue;
            }
        };
        let source_sensitivity = preflight.sensitivity;
        let source_content_class = preflight.content_class;
        let relative_path = actual_path
            .strip_prefix(&paths.project_root)
            .unwrap_or_else(|_| Path::new("../unsafe-proposal-path"));
        let content_allowed = crate::scan_repository_blob(
            paths.project_root.as_os_str().as_encoded_bytes(),
            relative_path,
            markdown.as_bytes(),
        )
        .allowed;
        let requires_redaction = preflight.sensitivity != crate::OkfProposalSensitivity::RepoSafe
            || preflight.content_class != RepositoryContentClass::GeneralRepoKnowledge
            || !content_allowed;
        let display_path = if requires_redaction {
            root.join(format!("{}.md", preflight.receipt_proposal.file_id))
        } else {
            actual_path.clone()
        };
        let parsed = if requires_redaction && expected_outcome.is_none() {
            Some(preflight.receipt_proposal.clone())
        } else {
            match okf::parse_okf_proposal_markdown(root, &actual_path, &markdown) {
                Ok(proposal) => proposal,
                Err(error) => {
                    errors.push(FileProposalInventoryError {
                        display_path,
                        error: if requires_redaction {
                            "invalid redacted resolved proposal packet".to_owned()
                        } else {
                            error.to_string()
                        },
                    });
                    continue;
                }
            }
        };
        if let Some(mut proposal) = parsed {
            if requires_redaction && expected_outcome.is_some() {
                proposal = redact_resolved_proposal_for_inventory(proposal, preflight);
            }
            let state_error = if proposal.status != expected_status {
                Some(format!(
                    "proposal identity token {} has status {} but this state requires {}",
                    okf::proposal_identity_tokens(&proposal)
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| "redacted-identity-unavailable".to_owned()),
                    proposal.status.as_str(),
                    expected_status.as_str()
                ))
            } else if expected_outcome.is_none() && proposal.resolution.is_some() {
                Some("pending proposal must be unresolved".to_owned())
            } else if let Some(expected_outcome) = expected_outcome {
                match proposal.resolution.as_ref() {
                    Some(resolution) if resolution.outcome == expected_outcome => None,
                    Some(resolution) => Some(format!(
                        "resolved proposal has outcome {} but this state requires {}",
                        resolution.outcome.as_str(),
                        expected_outcome.as_str()
                    )),
                    None => Some("resolved proposal is missing resolution metadata".to_owned()),
                }
            } else {
                None
            };
            if let Some(error) = state_error {
                errors.push(FileProposalInventoryError {
                    display_path,
                    error,
                });
            } else {
                entries.push(FileProposalInventoryEntry {
                    proposal,
                    source_sensitivity,
                    source_content_class,
                    display_path,
                    actual_path,
                });
            }
        }
    }
    Ok(())
}

fn collect_safe_markdown_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).context("failed to read proposal directory")? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if name.to_str().is_some_and(|name| name.starts_with('.')) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_safe_markdown_files(&path, files)?;
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("md")
        {
            files.push(path);
        }
    }
    Ok(())
}

pub(super) fn require_clean_file_proposal_inventory(
    inventory: &FileProposalInventory,
) -> Result<()> {
    if let Some(error) = inventory.errors.first() {
        bail!("{}", error.error);
    }
    Ok(())
}

pub(crate) fn prepare_pending_proposal_root(paths: &MemoryPaths) -> Result<()> {
    preflight_pending_proposal_root(paths)?;
    ensure_safe_directory(
        &paths.project_root,
        &paths.proposals_dir().join("pending"),
        true,
        "pending proposal root",
    )
    .context("failed to inspect pending proposal root")
}

pub(super) fn preflight_pending_proposal_root(paths: &MemoryPaths) -> Result<()> {
    let pending_root = paths.proposals_dir().join("pending");
    let relative = pending_root
        .strip_prefix(&paths.project_root)
        .context("pending proposal root is outside the project root")?;
    let root_metadata = fs::symlink_metadata(&paths.project_root).with_context(|| {
        format!(
            "failed to inspect project root {}",
            paths.project_root.display()
        )
    })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        bail!(
            "project root must be a real directory: {}",
            paths.project_root.display()
        );
    }

    let mut current = paths.project_root.clone();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!("pending proposal root contains traversal or an unsafe component");
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!(
                    "pending proposal root ancestor must be a real directory: {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => break,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to inspect pending proposal root ancestor {}",
                        current.display()
                    )
                });
            }
        }
    }
    Ok(())
}

fn proposal_inventory_identities(inventory: &FileProposalInventory) -> BTreeSet<String> {
    inventory
        .pending
        .iter()
        .chain(&inventory.resolved)
        .flat_map(|entry| okf::proposal_identity_tokens(&entry.proposal))
        .collect()
}

pub(super) fn db_proposal_identity_tokens(conn: &Connection) -> Result<BTreeSet<String>> {
    let mut statement = conn.prepare("SELECT id FROM proposal ORDER BY id")?;
    let ids = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut tokens = BTreeSet::new();
    for id in ids {
        tokens.insert(okf::okf_proposal_identity_token(&id?));
    }
    Ok(tokens)
}

pub(super) fn reserved_proposal_identities(
    conn: &Connection,
    inventory: &FileProposalInventory,
) -> Result<BTreeSet<String>> {
    let mut identities = proposal_inventory_identities(inventory);
    identities.extend(db_proposal_identity_tokens(conn)?);
    Ok(identities)
}

pub(super) fn ensure_planned_proposals_available<'a>(
    conn: &Connection,
    inventory: &FileProposalInventory,
    plans: impl IntoIterator<Item = &'a okf::OkfCreateProposalPlan>,
) -> Result<()> {
    let existing = reserved_proposal_identities(conn, inventory)?;
    for plan in plans {
        for identity in [&plan.proposal_id, &plan.parsed.file_id] {
            let identity_token = okf::okf_proposal_identity_token(identity);
            if existing.contains(&identity_token) {
                bail!(
                    "proposal packet identity token {identity_token} appeared after planning; recompute the operation"
                );
            }
        }
    }
    Ok(())
}

fn append_duplicate_identity_errors(
    entries: &[FileProposalInventoryEntry],
    state: &str,
    errors: &mut Vec<FileProposalInventoryError>,
) {
    let mut identities = BTreeMap::<String, usize>::new();
    for (index, entry) in entries.iter().enumerate() {
        for identity in okf::proposal_identity_tokens(&entry.proposal) {
            if let Some(previous) = identities.insert(identity.clone(), index)
                && previous != index
            {
                let error = format!("duplicate {state} proposal identity token {identity}");
                errors.push(FileProposalInventoryError {
                    display_path: entries[previous].display_path.clone(),
                    error: error.clone(),
                });
                errors.push(FileProposalInventoryError {
                    display_path: entry.display_path.clone(),
                    error,
                });
            }
        }
    }
}

fn append_cross_state_identity_errors(
    pending: &[FileProposalInventoryEntry],
    resolved: &[FileProposalInventoryEntry],
    errors: &mut Vec<FileProposalInventoryError>,
) {
    for pending_entry in pending {
        for resolved_entry in resolved {
            let pending_identities = okf::proposal_identity_tokens(&pending_entry.proposal);
            let resolved_identities = okf::proposal_identity_tokens(&resolved_entry.proposal);
            let overlap = pending_identities
                .intersection(&resolved_identities)
                .next()
                .cloned();
            if let Some(identity) = overlap {
                let outcome = resolved_entry
                    .proposal
                    .resolution
                    .as_ref()
                    .map(|resolution| resolution.outcome.as_str())
                    .unwrap_or("resolved");
                errors.push(FileProposalInventoryError {
                    display_path: pending_entry.display_path.clone(),
                    error: format!(
                        "pending proposal reintroduces resolved identity token {identity} already {outcome}"
                    ),
                });
            }
        }
    }
}

fn redact_resolved_proposal_for_inventory(
    parsed: OkfProposalFile,
    preflight: okf::OkfProposalPreflight,
) -> OkfProposalFile {
    let mut receipt = preflight.receipt_proposal;
    receipt.status = parsed.status;
    receipt.resolution = parsed.resolution.map(|resolution| OkfProposalResolution {
        outcome: resolution.outcome,
        resolved_by: "redacted".to_owned(),
        resolved_at: "1970-01-01T00:00:00Z".to_owned(),
        reason: Some(
            "Non-repo-safe proposal resolved at the repository trust boundary.".to_owned(),
        ),
        record_id: None,
        target_id: None,
    });
    receipt
}

pub fn scan_file_proposal_inventory(paths: &MemoryPaths) -> Result<FileProposalInventory> {
    let mut inventory = FileProposalInventory::default();
    let proposals_root = paths.proposals_dir();
    match fs::symlink_metadata(&proposals_root) {
        Ok(_) => {
            if let Err(error) =
                ensure_safe_directory(&paths.project_root, &proposals_root, false, "proposal root")
            {
                inventory.errors.push(FileProposalInventoryError {
                    display_path: proposals_root,
                    error: format!("failed to inspect proposal root: {error}"),
                });
                return Ok(inventory);
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(inventory),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect proposal root {}",
                    proposals_root.display()
                )
            });
        }
    }
    let pending_root = paths.proposals_dir().join("pending");
    scan_proposal_directory(
        paths,
        &pending_root,
        crate::OkfProposalStatus::Proposed,
        None,
        "pending proposal root",
        &mut inventory.pending,
        &mut inventory.errors,
    )?;

    let resolved_root = paths.proposals_dir().join("resolved");
    if fs::symlink_metadata(&resolved_root).is_ok()
        && let Err(error) = ensure_safe_directory(
            &paths.project_root,
            &resolved_root,
            false,
            "resolved proposal root",
        )
    {
        inventory.errors.push(FileProposalInventoryError {
            display_path: resolved_root,
            error: format!("failed to inspect resolved proposal root: {error}"),
        });
        return Ok(inventory);
    }
    for (directory, status, outcome) in [
        (
            "applied",
            crate::OkfProposalStatus::Applied,
            OkfProposalOutcome::Applied,
        ),
        (
            "rejected",
            crate::OkfProposalStatus::Rejected,
            OkfProposalOutcome::Rejected,
        ),
    ] {
        scan_proposal_directory(
            paths,
            &resolved_root.join(directory),
            status,
            Some(outcome),
            "resolved proposal root",
            &mut inventory.resolved,
            &mut inventory.errors,
        )?;
    }

    append_duplicate_identity_errors(&inventory.pending, "pending", &mut inventory.errors);
    append_duplicate_identity_errors(&inventory.resolved, "resolved", &mut inventory.errors);
    append_cross_state_identity_errors(
        &inventory.pending,
        &inventory.resolved,
        &mut inventory.errors,
    );
    inventory.pending.sort_by(|left, right| {
        left.proposal
            .id
            .cmp(&right.proposal.id)
            .then_with(|| left.display_path.cmp(&right.display_path))
    });
    inventory.resolved.sort_by(|left, right| {
        left.proposal
            .id
            .cmp(&right.proposal.id)
            .then_with(|| left.display_path.cmp(&right.display_path))
    });
    inventory.errors.sort_by(|left, right| {
        left.display_path
            .cmp(&right.display_path)
            .then_with(|| left.error.cmp(&right.error))
    });
    inventory.errors.dedup();
    Ok(inventory)
}
