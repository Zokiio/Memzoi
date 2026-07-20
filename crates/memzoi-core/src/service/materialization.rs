use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{
    AuthorizationProof, CANONICAL_REVISION_SCHEMA, CanonicalLifecycleProjection,
    CanonicalRecordSemanticContent, CanonicalRevision, CanonicalRevisionProjection,
    ExpectedPriorRevision, MATERIALIZATION_METADATA_SCHEMA, MaterializationAction,
    MaterializationMetadata, MaterializationOutputIntent, MaterializationOutputOutcome,
    MaterializationOutputResult, MaterializationOutputRole, OkfProposalSensitivity, OkfRecordFile,
    RepositoryContentClass, RepositoryMaterializationCandidate, RepositoryMaterializationDecision,
    RepositoryMaterializationMetadata, RepositoryMaterializationPlan,
    RepositoryMaterializationResult, RepositoryWriteRoute, SafetyFieldKind, ScopeKind, Visibility,
    canonical_revision_for_okf_record, canonical_revision_for_projection,
    git_repository::git_review_visibility, okf, repository_io,
    repository_materialization_candidate_plan, repository_materialization_candidate_to_okf_record,
};

use super::{
    MemoryService,
    repository_mutation::{
        OwnedRepositoryProjection, RepositoryMutationAuthorization,
        authorize_repository_projection_batch,
        capture_authorized_existing_repository_projection_identity, explicit_repository_provenance,
        install_authorized_repository_projection, memory_draft_safety_values,
        rollback_authorized_repository_projection, safety_value,
    },
    safe_files::RepoLifecycleLock,
};

struct PreparedMaterialization {
    record: OkfRecordFile,
    markdown: String,
    relative_path: PathBuf,
    output: MaterializationOutputIntent,
}

struct ExistingCanonicalRecord {
    bytes: Vec<u8>,
    semantic_revision: CanonicalRevision,
}

impl MemoryService {
    /// Atomically installs one fully pinned canonical record after explicit materialization review.
    ///
    /// This is the sole mutating materialization entry point. It refreshes the
    /// installed record in the derived index without performing a full rebuild.
    pub fn apply_repository_materialization(
        &self,
        plan: &RepositoryMaterializationPlan,
        decision: &RepositoryMaterializationDecision,
        candidate: &RepositoryMaterializationCandidate,
    ) -> Result<RepositoryMaterializationResult> {
        let prepared = prepare_materialization(plan, decision, candidate)?;
        let _lifecycle_lock = RepoLifecycleLock::acquire(&self.paths)?;
        self.ensure_repository_index_current()?;
        let destination = repository_io::verify_projection_path(
            &self.paths.project_root,
            &prepared.relative_path,
        )?;
        let existing = read_existing_canonical_record(
            &self.paths.records_dir(),
            &destination,
            &prepared.record.concept_id,
        )?;

        let review_visibility =
            git_review_visibility(&self.paths.project_root, &prepared.relative_path)
                .map_err(anyhow::Error::new)
                .context("materialization Git review visibility check failed")?;
        if !review_visibility.is_review_visible() {
            bail!("materialization_git_review_visibility_required");
        }
        if existing
            .as_ref()
            .is_some_and(|existing| existing.bytes.as_slice() == prepared.markdown.as_bytes())
        {
            return materialization_result(
                &prepared,
                &decision.decision_id,
                MaterializationOutputOutcome::AlreadyCurrent,
            );
        }

        match prepared.output.action {
            MaterializationAction::Create => {
                if existing.is_some() {
                    bail!("materialization_create_conflict");
                }
            }
            MaterializationAction::Update => {
                let existing = existing
                    .as_ref()
                    .context("materialization_update_conflict")?;
                let ExpectedPriorRevision::Revision(expected_prior_revision) =
                    &prepared.output.expected_prior_revision
                else {
                    bail!("materialization_update_requires_expected_prior_revision");
                };
                if &existing.semantic_revision != expected_prior_revision {
                    bail!("materialization_update_stale");
                }
            }
            MaterializationAction::Supersede | MaterializationAction::Tombstone => {
                // `prepare_materialization` rejects these before a lock, filesystem
                // inspection, authorization, or transaction artifacts are created.
                bail!("multi_record_transaction_required");
            }
        }

        let expected_existing_hash = existing
            .as_ref()
            .map(|existing| blake3::hash(&existing.bytes).to_hex().to_string());
        let mut projections = vec![OwnedRepositoryProjection::from_absolute(
            &self.paths,
            &destination,
            prepared.markdown.as_bytes(),
            expected_existing_hash.as_deref(),
        )?];
        if let (Some(existing), Some(expected_existing_hash)) =
            (existing.as_ref(), expected_existing_hash.as_deref())
        {
            projections.push(OwnedRepositoryProjection::existing_from_absolute(
                &self.paths,
                &destination,
                &existing.bytes,
                expected_existing_hash,
            )?);
        }

        let safety_values = materialization_safety_values(&prepared, plan, decision)?;
        let authorization = authorize_repository_projection_batch(
            &self.paths,
            RepositoryWriteRoute::Materialization,
            OkfProposalSensitivity::RepoSafe,
            ScopeKind::Repo,
            None,
            Visibility::Repo,
            AuthorizationProof::PinnedMaterialization {
                decision_id: &decision.decision_id,
            },
            explicit_repository_provenance(
                RepositoryContentClass::GeneralRepoKnowledge,
                &decision.decision_id,
            ),
            &safety_values,
            &projections,
        )?;
        let mutation = RepositoryMutationAuthorization {
            route: RepositoryWriteRoute::Materialization,
            authorization: &authorization,
            projections: &projections,
        };
        let expected_existing_identity = if existing.is_some() {
            Some(capture_authorized_existing_repository_projection_identity(
                &self.paths,
                mutation,
                &destination,
            )?)
        } else {
            None
        };
        let tx = self.conn.unchecked_transaction()?;
        okf::import_okf_records(&tx, std::slice::from_ref(&prepared.record))
            .context("failed to prepare derived repository index after materialization")?;
        let installed = install_authorized_repository_projection(
            &self.paths,
            mutation,
            &destination,
            expected_existing_identity,
        )?;
        let finalize_result = match self.ensure_repository_index_current_with_conn(&tx) {
            Ok(()) => tx
                .commit()
                .context("failed to commit derived repository index after materialization"),
            Err(error) => Err(error),
        };
        if let Err(error) = finalize_result {
            return match rollback_authorized_repository_projection(
                &self.paths,
                mutation,
                &installed,
            ) {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(error).context(format!(
                    "additionally failed to roll back materialized repository projection: {rollback_error:#}"
                )),
            };
        }

        materialization_result(
            &prepared,
            &decision.decision_id,
            MaterializationOutputOutcome::Written,
        )
    }
}

fn prepare_materialization(
    plan: &RepositoryMaterializationPlan,
    decision: &RepositoryMaterializationDecision,
    candidate: &RepositoryMaterializationCandidate,
) -> Result<PreparedMaterialization> {
    candidate.validate()?;
    if plan != &repository_materialization_candidate_plan(candidate)? {
        bail!("materialization_candidate_plan_mismatch");
    }
    plan.validate()?;
    decision.validate()?;
    if decision.plan_id != plan.plan_id || decision.candidate_id != plan.candidate_id {
        bail!("materialization_plan_decision_identity_mismatch");
    }
    if decision.outputs != plan.outputs || plan.outputs.len() != 1 || decision.outputs.len() != 1 {
        bail!("materialization_output_intent_mismatch");
    }
    if decision.authorization_capability
        != crate::MaterializationAuthorizationCapability::ExplicitCli
    {
        bail!("materialization_authorization_capability_mismatch");
    }
    if decision.policy != crate::repository_materialization_policy() {
        bail!("materialization_policy_mismatch");
    }

    let output = plan.outputs[0].clone();
    if matches!(
        output.action,
        MaterializationAction::Supersede | MaterializationAction::Tombstone
    ) {
        bail!("multi_record_transaction_required");
    }
    let mut record = repository_materialization_candidate_to_okf_record(candidate)?;
    validate_materialization_candidate(&record)?;

    let derived_path = format!(".memzoi/records/{}.md", record.concept_id);
    if output.path != derived_path
        || output.record_id != record.concept_id
        || output.role != MaterializationOutputRole::CanonicalRecord
    {
        bail!("materialization_output_intent_mismatch");
    }

    let semantic_revision = canonical_revision_for_projection(&CanonicalRevisionProjection {
        schema: CANONICAL_REVISION_SCHEMA.to_owned(),
        record_id: record.concept_id.clone(),
        record: CanonicalRecordSemanticContent::from(&record),
        lifecycle: CanonicalLifecycleProjection {
            action: Some(output.action),
            ..CanonicalLifecycleProjection::default()
        },
    })?;
    if output.intended_semantic_revision != semantic_revision {
        bail!("materialization_semantic_revision_mismatch");
    }
    let metadata = MaterializationMetadata {
        schema: MATERIALIZATION_METADATA_SCHEMA.to_owned(),
        action: output.action,
        plan_id: plan.plan_id.clone(),
        candidate_id: plan.candidate_id.clone(),
        decision_id: decision.decision_id.clone(),
        decision_at: decision.decision_at.clone(),
        safety_contract: decision.policy.safety_contract.clone(),
        revision: semantic_revision,
        target: None,
        reason: None,
    };
    metadata.validate()?;
    record.materialization = Some(RepositoryMaterializationMetadata::Direct(metadata));

    let markdown = okf::render_okf_record_markdown(&record)?;
    let relative_path = PathBuf::from(&derived_path);
    let canonical_path = Path::new(".memzoi")
        .join("records")
        .join(format!("{}.md", record.concept_id));
    if relative_path != canonical_path {
        bail!("materialization_output_intent_mismatch");
    }
    let parsed =
        okf::parse_okf_record_markdown(Path::new(".memzoi/records"), &relative_path, &markdown)?
            .context("materialization_candidate_not_canonical_record")?;
    if parsed != record {
        bail!("materialization_candidate_not_compact_canonical");
    }

    Ok(PreparedMaterialization {
        record,
        markdown,
        relative_path,
        output,
    })
}

fn validate_materialization_candidate(candidate: &OkfRecordFile) -> Result<()> {
    crate::validate_canonical_record_id(&candidate.concept_id)?;
    if candidate.draft.scope_kind != ScopeKind::Repo || candidate.draft.scope_id.is_some() {
        bail!("materialization_candidate_not_repository_scoped");
    }
    if candidate.draft.visibility != Visibility::Repo {
        bail!("materialization_candidate_not_repository_visible");
    }
    if candidate.draft.sensitivity != OkfProposalSensitivity::RepoSafe {
        bail!("materialization_candidate_not_repo_safe");
    }
    if candidate.draft.content_class != RepositoryContentClass::GeneralRepoKnowledge {
        bail!("materialization_candidate_not_general_repository_knowledge");
    }
    Ok(())
}

fn read_existing_canonical_record(
    records_root: &Path,
    destination: &Path,
    record_id: &str,
) -> Result<Option<ExistingCanonicalRecord>> {
    let bytes = match fs::read(destination) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to read existing materialization target {}",
                    destination.display()
                )
            });
        }
    };
    let markdown =
        std::str::from_utf8(&bytes).context("materialization_current_record_not_utf8")?;
    let record = okf::parse_okf_record_markdown(records_root, destination, markdown)?
        .context("materialization_current_record_invalid")?;
    if record.concept_id != record_id {
        bail!("materialization_current_record_id_mismatch");
    }
    Ok(Some(ExistingCanonicalRecord {
        bytes,
        semantic_revision: canonical_revision_for_okf_record(&record)?,
    }))
}

fn materialization_safety_values(
    prepared: &PreparedMaterialization,
    plan: &RepositoryMaterializationPlan,
    decision: &RepositoryMaterializationDecision,
) -> Result<Vec<super::repository_mutation::RepositorySafetyValue>> {
    let mut values =
        memory_draft_safety_values("materialization.candidate.draft", &prepared.record.draft);
    values.push(safety_value(
        "materialization.candidate.record".to_owned(),
        SafetyFieldKind::RenderedProjection,
        serde_json::to_vec(&prepared.record)
            .context("failed to serialize materialization candidate for repository safety")?,
    ));
    values.push(safety_value(
        "materialization.plan".to_owned(),
        SafetyFieldKind::RenderedProjection,
        serde_json::to_vec(plan)
            .context("failed to serialize materialization plan for repository safety")?,
    ));
    values.push(safety_value(
        "materialization.decision".to_owned(),
        SafetyFieldKind::RenderedProjection,
        serde_json::to_vec(decision)
            .context("failed to serialize materialization decision for repository safety")?,
    ));
    values.push(safety_value(
        "materialization.output_path".to_owned(),
        SafetyFieldKind::Path,
        prepared.relative_path.as_os_str().as_encoded_bytes(),
    ));
    values.push(safety_value(
        "materialization.final_markdown".to_owned(),
        SafetyFieldKind::RenderedProjection,
        prepared.markdown.as_bytes(),
    ));
    Ok(values)
}

fn materialization_result(
    prepared: &PreparedMaterialization,
    decision_id: &str,
    outcome: MaterializationOutputOutcome,
) -> Result<RepositoryMaterializationResult> {
    let result = RepositoryMaterializationResult {
        schema: crate::REPOSITORY_MATERIALIZATION_RESULT_SCHEMA.to_owned(),
        decision_id: decision_id.to_owned(),
        outputs: vec![MaterializationOutputResult {
            path: prepared.output.path.clone(),
            record_id: prepared.output.record_id.clone(),
            action: prepared.output.action,
            semantic_revision: prepared.output.intended_semantic_revision.clone(),
            role: prepared.output.role,
            outcome,
        }],
    };
    result.validate()?;
    Ok(result)
}
