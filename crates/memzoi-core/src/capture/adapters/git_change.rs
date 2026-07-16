use anyhow::{Context, Result, bail};

use super::source_lines;
use crate::{
    MemoryDestination, MemoryType, OkfProposalSensitivity, RepositoryContentClass, ScopeKind,
};

use super::super::{
    CAPTURE_MAX_GIT_CHANGED_FILES, CAPTURE_MAX_GIT_DIFF_HUNKS, CaptureDiagnostic, CaptureEvidence,
    CaptureExtraction, CaptureExtractorIdentity, CaptureMemoryDraft, CapturePlanningControl,
    CaptureScope, CaptureSemanticLocation, CaptureSourceDocument, CaptureSourceLocator,
    EvidenceLocation, candidate, check_planning_control, content_hash, evidence_for,
    parse_atx_heading, typed_heading,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl ChangeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
        }
    }
}

#[derive(Debug, Clone)]
struct AddedLine {
    content: String,
    start: usize,
    end: usize,
    source_line: u64,
    new_line: u64,
}

#[derive(Debug, Clone)]
struct RemovedLine {
    content: String,
    start: usize,
    end: usize,
    source_line: u64,
    old_line: u64,
}

#[derive(Debug, Clone)]
struct DiffHunk {
    header: String,
    old_start: u64,
    old_count: u64,
    new_start: u64,
    new_count: u64,
    added_blocks: Vec<Vec<AddedLine>>,
    removed_blocks: Vec<Vec<RemovedLine>>,
    deleted_typed_guidance: bool,
}

#[derive(Debug, Clone)]
struct DiffFile {
    source_line: u64,
    old_path: Option<String>,
    new_path: Option<String>,
    old_blob: Option<String>,
    new_blob: Option<String>,
    old_index_blob: Option<String>,
    new_index_blob: Option<String>,
    old_mode: Option<String>,
    new_mode: Option<String>,
    index_mode: Option<String>,
    index_present: bool,
    kind: ChangeKind,
    hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone)]
struct GitIdentity {
    repository: String,
    base: String,
    head: String,
    algorithm: String,
}

pub(super) fn mask_diff_side_markers_for_scan(bytes: &[u8]) -> Vec<u8> {
    let mut masked = bytes.to_vec();
    let mut line_start = true;
    for byte in &mut masked {
        if line_start && matches!(*byte, b'+' | b'-') {
            *byte = b' ';
        }
        line_start = *byte == b'\n';
    }
    masked
}

pub(super) fn changed_paths(text: &str) -> Result<Vec<String>> {
    let mut paths = parse_diff(text, None)?
        .into_iter()
        .flat_map(|file| file.old_path.into_iter().chain(file.new_path))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

pub(super) fn extract(
    document: &CaptureSourceDocument,
    extractor: &CaptureExtractorIdentity,
    control: Option<&CapturePlanningControl>,
) -> Result<CaptureExtraction> {
    check_planning_control(control)?;
    let text = std::str::from_utf8(&document.bytes)?;
    let identity = git_identity(document)?;
    let files = parse_diff(text, control)?;
    let mut candidates = Vec::new();
    let mut diagnostics = Vec::new();

    for file in files {
        check_planning_control(control)?;
        if let Some(code) = excluded_git_file_diagnostic(&file) {
            diagnostics.push(CaptureDiagnostic {
                code: code.to_owned(),
                source_id: Some(document.request.source_id.clone()),
                line: Some(file.source_line),
            });
            continue;
        }
        let mut file_candidates = 0usize;
        let deleted_typed = file.hunks.iter().any(|hunk| hunk.deleted_typed_guidance);
        for hunk in &file.hunks {
            for block in &hunk.removed_blocks {
                let mut index = 0usize;
                while index < block.len() {
                    let line = &block[index];
                    let Some((_, heading)) = parse_atx_heading(&line.content) else {
                        index += 1;
                        continue;
                    };
                    let (typed, typed_title) = typed_heading(&heading);
                    let Some((memory_type, lane, _, _, section_kind)) = typed else {
                        index += 1;
                        continue;
                    };
                    if !matches!(
                        memory_type,
                        MemoryType::Decision
                            | MemoryType::Procedure
                            | MemoryType::Warning
                            | MemoryType::Risk
                            | MemoryType::FailedAttempt
                    ) {
                        index += 1;
                        continue;
                    }
                    let mut end = index + 1;
                    while end < block.len() && parse_atx_heading(&block[end].content).is_none() {
                        end += 1;
                    }
                    let body = block[index + 1..end]
                        .iter()
                        .map(|line| line.content.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim()
                        .to_owned();
                    if body.is_empty() || typed_title.trim().is_empty() {
                        index = end;
                        continue;
                    }
                    require_removed_evidence_blob_ids(&file, &identity.algorithm)?;
                    let last = &block[end - 1];
                    let mut evidence = evidence_for(
                        &document.request,
                        &document.snapshot,
                        text,
                        EvidenceLocation {
                            start: line.start,
                            end: last.end,
                            line_start: line.source_line,
                            section_kind,
                            heading_path: std::slice::from_ref(&heading),
                        },
                    )?;
                    let new_line_end = inclusive_line_end(hunk.new_start, hunk.new_count)?;
                    apply_git_old_location(
                        &mut evidence,
                        &identity,
                        &file,
                        hunk,
                        line.old_line,
                        last.old_line,
                        new_line_end,
                    );
                    let path = file
                        .old_path
                        .as_ref()
                        .context("removed Git diff candidate has no safe old path")?
                        .clone();
                    candidates.push(candidate(
                        CaptureMemoryDraft {
                            memory_type,
                            lane,
                            title: typed_title.trim().to_owned(),
                            body,
                            scope: CaptureScope {
                                kind: ScopeKind::Repo,
                                id: None,
                                paths: vec![path],
                            },
                            tags: vec!["git-change".to_owned(), "git-deletion".to_owned()],
                        },
                        evidence,
                        extractor,
                        MemoryDestination::NeedsReview,
                        OkfProposalSensitivity::Unknown,
                        RepositoryContentClass::Unknown,
                        "deleted_git_guidance_requires_review",
                        "deleted_git_guidance_has_no_direct_lifecycle_authority",
                    )?);
                    file_candidates += 1;
                    index = end;
                }
            }
        }
        for hunk in &file.hunks {
            for block in &hunk.added_blocks {
                let mut index = 0usize;
                while index < block.len() {
                    let line = &block[index];
                    let Some((_, heading)) = parse_atx_heading(&line.content) else {
                        index += 1;
                        continue;
                    };
                    let (typed, typed_title) = typed_heading(&heading);
                    let Some((memory_type, lane, _, _, section_kind)) = typed else {
                        index += 1;
                        continue;
                    };
                    if !matches!(
                        memory_type,
                        MemoryType::Decision
                            | MemoryType::Procedure
                            | MemoryType::Warning
                            | MemoryType::Risk
                            | MemoryType::FailedAttempt
                    ) {
                        index += 1;
                        continue;
                    }
                    let mut end = index + 1;
                    while end < block.len() {
                        if parse_atx_heading(&block[end].content).is_some() {
                            break;
                        }
                        end += 1;
                    }
                    let body = block[index + 1..end]
                        .iter()
                        .map(|line| line.content.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim()
                        .to_owned();
                    if body.is_empty() || typed_title.trim().is_empty() {
                        index = end;
                        continue;
                    }
                    require_evidence_blob_ids(&file, &identity.algorithm)?;
                    let last = &block[end - 1];
                    let mut evidence = evidence_for(
                        &document.request,
                        &document.snapshot,
                        text,
                        EvidenceLocation {
                            start: line.start,
                            end: last.end,
                            line_start: line.source_line,
                            section_kind,
                            heading_path: std::slice::from_ref(&heading),
                        },
                    )?;
                    let new_line_end = block[index..end]
                        .last()
                        .map(|line| line.new_line)
                        .unwrap_or(line.new_line);
                    let old_line_end = inclusive_line_end(hunk.old_start, hunk.old_count)?;
                    apply_git_location(
                        &mut evidence,
                        &identity,
                        &file,
                        hunk,
                        line.new_line,
                        new_line_end,
                        old_line_end,
                    );
                    let path = file
                        .new_path
                        .as_ref()
                        .or(file.old_path.as_ref())
                        .context("Git diff candidate has no safe path")?
                        .clone();
                    candidates.push(candidate(
                        CaptureMemoryDraft {
                            memory_type,
                            lane,
                            title: typed_title.trim().to_owned(),
                            body,
                            scope: CaptureScope {
                                kind: ScopeKind::Repo,
                                id: None,
                                paths: vec![path],
                            },
                            tags: vec!["git-change".to_owned()],
                        },
                        evidence,
                        extractor,
                        MemoryDestination::Repo,
                        OkfProposalSensitivity::RepoSafe,
                        RepositoryContentClass::GeneralRepoKnowledge,
                        "deterministic_typed_added_git_guidance",
                        "explicit_repo_diff_passed_safeguards",
                    )?);
                    file_candidates += 1;
                    index = end;
                }
            }
        }
        if file_candidates == 0 {
            diagnostics.push(CaptureDiagnostic {
                code: if deleted_typed {
                    "git_deleted_durable_guidance_requires_review"
                } else {
                    match file.kind {
                        ChangeKind::Deleted => "git_deleted_file_without_durable_candidate",
                        ChangeKind::Renamed => "git_rename_without_durable_candidate",
                        ChangeKind::Added | ChangeKind::Modified => "unsupported_git_change",
                    }
                }
                .to_owned(),
                source_id: Some(document.request.source_id.clone()),
                line: Some(file.source_line),
            });
        }
    }
    Ok(CaptureExtraction {
        candidates,
        diagnostics,
    })
}

fn git_identity(document: &CaptureSourceDocument) -> Result<GitIdentity> {
    let (repository, base, head) = match &document.request.locator {
        CaptureSourceLocator::GitRange {
            repository,
            base,
            head,
            ..
        } => (repository.clone(), base.clone(), head.clone()),
        CaptureSourceLocator::ProjectPath { .. } | CaptureSourceLocator::SuppliedBytes { .. } => {
            let git = document
                .request
                .git
                .as_ref()
                .context("Git diff source is missing revision context")?;
            (git.repository.clone(), git.base.clone(), git.head.clone())
        }
        CaptureSourceLocator::ProjectDirectory { .. } => {
            return Err(anyhow::anyhow!(
                "Git-change extraction does not accept a project directory"
            ));
        }
    };
    let base_algorithm = object_algorithm(&base)?;
    let head_algorithm = object_algorithm(&head)?;
    if base_algorithm != head_algorithm {
        bail!("Git diff revision object algorithms do not match");
    }
    Ok(GitIdentity {
        repository,
        base,
        head,
        algorithm: base_algorithm.to_owned(),
    })
}

fn object_algorithm(value: &str) -> Result<&'static str> {
    let (algorithm, digest) = value
        .split_once(':')
        .context("Git diff revision object ID is malformed")?;
    let expected = match algorithm {
        "sha1" => 40,
        "sha256" => 64,
        _ => bail!("Git diff revision object algorithm is unsupported"),
    };
    if digest.len() != expected
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || digest.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        bail!("Git diff revision object ID must be full and lowercase");
    }
    Ok(if algorithm == "sha1" {
        "sha1"
    } else {
        "sha256"
    })
}

fn apply_git_location(
    evidence: &mut [CaptureEvidence],
    identity: &GitIdentity,
    file: &DiffFile,
    hunk: &DiffHunk,
    new_line_start: u64,
    new_line_end: u64,
    old_line_end: Option<u64>,
) {
    let location = CaptureSemanticLocation::GitChange {
        repository: identity.repository.clone(),
        base: identity.base.clone(),
        head: identity.head.clone(),
        old_blob: file.old_blob.clone(),
        new_blob: file.new_blob.clone(),
        old_path: file.old_path.clone(),
        new_path: file.new_path.clone(),
        change_kind: file.kind.as_str().to_owned(),
        hunk: content_hash(hunk.header.as_bytes()),
        side: "new".to_owned(),
        old_line_start: (hunk.old_count > 0).then_some(hunk.old_start),
        old_line_end,
        new_line_start: Some(new_line_start),
        new_line_end: Some(new_line_end),
    };
    for item in evidence {
        item.semantic_location = Some(location.clone());
    }
}

fn apply_git_old_location(
    evidence: &mut [CaptureEvidence],
    identity: &GitIdentity,
    file: &DiffFile,
    hunk: &DiffHunk,
    old_line_start: u64,
    old_line_end: u64,
    new_line_end: Option<u64>,
) {
    let location = CaptureSemanticLocation::GitChange {
        repository: identity.repository.clone(),
        base: identity.base.clone(),
        head: identity.head.clone(),
        old_blob: file.old_blob.clone(),
        new_blob: file.new_blob.clone(),
        old_path: file.old_path.clone(),
        new_path: file.new_path.clone(),
        change_kind: file.kind.as_str().to_owned(),
        hunk: content_hash(hunk.header.as_bytes()),
        side: "old".to_owned(),
        old_line_start: Some(old_line_start),
        old_line_end: Some(old_line_end),
        new_line_start: (hunk.new_count > 0).then_some(hunk.new_start),
        new_line_end,
    };
    for item in evidence {
        item.semantic_location = Some(location.clone());
    }
}

fn parse_diff(text: &str, control: Option<&CapturePlanningControl>) -> Result<Vec<DiffFile>> {
    let lines = source_lines(text);
    let mut files = Vec::new();
    let mut hunk_count = 0usize;
    let mut index = 0usize;
    while index < lines.len() {
        check_planning_control(control)?;
        let line = lines[index];
        if line.logical.is_empty() && index + 1 == lines.len() {
            break;
        }
        if line.logical.starts_with("diff --cc ")
            || line.logical.starts_with("diff --combined ")
            || line.logical.starts_with("@@@ ")
        {
            bail!("combined Git diffs are unsupported; select one explicit merge parent");
        }
        let Some(rest) = line.logical.strip_prefix("diff --git ") else {
            bail!("Git diff must begin each file with a strict diff --git header");
        };
        let mut paths = rest.split_whitespace();
        let old_header = paths
            .next()
            .context("Git diff header is missing old path")?;
        let new_header = paths
            .next()
            .context("Git diff header is missing new path")?;
        if paths.next().is_some() || old_header.starts_with('"') || new_header.starts_with('"') {
            bail!("quoted or whitespace-containing Git diff paths are unsupported");
        }
        let header_old_path = parse_prefixed_path(old_header, "a/")?;
        let header_new_path = parse_prefixed_path(new_header, "b/")?;
        let mut file = DiffFile {
            source_line: line.number,
            old_path: None,
            new_path: None,
            old_blob: None,
            new_blob: None,
            old_index_blob: None,
            new_index_blob: None,
            old_mode: None,
            new_mode: None,
            index_mode: None,
            index_present: false,
            kind: ChangeKind::Modified,
            hunks: Vec::new(),
        };
        let mut mode_kind = None;
        let mut rename_from = None;
        let mut rename_to = None;
        let mut old_marker: Option<Option<String>> = None;
        let mut new_marker: Option<Option<String>> = None;
        let mut saw_index = false;
        index += 1;
        while index < lines.len() && !lines[index].logical.starts_with("diff --git ") {
            check_planning_control(control)?;
            let current = lines[index];
            let logical = current.logical;
            if logical.starts_with("diff --cc ")
                || logical.starts_with("diff --combined ")
                || logical.starts_with("@@@ ")
            {
                bail!("combined Git diffs are unsupported; select one explicit merge parent");
            }
            if logical.starts_with("Binary files ") || logical.starts_with("GIT binary patch") {
                bail!("binary Git changes are unsupported");
            }
            if let Some(mode) = logical.strip_prefix("new file mode ") {
                validate_git_mode(mode)?;
                set_mode_once(&mut file.new_mode, mode, "new-file mode")?;
                set_change_mode(&mut mode_kind, ChangeKind::Added)?;
            } else if let Some(mode) = logical.strip_prefix("deleted file mode ") {
                validate_git_mode(mode)?;
                set_mode_once(&mut file.old_mode, mode, "deleted-file mode")?;
                set_change_mode(&mut mode_kind, ChangeKind::Deleted)?;
            } else if let Some(mode) = logical.strip_prefix("old mode ") {
                validate_git_mode(mode)?;
                set_mode_once(&mut file.old_mode, mode, "old mode")?;
            } else if let Some(mode) = logical.strip_prefix("new mode ") {
                validate_git_mode(mode)?;
                set_mode_once(&mut file.new_mode, mode, "new mode")?;
            } else if let Some(path) = logical.strip_prefix("rename from ") {
                set_path_once(&mut rename_from, parse_plain_path(path)?, "rename from")?;
            } else if let Some(path) = logical.strip_prefix("rename to ") {
                set_path_once(&mut rename_to, parse_plain_path(path)?, "rename to")?;
            } else if let Some(objects) = logical.strip_prefix("index ") {
                if saw_index {
                    bail!("Git diff contains a duplicate index authority");
                }
                saw_index = true;
                file.index_present = true;
                parse_index_line(objects, &mut file)?;
            } else if let Some(path) = logical.strip_prefix("--- ") {
                set_marker_once(
                    &mut old_marker,
                    parse_patch_path(path, "a/")?,
                    "old file marker",
                )?;
            } else if let Some(path) = logical.strip_prefix("+++ ") {
                set_marker_once(
                    &mut new_marker,
                    parse_patch_path(path, "b/")?,
                    "new file marker",
                )?;
            } else if logical.starts_with("@@ ") {
                let (hunk, next) = parse_hunk(&lines, index)?;
                hunk_count = hunk_count
                    .checked_add(1)
                    .context("Git diff hunk count overflows")?;
                if hunk_count > CAPTURE_MAX_GIT_DIFF_HUNKS {
                    bail!("Git diff exceeds the configured hunk limit");
                }
                file.hunks.push(hunk);
                index = next;
                continue;
            }
            index += 1;
        }
        finalize_diff_file(
            &mut file,
            header_old_path,
            header_new_path,
            mode_kind,
            rename_from,
            rename_to,
            old_marker,
            new_marker,
        )?;
        files.push(file);
        if files.len() > CAPTURE_MAX_GIT_CHANGED_FILES {
            bail!("Git diff exceeds the configured file limit");
        }
    }
    if files.is_empty() {
        bail!("Git diff contains no file changes");
    }
    Ok(files)
}

fn set_change_mode(slot: &mut Option<ChangeKind>, value: ChangeKind) -> Result<()> {
    if slot.is_some() {
        bail!("Git diff contains duplicate or contradictory file mode authorities");
    }
    *slot = Some(value);
    Ok(())
}

fn validate_git_mode(value: &str) -> Result<()> {
    if !matches!(value, "100644" | "100755" | "120000" | "160000") {
        bail!("Git diff file mode is invalid");
    }
    Ok(())
}

fn set_mode_once(slot: &mut Option<String>, value: &str, label: &str) -> Result<()> {
    if slot.is_some() {
        bail!("Git diff contains a duplicate {label} authority");
    }
    *slot = Some(value.to_owned());
    Ok(())
}

fn set_path_once(slot: &mut Option<String>, value: String, label: &str) -> Result<()> {
    if slot.is_some() {
        bail!("Git diff contains a duplicate {label} authority");
    }
    *slot = Some(value);
    Ok(())
}

fn set_marker_once(
    slot: &mut Option<Option<String>>,
    value: Option<String>,
    label: &str,
) -> Result<()> {
    if slot.is_some() {
        bail!("Git diff contains a duplicate {label} authority");
    }
    *slot = Some(value);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finalize_diff_file(
    file: &mut DiffFile,
    header_old_path: String,
    header_new_path: String,
    mode_kind: Option<ChangeKind>,
    rename_from: Option<String>,
    rename_to: Option<String>,
    old_marker: Option<Option<String>>,
    new_marker: Option<Option<String>>,
) -> Result<()> {
    let rename = match (rename_from, rename_to) {
        (Some(from), Some(to)) => Some((from, to)),
        (None, None) => None,
        _ => bail!("Git diff rename authority must include both rename from and rename to"),
    };
    if rename.is_some() && mode_kind.is_some() {
        bail!("Git diff rename and create/delete authorities contradict each other");
    }
    if rename.is_none() && header_old_path != header_new_path {
        bail!("Git diff header paths differ without exact rename authority");
    }
    if let Some((from, to)) = &rename
        && (from != &header_old_path || to != &header_new_path)
    {
        bail!("Git diff header and rename paths contradict each other");
    }
    if let Some(Some(path)) = &old_marker
        && path != &header_old_path
    {
        bail!("Git diff header and old file marker paths contradict each other");
    }
    if let Some(Some(path)) = &new_marker
        && path != &header_new_path
    {
        bail!("Git diff header and new file marker paths contradict each other");
    }
    if matches!(old_marker, Some(None)) && matches!(new_marker, Some(None)) {
        bail!("Git diff cannot create and delete the same file");
    }

    let marker_kind = if matches!(old_marker, Some(None)) {
        Some(ChangeKind::Added)
    } else if matches!(new_marker, Some(None)) {
        Some(ChangeKind::Deleted)
    } else {
        None
    };
    if let (Some(mode), Some(marker)) = (mode_kind, marker_kind)
        && mode != marker
    {
        bail!("Git diff file mode and /dev/null marker authorities contradict each other");
    }
    let kind = if rename.is_some() {
        ChangeKind::Renamed
    } else {
        mode_kind.or(marker_kind).unwrap_or(ChangeKind::Modified)
    };

    if file.old_mode.is_some() != file.new_mode.is_some()
        && matches!(kind, ChangeKind::Modified | ChangeKind::Renamed)
    {
        bail!("Git diff mode-change authority must include both old and new modes");
    }
    if let Some(index_mode) = &file.index_mode {
        match kind {
            ChangeKind::Added | ChangeKind::Deleted => {
                bail!("Git added/deleted index cannot claim a shared file mode")
            }
            ChangeKind::Modified | ChangeKind::Renamed => {
                if file
                    .old_mode
                    .as_ref()
                    .is_some_and(|mode| mode != index_mode)
                    || file
                        .new_mode
                        .as_ref()
                        .is_some_and(|mode| mode != index_mode)
                {
                    bail!("Git diff index and side mode authorities contradict each other");
                }
                file.old_mode.get_or_insert_with(|| index_mode.clone());
                file.new_mode.get_or_insert_with(|| index_mode.clone());
            }
        }
    }

    if !file.hunks.is_empty() && (old_marker.is_none() || new_marker.is_none()) {
        bail!("Git diff hunks require exact old and new file markers");
    }
    match kind {
        ChangeKind::Added => {
            if file.old_mode.is_some() {
                bail!("Git added file cannot claim an old-side mode");
            }
            if old_marker.is_some() && !matches!(old_marker, Some(None))
                || matches!(new_marker, Some(None))
            {
                bail!("Git added-file markers contradict create authority");
            }
            if file.index_present
                && (!is_null_blob(file.old_index_blob.as_deref())
                    || file.new_index_blob.is_none()
                    || is_null_blob(file.new_index_blob.as_deref()))
            {
                bail!("Git added-file index must contain exact null-old and non-null-new blobs");
            }
            file.old_path = None;
            file.new_path = Some(header_new_path);
            file.old_blob = None;
            file.new_blob = file.new_index_blob.clone();
        }
        ChangeKind::Deleted => {
            if file.new_mode.is_some() {
                bail!("Git deleted file cannot claim a new-side mode");
            }
            if new_marker.is_some() && !matches!(new_marker, Some(None))
                || matches!(old_marker, Some(None))
            {
                bail!("Git deleted-file markers contradict delete authority");
            }
            if file.index_present
                && (file.old_index_blob.is_none()
                    || is_null_blob(file.old_index_blob.as_deref())
                    || !is_null_blob(file.new_index_blob.as_deref()))
            {
                bail!("Git deleted-file index must contain non-null-old and exact null-new blobs");
            }
            file.old_path = Some(header_old_path);
            file.new_path = None;
            file.old_blob = file.old_index_blob.clone();
            file.new_blob = None;
        }
        ChangeKind::Renamed => {
            if matches!(old_marker, Some(None)) || matches!(new_marker, Some(None)) {
                bail!("Git rename markers cannot use /dev/null");
            }
            let (from, to) = rename.context("checked rename authority")?;
            if is_null_blob(file.old_index_blob.as_deref())
                || is_null_blob(file.new_index_blob.as_deref())
            {
                bail!("Git rename index cannot cite a null blob on a present side");
            }
            file.old_path = Some(from);
            file.new_path = Some(to);
            file.old_blob = file.old_index_blob.clone();
            file.new_blob = file.new_index_blob.clone();
        }
        ChangeKind::Modified => {
            if matches!(old_marker, Some(None)) || matches!(new_marker, Some(None)) {
                bail!("Git modified-file markers cannot use /dev/null");
            }
            if is_null_blob(file.old_index_blob.as_deref())
                || is_null_blob(file.new_index_blob.as_deref())
            {
                bail!("Git modified index cannot cite a null blob on a present side");
            }
            file.old_path = Some(header_old_path);
            file.new_path = Some(header_new_path);
            file.old_blob = file.old_index_blob.clone();
            file.new_blob = file.new_index_blob.clone();
        }
    }
    file.kind = kind;
    Ok(())
}

fn require_evidence_blob_ids(file: &DiffFile, algorithm: &str) -> Result<()> {
    require_regular_blob_mode(file.new_mode.as_deref(), "new")?;
    match file.kind {
        ChangeKind::Added => {
            require_null_blob_id(file.old_index_blob.as_deref(), algorithm, "old")?;
            require_blob_id(file.new_blob.as_deref(), algorithm, "new")?;
        }
        ChangeKind::Modified | ChangeKind::Renamed => {
            require_blob_id(file.old_blob.as_deref(), algorithm, "old")?;
            require_blob_id(file.new_blob.as_deref(), algorithm, "new")?;
        }
        ChangeKind::Deleted => {
            bail!("deleted Git content cannot be used as new-side evidence")
        }
    }
    Ok(())
}

fn require_removed_evidence_blob_ids(file: &DiffFile, algorithm: &str) -> Result<()> {
    require_regular_blob_mode(file.old_mode.as_deref(), "old")?;
    match file.kind {
        ChangeKind::Deleted => {
            require_blob_id(file.old_blob.as_deref(), algorithm, "old")?;
            require_null_blob_id(file.new_index_blob.as_deref(), algorithm, "new")?;
        }
        ChangeKind::Modified | ChangeKind::Renamed => {
            require_blob_id(file.old_blob.as_deref(), algorithm, "old")?;
            require_blob_id(file.new_blob.as_deref(), algorithm, "new")?;
        }
        ChangeKind::Added => bail!("added Git content cannot be used as old-side evidence"),
    }
    Ok(())
}

fn require_regular_blob_mode(value: Option<&str>, side: &str) -> Result<()> {
    let value = value.with_context(|| {
        format!("Git diff {side}-side evidence requires an authoritative file mode")
    })?;
    if !matches!(value, "100644" | "100755") {
        bail!("Git diff evidence must come from a regular blob mode");
    }
    Ok(())
}

fn require_null_blob_id(value: Option<&str>, algorithm: &str, side: &str) -> Result<()> {
    let value = value.with_context(|| {
        format!("Git diff {side} absent-side blob ID must be an exact null digest")
    })?;
    if !value.starts_with(&format!("{algorithm}:")) || !is_null_blob(Some(value)) {
        bail!("Git diff absent-side blob ID must match the revision algorithm");
    }
    Ok(())
}

fn require_blob_id(value: Option<&str>, algorithm: &str, side: &str) -> Result<()> {
    let value = value.with_context(|| {
        format!("Git diff {side} blob ID must be a full algorithm-prefixed digest")
    })?;
    if value.strip_prefix(&format!("{algorithm}:")).is_none() {
        bail!("Git diff blob algorithm does not match its revision identity");
    }
    Ok(())
}

fn excluded_git_file_diagnostic(file: &DiffFile) -> Option<&'static str> {
    if changed_content_is_managed_projection(file) {
        return Some("git_managed_projection_excluded");
    }
    let mut result = None;
    for path in file.old_path.iter().chain(file.new_path.iter()) {
        let Some(candidate) = excluded_git_path_diagnostic(path) else {
            continue;
        };
        if result.is_none() || exclusion_priority(candidate) < exclusion_priority(result?) {
            result = Some(candidate);
        }
    }
    result
}

fn exclusion_priority(code: &str) -> u8 {
    match code {
        "git_managed_projection_excluded" => 0,
        "git_vendor_path_excluded" => 1,
        "git_dependency_path_excluded" => 2,
        "git_generated_path_excluded" => 3,
        _ => 4,
    }
}

pub(super) fn excluded_git_path_diagnostic(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    let components = lower.split('/').collect::<Vec<_>>();
    if components.first() == Some(&".memzoi")
        || components
            .iter()
            .any(|component| matches!(*component, "memzoi-generated" | ".memzoi-generated"))
    {
        return Some("git_managed_projection_excluded");
    }
    if components.iter().any(|component| {
        matches!(
            *component,
            "vendor" | "vendored" | "third_party" | "third-party"
        )
    }) {
        return Some("git_vendor_path_excluded");
    }
    if components.iter().any(|component| {
        matches!(
            *component,
            "node_modules"
                | "bower_components"
                | "deps"
                | "target"
                | "venv"
                | ".venv"
                | "__pypackages__"
        )
    }) {
        return Some("git_dependency_path_excluded");
    }
    let file_name = components.last().copied().unwrap_or_default();
    if components
        .iter()
        .any(|component| matches!(*component, "generated" | "_generated" | "gen" | "build"))
        || file_name.contains(".generated.")
        || file_name.ends_with("_generated.md")
    {
        return Some("git_generated_path_excluded");
    }
    if components.iter().any(|component| {
        component.starts_with('.') && *component != ".github"
            || matches!(
                *component,
                "cache" | "coverage" | "dist" | "out" | "temp" | "tmp"
            )
    }) {
        return Some("git_ignored_like_path_excluded");
    }
    None
}

fn changed_content_is_managed_projection(file: &DiffFile) -> bool {
    file.hunks.iter().any(|hunk| {
        hunk.added_blocks
            .iter()
            .flatten()
            .any(|line| managed_projection_line(&line.content))
            || hunk
                .removed_blocks
                .iter()
                .flatten()
                .any(|line| managed_projection_line(&line.content))
    })
}

fn managed_projection_line(line: &str) -> bool {
    let line = line.trim().to_ascii_lowercase();
    line.contains("<!-- memzoi:start -->")
        || line.contains("<!-- memzoi:end -->")
        || line.contains("generated by memzoi")
        || line.contains("memzoi projection for")
}

fn parse_hunk(lines: &[super::SourceLine<'_>], index: usize) -> Result<(DiffHunk, usize)> {
    let header = lines[index];
    let (old_start, old_count, new_start, new_count) = parse_hunk_header(header.logical)?;
    let mut old_used = 0u64;
    let mut new_used = 0u64;
    let mut added_blocks: Vec<Vec<AddedLine>> = Vec::new();
    let mut removed_blocks: Vec<Vec<RemovedLine>> = Vec::new();
    let mut deleted_typed_guidance = false;
    let mut cursor = index + 1;
    let mut previous_added = false;
    let mut previous_removed = false;
    while cursor < lines.len() {
        let line = lines[cursor];
        if old_used == old_count && new_used == new_count {
            break;
        }
        let logical = line.logical;
        if let Some(content) = logical.strip_prefix('+') {
            let new_line = new_start
                .checked_add(new_used)
                .context("Git diff new line coordinate overflows")?;
            if !previous_added {
                added_blocks.push(Vec::new());
            }
            added_blocks
                .last_mut()
                .expect("added block exists")
                .push(AddedLine {
                    content: content.to_owned(),
                    start: line.start,
                    end: line.end,
                    source_line: line.number,
                    new_line,
                });
            new_used = new_used
                .checked_add(1)
                .context("Git diff new line count overflows")?;
            previous_added = true;
            previous_removed = false;
        } else if let Some(content) = logical.strip_prefix('-') {
            let old_line = old_start
                .checked_add(old_used)
                .context("Git diff old line coordinate overflows")?;
            if !previous_removed {
                removed_blocks.push(Vec::new());
            }
            removed_blocks
                .last_mut()
                .expect("removed block exists")
                .push(RemovedLine {
                    content: content.to_owned(),
                    start: line.start,
                    end: line.end,
                    source_line: line.number,
                    old_line,
                });
            deleted_typed_guidance |= parse_atx_heading(content)
                .and_then(|(_, heading)| typed_heading(&heading).0)
                .is_some();
            old_used = old_used
                .checked_add(1)
                .context("Git diff old line count overflows")?;
            previous_added = false;
            previous_removed = true;
        } else if logical.starts_with(' ') {
            old_used = old_used
                .checked_add(1)
                .context("Git diff old line count overflows")?;
            new_used = new_used
                .checked_add(1)
                .context("Git diff new line count overflows")?;
            previous_added = false;
            previous_removed = false;
        } else if logical == "\\ No newline at end of file" {
            previous_added = false;
            previous_removed = false;
        } else {
            bail!("Git diff hunk contains an invalid line prefix");
        }
        cursor += 1;
    }
    if old_used != old_count || new_used != new_count {
        bail!("Git diff hunk line counts do not match its header");
    }
    Ok((
        DiffHunk {
            header: header.logical.to_owned(),
            old_start,
            old_count,
            new_start,
            new_count,
            added_blocks,
            removed_blocks,
            deleted_typed_guidance,
        },
        cursor,
    ))
}

fn parse_hunk_header(header: &str) -> Result<(u64, u64, u64, u64)> {
    let rest = header
        .strip_prefix("@@ -")
        .context("Git diff hunk header must start with @@ -")?;
    let (old, rest) = rest
        .split_once(" +")
        .context("Git diff hunk header is missing the new range")?;
    let (new, _suffix) = rest
        .split_once(" @@")
        .context("Git diff hunk header is not terminated")?;
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    inclusive_line_end(old_start, old_count)?;
    inclusive_line_end(new_start, new_count)?;
    Ok((old_start, old_count, new_start, new_count))
}

fn inclusive_line_end(start: u64, count: u64) -> Result<Option<u64>> {
    if count == 0 {
        return Ok(None);
    }
    if start == 0 {
        bail!("Git diff non-empty line range cannot start at zero");
    }
    start
        .checked_add(count - 1)
        .map(Some)
        .context("Git diff line range overflows")
}

fn parse_range(value: &str) -> Result<(u64, u64)> {
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    let start = start
        .parse::<u64>()
        .context("Git diff line start is invalid")?;
    let count = count
        .parse::<u64>()
        .context("Git diff line count is invalid")?;
    Ok((start, count))
}

fn parse_index_pair(pair: &str, file: &mut DiffFile) -> Result<()> {
    let (old, new) = pair
        .split_once("..")
        .context("Git diff index line is malformed")?;
    file.old_index_blob = full_object_id(old);
    file.new_index_blob = full_object_id(new);
    Ok(())
}

fn parse_index_line(value: &str, file: &mut DiffFile) -> Result<()> {
    let mut fields = value.split_whitespace();
    let pair = fields.next().context("Git diff index line is empty")?;
    if let Some(mode) = fields.next() {
        validate_git_mode(mode)?;
        set_mode_once(&mut file.index_mode, mode, "index mode")?;
    }
    if fields.next().is_some() {
        bail!("Git diff index line contains unexpected fields");
    }
    parse_index_pair(pair, file)
}

fn is_null_blob(value: Option<&str>) -> bool {
    value
        .and_then(|value| value.split_once(':').map(|(_, digest)| digest))
        .is_some_and(|digest| !digest.is_empty() && digest.bytes().all(|byte| byte == b'0'))
}

fn full_object_id(value: &str) -> Option<String> {
    let algorithm = match value.len() {
        40 => "sha1",
        64 => "sha256",
        _ => return None,
    };
    (value.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !value.bytes().any(|byte| byte.is_ascii_uppercase()))
    .then(|| format!("{algorithm}:{value}"))
}

fn parse_patch_path(path: &str, prefix: &str) -> Result<Option<String>> {
    let path = path.split('\t').next().unwrap_or(path);
    if path == "/dev/null" {
        return Ok(None);
    }
    parse_prefixed_path(path, prefix).map(Some)
}

fn parse_prefixed_path(path: &str, prefix: &str) -> Result<String> {
    let path = path
        .strip_prefix(prefix)
        .with_context(|| format!("Git diff path must use the {prefix} prefix"))?;
    parse_plain_path(path)
}

fn parse_plain_path(path: &str) -> Result<String> {
    let drive_prefixed =
        path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic();
    if path.is_empty()
        || path.len() > 4096
        || path.starts_with('"')
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.bytes().any(|byte| byte.is_ascii_whitespace())
        || path.chars().any(char::is_control)
        || drive_prefixed
        || path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        bail!("Git diff path must be a safe POSIX project-relative path");
    }
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_parser_tracks_added_lines_and_ranges() {
        let text =
            "@@ -1,2 +1,5 @@\n old\n+# Decision: Keep evidence\n+Use exact spans.\n+\n old2\n";
        let lines = source_lines(text);
        let (hunk, next) = parse_hunk(&lines, 0).unwrap();
        assert_eq!(next, lines.len());
        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.added_blocks.len(), 1);
        assert_eq!(hunk.added_blocks[0][0].new_line, 2);
    }

    #[test]
    fn malformed_hunk_counts_fail_closed() {
        let text = "@@ -1,1 +1,2 @@\n-old\n+new\n";
        let lines = source_lines(text);
        assert!(parse_hunk(&lines, 0).is_err());
    }

    #[test]
    fn hunk_coordinate_overflow_fails_closed() {
        assert!(parse_hunk_header("@@ -1,1 +18446744073709551615,2 @@").is_err());
        assert!(parse_hunk_header("@@ -18446744073709551615,2 +1,1 @@").is_err());
    }

    #[test]
    fn abbreviated_blob_ids_are_not_claimed_as_exact() {
        assert_eq!(full_object_id("abc123"), None);
        assert!(
            full_object_id(&"a".repeat(40))
                .unwrap()
                .starts_with("sha1:")
        );
    }

    #[test]
    fn null_blob_ids_are_only_valid_on_exact_absent_sides() {
        let null = "0".repeat(40);
        let old = "1".repeat(40);
        let new = "2".repeat(40);
        let modified_null = format!(
            "diff --git a/docs/a.md b/docs/a.md\nindex {null}..{new}\n--- a/docs/a.md\n+++ b/docs/a.md\n@@ -0,0 +0,0 @@\n"
        );
        assert!(parse_diff(&modified_null, None).is_err());

        let added_non_null_old = format!(
            "diff --git a/docs/a.md b/docs/a.md\nnew file mode 100644\nindex {old}..{new}\n--- /dev/null\n+++ b/docs/a.md\n@@ -0,0 +0,0 @@\n"
        );
        assert!(parse_diff(&added_non_null_old, None).is_err());

        let added_abbreviated_null = format!(
            "diff --git a/docs/a.md b/docs/a.md\nnew file mode 100644\nindex 0000000..{new}\n--- /dev/null\n+++ b/docs/a.md\n@@ -0,0 +0,0 @@\n"
        );
        assert!(parse_diff(&added_abbreviated_null, None).is_err());

        let valid_added = format!(
            "diff --git a/docs/a.md b/docs/a.md\nnew file mode 100644\nindex {null}..{new}\n--- /dev/null\n+++ b/docs/a.md\n@@ -0,0 +0,0 @@\n"
        );
        let files = parse_diff(&valid_added, None).unwrap();
        assert!(files[0].old_blob.is_none());
        let expected_new = format!("sha1:{new}");
        assert_eq!(files[0].new_blob.as_deref(), Some(expected_new.as_str()));
    }

    #[test]
    fn duplicate_diff_authority_fields_are_rejected() {
        let digest = "1".repeat(40);
        let text = format!(
            "diff --git a/docs/a.md b/docs/a.md\nindex {digest}..{digest}\nindex {digest}..{digest}\n"
        );
        assert!(parse_diff(&text, None).is_err());
        let unexpected_index_field =
            format!("diff --git a/docs/a.md b/docs/a.md\nindex {digest}..{digest} 100644 extra\n");
        assert!(parse_diff(&unexpected_index_field, None).is_err());
        assert!(
            parse_diff(
                "diff --git a/docs/a.md b/docs/a.md\nnew file mode invalid\n",
                None
            )
            .is_err()
        );
    }

    #[test]
    fn file_mode_authorities_must_be_complete_and_consistent() {
        let old = "1".repeat(40);
        let new = "2".repeat(40);
        let incomplete =
            format!("diff --git a/docs/a.md b/docs/a.md\nold mode 100644\nindex {old}..{new}\n");
        assert!(parse_diff(&incomplete, None).is_err());

        let contradictory = format!(
            "diff --git a/docs/a.md b/docs/a.md\nold mode 100755\nnew mode 100755\nindex {old}..{new} 100644\n"
        );
        assert!(parse_diff(&contradictory, None).is_err());

        let added_shared_mode = format!(
            "diff --git a/docs/a.md b/docs/a.md\nnew file mode 100644\nindex {}..{new} 100644\n",
            "0".repeat(40),
        );
        assert!(parse_diff(&added_shared_mode, None).is_err());
    }

    #[test]
    fn excessive_file_and_hunk_counts_fail_closed() {
        let files = (0..=CAPTURE_MAX_GIT_CHANGED_FILES)
            .map(|index| format!("diff --git a/docs/{index}.md b/docs/{index}.md\n"))
            .collect::<String>();
        assert!(parse_diff(&files, None).is_err());

        let mut hunks = format!(
            "diff --git a/docs/a.md b/docs/a.md\nindex {}..{}\n--- a/docs/a.md\n+++ b/docs/a.md\n",
            "1".repeat(40),
            "2".repeat(40)
        );
        for _ in 0..=CAPTURE_MAX_GIT_DIFF_HUNKS {
            hunks.push_str("@@ -0,0 +0,0 @@\n");
        }
        assert!(parse_diff(&hunks, None).is_err());
    }
}
