use std::ops::Range;

use anyhow::{Result, bail};

use super::source_lines;
use crate::{MemoryDestination, MemoryLane, MemoryType, OkfProposalSensitivity};

use super::super::{
    CaptureDiagnostic, CaptureEvidence, CaptureExtraction, CaptureExtractorIdentity,
    CaptureLoadedSource, CaptureMemoryDraft, CapturePlanningControl, CaptureSemanticLocation,
    CaptureSourceDocument, EvidenceLocation, candidate, check_planning_control, default_scope,
    evidence_for, markdown_sections, parse_atx_heading,
};

#[derive(Debug, Clone)]
struct FrontmatterField {
    value: String,
    range: Range<usize>,
    line: u64,
}

#[derive(Debug, Clone)]
struct AdrEvidenceSpan {
    range: Range<usize>,
    line: u64,
    heading_path: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdrStatus {
    Accepted,
    Draft,
    Rejected,
    Superseded,
    Deprecated,
    Unknown,
}

impl AdrStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Draft => "draft",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
            Self::Deprecated => "deprecated",
            Self::Unknown => "unknown",
        }
    }

    fn from_text(value: &str) -> Self {
        let normalized = value
            .trim()
            .trim_matches(|ch: char| matches!(ch, '[' | ']' | '"' | '\'' | '`'))
            .to_ascii_lowercase();
        match normalized.as_str() {
            "accepted" | "adopted" | "approved" => Self::Accepted,
            "draft" | "proposed" | "pending" => Self::Draft,
            "rejected" | "declined" => Self::Rejected,
            "superseded" | "replaced" => Self::Superseded,
            "deprecated" | "obsolete" => Self::Deprecated,
            _ => Self::Unknown,
        }
    }
}

pub(super) fn extract(
    loaded: &CaptureLoadedSource,
    extractor: &CaptureExtractorIdentity,
    control: Option<&CapturePlanningControl>,
) -> Result<CaptureExtraction> {
    let mut candidates = Vec::new();
    let mut diagnostics = Vec::new();
    for document in &loaded.documents {
        check_planning_control(control)?;
        let extraction = extract_document(document, extractor, control)?;
        candidates.extend(extraction.candidates);
        diagnostics.extend(extraction.diagnostics);
    }
    Ok(CaptureExtraction {
        candidates,
        diagnostics,
    })
}

fn extract_document(
    document: &CaptureSourceDocument,
    extractor: &CaptureExtractorIdentity,
    control: Option<&CapturePlanningControl>,
) -> Result<CaptureExtraction> {
    let text = std::str::from_utf8(&document.bytes)?;
    let frontmatter = frontmatter(text)?;
    let sections = markdown_sections(text, control)?;
    let heading_title = first_level_one_heading(text, &sections);
    let malformed_title_field = frontmatter
        .get("title")
        .is_some_and(|field| field.value.trim().is_empty());
    let mut title_authorities = Vec::<(String, AdrEvidenceSpan)>::new();
    if let Some(field) = frontmatter
        .get("title")
        .filter(|field| !field.value.trim().is_empty())
    {
        title_authorities.push((
            field.value.clone(),
            AdrEvidenceSpan {
                range: field.range.clone(),
                line: field.line,
                heading_path: Vec::new(),
            },
        ));
    }
    if let Some(heading) = heading_title {
        title_authorities.push(heading);
    }
    let titles_agree = title_authorities.first().is_some_and(|(first, _)| {
        title_authorities
            .iter()
            .all(|(title, _)| title.trim() == first.trim())
    });
    let conflicting_title = title_authorities.len() > 1 && !titles_agree;

    let mut status_authorities = Vec::<(AdrStatus, AdrEvidenceSpan)>::new();
    if let Some(field) = frontmatter.get("status") {
        status_authorities.push((
            AdrStatus::from_text(&field.value),
            AdrEvidenceSpan {
                range: field.range.clone(),
                line: field.line,
                heading_path: Vec::new(),
            },
        ));
    }
    for section in sections
        .iter()
        .filter(|section| normalized_heading(&section.title) == "status")
    {
        let value = section_body(text, section.start, section.direct_end).unwrap_or_default();
        status_authorities.push((
            AdrStatus::from_text(&value),
            AdrEvidenceSpan {
                range: section.start..section.direct_end,
                line: section.line,
                heading_path: section.heading_path.clone(),
            },
        ));
    }
    let statuses_agree = status_authorities
        .first()
        .is_some_and(|(first, _)| status_authorities.iter().all(|(status, _)| status == first));
    let conflicting_status = status_authorities.len() > 1 && !statuses_agree;
    let status = if statuses_agree {
        status_authorities[0].0
    } else {
        AdrStatus::Unknown
    };
    let mut diagnostics = Vec::new();
    if title_authorities.is_empty()
        || status_authorities.is_empty()
        || status == AdrStatus::Unknown
        || malformed_title_field
        || conflicting_title
    {
        diagnostics.push(CaptureDiagnostic {
            code: "malformed_adr_metadata".to_owned(),
            source_id: Some(document.request.source_id.clone()),
            line: Some(1),
        });
    }
    if conflicting_status {
        diagnostics.push(CaptureDiagnostic {
            code: "conflicting_adr_status".to_owned(),
            source_id: Some(document.request.source_id.clone()),
            line: status_authorities.first().map(|(_, span)| span.line),
        });
    }
    if conflicting_title {
        diagnostics.push(CaptureDiagnostic {
            code: "conflicting_adr_title".to_owned(),
            source_id: Some(document.request.source_id.clone()),
            line: title_authorities.first().map(|(_, span)| span.line),
        });
    }

    if malformed_title_field || conflicting_title {
        return Ok(malformed_metadata_extraction(
            document,
            &sections,
            diagnostics,
        ));
    }
    let Some((title, _)) = title_authorities.first() else {
        return Ok(malformed_metadata_extraction(
            document,
            &sections,
            diagnostics,
        ));
    };
    if status_authorities.is_empty() {
        return Ok(malformed_metadata_extraction(
            document,
            &sections,
            diagnostics,
        ));
    }
    let authority_evidence = adr_authority_evidence(
        document,
        text,
        &title_authorities,
        &status_authorities,
        status,
    )?;

    let mut candidates = Vec::new();
    let mut saw_decision = false;
    for section in &sections {
        check_planning_control(control)?;
        let Some((field, memory_type, lane)) = adr_field(&section.title) else {
            continue;
        };
        let Some(body) = section_body(text, section.start, section.direct_end) else {
            continue;
        };
        if field == "decision" {
            saw_decision = true;
        }
        let mut evidence = evidence_for(
            &document.request,
            &document.snapshot,
            text,
            EvidenceLocation {
                start: section.start,
                end: section.direct_end,
                line_start: section.line,
                section_kind: field,
                heading_path: &section.heading_path,
            },
        )?;
        apply_adr_location(
            &mut evidence,
            field,
            status,
            (field == "supersession").then_some(body.as_str()),
        );
        evidence.extend(authority_evidence.iter().cloned());
        sort_and_deduplicate_evidence(&mut evidence);
        candidates.push(adr_candidate(
            document,
            extractor,
            title,
            field,
            memory_type,
            lane,
            body,
            status,
            evidence,
        )?);
    }

    if let Some(supersedes) = frontmatter.get("supersedes")
        && !supersedes.value.trim().is_empty()
    {
        let mut evidence = evidence_for(
            &document.request,
            &document.snapshot,
            text,
            EvidenceLocation {
                start: supersedes.range.start,
                end: supersedes.range.end,
                line_start: supersedes.line,
                section_kind: "supersession",
                heading_path: &[],
            },
        )?;
        apply_adr_location(
            &mut evidence,
            "supersession",
            status,
            Some(supersedes.value.as_str()),
        );
        evidence.extend(authority_evidence.iter().cloned());
        sort_and_deduplicate_evidence(&mut evidence);
        candidates.push(adr_candidate(
            document,
            extractor,
            title,
            "supersession",
            MemoryType::Decision,
            MemoryLane::Semantic,
            supersedes.value.clone(),
            status,
            evidence,
        )?);
    }

    if !saw_decision {
        diagnostics.push(CaptureDiagnostic {
            code: "adr_decision_missing".to_owned(),
            source_id: Some(document.request.source_id.clone()),
            line: Some(1),
        });
    }
    if candidates.is_empty() {
        diagnostics.push(CaptureDiagnostic {
            code: "unsupported_adr_content".to_owned(),
            source_id: Some(document.request.source_id.clone()),
            line: Some(1),
        });
    }
    Ok(CaptureExtraction {
        candidates,
        diagnostics,
    })
}

fn first_level_one_heading(
    text: &str,
    sections: &[super::super::MarkdownSection],
) -> Option<(String, AdrEvidenceSpan)> {
    let lines = source_lines(text);
    let section = sections.first()?;
    let line = lines.iter().find(|line| line.start == section.start)?;
    let (level, heading) = parse_atx_heading(line.logical)?;
    (level == 1 && !heading.trim().is_empty()).then(|| {
        (
            heading.clone(),
            AdrEvidenceSpan {
                range: line.start..line.end,
                line: line.number,
                heading_path: vec![heading],
            },
        )
    })
}

fn adr_authority_evidence(
    document: &CaptureSourceDocument,
    text: &str,
    titles: &[(String, AdrEvidenceSpan)],
    statuses: &[(AdrStatus, AdrEvidenceSpan)],
    status: AdrStatus,
) -> Result<Vec<CaptureEvidence>> {
    let mut evidence = Vec::new();
    for (_, span) in titles {
        let mut title_evidence = evidence_for(
            &document.request,
            &document.snapshot,
            text,
            EvidenceLocation {
                start: span.range.start,
                end: span.range.end,
                line_start: span.line,
                section_kind: "title",
                heading_path: &span.heading_path,
            },
        )?;
        apply_adr_location(&mut title_evidence, "title", status, None);
        evidence.extend(title_evidence);
    }
    for (_, span) in statuses {
        let mut status_evidence = evidence_for(
            &document.request,
            &document.snapshot,
            text,
            EvidenceLocation {
                start: span.range.start,
                end: span.range.end,
                line_start: span.line,
                section_kind: "status",
                heading_path: &span.heading_path,
            },
        )?;
        apply_adr_location(&mut status_evidence, "status", status, None);
        evidence.extend(status_evidence);
    }
    Ok(evidence)
}

fn sort_and_deduplicate_evidence(evidence: &mut Vec<CaptureEvidence>) {
    evidence.sort_by(|left, right| {
        left.source_id
            .cmp(&right.source_id)
            .then_with(|| {
                left.locator
                    .durable_reference()
                    .cmp(&right.locator.durable_reference())
            })
            .then_with(|| left.span.byte_start.cmp(&right.span.byte_start))
            .then_with(|| left.span.byte_end.cmp(&right.span.byte_end))
            .then_with(|| left.section_kind.cmp(&right.section_kind))
    });
    evidence.dedup_by(|right, left| {
        left.source_id == right.source_id
            && left.locator == right.locator
            && left.span == right.span
            && left.section_kind == right.section_kind
    });
}

fn malformed_metadata_extraction(
    document: &CaptureSourceDocument,
    sections: &[super::super::MarkdownSection],
    mut diagnostics: Vec<CaptureDiagnostic>,
) -> CaptureExtraction {
    if !sections
        .iter()
        .any(|section| adr_field(&section.title).is_some_and(|(field, _, _)| field == "decision"))
    {
        diagnostics.push(CaptureDiagnostic {
            code: "adr_decision_missing".to_owned(),
            source_id: Some(document.request.source_id.clone()),
            line: Some(1),
        });
    }
    diagnostics.push(CaptureDiagnostic {
        code: "unsupported_adr_content".to_owned(),
        source_id: Some(document.request.source_id.clone()),
        line: Some(1),
    });
    CaptureExtraction {
        candidates: Vec::new(),
        diagnostics,
    }
}

#[allow(clippy::too_many_arguments)]
fn adr_candidate(
    document: &CaptureSourceDocument,
    extractor: &CaptureExtractorIdentity,
    document_title: &str,
    field: &str,
    memory_type: MemoryType,
    lane: MemoryLane,
    body: String,
    status: AdrStatus,
    evidence: Vec<CaptureEvidence>,
) -> Result<super::super::CaptureCandidate> {
    let status_allows_repo = status == AdrStatus::Accepted && field != "supersession";
    let destination = if status_allows_repo {
        MemoryDestination::Repo
    } else {
        MemoryDestination::NeedsReview
    };
    let sensitivity = if status_allows_repo {
        OkfProposalSensitivity::RepoSafe
    } else {
        OkfProposalSensitivity::Unknown
    };
    let title = adr_memory_title(document_title, field);
    candidate(
        CaptureMemoryDraft {
            memory_type,
            lane,
            title,
            body,
            scope: default_scope(&document.request),
            tags: adr_tags(status, field),
        },
        evidence,
        extractor,
        destination,
        sensitivity,
        if status_allows_repo {
            "accepted_adr_field"
        } else if field == "supersession" {
            "adr_supersession_requires_lifecycle_review"
        } else {
            "adr_status_requires_review"
        },
        if status_allows_repo {
            "explicit_repo_adr_passed_safeguards"
        } else {
            "adr_status_or_lifecycle_authority_is_not_repo_safe"
        },
    )
}

fn adr_memory_title(document_title: &str, field: &str) -> String {
    match field {
        "decision" => document_title.to_owned(),
        "context" => format!("{document_title} — context"),
        "risk" => format!("{document_title} — risks"),
        "consequences" => format!("{document_title} — consequences"),
        "supersession" => format!("{document_title} — supersession"),
        _ => format!("{document_title} — {field}"),
    }
}

fn adr_tags(status: AdrStatus, field: &str) -> Vec<String> {
    vec![
        "adr".to_owned(),
        format!("adr-status:{}", status.as_str()),
        format!("adr-field:{field}"),
    ]
}

fn adr_field(heading: &str) -> Option<(&'static str, MemoryType, MemoryLane)> {
    match normalized_heading(heading).as_str() {
        "context" | "problem" | "motivation" => {
            Some(("context", MemoryType::Fact, MemoryLane::Semantic))
        }
        "decision" | "chosen option" => {
            Some(("decision", MemoryType::Decision, MemoryLane::Semantic))
        }
        "consequences" | "positive consequences" | "outcome" => {
            Some(("consequences", MemoryType::Fact, MemoryLane::Semantic))
        }
        "risks" | "risk" | "negative consequences" | "disadvantages" => {
            Some(("risk", MemoryType::Risk, MemoryLane::Semantic))
        }
        "supersedes" | "superseded by" | "replaces" => {
            Some(("supersession", MemoryType::Decision, MemoryLane::Semantic))
        }
        _ => None,
    }
}

fn normalized_heading(heading: &str) -> String {
    heading
        .trim()
        .trim_end_matches(':')
        .trim()
        .to_ascii_lowercase()
}

fn section_body(text: &str, start: usize, end: usize) -> Option<String> {
    let body_start = text[start..end]
        .find('\n')
        .map(|offset| start + offset + 1)
        .unwrap_or(end);
    let body = text[body_start..end].trim();
    (!body.is_empty()).then(|| body.to_owned())
}

fn apply_adr_location(
    evidence: &mut [CaptureEvidence],
    field: &str,
    status: AdrStatus,
    target: Option<&str>,
) {
    for item in evidence {
        item.semantic_location = Some(CaptureSemanticLocation::Adr {
            field: field.to_owned(),
            status: status.as_str().to_owned(),
            target: target.map(str::to_owned),
        });
    }
}

fn frontmatter(text: &str) -> Result<std::collections::BTreeMap<String, FrontmatterField>> {
    let lines = source_lines(text);
    if lines.first().map(|line| line.logical.trim()) != Some("---") {
        return Ok(std::collections::BTreeMap::new());
    }
    let Some(close) = lines
        .iter()
        .skip(1)
        .position(|line| line.logical.trim() == "---")
        .map(|index| index + 1)
    else {
        bail!("ADR frontmatter is not terminated");
    };
    let mut fields = std::collections::BTreeMap::new();
    for line in &lines[1..close] {
        let Some((key, raw_value)) = line.logical.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if !matches!(key.as_str(), "title" | "status" | "supersedes") {
            continue;
        }
        let value = raw_value
            .trim()
            .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '[' | ']'))
            .trim()
            .to_owned();
        if fields.contains_key(&key) {
            bail!("ADR frontmatter contains a duplicate authority field");
        }
        fields.insert(
            key,
            FrontmatterField {
                value,
                range: line.start..line.end,
                line: line.number,
            },
        );
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_normalization_is_conservative() {
        assert_eq!(AdrStatus::from_text("Accepted"), AdrStatus::Accepted);
        assert_eq!(AdrStatus::from_text("Proposed"), AdrStatus::Draft);
        assert_eq!(AdrStatus::from_text("Maybe"), AdrStatus::Unknown);
    }

    #[test]
    fn frontmatter_keeps_source_line_range() {
        let text = "---\ntitle: Test ADR\nstatus: accepted\n---\n# Test ADR\n";
        let fields = frontmatter(text).unwrap();
        assert_eq!(fields["status"].line, 3);
        assert_eq!(&text[fields["status"].range.clone()], "status: accepted\n");
    }

    #[test]
    fn duplicate_authority_fields_are_rejected() {
        let text = "---\ntitle: First\ntitle: Second\nstatus: accepted\n---\n";
        assert!(frontmatter(text).is_err());
    }
}
