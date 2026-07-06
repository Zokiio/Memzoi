use std::{
    fs,
    fs::OpenOptions,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::Deserialize;

use crate::{
    MemoryDraft, MemoryLane, MemoryRecord, MemoryStatus, MemoryType, ScopeKind, Visibility,
};

#[derive(Debug, Clone, PartialEq)]
pub struct OkfRecordFile {
    pub concept_id: String,
    pub draft: MemoryDraft,
    pub status: MemoryStatus,
    pub applies_to: Vec<String>,
    pub created: String,
    pub updated: Option<String>,
    pub supersedes_id: Option<String>,
    pub expires_at: Option<String>,
}

pub fn read_okf_record_files(bundle_root: impl AsRef<Path>) -> Result<Vec<OkfRecordFile>> {
    let bundle_root = bundle_root.as_ref();
    if !bundle_root.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    collect_markdown_files(bundle_root, &mut files)?;
    let mut records = Vec::new();
    for file in files {
        if let Some(record) = parse_okf_record_file(bundle_root, &file)? {
            records.push(record);
        }
    }
    records.sort_by(|left, right| left.concept_id.cmp(&right.concept_id));
    Ok(records)
}

pub fn parse_okf_record_file(
    bundle_root: impl AsRef<Path>,
    file_path: impl AsRef<Path>,
) -> Result<Option<OkfRecordFile>> {
    let markdown = fs::read_to_string(file_path.as_ref())
        .with_context(|| format!("failed to read OKF record {}", file_path.as_ref().display()))?;
    parse_okf_record_markdown(bundle_root, file_path, &markdown)
}

pub fn parse_okf_record_markdown(
    bundle_root: impl AsRef<Path>,
    file_path: impl AsRef<Path>,
    markdown: &str,
) -> Result<Option<OkfRecordFile>> {
    let bundle_root = bundle_root.as_ref();
    let file_path = file_path.as_ref();
    if is_reserved_record_file(file_path) {
        return Ok(None);
    }

    let concept_id = concept_id(bundle_root, file_path)?;
    let (frontmatter, body) = split_frontmatter(markdown)?;
    let frontmatter: OkfFrontmatter = serde_yaml::from_str(frontmatter)
        .with_context(|| format!("failed to parse OKF frontmatter for {concept_id}"))?;

    let title = required_string(frontmatter.title, "title")?;
    let memory_type = parse_required_enum::<MemoryType>(frontmatter.memory_type, "type")?;
    let lane = parse_optional_enum::<MemoryLane>(frontmatter.lane, "lane")?.unwrap_or_default();
    let scope_kind = parse_scope(frontmatter.scope_kind.or(frontmatter.scope))?;
    let visibility = parse_required_enum::<Visibility>(frontmatter.visibility, "visibility")?;
    let status = parse_status(required_string(frontmatter.status, "status")?)?;
    let confidence = parse_confidence(frontmatter.confidence)?;
    let source_kind = required_string(frontmatter.source.or(frontmatter.source_kind), "source")?;
    let created = required_string(
        frontmatter
            .created
            .or(frontmatter.created_at)
            .or(frontmatter.timestamp),
        "created",
    )?;
    ensure_timestampish(&created, "created")?;
    let updated = frontmatter.updated.or(frontmatter.updated_at);
    if let Some(updated) = updated.as_deref() {
        ensure_timestampish(updated, "updated")?;
    }
    let applies_to = validate_applies_to(frontmatter.applies_to.unwrap_or_default())?;
    let supersedes_id = frontmatter.supersedes.or(frontmatter.supersedes_id);
    let expires_at = frontmatter.expires.or(frontmatter.expires_at);
    let body = body_without_matching_h1(body, &title)?;

    Ok(Some(OkfRecordFile {
        concept_id: concept_id.clone(),
        draft: MemoryDraft {
            memory_type,
            lane,
            scope_kind,
            scope_id: frontmatter.scope_id,
            visibility,
            title,
            body,
            tags: frontmatter.tags.unwrap_or_default(),
            source_kind: Some(source_kind),
            source_ref: frontmatter.source_ref.or(Some(concept_id)),
            confidence,
        },
        status,
        applies_to,
        created,
        updated,
        supersedes_id,
        expires_at,
    }))
}

pub fn import_okf_records(conn: &Connection, records: &[OkfRecordFile]) -> Result<usize> {
    for record in records {
        import_okf_record(conn, record)?;
    }
    Ok(records.len())
}

pub fn write_memory_record_file(records_root: &Path, record: &MemoryRecord) -> Result<PathBuf> {
    write_memory_record_file_with_tags(records_root, record, &[])
}

pub fn write_memory_record_file_with_tags(
    records_root: &Path,
    record: &MemoryRecord,
    tags: &[String],
) -> Result<PathBuf> {
    write_memory_record_file_with_metadata(records_root, record, tags, &[])
}

pub fn write_memory_record_file_with_metadata(
    records_root: &Path,
    record: &MemoryRecord,
    tags: &[String],
    applies_to: &[String],
) -> Result<PathBuf> {
    write_memory_record_file_internal(records_root, record, tags, applies_to, WriteMode::Overwrite)
}

pub fn create_memory_record_file_with_metadata(
    records_root: &Path,
    record: &MemoryRecord,
    tags: &[String],
    applies_to: &[String],
) -> Result<PathBuf> {
    write_memory_record_file_internal(records_root, record, tags, applies_to, WriteMode::CreateNew)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteMode {
    CreateNew,
    Overwrite,
}

fn write_memory_record_file_internal(
    records_root: &Path,
    record: &MemoryRecord,
    tags: &[String],
    applies_to: &[String],
    mode: WriteMode,
) -> Result<PathBuf> {
    let destination = records_root.join(format!("{}.md", record.id));
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create records directory {}", parent.display()))?;
    }
    let markdown = render_memory_record(record, tags, applies_to);
    match mode {
        WriteMode::CreateNew => OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .and_then(|mut file| std::io::Write::write_all(&mut file, markdown.as_bytes()))
            .with_context(|| format!("failed to create memory record {}", destination.display()))?,
        WriteMode::Overwrite => fs::write(&destination, markdown)
            .with_context(|| format!("failed to write memory record {}", destination.display()))?,
    }
    Ok(destination)
}

#[derive(Debug, Deserialize)]
struct OkfFrontmatter {
    #[serde(rename = "type")]
    memory_type: Option<String>,
    lane: Option<String>,
    title: Option<String>,
    scope: Option<String>,
    scope_kind: Option<String>,
    scope_id: Option<String>,
    visibility: Option<String>,
    status: Option<String>,
    confidence: Option<ConfidenceValue>,
    source: Option<String>,
    source_kind: Option<String>,
    source_ref: Option<String>,
    supersedes: Option<String>,
    supersedes_id: Option<String>,
    expires: Option<String>,
    expires_at: Option<String>,
    timestamp: Option<String>,
    created: Option<String>,
    created_at: Option<String>,
    updated: Option<String>,
    updated_at: Option<String>,
    applies_to: Option<Vec<String>>,
    tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ConfidenceValue {
    Number(f64),
    Label(String),
}

fn collect_markdown_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if is_hidden(&path) {
            continue;
        }
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_markdown_files(&path, files)?;
        } else if metadata.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md")
        {
            files.push(path);
        }
    }
    Ok(())
}

fn import_okf_record(conn: &Connection, record: &OkfRecordFile) -> Result<()> {
    let hash = blake3::hash(record.draft.body.as_bytes())
        .to_hex()
        .to_string();
    let updated = record.updated.as_deref().unwrap_or(record.created.as_str());
    conn.execute(
        "INSERT OR REPLACE INTO memory_record (
          id, type, lane, scope_kind, scope_id, visibility, title, body, status, confidence,
          source_kind, source_ref, content_hash, created_at, updated_at, supersedes_id, expires_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            record.concept_id,
            record.draft.memory_type.as_str(),
            record.draft.lane.as_str(),
            record.draft.scope_kind.as_str(),
            record.draft.scope_id,
            record.draft.visibility.as_str(),
            record.draft.title.trim(),
            record.draft.body.trim(),
            record.status.as_str(),
            record.draft.confidence,
            record.draft.source_kind,
            record.draft.source_ref,
            hash,
            record.created,
            updated,
            record.supersedes_id,
            record.expires_at,
        ],
    )?;
    conn.execute(
        "DELETE FROM memory_path WHERE record_id = ?1",
        [&record.concept_id],
    )?;
    conn.execute(
        "DELETE FROM memory_tag WHERE record_id = ?1",
        [&record.concept_id],
    )?;
    for (index, path) in record.applies_to.iter().enumerate() {
        conn.execute(
            "INSERT INTO memory_path(id, record_id, path, line_start, line_end) VALUES (?1, ?2, ?3, NULL, NULL)",
            params![format!("{}_path_{}", record.concept_id, index), record.concept_id, path],
        )?;
    }
    for tag in &record.draft.tags {
        conn.execute(
            "INSERT OR IGNORE INTO memory_tag(record_id, tag) VALUES (?1, ?2)",
            params![record.concept_id, tag],
        )?;
    }
    Ok(())
}

fn render_memory_record(record: &MemoryRecord, tags: &[String], applies_to: &[String]) -> String {
    let mut output = String::new();
    output.push_str("---\n");
    push_yaml_string(&mut output, "type", record.memory_type.as_str());
    push_yaml_string(&mut output, "lane", record.lane.as_str());
    push_yaml_string(&mut output, "title", &record.title);
    push_yaml_string(
        &mut output,
        "description",
        first_non_empty_line(&record.body),
    );
    push_yaml_string(&mut output, "timestamp", &record.created_at);
    push_yaml_string(&mut output, "updated", &record.updated_at);
    push_yaml_string(&mut output, "status", record.status.as_str());
    push_yaml_string(&mut output, "scope", record.scope_kind.as_str());
    if let Some(scope_id) = &record.scope_id {
        push_yaml_string(&mut output, "scope_id", scope_id);
    }
    push_yaml_string(&mut output, "visibility", record.visibility.as_str());
    output.push_str(&format!("confidence: {}\n", record.confidence));
    push_yaml_string(
        &mut output,
        "source",
        record.source_kind.as_deref().unwrap_or("memzoi-apply"),
    );
    if let Some(source_ref) = &record.source_ref {
        push_yaml_string(&mut output, "source_ref", source_ref);
    }
    if !tags.is_empty() {
        output.push_str("tags:\n");
        for tag in tags {
            output.push_str("  - ");
            output.push_str(&quote_yaml_string(tag));
            output.push('\n');
        }
    }
    if !applies_to.is_empty() {
        output.push_str("applies_to:\n");
        for path in applies_to {
            output.push_str("  - ");
            output.push_str(&quote_yaml_string(path));
            output.push('\n');
        }
    }
    if let Some(supersedes_id) = &record.supersedes_id {
        push_yaml_string(&mut output, "supersedes", supersedes_id);
    }
    if let Some(expires_at) = &record.expires_at {
        push_yaml_string(&mut output, "expires", expires_at);
    }
    output.push_str("---\n\n");
    output.push_str(&format!("# {}\n\n{}\n", record.title, record.body.trim()));
    output
}

fn first_non_empty_line(body: &str) -> &str {
    body.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
}

fn push_yaml_string(output: &mut String, key: &str, value: &str) {
    output.push_str(key);
    output.push_str(": ");
    output.push_str(&quote_yaml_string(value));
    output.push('\n');
}

fn quote_yaml_string(value: &str) -> String {
    if is_plain_yaml_scalar(value) {
        return value.to_owned();
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn is_plain_yaml_scalar(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/'))
}

fn is_reserved_record_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("index.md" | "log.md")
    )
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.') && name != ".")
}

fn concept_id(bundle_root: &Path, file_path: &Path) -> Result<String> {
    if file_path
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("md")
    {
        bail!(
            "OKF record files must use .md extension: {}",
            file_path.display()
        );
    }
    let relative = file_path.strip_prefix(bundle_root).with_context(|| {
        format!(
            "OKF record path {} is not under bundle root {}",
            file_path.display(),
            bundle_root.display()
        )
    })?;
    let without_extension = strip_md_extension(relative);
    for component in without_extension.components() {
        match component {
            Component::Normal(_) => {}
            _ => bail!("OKF concept path contains unsafe component"),
        }
    }
    let concept = without_extension
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    validate_concept_id(&concept)?;
    Ok(concept)
}

fn strip_md_extension(path: &Path) -> PathBuf {
    let mut output = path.to_path_buf();
    output.set_extension("");
    output
}

fn validate_concept_id(concept: &str) -> Result<()> {
    if concept.is_empty() {
        bail!("OKF concept id cannot be empty");
    }
    for segment in concept.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            bail!("OKF concept id contains invalid segment {segment:?}");
        }
        if !segment
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        {
            bail!("OKF concept id segments must use lowercase ASCII letters, digits, and hyphens");
        }
        let starts_and_ends_alnum = segment
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
            && segment
                .chars()
                .last()
                .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit());
        if !starts_and_ends_alnum {
            bail!("OKF concept id segments must start and end with a letter or digit");
        }
    }
    Ok(())
}

fn split_frontmatter(markdown: &str) -> Result<(&str, &str)> {
    let rest = markdown
        .strip_prefix("---\n")
        .or_else(|| markdown.strip_prefix("---\r\n"))
        .context("OKF Markdown must start with YAML frontmatter")?;
    let Some(separator_start) = rest.find("\n---") else {
        bail!("OKF Markdown frontmatter must be closed with ---");
    };
    let frontmatter = &rest[..separator_start];
    let after_separator = &rest[separator_start + "\n---".len()..];
    let body = after_separator
        .strip_prefix("\r\n")
        .or_else(|| after_separator.strip_prefix('\n'))
        .unwrap_or(after_separator);
    Ok((frontmatter, body))
}

fn required_string(value: Option<String>, key: &str) -> Result<String> {
    let value = value.with_context(|| format!("OKF frontmatter missing required field {key}"))?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("OKF frontmatter field {key} cannot be empty");
    }
    Ok(trimmed.to_owned())
}

fn parse_required_enum<T>(value: Option<String>, key: &str) -> Result<T>
where
    T: std::str::FromStr<Err = String>,
{
    let value = required_string(value, key)?;
    value.parse().map_err(anyhow::Error::msg)
}

fn parse_optional_enum<T>(value: Option<String>, key: &str) -> Result<Option<T>>
where
    T: std::str::FromStr<Err = String>,
{
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("OKF frontmatter field {key} cannot be empty");
    }
    trimmed.parse().map(Some).map_err(anyhow::Error::msg)
}

fn parse_scope(value: Option<String>) -> Result<ScopeKind> {
    match value {
        Some(value) => value.parse().map_err(anyhow::Error::msg),
        None => Ok(ScopeKind::Repo),
    }
}

fn parse_status(value: String) -> Result<MemoryStatus> {
    match value.as_str() {
        "current" => Ok(MemoryStatus::Active),
        other => other.parse().map_err(anyhow::Error::msg),
    }
}

fn parse_confidence(value: Option<ConfidenceValue>) -> Result<f64> {
    let value = value.context("OKF frontmatter missing required field confidence")?;
    let confidence = match value {
        ConfidenceValue::Number(value) => value,
        ConfidenceValue::Label(value) => match value.as_str() {
            "confirmed" => 1.0,
            "likely" => 0.75,
            "uncertain" => 0.4,
            other => other
                .parse::<f64>()
                .with_context(|| format!("unknown OKF confidence label {other:?}"))?,
        },
    };
    if !(0.0..=1.0).contains(&confidence) {
        bail!("OKF confidence must be between 0.0 and 1.0");
    }
    Ok(confidence)
}

fn ensure_timestampish(value: &str, key: &str) -> Result<()> {
    if value.len() < 10
        || !value
            .chars()
            .take(10)
            .all(|ch| ch.is_ascii_digit() || ch == '-')
    {
        bail!("OKF frontmatter field {key} must be timestamp-ish");
    }
    Ok(())
}

fn validate_applies_to(values: Vec<String>) -> Result<Vec<String>> {
    let mut output = Vec::with_capacity(values.len());
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            bail!("applies_to entries cannot be empty");
        }
        let path = Path::new(trimmed);
        if path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            bail!("applies_to entries must be relative and cannot contain traversal");
        }
        output.push(trimmed.to_owned());
    }
    Ok(output)
}

fn body_without_matching_h1(body: &str, title: &str) -> Result<String> {
    let body = body.trim_start_matches(['\r', '\n']);
    let Some(rest) = body.strip_prefix("# ") else {
        return Ok(body.trim().to_owned());
    };
    let line_end = rest.find('\n').unwrap_or(rest.len());
    let h1 = rest[..line_end].trim();
    if h1 != title {
        bail!("OKF H1 title must match frontmatter title");
    }
    Ok(rest[line_end..]
        .trim_start_matches(['\r', '\n'])
        .trim()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::{MemoryLane, MemoryStatus, MemoryType, ScopeKind, Visibility};

    const EXAMPLE_MEMORY: &str = include_str!("../../../examples/example-memory.md");

    #[test]
    fn parses_example_memory_into_importable_draft() -> anyhow::Result<()> {
        let bundle_root = Path::new("/bundle");
        let file_path = bundle_root.join("memories/repo/frontend/swedish-first.md");

        let parsed = super::parse_okf_record_markdown(bundle_root, &file_path, EXAMPLE_MEMORY)?
            .expect("example memory should be a concept record");

        assert_eq!(parsed.concept_id, "memories/repo/frontend/swedish-first");
        assert_eq!(parsed.status, MemoryStatus::Active);
        assert_eq!(parsed.applies_to, vec!["apps/web/**"]);
        assert_eq!(parsed.created, "2026-07-04T00:00:00Z");
        assert_eq!(parsed.updated, None);
        assert_eq!(parsed.draft.memory_type, MemoryType::Preference);
        assert_eq!(parsed.draft.lane, MemoryLane::Semantic);
        assert_eq!(parsed.draft.scope_kind, ScopeKind::Repo);
        assert_eq!(parsed.draft.visibility, Visibility::Team);
        assert_eq!(parsed.draft.title, "Swedish-first UI copy");
        assert_eq!(parsed.draft.tags, vec!["frontend", "i18n"]);
        assert_eq!(parsed.draft.source_kind.as_deref(), Some("human"));
        assert_eq!(
            parsed.draft.source_ref.as_deref(),
            Some("memories/repo/frontend/swedish-first")
        );
        assert_eq!(parsed.draft.confidence, 1.0);
        assert!(parsed.draft.body.contains("User-facing UI"));
        assert!(!parsed.draft.body.contains("# Swedish-first UI copy"));
        Ok(())
    }

    #[test]
    fn parses_supported_memory_lanes() -> anyhow::Result<()> {
        for (lane, memory_type, expected_lane, expected_type) in [
            (
                "session",
                "episode",
                MemoryLane::Session,
                MemoryType::Episode,
            ),
            (
                "semantic",
                "decision",
                MemoryLane::Semantic,
                MemoryType::Decision,
            ),
            (
                "episodic",
                "episode",
                MemoryLane::Episodic,
                MemoryType::Episode,
            ),
            (
                "procedural",
                "procedure",
                MemoryLane::Procedural,
                MemoryType::Procedure,
            ),
        ] {
            let parsed = super::parse_okf_record_markdown(
                Path::new("/bundle"),
                Path::new("/bundle/memories/lane-test.md"),
                &record_markdown(lane, memory_type),
            )?
            .expect("lane test record should parse");

            assert_eq!(parsed.draft.lane, expected_lane);
            assert_eq!(parsed.draft.memory_type, expected_type);
        }

        Ok(())
    }

    #[test]
    fn missing_lane_defaults_to_semantic_for_backward_compatibility() -> anyhow::Result<()> {
        let parsed = super::parse_okf_record_markdown(
            Path::new("/bundle"),
            Path::new("/bundle/memories/legacy.md"),
            r#"---
type: decision
title: Legacy memory
scope: repo
visibility: repo
source: human
status: active
confidence: confirmed
created: 2026-07-04
---

# Legacy memory

Legacy records without lane remain valid.
"#,
        )?
        .expect("legacy memory should parse");

        assert_eq!(parsed.draft.lane, MemoryLane::Semantic);
        Ok(())
    }

    #[test]
    fn rejects_unknown_memory_lane() {
        let error = super::parse_okf_record_markdown(
            Path::new("/bundle"),
            Path::new("/bundle/memories/invalid-lane.md"),
            &record_markdown("mystery", "decision"),
        )
        .expect_err("unknown memory lane must be rejected");

        assert!(
            error.to_string().contains("unknown memory lane"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn rejects_applies_to_traversal() {
        let invalid = r#"---
type: preference
title: Unsafe path
scope: repo
visibility: team
source: human-authored
status: current
confidence: confirmed
created: 2026-07-04
applies_to:
  - ../secrets
---

# Unsafe path

Do not import this.
"#;

        let error = super::parse_okf_record_markdown(
            Path::new("/bundle"),
            Path::new("/bundle/memories/unsafe.md"),
            invalid,
        )
        .expect_err("applies_to traversal must be rejected");

        assert!(
            error
                .to_string()
                .contains("applies_to entries must be relative and cannot contain traversal"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn skips_reserved_index_and_log_files() -> anyhow::Result<()> {
        let bundle_root = Path::new("/bundle");

        assert!(
            super::parse_okf_record_markdown(
                bundle_root,
                bundle_root.join("memories/index.md"),
                EXAMPLE_MEMORY,
            )?
            .is_none()
        );
        assert!(
            super::parse_okf_record_markdown(
                bundle_root,
                bundle_root.join("memories/log.md"),
                EXAMPLE_MEMORY,
            )?
            .is_none()
        );
        Ok(())
    }

    fn record_markdown(lane: &str, memory_type: &str) -> String {
        format!(
            r#"---
type: {memory_type}
lane: {lane}
title: Lane test
scope: repo
visibility: repo
source: human
status: active
confidence: confirmed
created: 2026-07-04
---

# Lane test

This record exercises lane parsing.
"#
        )
    }
}
