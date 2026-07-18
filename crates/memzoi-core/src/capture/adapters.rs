use anyhow::{Context, Result, bail};

use super::{
    ADR_EXTRACTOR_PROFILE, CAPTURE_MAX_CANDIDATES, CAPTURE_MAX_EVIDENCE_BYTES, CaptureExtraction,
    CaptureExtractorIdentity, CaptureLoadedSource, CapturePlanningControl, CaptureSourceRequest,
    GIT_CHANGE_EXTRACTOR_PROFILE, INSTRUCTION_EXTRACTOR_PROFILE, MARKDOWN_EXTRACTOR_PROFILE,
    extract_candidates, prohibited_finding,
};

mod adr;
mod git_change;
mod instruction;

pub(super) fn prohibited_finding_for_profile(
    profile: &str,
    bytes: &[u8],
) -> Option<(String, Option<u64>)> {
    if profile == INSTRUCTION_EXTRACTOR_PROFILE {
        let masked = instruction::mask_generated_markers_for_scan(bytes);
        prohibited_finding(&masked)
    } else if profile == GIT_CHANGE_EXTRACTOR_PROFILE {
        let masked = git_change::mask_diff_side_markers_for_scan(bytes);
        prohibited_finding(&masked)
    } else {
        prohibited_finding(bytes)
    }
}

pub(super) fn git_path_exclusion_code(path: &str) -> Option<&'static str> {
    git_change::excluded_git_path_diagnostic(path)
}

pub(super) fn git_changed_paths(bytes: &[u8]) -> Result<Vec<String>> {
    let text = std::str::from_utf8(bytes).context("Git diff source must be UTF-8")?;
    git_change::changed_paths(text)
}

pub(super) fn extract_profile(
    source: &CaptureSourceRequest,
    loaded: &CaptureLoadedSource,
    extractor: &CaptureExtractorIdentity,
    profile: &str,
    control: Option<&CapturePlanningControl>,
) -> Result<CaptureExtraction> {
    let extraction = match profile {
        MARKDOWN_EXTRACTOR_PROFILE => {
            let document = loaded
                .documents
                .first()
                .context("Markdown capture source is missing its document")?;
            let text = std::str::from_utf8(&document.bytes)
                .context("Markdown capture source must be UTF-8")?;
            extract_candidates(source, &document.snapshot, text, extractor, control)?
        }
        INSTRUCTION_EXTRACTOR_PROFILE => {
            let document = only_document(loaded, "instruction")?;
            instruction::extract(document, extractor, control)?
        }
        ADR_EXTRACTOR_PROFILE => adr::extract(loaded, extractor, control)?,
        GIT_CHANGE_EXTRACTOR_PROFILE => {
            let document = only_document(loaded, "Git-change")?;
            git_change::extract(document, extractor, control)?
        }
        _ => bail!("capture extractor profile is not implemented"),
    };
    if extraction.candidates.len() > CAPTURE_MAX_CANDIDATES {
        bail!("capture source exceeds the configured candidate limit");
    }
    let evidence_bytes = extraction
        .candidates
        .iter()
        .flat_map(|candidate| &candidate.evidence)
        .map(|evidence| evidence.span.byte_end - evidence.span.byte_start)
        .sum::<u64>();
    if evidence_bytes > CAPTURE_MAX_EVIDENCE_BYTES as u64 {
        bail!("capture source exceeds the configured evidence limit");
    }
    Ok(extraction)
}

fn only_document<'a>(
    loaded: &'a CaptureLoadedSource,
    profile: &str,
) -> Result<&'a super::CaptureSourceDocument> {
    if loaded.documents.len() != 1 {
        bail!("{profile} capture requires exactly one resolved document");
    }
    loaded
        .documents
        .first()
        .with_context(|| format!("{profile} capture source is missing its document"))
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SourceLine<'a> {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) number: u64,
    pub(super) logical: &'a str,
}

pub(super) fn source_lines(text: &str) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut number = 1u64;
    for raw in text.split_inclusive('\n') {
        let end = start + raw.len();
        let logical = raw
            .strip_suffix('\n')
            .unwrap_or(raw)
            .strip_suffix('\r')
            .unwrap_or_else(|| raw.strip_suffix('\n').unwrap_or(raw));
        lines.push(SourceLine {
            start,
            end,
            number,
            logical,
        });
        start = end;
        number += 1;
    }
    if start < text.len() || text.is_empty() {
        lines.push(SourceLine {
            start,
            end: text.len(),
            number,
            logical: &text[start..],
        });
    }
    lines
}

pub(super) fn line_number_at(text: &str, offset: usize) -> u64 {
    1 + text.as_bytes()[..offset]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryDestination, MemoryType};

    use super::super::{
        CaptureGitSourceContext, CaptureSemanticLocation, CaptureSourceDocument,
        CaptureSourceLocator, CaptureSourceMemberSnapshot, CaptureSourceSnapshot, content_hash,
        extractor_identity,
    };

    fn path_document(path: &str, text: &str) -> CaptureSourceDocument {
        let locator = CaptureSourceLocator::ProjectPath {
            path: path.to_owned(),
        };
        let source = CaptureSourceRequest {
            source_id: "source".to_owned(),
            locator: locator.clone(),
            media_type: if matches!(
                std::path::Path::new(path)
                    .extension()
                    .and_then(|value| value.to_str()),
                Some("diff" | "patch")
            ) {
                "text/x-diff".to_owned()
            } else {
                "text/markdown".to_owned()
            },
            git: None,
        };
        CaptureSourceDocument {
            request: source.clone(),
            snapshot: CaptureSourceSnapshot {
                source_id: source.source_id.clone(),
                locator,
                media_type: source.media_type.clone(),
                byte_length: text.len() as u64,
                source_content_hash: content_hash(text.as_bytes()),
                members: Vec::<CaptureSourceMemberSnapshot>::new(),
                policy_inputs: Vec::new(),
            },
            bytes: text.as_bytes().to_vec(),
        }
    }

    fn loaded(document: CaptureSourceDocument) -> CaptureLoadedSource {
        CaptureLoadedSource {
            snapshot: document.snapshot.clone(),
            documents: vec![document],
        }
    }

    fn git_document(text: &str, algorithm: &str) -> CaptureSourceDocument {
        let mut document = path_document("reviewed.diff", text);
        let digest_len = if algorithm == "sha256" { 64 } else { 40 };
        document.request.git = Some(CaptureGitSourceContext {
            repository: ".".to_owned(),
            base: format!("{algorithm}:{}", "1".repeat(digest_len)),
            head: format!("{algorithm}:{}", "2".repeat(digest_len)),
        });
        document
    }

    fn supplied_git_document(text: &str, algorithm: &str) -> CaptureSourceDocument {
        let mut document = git_document(text, algorithm);
        let locator = CaptureSourceLocator::SuppliedBytes {
            display_name: "reviewed.diff".to_owned(),
            media_type: "text/x-diff".to_owned(),
            byte_length: text.len() as u64,
            source_content_hash: content_hash(text.as_bytes()),
        };
        document.request.locator = locator.clone();
        document.snapshot.locator = locator;
        document
    }

    fn git_range_document(text: &str, algorithm: &str) -> CaptureSourceDocument {
        let digest_len = if algorithm == "sha256" { 64 } else { 40 };
        let locator = CaptureSourceLocator::GitRange {
            repository: ".".to_owned(),
            base: format!("{algorithm}:{}", "1".repeat(digest_len)),
            head: format!("{algorithm}:{}", "2".repeat(digest_len)),
            merge_parent: "base_to_head".to_owned(),
            rename_detection: false,
            diff_format: "git-unified".to_owned(),
        };
        let mut document = path_document("reviewed.diff", text);
        document.request.locator = locator.clone();
        document.request.git = None;
        document.snapshot.locator = locator;
        document
    }

    fn added_guidance_diff(path: &str, prefix: &str) -> String {
        format!(
            "diff --git a/{path} b/{path}\nnew file mode 100644\nindex {}..{}\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,3 @@\n+{prefix}\n+# Decision: Keep evidence\n+Keep the source span.\n",
            "0".repeat(40),
            "2".repeat(40),
        )
    }

    fn assert_exact_evidence(document: &CaptureSourceDocument, extraction: &CaptureExtraction) {
        let text = std::str::from_utf8(&document.bytes).unwrap();
        for evidence in extraction
            .candidates
            .iter()
            .flat_map(|candidate| &candidate.evidence)
        {
            let excerpt = &text[evidence.span.byte_start as usize..evidence.span.byte_end as usize];
            assert_eq!(evidence.text.as_deref(), Some(excerpt));
            assert_eq!(
                evidence.evidence_content_hash,
                content_hash(excerpt.as_bytes())
            );
        }
    }

    #[test]
    fn instruction_adapter_preserves_nested_types_and_excludes_generated_block() {
        let text = "# Agent rules\n\nAlways review changes.\n\n<!-- memzoi:start -->\n## Memzoi\nGenerated policy text.\n<!-- memzoi:end -->\n\n## Decision: Preserve provenance\n\nKeep exact source spans.\n\n### Temporary\n\nOnly for this task.\n";
        let document = path_document("nested/AGENTS.md", text);
        let extractor = extractor_identity(INSTRUCTION_EXTRACTOR_PROFILE).unwrap();
        let extraction = instruction::extract(&document, &extractor, None).unwrap();

        assert_eq!(extraction.candidates.len(), 3);
        assert_eq!(
            extraction.candidates[0].memory.memory_type,
            MemoryType::Procedure
        );
        assert_eq!(extraction.candidates[0].memory.scope.paths, ["nested"]);
        assert_eq!(
            extraction.candidates[1].memory.memory_type,
            MemoryType::Decision
        );
        assert_eq!(extraction.candidates[1].memory.title, "Preserve provenance");
        assert_eq!(
            extraction.candidates[1].evidence[0].heading_path,
            ["Agent rules", "Decision: Preserve provenance"]
        );
        assert_eq!(
            extraction.candidates[2].classification.destination,
            MemoryDestination::NeedsReview
        );
        assert!(
            extraction
                .candidates
                .iter()
                .flat_map(|candidate| &candidate.evidence)
                .all(|evidence| !evidence.text.as_deref().unwrap().contains("memzoi:start"))
        );
        assert_exact_evidence(&document, &extraction);
    }

    #[test]
    fn instruction_marker_scan_still_finds_secret_inside_excluded_block() {
        let text = "<!-- memzoi:start -->\npassword = hunter2\n<!-- memzoi:end -->\n";
        assert!(
            prohibited_finding_for_profile(INSTRUCTION_EXTRACTOR_PROFILE, text.as_bytes())
                .is_some()
        );
    }

    #[test]
    fn ambiguous_instruction_body_and_preamble_never_route_repo_safe() {
        let extractor = extractor_identity(INSTRUCTION_EXTRACTOR_PROFILE).unwrap();
        let preamble = path_document(
            "AGENTS.md",
            "Private: only use this for the current task.\n",
        );
        let extraction = instruction::extract(&preamble, &extractor, None).unwrap();
        assert_eq!(
            extraction.candidates[0].classification.destination,
            MemoryDestination::NeedsReview
        );

        let section = path_document(
            "AGENTS.md",
            "# Repository rules\n\nLocal only: keep this instruction on this machine.\n",
        );
        let extraction = instruction::extract(&section, &extractor, None).unwrap();
        assert_eq!(
            extraction.candidates[0].classification.destination,
            MemoryDestination::NeedsReview
        );
    }

    #[test]
    fn git_diff_marker_scan_still_finds_raw_chat_roles() {
        let text = "diff --git a/chat.txt b/chat.txt\n--- a/chat.txt\n+++ b/chat.txt\n@@ -0,0 +1,2 @@\n+User: keep this\n+Assistant: no\n";
        assert!(
            prohibited_finding_for_profile(GIT_CHANGE_EXTRACTOR_PROFILE, text.as_bytes()).is_some()
        );
    }

    #[test]
    fn generated_instruction_projection_produces_no_candidates() {
        let text = "<!-- Generated by memzoi. Do not edit directly. -->\n# Memzoi Projection for AGENTS.md\n";
        let document = path_document("AGENTS.md", text);
        let extractor = extractor_identity(INSTRUCTION_EXTRACTOR_PROFILE).unwrap();
        let extraction = instruction::extract(&document, &extractor, None).unwrap();
        assert!(extraction.candidates.is_empty());
        assert_eq!(
            extraction.diagnostics[0].code,
            "generated_projection_excluded"
        );
    }

    #[test]
    fn adr_adapter_maps_status_fields_and_supersession_safely() {
        let text = "# ADR 7: Evidence identity\n\n## Status\n\nAccepted\n\n## Context\n\nClaims need exact evidence.\n\n## Decision\n\nUse byte and line spans.\n\n## Risks\n\nOffsets can drift after edits.\n\n## Supersedes\n\nADR 3\n";
        let document = path_document("docs/adr/0007-evidence.md", text);
        let extractor = extractor_identity(ADR_EXTRACTOR_PROFILE).unwrap();
        let extraction = adr::extract(&loaded(document.clone()), &extractor, None).unwrap();

        assert_eq!(extraction.candidates.len(), 4);
        assert_eq!(
            extraction.candidates[0].memory.memory_type,
            MemoryType::Fact
        );
        assert_eq!(
            extraction.candidates[1].memory.memory_type,
            MemoryType::Decision
        );
        assert_eq!(
            extraction.candidates[2].memory.memory_type,
            MemoryType::Risk
        );
        assert_eq!(
            extraction.candidates[3].classification.destination,
            MemoryDestination::NeedsReview
        );
        assert!(
            extraction.candidates[3]
                .evidence
                .iter()
                .any(|evidence| matches!(
                    evidence.semantic_location,
                    Some(CaptureSemanticLocation::Adr {
                        ref field,
                        target: Some(ref target),
                        ..
                    }) if field == "supersession" && target == "ADR 3"
                ))
        );
        let decision_evidence = &extraction.candidates[1].evidence;
        assert_eq!(decision_evidence.len(), 3);
        assert!(decision_evidence.iter().any(|evidence| matches!(
            evidence.semantic_location,
            Some(CaptureSemanticLocation::Adr { ref field, ref status, .. })
                if field == "decision" && status == "accepted"
        )));
        assert!(decision_evidence.iter().any(|evidence| {
            evidence.text.as_deref() == Some("# ADR 7: Evidence identity\n")
                && matches!(
                    evidence.semantic_location,
                    Some(CaptureSemanticLocation::Adr { ref field, ref status, .. })
                        if field == "title" && status == "accepted"
                )
        }));
        assert!(decision_evidence.iter().any(|evidence| {
            evidence.text.as_deref() == Some("## Status\n\nAccepted\n\n")
                && matches!(
                    evidence.semantic_location,
                    Some(CaptureSemanticLocation::Adr { ref field, ref status, .. })
                        if field == "status" && status == "accepted"
                )
        }));
        assert_exact_evidence(&document, &extraction);
    }

    #[test]
    fn adr_frontmatter_title_and_status_are_exact_candidate_evidence() {
        let text = "---\ntitle: Durable evidence\nstatus: accepted\n---\n\n## Decision\n\nKeep exact source spans.\n";
        let document = path_document("docs/adr/0010-evidence.md", text);
        let extractor = extractor_identity(ADR_EXTRACTOR_PROFILE).unwrap();
        let extraction = adr::extract(&loaded(document.clone()), &extractor, None).unwrap();
        let evidence = &extraction.candidates[0].evidence;
        assert_eq!(evidence.len(), 3);
        assert!(evidence.iter().any(|item| {
            item.text.as_deref() == Some("title: Durable evidence\n")
                && item.section_kind == "title"
        }));
        assert!(evidence.iter().any(|item| {
            item.text.as_deref() == Some("status: accepted\n") && item.section_kind == "status"
        }));
        assert_exact_evidence(&document, &extraction);
    }

    #[test]
    fn conflicting_adr_status_authorities_never_route_to_repo() {
        let text = "---\ntitle: Conflicted ADR\nstatus: accepted\n---\n\n# Conflicted ADR\n\n## Status\n\nDraft\n\n## Decision\n\nDo not trust conflicting authority.\n";
        let document = path_document("docs/adr/conflicted.md", text);
        let extractor = extractor_identity(ADR_EXTRACTOR_PROFILE).unwrap();
        let extraction = adr::extract(&loaded(document), &extractor, None).unwrap();
        assert!(extraction.candidates.iter().all(|candidate| {
            candidate.classification.destination == MemoryDestination::NeedsReview
                && candidate.evidence.iter().all(|evidence| {
                    matches!(
                        evidence.semantic_location,
                        Some(CaptureSemanticLocation::Adr { ref status, .. }) if status == "unknown"
                    )
                })
        }));
        assert!(
            extraction
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "conflicting_adr_status")
        );
    }

    #[test]
    fn adr_without_evidenced_title_or_status_emits_no_candidate() {
        let text = "## Decision\n\nMissing ADR authority.\n";
        let document = path_document("docs/adr/malformed.md", text);
        let extractor = extractor_identity(ADR_EXTRACTOR_PROFILE).unwrap();
        let extraction = adr::extract(&loaded(document), &extractor, None).unwrap();
        assert!(extraction.candidates.is_empty());
        assert!(
            extraction
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "malformed_adr_metadata")
        );
    }

    #[test]
    fn draft_adr_never_routes_directly_to_repo() {
        let text = "# Draft ADR\n\n## Status\n\nProposed\n\n## Decision\n\nTry a new index.\n";
        let document = path_document("docs/adr/draft.md", text);
        let extractor = extractor_identity(ADR_EXTRACTOR_PROFILE).unwrap();
        let extraction = adr::extract(&loaded(document), &extractor, None).unwrap();
        assert!(extraction.candidates.iter().all(|candidate| {
            candidate.classification.destination == MemoryDestination::NeedsReview
        }));
    }

    #[test]
    fn git_change_adapter_emits_only_typed_added_guidance_with_raw_span() {
        let old = "1".repeat(40);
        let new = "2".repeat(40);
        let text = format!(
            "diff --git a/docs/rules.md b/docs/rules.md\nindex {old}..{new} 100644\n--- a/docs/rules.md\n+++ b/docs/rules.md\n@@ -1,1 +1,4 @@\n existing\n+# Warning: Preserve evidence\n+Never detach claims from source spans.\n+\n"
        );
        let mut document = path_document("reviewed.diff", &text);
        document.request.git = Some(CaptureGitSourceContext {
            repository: ".".to_owned(),
            base: format!("sha1:{old}"),
            head: format!("sha1:{new}"),
        });
        let extractor = extractor_identity(GIT_CHANGE_EXTRACTOR_PROFILE).unwrap();
        let extraction = git_change::extract(&document, &extractor, None).unwrap();

        assert_eq!(extraction.candidates.len(), 1);
        let candidate = &extraction.candidates[0];
        assert_eq!(candidate.memory.memory_type, MemoryType::Warning);
        assert_eq!(candidate.memory.title, "Preserve evidence");
        assert!(
            candidate.evidence[0]
                .text
                .as_deref()
                .unwrap()
                .starts_with("+# Warning:")
        );
        assert!(matches!(
            candidate.evidence[0].semantic_location,
            Some(CaptureSemanticLocation::GitChange {
                ref new_path,
                new_line_start: Some(2),
                new_line_end: Some(4),
                ..
            }) if new_path.as_deref() == Some("docs/rules.md")
        ));
        assert_exact_evidence(&document, &extraction);
    }

    #[test]
    fn git_rename_and_deleted_guidance_are_diagnostics_not_fabricated_summaries() {
        let rename = "diff --git a/docs/old.md b/docs/new.md\nsimilarity index 100%\nrename from docs/old.md\nrename to docs/new.md\n";
        let mut rename_document = path_document("rename.diff", rename);
        rename_document.request.git = Some(CaptureGitSourceContext {
            repository: ".".to_owned(),
            base: format!("sha1:{}", "1".repeat(40)),
            head: format!("sha1:{}", "2".repeat(40)),
        });
        let extractor = extractor_identity(GIT_CHANGE_EXTRACTOR_PROFILE).unwrap();
        let extraction = git_change::extract(&rename_document, &extractor, None).unwrap();
        assert!(extraction.candidates.is_empty());
        assert_eq!(
            extraction.diagnostics[0].code,
            "git_rename_without_durable_candidate"
        );

        let old = "1".repeat(40);
        let deleted = format!(
            "diff --git a/docs/old.md b/docs/old.md\ndeleted file mode 100644\nindex {old}..{}\n--- a/docs/old.md\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-# Decision: Old rule\n-Do the old thing.\n",
            "0".repeat(40)
        );
        let mut deleted_document = path_document("deleted.diff", &deleted);
        deleted_document.request.git = rename_document.request.git.clone();
        let extraction = git_change::extract(&deleted_document, &extractor, None).unwrap();
        assert_eq!(extraction.candidates.len(), 1);
        let deleted_candidate = &extraction.candidates[0];
        assert_eq!(
            deleted_candidate.classification.destination,
            MemoryDestination::NeedsReview
        );
        assert!(
            deleted_candidate
                .memory
                .tags
                .contains(&"git-deletion".to_owned())
        );
        let expected_old_blob = format!("sha1:{old}");
        assert!(matches!(
            deleted_candidate.evidence[0].semantic_location,
            Some(CaptureSemanticLocation::GitChange {
                ref side,
                ref old_blob,
                ref new_blob,
                old_line_start: Some(1),
                old_line_end: Some(2),
                ..
            }) if side == "old"
                && old_blob.as_deref() == Some(expected_old_blob.as_str())
                && new_blob.is_none()
        ));
    }

    #[test]
    fn git_change_candidate_stops_at_every_markdown_heading() {
        let old = "1".repeat(40);
        let new = "2".repeat(40);
        let text = format!(
            "diff --git a/docs/rules.md b/docs/rules.md\nindex {old}..{new} 100644\n--- a/docs/rules.md\n+++ b/docs/rules.md\n@@ -1,1 +1,5 @@\n existing\n+# Decision: Keep evidence\n+Keep the exact span.\n+## Notes\n+This is not decision evidence.\n"
        );
        let document = git_document(&text, "sha1");
        let extractor = extractor_identity(GIT_CHANGE_EXTRACTOR_PROFILE).unwrap();
        let extraction = git_change::extract(&document, &extractor, None).unwrap();
        assert_eq!(extraction.candidates.len(), 1);
        assert_eq!(extraction.candidates[0].memory.body, "Keep the exact span.");
        assert_eq!(
            extraction.candidates[0].evidence[0].text.as_deref(),
            Some("+# Decision: Keep evidence\n+Keep the exact span.\n")
        );
    }

    #[test]
    fn git_diff_path_authorities_must_not_contradict_each_other() {
        let old = "1".repeat(40);
        let new = "2".repeat(40);
        let marker_mismatch = format!(
            "diff --git a/docs/rules.md b/docs/rules.md\nindex {old}..{new} 100644\n--- a/docs/other.md\n+++ b/docs/rules.md\n@@ -1,1 +1,3 @@\n existing\n+# Decision: Keep evidence\n+Keep the exact span.\n"
        );
        let extractor = extractor_identity(GIT_CHANGE_EXTRACTOR_PROFILE).unwrap();
        assert!(
            git_change::extract(&git_document(&marker_mismatch, "sha1"), &extractor, None).is_err()
        );

        let rename_mismatch = "diff --git a/docs/old.md b/docs/new.md\nsimilarity index 100%\nrename from docs/not-old.md\nrename to docs/new.md\n";
        assert!(
            git_change::extract(&git_document(rename_mismatch, "sha1"), &extractor, None).is_err()
        );
    }

    #[test]
    fn evidence_bearing_git_changes_require_full_matching_blob_algorithms() {
        let abbreviated = "diff --git a/docs/rules.md b/docs/rules.md\nindex abc123..def456 100644\n--- a/docs/rules.md\n+++ b/docs/rules.md\n@@ -1,1 +1,3 @@\n existing\n+# Decision: Keep evidence\n+Keep the exact span.\n";
        let extractor = extractor_identity(GIT_CHANGE_EXTRACTOR_PROFILE).unwrap();
        assert!(git_change::extract(&git_document(abbreviated, "sha1"), &extractor, None).is_err());

        let old = "1".repeat(40);
        let new = "2".repeat(40);
        let wrong_algorithm = format!(
            "diff --git a/docs/rules.md b/docs/rules.md\nindex {old}..{new} 100644\n--- a/docs/rules.md\n+++ b/docs/rules.md\n@@ -1,1 +1,3 @@\n existing\n+# Decision: Keep evidence\n+Keep the exact span.\n"
        );
        assert!(
            git_change::extract(&git_document(&wrong_algorithm, "sha256"), &extractor, None)
                .is_err()
        );

        let valid_create = added_guidance_diff("docs/new.md", "");
        let extraction =
            git_change::extract(&git_document(&valid_create, "sha1"), &extractor, None).unwrap();
        assert_eq!(extraction.candidates.len(), 1);
    }

    #[test]
    fn git_evidence_requires_authoritative_regular_blob_modes() {
        let extractor = extractor_identity(GIT_CHANGE_EXTRACTOR_PROFILE).unwrap();
        let old = "1".repeat(40);
        let new = "2".repeat(40);
        let missing_mode = format!(
            "diff --git a/docs/rules.md b/docs/rules.md\nindex {old}..{new}\n--- a/docs/rules.md\n+++ b/docs/rules.md\n@@ -1,1 +1,3 @@\n existing\n+# Decision: Keep evidence\n+Keep the exact span.\n"
        );
        assert!(
            git_change::extract(
                &supplied_git_document(&missing_mode, "sha1"),
                &extractor,
                None,
            )
            .is_err()
        );

        for mode in ["120000", "160000"] {
            let nonregular = format!(
                "diff --git a/docs/link.md b/docs/link.md\nnew file mode {mode}\nindex {}..{new}\n--- /dev/null\n+++ b/docs/link.md\n@@ -0,0 +1,2 @@\n+# Decision: Never treat this as a blob\n+Nonregular content is not durable evidence.\n",
                "0".repeat(40),
            );
            assert!(
                git_change::extract(
                    &supplied_git_document(&nonregular, "sha1"),
                    &extractor,
                    None,
                )
                .is_err(),
                "supplied mode {mode}"
            );
            assert!(
                git_change::extract(&git_range_document(&nonregular, "sha1"), &extractor, None,)
                    .is_err(),
                "GitRange mode {mode}"
            );
        }

        let regular_mode_change = format!(
            "diff --git a/docs/rules.md b/docs/rules.md\nold mode 100644\nnew mode 100755\nindex {old}..{new}\n--- a/docs/rules.md\n+++ b/docs/rules.md\n@@ -1,1 +1,3 @@\n existing\n+# Decision: Keep evidence\n+Keep the exact span.\n"
        );
        let extraction = git_change::extract(
            &supplied_git_document(&regular_mode_change, "sha1"),
            &extractor,
            None,
        )
        .unwrap();
        assert_eq!(extraction.candidates.len(), 1);
    }

    #[test]
    fn unsafe_git_paths_and_managed_projections_only_emit_diagnostics() {
        let extractor = extractor_identity(GIT_CHANGE_EXTRACTOR_PROFILE).unwrap();
        let excluded = [
            ("generated/guide.md", "git_generated_path_excluded"),
            ("vendor/guide.md", "git_vendor_path_excluded"),
            ("node_modules/pkg/guide.md", "git_dependency_path_excluded"),
            (
                ".memzoi/exports/AGENTS.md",
                "git_managed_projection_excluded",
            ),
            (".cache/guide.md", "git_ignored_like_path_excluded"),
        ];
        for (path, diagnostic) in excluded {
            let diff = added_guidance_diff(path, "");
            let extraction =
                git_change::extract(&git_document(&diff, "sha1"), &extractor, None).unwrap();
            assert!(extraction.candidates.is_empty(), "unsafe path {path}");
            assert_eq!(
                extraction.diagnostics[0].code, diagnostic,
                "unsafe path {path}"
            );
        }

        let projection = added_guidance_diff("AGENTS.md", "<!-- memzoi:start -->");
        let extraction =
            git_change::extract(&git_document(&projection, "sha1"), &extractor, None).unwrap();
        assert!(extraction.candidates.is_empty());
        assert_eq!(
            extraction.diagnostics[0].code,
            "git_managed_projection_excluded"
        );
    }

    #[test]
    fn dispatch_keeps_markdown_adapter_on_default_path() {
        let document = path_document("notes.md", "# Fact: Existing\n\nDefault behavior.\n");
        let source = document.request.clone();
        let loaded = loaded(document);
        let extractor = extractor_identity(MARKDOWN_EXTRACTOR_PROFILE).unwrap();
        let extraction = extract_profile(
            &source,
            &loaded,
            &extractor,
            MARKDOWN_EXTRACTOR_PROFILE,
            None,
        )
        .unwrap();
        assert_eq!(extraction.candidates.len(), 1);
        assert_eq!(
            extraction.candidates[0].memory.memory_type,
            MemoryType::Fact
        );
    }
}
