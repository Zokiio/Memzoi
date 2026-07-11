use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::{
    CAPTURE_GIT_PROCESS_TIMEOUT_MILLIS, CAPTURE_MAX_AGGREGATE_SOURCE_BYTES,
    CAPTURE_MAX_DIRECTORY_DEPTH, CAPTURE_MAX_DIRECTORY_FILES, CAPTURE_MAX_GIT_POLICY_BYTES,
    CAPTURE_MAX_GIT_POLICY_FILE_BYTES, CaptureLoadedSource, CapturePlanningControl,
    CapturePolicyInputSnapshot, CaptureSourceDocument, CaptureSourceInputs, CaptureSourceLocator,
    CaptureSourceMemberSnapshot, CaptureSourceRequest, CaptureSourceSnapshot,
    MAX_DIFF_SOURCE_BYTES, MAX_MARKDOWN_SOURCE_BYTES, MemoryPaths, check_planning_control,
    content_hash, domain_hash, normalize_absolute_path, open_capture_source, prohibited_finding,
};

const GITIGNORE_ENGINE_VERSION: &str = "memzoi/gitignore-v1+ignore-0.4.28";
const MAX_DIRECTORY_ENTRIES: usize = CAPTURE_MAX_DIRECTORY_FILES * 32;
const MAX_GIT_STDERR_BYTES: usize = 64 * 1024;
const MAX_GIT_COMMIT_BYTES: usize = 1024 * 1024;
const MAX_GITFILE_BYTES: u64 = 4096;
const GIT_PROCESS_TIMEOUT: Duration = Duration::from_millis(CAPTURE_GIT_PROCESS_TIMEOUT_MILLIS);
const GIT_POLL_INTERVAL: Duration = Duration::from_millis(5);
const GIT_REPOSITORY_IDENTITY_VERSION: &str = "memzoi/git-repository-identity-v1";

#[derive(Debug, Clone)]
struct ResolvedGitDirectory {
    git_dir: PathBuf,
    common_dir: PathBuf,
    identity_nodes: Vec<GitFilesystemIdentity>,
    attribute_source: Option<String>,
    local_config_identity: GitLocalConfigIdentity,
    local_config_policy_input: CapturePolicyInputSnapshot,
    policy_input: CapturePolicyInputSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitFilesystemIdentity {
    path: PathBuf,
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitLocalConfigIdentity {
    path: PathBuf,
    source_content_hash: String,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
}

pub(super) fn load_capture_source(
    paths: &MemoryPaths,
    source: &CaptureSourceRequest,
    source_inputs: &CaptureSourceInputs,
    control: Option<&CapturePlanningControl>,
) -> Result<CaptureLoadedSource> {
    check_planning_control(control)?;
    match &source.locator {
        CaptureSourceLocator::ProjectPath { path } => {
            require_no_supplied_bytes(source_inputs)?;
            let limit = if source.media_type == "text/x-diff" {
                MAX_DIFF_SOURCE_BYTES
            } else {
                MAX_MARKDOWN_SOURCE_BYTES
            };
            let (snapshot, bytes) = read_project_source(paths, source, path, limit, control)?;
            Ok(single_document(source, snapshot, bytes))
        }
        CaptureSourceLocator::ProjectDirectory {
            path,
            recursive,
            ignore_policy,
            include,
        } => {
            require_no_supplied_bytes(source_inputs)?;
            if ignore_policy != "git-v1" || include.as_slice() != ["*.md"] {
                bail!("capture directory policy is unsupported");
            }
            load_project_directory(paths, source, path, *recursive, control)
        }
        CaptureSourceLocator::SuppliedBytes {
            byte_length,
            source_content_hash,
            ..
        } => load_supplied_bytes(
            source,
            source_inputs,
            *byte_length,
            source_content_hash,
            control,
        ),
        CaptureSourceLocator::GitRange {
            repository,
            base,
            head,
            merge_parent,
            rename_detection,
            diff_format,
        } => {
            require_no_supplied_bytes(source_inputs)?;
            load_git_range(
                paths,
                source,
                repository,
                base,
                head,
                merge_parent,
                *rename_detection,
                diff_format,
                control,
            )
        }
    }
}

fn require_no_supplied_bytes(source_inputs: &CaptureSourceInputs) -> Result<()> {
    if !source_inputs.is_empty() {
        bail!("capture source material was supplied for a resolver-backed source");
    }
    Ok(())
}

fn single_document(
    source: &CaptureSourceRequest,
    snapshot: CaptureSourceSnapshot,
    bytes: Vec<u8>,
) -> CaptureLoadedSource {
    CaptureLoadedSource {
        snapshot: snapshot.clone(),
        documents: vec![CaptureSourceDocument {
            request: source.clone(),
            snapshot,
            bytes,
        }],
    }
}

fn load_supplied_bytes(
    source: &CaptureSourceRequest,
    source_inputs: &CaptureSourceInputs,
    declared_length: u64,
    declared_hash: &str,
    control: Option<&CapturePlanningControl>,
) -> Result<CaptureLoadedSource> {
    check_planning_control(control)?;
    if source_inputs.supplied_bytes.len() != 1 {
        bail!("capture supplied-bytes input is missing or contains unexpected material");
    }
    let bytes = source_inputs
        .supplied_bytes(&source.source_id)
        .context("capture supplied-bytes input is missing or contains unexpected material")?;
    if bytes.len() as u64 > MAX_DIFF_SOURCE_BYTES {
        bail!("capture supplied-bytes input exceeds the configured size limit");
    }
    if bytes.len() as u64 != declared_length || content_hash(bytes) != declared_hash {
        bail!("capture supplied-bytes input does not match its descriptor");
    }
    let snapshot = CaptureSourceSnapshot {
        source_id: source.source_id.clone(),
        locator: source.locator.clone(),
        media_type: source.media_type.clone(),
        byte_length: bytes.len() as u64,
        source_content_hash: declared_hash.to_owned(),
        members: Vec::new(),
        policy_inputs: Vec::new(),
    };
    Ok(single_document(source, snapshot, bytes.to_vec()))
}

fn read_project_source(
    paths: &MemoryPaths,
    source: &CaptureSourceRequest,
    relative: &str,
    max_bytes: u64,
    control: Option<&CapturePlanningControl>,
) -> Result<(CaptureSourceSnapshot, Vec<u8>)> {
    let bytes = read_project_bytes(paths, relative, max_bytes, control)?;
    let snapshot = CaptureSourceSnapshot {
        source_id: source.source_id.clone(),
        locator: source.locator.clone(),
        media_type: source.media_type.clone(),
        byte_length: bytes.len() as u64,
        source_content_hash: content_hash(&bytes),
        members: Vec::new(),
        policy_inputs: Vec::new(),
    };
    Ok((snapshot, bytes))
}

fn read_project_bytes(
    paths: &MemoryPaths,
    relative: &str,
    max_bytes: u64,
    control: Option<&CapturePlanningControl>,
) -> Result<Vec<u8>> {
    check_planning_control(control)?;
    ensure_source_not_protected(paths, relative)?;
    let mut file = open_capture_source(&paths.project_root, relative)?;
    let before = file
        .metadata()
        .context("failed to inspect capture source")?;
    if !before.is_file() {
        bail!("capture source must be a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if before.nlink() != 1 {
            bail!("capture source must not be hard-linked");
        }
    }
    if before.len() > max_bytes {
        bail!("capture source exceeds the configured size limit");
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    file.by_ref()
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .context("failed to read capture source")?;
    check_planning_control(control)?;
    if bytes.len() as u64 > max_bytes {
        bail!("capture source exceeds the configured size limit");
    }
    let after = file
        .metadata()
        .context("failed to inspect capture source")?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        bail!("capture source changed while it was read");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if after.nlink() != 1
            || before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            bail!("capture source metadata changed while it was read");
        }
    }
    Ok(bytes)
}

fn ensure_source_not_protected(paths: &MemoryPaths, relative: &str) -> Result<()> {
    let candidate_path = paths.project_root.join(relative);
    let current_dir = std::env::current_dir().context("failed to resolve capture runtime paths")?;
    let candidate_resolved = candidate_path
        .canonicalize()
        .context("failed to resolve capture source containment")?;
    for protected in [&paths.memory_dir, &paths.runtime_dir, &paths.exports_dir] {
        let absolute = if protected.is_absolute() {
            protected.clone()
        } else {
            current_dir.join(protected)
        };
        let normalized = normalize_absolute_path(&absolute);
        let resolved = normalized.canonicalize().unwrap_or(normalized);
        if candidate_resolved.starts_with(resolved) {
            bail!("capture source cannot read Memzoi runtime or generated export state");
        }
    }
    Ok(())
}

#[derive(Clone)]
struct IgnoreRuleSet {
    matcher: Gitignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectoryLayout {
    members: Vec<String>,
    policy_inputs: Vec<CapturePolicyInputSnapshot>,
}

struct DirectoryWalk<'a> {
    paths: &'a MemoryPaths,
    recursive: bool,
    control: Option<&'a CapturePlanningControl>,
    members: Vec<String>,
    policy_inputs: BTreeMap<String, CapturePolicyInputSnapshot>,
    policy_bytes: u64,
    entries_seen: usize,
}

fn load_project_directory(
    paths: &MemoryPaths,
    source: &CaptureSourceRequest,
    root: &str,
    recursive: bool,
    control: Option<&CapturePlanningControl>,
) -> Result<CaptureLoadedSource> {
    let first = enumerate_directory_layout(paths, root, recursive, control)?;
    let mut documents = Vec::with_capacity(first.members.len());
    let mut members = Vec::with_capacity(first.members.len());
    let mut aggregate_bytes = 0u64;
    for member_path in &first.members {
        check_planning_control(control)?;
        let mut member_request = source.clone();
        member_request.locator = CaptureSourceLocator::ProjectPath {
            path: member_path.clone(),
        };
        let (snapshot, bytes) = read_project_source(
            paths,
            &member_request,
            member_path,
            MAX_MARKDOWN_SOURCE_BYTES,
            control,
        )?;
        aggregate_bytes = aggregate_bytes
            .checked_add(snapshot.byte_length)
            .context("capture directory aggregate size overflowed")?;
        if aggregate_bytes > CAPTURE_MAX_AGGREGATE_SOURCE_BYTES {
            bail!("capture directory exceeds the configured aggregate size limit");
        }
        members.push(CaptureSourceMemberSnapshot {
            path: member_path.clone(),
            byte_length: snapshot.byte_length,
            source_content_hash: snapshot.source_content_hash.clone(),
        });
        documents.push(CaptureSourceDocument {
            request: member_request,
            snapshot,
            bytes,
        });
    }

    let second = enumerate_directory_layout(paths, root, recursive, control)?;
    if first != second {
        bail!("capture directory changed while it was being read");
    }
    let manifest = serde_json_canonicalizer::to_vec(&(&members, &first.policy_inputs))
        .context("failed to fingerprint capture directory manifest")?;
    let snapshot = CaptureSourceSnapshot {
        source_id: source.source_id.clone(),
        locator: source.locator.clone(),
        media_type: source.media_type.clone(),
        byte_length: aggregate_bytes,
        source_content_hash: domain_hash("memzoi/capture-directory-manifest-v1", &manifest),
        members,
        policy_inputs: first.policy_inputs,
    };
    Ok(CaptureLoadedSource {
        snapshot,
        documents,
    })
}

fn enumerate_directory_layout(
    paths: &MemoryPaths,
    root: &str,
    recursive: bool,
    control: Option<&CapturePlanningControl>,
) -> Result<DirectoryLayout> {
    if Path::new(root).components().any(|component| {
        matches!(component, Component::Normal(value) if value.eq_ignore_ascii_case(".git") || value.eq_ignore_ascii_case(".memzoi"))
    }) {
        bail!("capture directory cannot read repository or Memzoi-managed state");
    }
    let mut walk = DirectoryWalk {
        paths,
        recursive,
        control,
        members: Vec::new(),
        policy_inputs: BTreeMap::new(),
        policy_bytes: 0,
        entries_seen: 0,
    };
    let mut rules = Vec::new();
    let mut prefix = PathBuf::new();
    walk.load_ignore_file(&prefix, &mut rules)?;
    for component in Path::new(root).components() {
        let Component::Normal(component) = component else {
            bail!("capture directory path contains an unsafe component");
        };
        prefix.push(component);
        walk.load_ignore_file(&prefix, &mut rules)?;
    }
    walk.walk(Path::new(root), 0, rules)?;
    walk.members.sort();
    if walk.members.len() > CAPTURE_MAX_DIRECTORY_FILES {
        bail!("capture directory exceeds the configured file limit");
    }
    Ok(DirectoryLayout {
        members: walk.members,
        policy_inputs: walk.policy_inputs.into_values().collect(),
    })
}

impl DirectoryWalk<'_> {
    fn walk(&mut self, relative: &Path, depth: usize, rules: Vec<IgnoreRuleSet>) -> Result<()> {
        check_planning_control(self.control)?;
        let entries = list_directory(self.paths, relative)?;
        for (index, entry) in entries.into_iter().enumerate() {
            if index % 64 == 0 {
                check_planning_control(self.control)?;
            }
            self.entries_seen += 1;
            if self.entries_seen > MAX_DIRECTORY_ENTRIES {
                bail!("capture directory exceeds the configured traversal limit");
            }
            if entry.name.eq_ignore_ascii_case(".git") || entry.name.eq_ignore_ascii_case(".memzoi")
            {
                continue;
            }
            let child = relative.join(&entry.name);
            let child_absolute = self.paths.project_root.join(&child);
            match entry.kind {
                DirectoryEntryKind::File => {
                    if is_ignored(&rules, &child_absolute, false) {
                        continue;
                    }
                    if child.extension().and_then(|value| value.to_str()) == Some("md") {
                        let path = posix_relative_path(&child)?;
                        self.members.push(path);
                        if self.members.len() > CAPTURE_MAX_DIRECTORY_FILES {
                            bail!("capture directory exceeds the configured file limit");
                        }
                    }
                }
                DirectoryEntryKind::Directory => {
                    if is_ignored(&rules, &child_absolute, true) || !self.recursive {
                        continue;
                    }
                    if depth >= CAPTURE_MAX_DIRECTORY_DEPTH {
                        bail!("capture directory exceeds the configured depth limit");
                    }
                    let mut child_rules = rules.clone();
                    self.load_ignore_file(&child, &mut child_rules)?;
                    self.walk(&child, depth + 1, child_rules)?;
                }
                DirectoryEntryKind::Special => {
                    bail!("capture directory contains a symbolic link or special file");
                }
            }
        }
        Ok(())
    }

    fn load_ignore_file(&mut self, directory: &Path, rules: &mut Vec<IgnoreRuleSet>) -> Result<()> {
        let relative = directory.join(".gitignore");
        let absolute = self.paths.project_root.join(&relative);
        match fs::symlink_metadata(&absolute) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    bail!("capture ignore policy must be a regular nonsymlink file");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).context("failed to inspect capture ignore policy");
            }
        }
        let relative_string = posix_relative_path(&relative)?;
        if self.policy_inputs.len() >= CAPTURE_MAX_DIRECTORY_FILES {
            bail!("capture directory exceeds the configured ignore-policy file limit");
        }
        let bytes = read_project_bytes(
            self.paths,
            &relative_string,
            MAX_MARKDOWN_SOURCE_BYTES,
            self.control,
        )?;
        reject_prohibited_policy_bytes(&bytes, "ignore policy")?;
        self.policy_bytes = self
            .policy_bytes
            .checked_add(bytes.len() as u64)
            .context("capture directory ignore-policy size overflowed")?;
        if self.policy_bytes > CAPTURE_MAX_AGGREGATE_SOURCE_BYTES {
            bail!("capture directory exceeds the configured ignore-policy size limit");
        }
        let text = std::str::from_utf8(&bytes).context("capture ignore policy must be UTF-8")?;
        let mut builder = GitignoreBuilder::new(self.paths.project_root.join(directory));
        for (index, line) in text.lines().enumerate() {
            let line = if index == 0 {
                line.trim_start_matches('\u{feff}')
            } else {
                line
            };
            builder
                .add_line(Some(absolute.clone()), line)
                .context("capture ignore policy contains an invalid pattern")?;
        }
        let matcher = builder
            .build()
            .context("failed to compile capture ignore policy")?;
        rules.push(IgnoreRuleSet { matcher });
        self.policy_inputs.insert(
            relative_string.clone(),
            CapturePolicyInputSnapshot {
                path: relative_string,
                source_content_hash: content_hash(&bytes),
                engine_version: GITIGNORE_ENGINE_VERSION.to_owned(),
            },
        );
        Ok(())
    }
}

fn is_ignored(rules: &[IgnoreRuleSet], path: &Path, is_dir: bool) -> bool {
    let mut ignored = false;
    for ruleset in rules {
        let matched = ruleset.matcher.matched(path, is_dir);
        if matched.is_ignore() {
            ignored = true;
        } else if matched.is_whitelist() {
            ignored = false;
        }
    }
    ignored
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectoryEntryKind {
    File,
    Directory,
    Special,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectoryEntry {
    name: String,
    kind: DirectoryEntryKind,
}

#[cfg(unix)]
fn list_directory(paths: &MemoryPaths, relative: &Path) -> Result<Vec<DirectoryEntry>> {
    use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags, openat, statat};

    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = openat(CWD, &paths.project_root, directory_flags, Mode::empty())
        .context("failed to open capture project root without following symbolic links")?;
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!("capture directory path contains an unsafe component");
        };
        directory = openat(&directory, component, directory_flags, Mode::empty())
            .context("failed to open capture directory without following symbolic links")?;
    }
    let mut reader = Dir::read_from(&directory)
        .context("failed to read capture directory without following symbolic links")?;
    let mut entries = Vec::new();
    while let Some(entry) = reader.next() {
        let entry = entry.context("failed to enumerate capture directory")?;
        let name_bytes = entry.file_name().to_bytes();
        if matches!(name_bytes, b"." | b"..") {
            continue;
        }
        let name = std::str::from_utf8(name_bytes)
            .context("capture directory contains a non-UTF-8 name")?
            .to_owned();
        if name.contains('/') || name.contains('\0') || name.chars().any(char::is_control) {
            bail!("capture directory contains an unsafe entry name");
        }
        let mut file_type = entry.file_type();
        if file_type == FileType::Unknown {
            let stat = statat(
                reader.fd().context("failed to inspect capture directory")?,
                entry.file_name(),
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .context("failed to inspect capture directory entry")?;
            file_type = FileType::from_raw_mode(stat.st_mode);
        }
        let kind = if file_type.is_file() {
            DirectoryEntryKind::File
        } else if file_type.is_dir() {
            DirectoryEntryKind::Directory
        } else {
            DirectoryEntryKind::Special
        };
        entries.push(DirectoryEntry { name, kind });
        if entries.len() > MAX_DIRECTORY_ENTRIES {
            bail!("capture directory exceeds the configured traversal limit");
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

#[cfg(not(unix))]
fn list_directory(_paths: &MemoryPaths, _relative: &Path) -> Result<Vec<DirectoryEntry>> {
    bail!("secure capture directory access is unavailable on this platform; capture fails closed")
}

fn posix_relative_path(path: &Path) -> Result<String> {
    let mut output = String::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            bail!("capture path contains an unsafe component");
        };
        let value = component
            .to_str()
            .context("capture path contains a non-UTF-8 component")?;
        if !output.is_empty() {
            output.push('/');
        }
        output.push_str(value);
    }
    if output.is_empty() {
        bail!("capture path cannot be empty");
    }
    Ok(output)
}

fn git_diff_policy_paths(
    bytes: &[u8],
    control: Option<&CapturePlanningControl>,
) -> Result<(Vec<String>, Vec<PathBuf>)> {
    let changed_paths = super::adapters::git_changed_paths(bytes)?;
    let mut policy_paths = BTreeSet::<PathBuf>::new();
    policy_paths.insert(PathBuf::from(".gitignore"));
    for changed_path in &changed_paths {
        check_planning_control(control)?;
        if super::adapters::git_path_exclusion_code(changed_path).is_some() {
            continue;
        }
        let parent = Path::new(changed_path)
            .parent()
            .unwrap_or_else(|| Path::new(""));
        if parent.components().count() > CAPTURE_MAX_DIRECTORY_DEPTH {
            bail!("capture Git diff path exceeds the configured ignore-policy depth");
        }
        let mut directory = PathBuf::new();
        for component in parent.components() {
            directory.push(component.as_os_str());
            policy_paths.insert(directory.join(".gitignore"));
        }
    }
    if policy_paths.len() > CAPTURE_MAX_DIRECTORY_FILES {
        bail!("capture Git diff exceeds the configured ignore-policy file limit");
    }
    let mut ordered = policy_paths.into_iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    Ok((changed_paths, ordered))
}

fn git_range_ignore_policy(
    paths: &MemoryPaths,
    git_directory: &ResolvedGitDirectory,
    head: &str,
    bytes: &[u8],
    control: Option<&CapturePlanningControl>,
) -> Result<Vec<CapturePolicyInputSnapshot>> {
    let (changed_paths, ordered) = git_diff_policy_paths(bytes, control)?;
    let mut rules = Vec::new();
    let mut snapshots = Vec::new();
    let mut total_bytes = 0u64;
    for relative in ordered {
        check_planning_control(control)?;
        let relative_string = posix_relative_path(&relative)?;
        let Some(policy_bytes) = read_git_tree_file(
            git_directory,
            head,
            &relative_string,
            CAPTURE_MAX_GIT_POLICY_FILE_BYTES as usize,
            control,
        )?
        else {
            continue;
        };
        reject_prohibited_policy_bytes(&policy_bytes, "Git range ignore policy")?;
        total_bytes = total_bytes
            .checked_add(policy_bytes.len() as u64)
            .context("capture Git range ignore-policy size overflowed")?;
        if total_bytes > CAPTURE_MAX_GIT_POLICY_BYTES {
            bail!("capture Git range exceeds the configured ignore-policy size limit");
        }
        let text = std::str::from_utf8(&policy_bytes)
            .context("capture Git range ignore policy must be UTF-8")?;
        let directory = relative.parent().unwrap_or_else(|| Path::new(""));
        let virtual_policy_path = paths.project_root.join(&relative);
        let mut builder = GitignoreBuilder::new(paths.project_root.join(directory));
        for (index, line) in text.lines().enumerate() {
            let line = if index == 0 {
                line.trim_start_matches('\u{feff}')
            } else {
                line
            };
            builder
                .add_line(Some(virtual_policy_path.clone()), line)
                .context("capture Git range ignore policy contains an invalid pattern")?;
        }
        rules.push(IgnoreRuleSet {
            matcher: builder
                .build()
                .context("failed to compile capture Git range ignore policy")?,
        });
        snapshots.push(CapturePolicyInputSnapshot {
            path: relative_string,
            source_content_hash: content_hash(&policy_bytes),
            engine_version: format!("{GITIGNORE_ENGINE_VERSION}+git-tree-v1"),
        });
    }
    for changed_path in changed_paths {
        if super::adapters::git_path_exclusion_code(&changed_path).is_some() {
            continue;
        }
        if is_ignored(&rules, &paths.project_root.join(&changed_path), false) {
            bail!("capture Git range contains a path ignored by its named head tree");
        }
    }
    Ok(snapshots)
}

fn read_git_tree_file(
    git_directory: &ResolvedGitDirectory,
    tree: &str,
    path: &str,
    limit: usize,
    control: Option<&CapturePlanningControl>,
) -> Result<Option<Vec<u8>>> {
    let mut list_args = vec![
        OsString::from("ls-tree"),
        OsString::from("-z"),
        OsString::from("--full-tree"),
        OsString::from(tree),
        OsString::from("--"),
        OsString::from(path),
    ];
    let listing = run_git_bounded_in(git_directory, &mut list_args, 4096, control)?;
    if listing.is_empty() {
        return Ok(None);
    }
    let record = listing
        .strip_suffix(b"\0")
        .context("capture Git tree policy listing is not NUL terminated")?;
    if record.contains(&b'\0') {
        bail!("capture Git tree policy listing is ambiguous");
    }
    let separator = record
        .iter()
        .position(|byte| *byte == b'\t')
        .context("capture Git tree policy listing is malformed")?;
    let (metadata, listed_path) = record.split_at(separator);
    let listed_path = &listed_path[1..];
    if listed_path != path.as_bytes() {
        bail!("capture Git tree policy listing changed path identity");
    }
    let metadata = std::str::from_utf8(metadata)?;
    let fields = metadata.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 3
        || !matches!(fields[0], "100644" | "100755")
        || fields[1] != "blob"
        || full_raw_object_id(fields[2]).is_none()
    {
        bail!("capture Git tree ignore policy is not a regular blob");
    }
    let mut read_args = vec![
        OsString::from("cat-file"),
        OsString::from("blob"),
        OsString::from(fields[2]),
    ];
    run_git_bounded_in(git_directory, &mut read_args, limit, control).map(Some)
}

fn full_raw_object_id(value: &str) -> Option<&str> {
    (matches!(value.len(), 40 | 64)
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !value.bytes().any(|byte| byte.is_ascii_uppercase()))
    .then_some(value)
}

#[allow(clippy::too_many_arguments)]
fn load_git_range(
    paths: &MemoryPaths,
    source: &CaptureSourceRequest,
    repository: &str,
    base: &str,
    head: &str,
    merge_parent: &str,
    rename_detection: bool,
    diff_format: &str,
    control: Option<&CapturePlanningControl>,
) -> Result<CaptureLoadedSource> {
    check_planning_control(control)?;
    if repository != "." || diff_format != "git-unified-v1" {
        bail!("capture git_range configuration is unsupported");
    }
    let (base_algorithm, base_oid) = split_object_id(base)?;
    let (head_algorithm, head_oid) = split_object_id(head)?;
    if base_algorithm != head_algorithm {
        bail!("capture git_range object algorithms do not match");
    }
    let mut git_directory = resolve_git_dir(paths, control)?;
    reject_git_ambient_repository_files(&git_directory)?;
    require_commit(&git_directory, base_oid, control)?;
    require_commit(&git_directory, head_oid, control)?;
    if merge_parent == "first_parent" {
        require_first_parent(&git_directory, head_oid, base_oid, control)?;
    } else if merge_parent != "base_to_head" {
        bail!("capture git_range merge-parent mode is unsupported");
    }
    git_directory.attribute_source = Some(head_oid.to_owned());
    let renderer_policy = git_renderer_policy_input(&git_directory, control)?;

    let mut args = vec![
        OsString::from("diff-tree"),
        OsString::from("-r"),
        OsString::from("-p"),
        OsString::from("--no-commit-id"),
        OsString::from("--full-index"),
        OsString::from("--no-ext-diff"),
        OsString::from("--no-textconv"),
        OsString::from("--no-color"),
        OsString::from(if cfg!(windows) {
            "-ONUL"
        } else {
            "-O/dev/null"
        }),
        OsString::from("--diff-algorithm=myers"),
        OsString::from("--no-indent-heuristic"),
        OsString::from("--inter-hunk-context=0"),
        OsString::from("--unified=3"),
        OsString::from("--src-prefix=a/"),
        OsString::from("--dst-prefix=b/"),
        OsString::from("--line-prefix="),
        OsString::from("--output-indicator-new=+"),
        OsString::from("--output-indicator-old=-"),
        OsString::from("--output-indicator-context= "),
        OsString::from("--no-relative"),
        OsString::from("--submodule=short"),
        OsString::from(if rename_detection {
            "--find-renames=100%"
        } else {
            "--no-renames"
        }),
        OsString::from("--rename-empty"),
        OsString::from("-l0"),
        OsString::from(base_oid),
        OsString::from(head_oid),
        OsString::from("--"),
    ];
    let bytes = run_git_bounded_in(
        &git_directory,
        &mut args,
        MAX_DIFF_SOURCE_BYTES as usize,
        control,
    )?;
    let mut policy_inputs = vec![
        git_directory.policy_input.clone(),
        git_directory.local_config_policy_input.clone(),
        renderer_policy,
    ];
    policy_inputs.extend(git_range_ignore_policy(
        paths,
        &git_directory,
        head_oid,
        &bytes,
        control,
    )?);
    let snapshot = CaptureSourceSnapshot {
        source_id: source.source_id.clone(),
        locator: source.locator.clone(),
        media_type: source.media_type.clone(),
        byte_length: bytes.len() as u64,
        source_content_hash: content_hash(&bytes),
        members: Vec::new(),
        policy_inputs,
    };
    Ok(single_document(source, snapshot, bytes))
}

fn split_object_id(value: &str) -> Result<(&str, &str)> {
    if let Some(digest) = value.strip_prefix("sha1:")
        && digest.len() == 40
        && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !digest.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Ok(("sha1", digest));
    }
    if let Some(digest) = value.strip_prefix("sha256:")
        && digest.len() == 64
        && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !digest.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Ok(("sha256", digest));
    }
    bail!("capture Git object ID is invalid")
}

fn reject_git_ambient_repository_files(git_directory: &ResolvedGitDirectory) -> Result<()> {
    for (relative, label) in [
        ("info/attributes", "Git info attributes"),
        ("info/grafts", "Git grafts"),
        ("objects/info/alternates", "Git object alternates"),
        ("objects/info/http-alternates", "Git HTTP object alternates"),
    ] {
        reject_nonempty_git_ambient_file(git_directory, relative, label)?;
    }
    Ok(())
}

fn reject_nonempty_git_ambient_file(
    git_directory: &ResolvedGitDirectory,
    relative: &str,
    label: &str,
) -> Result<()> {
    let path = git_directory.common_dir.join(relative);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("capture {label} must not be a symlink or special file");
            }
            let bytes = read_bounded_nonsymlink_file(
                &path,
                CAPTURE_MAX_GIT_POLICY_FILE_BYTES,
                &format!("capture {label}"),
            )?;
            if !bytes.is_empty() {
                bail!("capture git_range does not allow ambient {label}");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect capture {label}"));
        }
    }
    Ok(())
}

fn git_renderer_policy_input(
    git_directory: &ResolvedGitDirectory,
    control: Option<&CapturePlanningControl>,
) -> Result<CapturePolicyInputSnapshot> {
    let mut args = vec![OsString::from("version")];
    let version = run_git_bounded_in(git_directory, &mut args, 256, control)?;
    let rendered = std::str::from_utf8(&version)
        .context("capture Git renderer version must be UTF-8")?
        .trim();
    let token = rendered
        .strip_prefix("git version ")
        .and_then(|value| value.split_whitespace().next())
        .context("capture Git renderer version output is unsupported")?;
    let mut components = token.split('.');
    let major = components
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .context("capture Git renderer major version is invalid")?;
    let minor = components
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .context("capture Git renderer minor version is invalid")?;
    if (major, minor) < (2, 43) {
        bail!("capture Git renderer must be Git 2.43 or newer");
    }
    Ok(CapturePolicyInputSnapshot {
        path: ".git/renderer-version".to_owned(),
        source_content_hash: content_hash(&version),
        engine_version: format!("memzoi/git-unified-renderer-v1+git-{token}"),
    })
}

fn require_commit(
    git_directory: &ResolvedGitDirectory,
    oid: &str,
    control: Option<&CapturePlanningControl>,
) -> Result<()> {
    let mut args = vec![
        OsString::from("cat-file"),
        OsString::from("-t"),
        OsString::from(oid),
    ];
    let output = run_git_bounded_in(git_directory, &mut args, 32, control)?;
    if output.as_slice() != b"commit\n" {
        bail!("capture git_range object is not a commit");
    }
    Ok(())
}

fn require_first_parent(
    git_directory: &ResolvedGitDirectory,
    head: &str,
    base: &str,
    control: Option<&CapturePlanningControl>,
) -> Result<()> {
    let mut args = vec![
        OsString::from("cat-file"),
        OsString::from("commit"),
        OsString::from(head),
    ];
    let commit = run_git_bounded_in(git_directory, &mut args, MAX_GIT_COMMIT_BYTES, control)?;
    let headers = commit
        .split(|byte| *byte == b'\n')
        .take_while(|line| !line.is_empty());
    let first_parent = headers
        .filter_map(|line| line.strip_prefix(b"parent "))
        .next()
        .context("capture git_range head commit has no first parent")?;
    if first_parent != base.as_bytes() {
        bail!("capture git_range base is not the selected first parent");
    }
    Ok(())
}

fn run_git_bounded_in(
    git_directory: &ResolvedGitDirectory,
    args: &mut [OsString],
    max_stdout: usize,
    control: Option<&CapturePlanningControl>,
) -> Result<Vec<u8>> {
    check_planning_control(control)?;
    validate_git_filesystem_identity(git_directory)?;
    validate_git_local_config_identity(git_directory)?;
    reject_git_ambient_repository_files(git_directory)?;
    let mut command = git_command(git_directory, args)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .context("failed to start bounded local Git object reader")?;
    let stdout = child
        .stdout
        .take()
        .context("failed to capture local Git object output")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture local Git diagnostic output")?;
    let stdout_overflow = Arc::new(AtomicBool::new(false));
    let stderr_overflow = Arc::new(AtomicBool::new(false));
    let (stdout_tx, stdout_rx) = mpsc::sync_channel(1);
    let (stderr_tx, stderr_rx) = mpsc::sync_channel(1);
    let stdout_reader =
        spawn_bounded_reader(stdout, max_stdout, Arc::clone(&stdout_overflow), stdout_tx);
    let stderr_reader = spawn_bounded_reader(
        stderr,
        MAX_GIT_STDERR_BYTES,
        Arc::clone(&stderr_overflow),
        stderr_tx,
    );

    let started = Instant::now();
    let status = loop {
        if let Err(error) = check_planning_control(control) {
            terminate_child(&mut child);
            join_reader(stdout_reader);
            join_reader(stderr_reader);
            return Err(error);
        }
        if stdout_overflow.load(Ordering::Acquire) || stderr_overflow.load(Ordering::Acquire) {
            terminate_child(&mut child);
            join_reader(stdout_reader);
            join_reader(stderr_reader);
            bail!("capture local Git object output exceeds the configured limit");
        }
        if started.elapsed() >= GIT_PROCESS_TIMEOUT {
            terminate_child(&mut child);
            join_reader(stdout_reader);
            join_reader(stderr_reader);
            bail!("capture local Git object read timed out");
        }
        if let Some(status) = child
            .try_wait()
            .context("failed to monitor local Git object reader")?
        {
            break status;
        }
        thread::sleep(GIT_POLL_INTERVAL);
    };
    let stdout = stdout_rx
        .recv()
        .context("failed to receive local Git object output")?
        .context("failed to read local Git object output")?;
    let _stderr = stderr_rx
        .recv()
        .context("failed to receive local Git diagnostic output")?
        .context("failed to read local Git diagnostic output")?;
    join_reader(stdout_reader);
    join_reader(stderr_reader);
    if stdout_overflow.load(Ordering::Acquire) || stderr_overflow.load(Ordering::Acquire) {
        bail!("capture local Git object output exceeds the configured limit");
    }
    validate_git_filesystem_identity(git_directory)?;
    validate_git_local_config_identity(git_directory)?;
    reject_git_ambient_repository_files(git_directory)?;
    require_git_success(status)?;
    Ok(stdout)
}

fn git_command(git_directory: &ResolvedGitDirectory, args: &[OsString]) -> Result<Command> {
    let mut command = hermetic_git_command()?;
    command
        .arg("--no-pager")
        .arg("--git-dir")
        .arg(&git_directory.git_dir)
        .arg("--literal-pathspecs")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("color.ui=false")
        .arg("-c")
        .arg("core.quotePath=true")
        .arg("-c")
        .arg("core.bigFileThreshold=16m")
        .arg("-c")
        .arg(if cfg!(windows) {
            "core.attributesFile=NUL"
        } else {
            "core.attributesFile=/dev/null"
        })
        .arg("-c")
        .arg("diff.external=")
        .arg("-c")
        .arg("diff.orderFile=")
        .arg("-c")
        .arg("diff.interHunkContext=0")
        .arg("-c")
        .arg("diff.noprefix=false")
        .arg("-c")
        .arg("diff.mnemonicPrefix=false")
        .arg("-c")
        .arg("diff.srcPrefix=a/")
        .arg("-c")
        .arg("diff.dstPrefix=b/")
        .arg("-c")
        .arg("diff.linePrefix=")
        .arg("-c")
        .arg("diff.outputIndicatorNew=+")
        .arg("-c")
        .arg("diff.outputIndicatorOld=-")
        .arg("-c")
        .arg("diff.outputIndicatorContext= ")
        .arg("-c")
        .arg("diff.suppressBlankEmpty=false")
        .arg("-c")
        .arg("submodule.recurse=false")
        .arg("-c")
        .arg("trace2.normalTarget=0")
        .arg("-c")
        .arg("trace2.perfTarget=0")
        .arg("-c")
        .arg("trace2.eventTarget=0")
        .args(args.iter())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_SYSTEM",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_CONFIG_COUNT", "0")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("TZ", "UTC");
    if let Some(source) = &git_directory.attribute_source {
        command.env("GIT_ATTR_SOURCE", source);
    }
    Ok(command)
}

fn hermetic_git_command() -> Result<Command> {
    let path = std::env::var_os("PATH").context("capture Git requires an explicit PATH")?;
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("PATH", path)
        .env("HOME", if cfg!(windows) { "NUL" } else { "/nonexistent" })
        .env(
            "XDG_CONFIG_HOME",
            if cfg!(windows) { "NUL" } else { "/nonexistent" },
        )
        .env("TMPDIR", if cfg!(windows) { "NUL" } else { "/nonexistent" })
        .env("TMP", if cfg!(windows) { "NUL" } else { "/nonexistent" })
        .env("TEMP", if cfg!(windows) { "NUL" } else { "/nonexistent" })
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("TZ", "UTC");
    Ok(command)
}

fn resolve_git_dir(
    paths: &MemoryPaths,
    control: Option<&CapturePlanningControl>,
) -> Result<ResolvedGitDirectory> {
    check_planning_control(control)?;
    let git_marker = paths.project_root.join(".git");
    let metadata = fs::symlink_metadata(&git_marker)
        .context("capture git_range requires an explicit local Git repository")?;
    if metadata.file_type().is_symlink() {
        bail!("capture git_range requires an explicit local Git repository");
    }
    if metadata.is_dir() {
        let git_dir = git_marker
            .canonicalize()
            .context("failed to resolve capture Git directory")?;
        require_git_object_directory(&git_dir)?;
        return resolved_git_directory(git_dir.clone(), &git_dir, b"directory");
    }
    if !metadata.is_file() || metadata.len() > MAX_GITFILE_BYTES {
        bail!("capture git_range requires a bounded regular Git directory pointer");
    }

    let bytes = read_project_bytes(paths, ".git", MAX_GITFILE_BYTES, control)?;
    let pointer = parse_git_dir_pointer(&bytes)?;
    let unresolved = if pointer.is_absolute() {
        pointer
    } else {
        paths.project_root.join(pointer)
    };
    let target_metadata = fs::symlink_metadata(&unresolved)
        .context("capture Git directory pointer target is unavailable")?;
    if target_metadata.file_type().is_symlink() || !target_metadata.is_dir() {
        bail!("capture Git directory pointer must name a nonsymlink directory");
    }
    let git_dir = unresolved
        .canonicalize()
        .context("failed to resolve capture Git directory pointer")?;
    resolve_linked_worktree_git_dir(paths, git_dir, &bytes)
}

fn resolve_linked_worktree_git_dir(
    paths: &MemoryPaths,
    git_dir: PathBuf,
    marker_bytes: &[u8],
) -> Result<ResolvedGitDirectory> {
    let commondir_bytes = read_bounded_nonsymlink_file(
        &git_dir.join("commondir"),
        MAX_GITFILE_BYTES,
        "capture linked-worktree commondir",
    )?;
    let commondir_pointer = parse_single_git_path(&commondir_bytes, "commondir")?;
    if commondir_pointer.is_absolute() {
        bail!("capture linked-worktree commondir must be relative");
    }
    let common_unresolved = normalize_absolute_path(&git_dir.join(commondir_pointer));
    let common_metadata = fs::symlink_metadata(&common_unresolved)
        .context("capture linked-worktree common directory is unavailable")?;
    if common_metadata.file_type().is_symlink() || !common_metadata.is_dir() {
        bail!("capture linked-worktree commondir must name a nonsymlink directory");
    }
    let common_dir = common_unresolved
        .canonicalize()
        .context("failed to resolve capture linked-worktree common directory")?;
    require_git_object_directory(&common_dir)?;

    let worktrees = common_dir.join("worktrees");
    let worktrees_metadata = fs::symlink_metadata(&worktrees)
        .context("capture linked-worktree registry is unavailable")?;
    if worktrees_metadata.file_type().is_symlink() || !worktrees_metadata.is_dir() {
        bail!("capture linked-worktree registry must be a nonsymlink directory");
    }
    let worktrees = worktrees
        .canonicalize()
        .context("failed to resolve capture linked-worktree registry")?;
    if git_dir.parent() != Some(worktrees.as_path())
        || git_dir.file_name().is_none()
        || git_dir.file_name() == Some(std::ffi::OsStr::new(""))
    {
        bail!("capture Git directory pointer is not a registered linked worktree");
    }

    let backref_bytes = read_bounded_nonsymlink_file(
        &git_dir.join("gitdir"),
        MAX_GITFILE_BYTES,
        "capture linked-worktree gitdir back-reference",
    )?;
    let backref = parse_single_git_path(&backref_bytes, "gitdir back-reference")?;
    let backref = if backref.is_absolute() {
        normalize_absolute_path(&backref)
    } else {
        normalize_absolute_path(&git_dir.join(backref))
    };
    let project_root = paths
        .project_root
        .canonicalize()
        .context("failed to resolve capture project root")?;
    let expected_backref = normalize_absolute_path(&project_root.join(".git"));
    if backref != expected_backref {
        bail!("capture linked-worktree gitdir back-reference does not name this worktree");
    }

    let mut material = Vec::new();
    append_identity_field(&mut material, marker_bytes);
    append_identity_field(&mut material, &commondir_bytes);
    append_identity_field(&mut material, &backref_bytes);
    resolved_git_directory(git_dir, &common_dir, &material)
}

fn require_git_object_directory(git_dir: &Path) -> Result<()> {
    let objects = git_dir.join("objects");
    let metadata = fs::symlink_metadata(&objects)
        .context("capture Git common directory has no local object database")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("capture Git object database must be a nonsymlink directory");
    }
    Ok(())
}

fn resolved_git_directory(
    git_dir: PathBuf,
    common_dir: &Path,
    additional_material: &[u8],
) -> Result<ResolvedGitDirectory> {
    let identity_nodes = capture_git_filesystem_identity(&git_dir, common_dir)?;
    let local_config_identity = read_git_local_config_identity(common_dir)?;
    let local_config_policy_input = CapturePolicyInputSnapshot {
        path: ".git/config".to_owned(),
        source_content_hash: local_config_identity.source_content_hash.clone(),
        engine_version: "memzoi/git-local-config-v1".to_owned(),
    };
    let mut identity = Vec::new();
    append_path_identity(&mut identity, &git_dir);
    append_path_identity(&mut identity, common_dir);
    append_identity_field(&mut identity, additional_material);
    for node in &identity_nodes {
        append_path_identity(&mut identity, &node.path);
        append_identity_field(&mut identity, &node.device.to_be_bytes());
        append_identity_field(&mut identity, &node.inode.to_be_bytes());
    }
    Ok(ResolvedGitDirectory {
        git_dir,
        common_dir: common_dir.to_owned(),
        identity_nodes,
        attribute_source: None,
        local_config_identity,
        local_config_policy_input,
        policy_input: CapturePolicyInputSnapshot {
            path: ".git".to_owned(),
            source_content_hash: domain_hash(GIT_REPOSITORY_IDENTITY_VERSION, &identity),
            engine_version: GIT_REPOSITORY_IDENTITY_VERSION.to_owned(),
        },
    })
}

#[cfg(unix)]
fn capture_git_filesystem_identity(
    git_dir: &Path,
    common_dir: &Path,
) -> Result<Vec<GitFilesystemIdentity>> {
    use std::os::unix::fs::MetadataExt;

    let mut paths = BTreeSet::new();
    paths.insert(git_dir.to_owned());
    paths.insert(common_dir.to_owned());
    paths.insert(common_dir.join("objects"));
    let mut identities = Vec::new();
    for path in paths {
        let metadata =
            fs::symlink_metadata(&path).context("capture Git identity path is unavailable")?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("capture Git identity path must be a nonsymlink directory");
        }
        identities.push(GitFilesystemIdentity {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        });
    }
    Ok(identities)
}

#[cfg(not(unix))]
fn capture_git_filesystem_identity(
    _git_dir: &Path,
    _common_dir: &Path,
) -> Result<Vec<GitFilesystemIdentity>> {
    bail!("capture Git repository filesystem identity is unavailable on this platform")
}

fn validate_git_filesystem_identity(git_directory: &ResolvedGitDirectory) -> Result<()> {
    let current =
        capture_git_filesystem_identity(&git_directory.git_dir, &git_directory.common_dir)?;
    if current != git_directory.identity_nodes {
        bail!("capture Git repository identity changed during object access");
    }
    Ok(())
}

fn read_git_local_config_identity(common_dir: &Path) -> Result<GitLocalConfigIdentity> {
    let path = common_dir.join("config");
    let bytes = read_bounded_nonsymlink_file(
        &path,
        CAPTURE_MAX_GIT_POLICY_FILE_BYTES,
        "capture Git local config",
    )?;
    reject_prohibited_policy_bytes(&bytes, "Git local config")?;
    validate_git_local_config(&path, &bytes)?;
    #[cfg(unix)]
    let metadata = {
        use std::os::unix::fs::MetadataExt;

        let metadata = fs::symlink_metadata(&path)
            .context("capture Git local config is unavailable after validation")?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.nlink() != 1 {
            bail!("capture Git local config changed during validation");
        }
        metadata
    };
    Ok(GitLocalConfigIdentity {
        path,
        source_content_hash: content_hash(&bytes),
        #[cfg(unix)]
        device: {
            use std::os::unix::fs::MetadataExt;
            metadata.dev()
        },
        #[cfg(unix)]
        inode: {
            use std::os::unix::fs::MetadataExt;
            metadata.ino()
        },
        #[cfg(unix)]
        ctime: {
            use std::os::unix::fs::MetadataExt;
            metadata.ctime()
        },
        #[cfg(unix)]
        ctime_nsec: {
            use std::os::unix::fs::MetadataExt;
            metadata.ctime_nsec()
        },
    })
}

fn validate_git_local_config(path: &Path, bytes: &[u8]) -> Result<()> {
    std::str::from_utf8(bytes).context("capture Git local config must be UTF-8")?;
    let mut command = hermetic_git_command()?;
    let output = command
        .arg("config")
        .arg("--file")
        .arg(path)
        .arg("--no-includes")
        .arg("--null")
        .arg("--list")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .context("failed to parse bounded Git local config")?;
    if !output.status.success()
        || output.stdout.len() > CAPTURE_MAX_GIT_POLICY_FILE_BYTES as usize * 2
    {
        bail!("capture Git local config is malformed or exceeds the parse limit");
    }
    for record in output.stdout.split(|byte| *byte == b'\0') {
        if record.is_empty() {
            continue;
        }
        let separator = record
            .iter()
            .position(|byte| *byte == b'\n')
            .context("capture Git local config parse output is malformed")?;
        let key = std::str::from_utf8(&record[..separator])?.to_ascii_lowercase();
        if key == "include.path" || key.starts_with("includeif.") && key.ends_with(".path") {
            bail!("capture Git local config cannot include external config files");
        }
        if key == "extensions.worktreeconfig" {
            bail!("capture Git local config cannot enable worktree-specific config");
        }
    }
    Ok(())
}

fn reject_prohibited_policy_bytes(bytes: &[u8], label: &str) -> Result<()> {
    if prohibited_finding(bytes).is_some() {
        bail!("capture {label} contains prohibited content");
    }
    Ok(())
}

fn validate_git_local_config_identity(git_directory: &ResolvedGitDirectory) -> Result<()> {
    let current = read_git_local_config_identity(&git_directory.common_dir)?;
    if current != git_directory.local_config_identity {
        bail!("capture Git local config changed during object access");
    }
    Ok(())
}

fn append_path_identity(output: &mut Vec<u8>, path: &Path) {
    append_identity_field(output, path.to_string_lossy().as_bytes());
}

fn append_identity_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn read_bounded_nonsymlink_file(path: &Path, limit: u64, label: &str) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("{label} is unavailable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > limit {
        bail!("{label} must be a bounded nonsymlink regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if metadata.nlink() != 1 {
            bail!("{label} must not be hard-linked");
        }
    }
    let mut file = open_nonsymlink_file(path).with_context(|| format!("failed to open {label}"))?;
    let before = file
        .metadata()
        .with_context(|| format!("failed to inspect {label}"))?;
    if !before.is_file()
        || before.len() != metadata.len()
        || before.modified().ok() != metadata.modified().ok()
        || before.len() > limit
    {
        bail!("{label} changed while it was opened");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if before.nlink() != 1
            || metadata.dev() != before.dev()
            || metadata.ino() != before.ino()
            || metadata.ctime() != before.ctime()
            || metadata.ctime_nsec() != before.ctime_nsec()
        {
            bail!("{label} metadata changed while it was opened");
        }
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    file.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("{label} exceeds the configured limit");
    }
    let after = file
        .metadata()
        .with_context(|| format!("failed to re-inspect {label}"))?;
    if bytes.len() as u64 != before.len()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        bail!("{label} changed while it was read");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if after.nlink() != 1
            || before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            bail!("{label} metadata changed while it was read");
        }
    }
    Ok(bytes)
}

#[cfg(unix)]
fn open_nonsymlink_file(path: &Path) -> Result<fs::File> {
    use rustix::fs::{Mode, OFlags, open};

    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    Ok(fs::File::from(descriptor))
}

#[cfg(not(unix))]
fn open_nonsymlink_file(path: &Path) -> Result<fs::File> {
    Ok(fs::File::open(path)?)
}

fn parse_git_dir_pointer(bytes: &[u8]) -> Result<PathBuf> {
    let line = single_git_path_line(bytes, "Git directory pointer")?;
    let value = line
        .strip_prefix(b"gitdir: ")
        .context("capture Git directory pointer has invalid syntax")?;
    parse_git_path_value(value, "Git directory pointer")
}

fn parse_single_git_path(bytes: &[u8], label: &str) -> Result<PathBuf> {
    let line = single_git_path_line(bytes, label)?;
    parse_git_path_value(line, label)
}

fn single_git_path_line<'a>(bytes: &'a [u8], label: &str) -> Result<&'a [u8]> {
    let line = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.contains(&b'\n') || line.contains(&b'\r') {
        bail!("capture {label} must contain exactly one line");
    }
    Ok(line)
}

fn parse_git_path_value(value: &[u8], label: &str) -> Result<PathBuf> {
    let value = std::str::from_utf8(value)
        .with_context(|| format!("capture {label} must be valid UTF-8"))?;
    if value.is_empty() || value.contains('\0') || value.chars().any(char::is_control) {
        bail!("capture {label} has an unsafe path");
    }
    Ok(PathBuf::from(value))
}

#[cfg(test)]
fn run_git_bounded(
    paths: &MemoryPaths,
    args: &mut [OsString],
    max_stdout: usize,
    control: Option<&CapturePlanningControl>,
) -> Result<Vec<u8>> {
    let git_directory = resolve_git_dir(paths, control)?;
    run_git_bounded_in(&git_directory, args, max_stdout, control)
}

fn spawn_bounded_reader<R: Read + Send + 'static>(
    mut reader: R,
    limit: usize,
    overflow: Arc<AtomicBool>,
    sender: mpsc::SyncSender<std::io::Result<Vec<u8>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let result = (|| -> std::io::Result<Vec<u8>> {
            let mut output = Vec::new();
            let mut buffer = [0u8; 8192];
            loop {
                let count = reader.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                let remaining = limit.saturating_sub(output.len());
                output.extend_from_slice(&buffer[..count.min(remaining)]);
                if count > remaining {
                    overflow.store(true, Ordering::Release);
                }
            }
            Ok(output)
        })();
        let _ = sender.send(result);
    })
}

fn terminate_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn join_reader(reader: thread::JoinHandle<()>) {
    let _ = reader.join();
}

fn require_git_success(status: ExitStatus) -> Result<()> {
    if !status.success() {
        bail!("capture local Git object read failed safely");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{env, fs, process::Command};

    use tempfile::TempDir;

    use super::*;
    use crate::capture::CaptureSourceInputs;

    fn paths() -> (TempDir, MemoryPaths) {
        let temp = tempfile::tempdir().expect("create capture source fixture");
        let project = temp.path().join("repo");
        fs::create_dir_all(&project).expect("create capture project");
        let paths = MemoryPaths::with_runtime_home(project, temp.path().join("runtime"));
        (temp, paths)
    }

    fn source(locator: CaptureSourceLocator, media_type: &str) -> CaptureSourceRequest {
        CaptureSourceRequest {
            source_id: "fixture".to_owned(),
            locator,
            media_type: media_type.to_owned(),
            git: None,
        }
    }

    fn git_range_source(base: &str, head: &str) -> CaptureSourceRequest {
        source(
            CaptureSourceLocator::GitRange {
                repository: ".".to_owned(),
                base: format!("sha1:{base}"),
                head: format!("sha1:{head}"),
                merge_parent: "base_to_head".to_owned(),
                rename_detection: false,
                diff_format: "git-unified-v1".to_owned(),
            },
            "text/x-diff",
        )
    }

    #[test]
    fn supplied_bytes_require_exactly_one_matching_bounded_input() -> Result<()> {
        let (_temp, paths) = paths();
        let bytes = b"diff --git a/a b/a\n".to_vec();
        let request = source(
            CaptureSourceLocator::SuppliedBytes {
                display_name: "change.diff".to_owned(),
                media_type: "text/x-diff".to_owned(),
                byte_length: bytes.len() as u64,
                source_content_hash: content_hash(&bytes),
            },
            "text/x-diff",
        );
        assert!(load_capture_source(&paths, &request, &CaptureSourceInputs::new(), None).is_err());

        let mut inputs = CaptureSourceInputs::new();
        inputs.insert_supplied_bytes("fixture", bytes.clone())?;
        let loaded = load_capture_source(&paths, &request, &inputs, None)?;
        assert_eq!(loaded.documents[0].bytes, bytes);

        let mut wrong = CaptureSourceInputs::new();
        wrong.insert_supplied_bytes("fixture", b"different".to_vec())?;
        assert!(load_capture_source(&paths, &request, &wrong, None).is_err());
        Ok(())
    }

    #[test]
    fn supplied_diff_does_not_discover_ambient_worktree_ignore_policy() -> Result<()> {
        let (_temp, paths) = paths();
        let bytes = b"diff --git a/docs/a.md b/docs/a.md\n".to_vec();
        let request = source(
            CaptureSourceLocator::SuppliedBytes {
                display_name: "change.diff".to_owned(),
                media_type: "text/x-diff".to_owned(),
                byte_length: bytes.len() as u64,
                source_content_hash: content_hash(&bytes),
            },
            "text/x-diff",
        );
        let mut inputs = CaptureSourceInputs::new();
        inputs.insert_supplied_bytes("fixture", bytes)?;
        fs::write(paths.project_root.join(".gitignore"), "docs/a.md\n")?;
        let first = load_capture_source(&paths, &request, &inputs, None)?;
        fs::write(paths.project_root.join(".gitignore"), "*.md\n")?;
        let second = load_capture_source(&paths, &request, &inputs, None)?;
        assert!(first.snapshot.policy_inputs.is_empty());
        assert_eq!(first.snapshot, second.snapshot);
        assert_eq!(first.documents[0].bytes, second.documents[0].bytes);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn directory_members_and_ignore_policy_are_sorted_and_snapshotted() -> Result<()> {
        let (_temp, paths) = paths();
        fs::create_dir_all(paths.project_root.join("docs/adr/nested"))?;
        fs::write(paths.project_root.join(".gitignore"), "ignored.md\n")?;
        fs::write(paths.project_root.join("docs/adr/z.md"), "# Z\n")?;
        fs::write(paths.project_root.join("docs/adr/a.md"), "# A\n")?;
        fs::write(
            paths.project_root.join("docs/adr/ignored.md"),
            "# ignored\n",
        )?;
        fs::write(
            paths.project_root.join("docs/adr/nested/n.md"),
            "# nested\n",
        )?;
        let request = source(
            CaptureSourceLocator::ProjectDirectory {
                path: "docs/adr".to_owned(),
                recursive: false,
                ignore_policy: "git-v1".to_owned(),
                include: vec!["*.md".to_owned()],
            },
            "text/markdown",
        );
        let loaded = load_capture_source(&paths, &request, &CaptureSourceInputs::new(), None)?;
        let paths = loaded
            .snapshot
            .members
            .iter()
            .map(|member| member.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(paths, ["docs/adr/a.md", "docs/adr/z.md"]);
        assert_eq!(loaded.snapshot.policy_inputs.len(), 1);
        assert_eq!(loaded.snapshot.policy_inputs[0].path, ".gitignore");
        Ok(())
    }

    #[test]
    fn directory_ignore_policy_prohibited_bytes_fail_without_echo() -> Result<()> {
        let (_temp, paths) = paths();
        const CANARY: &str = "ghp_policycanarymustneverecho";
        let policy = format!("ignored.md\n# {CANARY}\n");
        fs::create_dir_all(paths.project_root.join("docs/adr"))?;
        fs::write(paths.project_root.join("docs/adr/a.md"), "# A\n")?;
        fs::write(paths.project_root.join(".gitignore"), &policy)?;
        let request = source(
            CaptureSourceLocator::ProjectDirectory {
                path: "docs/adr".to_owned(),
                recursive: false,
                ignore_policy: "git-v1".to_owned(),
                include: vec!["*.md".to_owned()],
            },
            "text/markdown",
        );
        let error = load_capture_source(&paths, &request, &CaptureSourceInputs::new(), None)
            .expect_err("prohibited ignore policy must fail before snapshotting");
        let rendered = error.to_string();
        assert!(!rendered.contains(CANARY));
        assert!(!rendered.contains(&content_hash(policy.as_bytes())));
        assert_eq!(
            fs::read_to_string(paths.project_root.join(".gitignore"))?,
            policy
        );
        Ok(())
    }

    #[test]
    fn git_range_reads_full_commit_objects_without_changing_git_state() -> Result<()> {
        let (_temp, paths) = paths();
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        run_fixture_git(&paths.project_root, &["init", "-q"])?;
        run_fixture_git(
            &paths.project_root,
            &["config", "user.email", "fixture@example.test"],
        )?;
        run_fixture_git(&paths.project_root, &["config", "user.name", "Fixture"])?;
        fs::write(paths.project_root.join("note.txt"), "before\n")?;
        run_fixture_git(&paths.project_root, &["add", "note.txt"])?;
        run_fixture_git(&paths.project_root, &["commit", "-qm", "base"])?;
        let base = fixture_git_stdout(&paths.project_root, &["rev-parse", "HEAD"])?;
        fs::write(paths.project_root.join("note.txt"), "after\n")?;
        run_fixture_git(&paths.project_root, &["commit", "-qam", "head"])?;
        let head = fixture_git_stdout(&paths.project_root, &["rev-parse", "HEAD"])?;
        let index_before = fs::read(paths.project_root.join(".git/index"))?;
        let request = CaptureSourceRequest {
            source_id: "fixture".to_owned(),
            locator: CaptureSourceLocator::GitRange {
                repository: ".".to_owned(),
                base: format!("sha1:{base}"),
                head: format!("sha1:{head}"),
                merge_parent: "base_to_head".to_owned(),
                rename_detection: false,
                diff_format: "git-unified-v1".to_owned(),
            },
            media_type: "text/x-diff".to_owned(),
            git: None,
        };
        let first = load_capture_source(&paths, &request, &CaptureSourceInputs::new(), None)?;
        fs::write(paths.project_root.join(".gitignore"), "note.txt\n")?;
        fs::write(
            paths.project_root.join(".gitattributes"),
            "note.txt -diff\n",
        )?;
        let second = load_capture_source(&paths, &request, &CaptureSourceInputs::new(), None)?;
        assert_eq!(first.snapshot, second.snapshot);
        assert_eq!(first.snapshot.policy_inputs.len(), 3);
        assert!(first.snapshot.policy_inputs.iter().any(|input| {
            input.path == ".git" && input.engine_version == "memzoi/git-repository-identity-v1"
        }));
        assert!(first.snapshot.policy_inputs.iter().any(|input| {
            input.path == ".git/config" && input.engine_version == "memzoi/git-local-config-v1"
        }));
        assert!(first.snapshot.policy_inputs.iter().any(|input| {
            input.path == ".git/renderer-version"
                && input
                    .engine_version
                    .starts_with("memzoi/git-unified-renderer-v1+git-")
        }));
        assert_eq!(first.documents[0].bytes, second.documents[0].bytes);
        assert!(String::from_utf8_lossy(&first.documents[0].bytes).contains("-before"));
        assert!(String::from_utf8_lossy(&first.documents[0].bytes).contains("+after"));
        assert_eq!(
            fs::read(paths.project_root.join(".git/index"))?,
            index_before
        );
        let mut oversized_args = vec![
            OsString::from("cat-file"),
            OsString::from("commit"),
            OsString::from(&head),
        ];
        assert!(run_git_bounded(&paths, &mut oversized_args, 1, None).is_err());
        Ok(())
    }

    #[test]
    fn git_range_resolves_a_linked_worktree_git_directory_pointer() -> Result<()> {
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let main = temp.path().join("main");
        let worktree = temp.path().join("linked");
        fs::create_dir_all(&main)?;
        run_fixture_git(&main, &["init", "-q"])?;
        run_fixture_git(&main, &["config", "user.email", "fixture@example.test"])?;
        run_fixture_git(&main, &["config", "user.name", "Fixture"])?;
        fs::write(main.join("note.txt"), "before\n")?;
        run_fixture_git(&main, &["add", "note.txt"])?;
        run_fixture_git(&main, &["commit", "-qm", "base"])?;
        let worktree_path = worktree
            .to_str()
            .context("worktree fixture path must be UTF-8")?;
        run_fixture_git(&main, &["worktree", "add", "-qb", "linked", worktree_path])?;
        let base = fixture_git_stdout(&worktree, &["rev-parse", "HEAD"])?;
        fs::write(worktree.join("note.txt"), "after\n")?;
        run_fixture_git(&worktree, &["commit", "-qam", "head"])?;
        let head = fixture_git_stdout(&worktree, &["rev-parse", "HEAD"])?;
        assert!(fs::symlink_metadata(worktree.join(".git"))?.is_file());

        let paths = MemoryPaths::with_runtime_home(worktree, temp.path().join("runtime"));
        let request = source(
            CaptureSourceLocator::GitRange {
                repository: ".".to_owned(),
                base: format!("sha1:{base}"),
                head: format!("sha1:{head}"),
                merge_parent: "base_to_head".to_owned(),
                rename_detection: false,
                diff_format: "git-unified-v1".to_owned(),
            },
            "text/x-diff",
        );
        let loaded = load_capture_source(&paths, &request, &CaptureSourceInputs::new(), None)?;
        let diff = String::from_utf8(loaded.documents[0].bytes.clone())?;
        assert!(diff.contains("-before"));
        assert!(diff.contains("+after"));
        Ok(())
    }

    #[test]
    fn git_directory_pointer_rejects_extra_lines() {
        assert!(parse_git_dir_pointer(b"gitdir: ../repo.git\nextra\n").is_err());
        assert!(parse_git_dir_pointer(b"gitdir: ../repo.git\n").is_ok());
    }

    #[test]
    fn git_directory_pointer_rejects_an_arbitrary_object_database() -> Result<()> {
        let (temp, paths) = paths();
        let arbitrary = temp.path().join("arbitrary.git");
        fs::create_dir_all(&arbitrary)?;
        fs::write(
            paths.project_root.join(".git"),
            format!("gitdir: {}\n", arbitrary.display()),
        )?;
        assert!(resolve_git_dir(&paths, None).is_err());
        Ok(())
    }

    #[test]
    fn linked_worktree_pointer_requires_exact_gitdir_back_reference() -> Result<()> {
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let main = temp.path().join("main");
        let worktree = temp.path().join("linked");
        fs::create_dir_all(&main)?;
        run_fixture_git(&main, &["init", "-q"])?;
        run_fixture_git(&main, &["config", "user.email", "fixture@example.test"])?;
        run_fixture_git(&main, &["config", "user.name", "Fixture"])?;
        fs::write(main.join("note.txt"), "before\n")?;
        run_fixture_git(&main, &["add", "note.txt"])?;
        run_fixture_git(&main, &["commit", "-qm", "base"])?;
        let worktree_path = worktree
            .to_str()
            .context("worktree fixture path must be UTF-8")?;
        run_fixture_git(&main, &["worktree", "add", "-qb", "linked", worktree_path])?;

        let marker = fs::read(worktree.join(".git"))?;
        let target = parse_git_dir_pointer(&marker)?;
        fs::write(
            target.join("gitdir"),
            main.join(".git").as_os_str().as_encoded_bytes(),
        )?;
        let paths = MemoryPaths::with_runtime_home(worktree, temp.path().join("runtime"));
        assert!(resolve_git_dir(&paths, None).is_err());
        Ok(())
    }

    #[test]
    fn every_capture_git_command_disables_lazy_fetch() -> Result<()> {
        let (_temp, paths) = paths();
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        run_fixture_git(&paths.project_root, &["init", "-q"])?;
        let mut git_directory = resolve_git_dir(&paths, None)?;
        let expected_attribute_source = "1".repeat(40);
        git_directory.attribute_source = Some(expected_attribute_source.clone());
        let command = git_command(&git_directory, &[OsString::from("version")])?;
        let lazy_fetch = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("GIT_NO_LAZY_FETCH"))
            .and_then(|(_, value)| value);
        assert_eq!(lazy_fetch, Some(std::ffi::OsStr::new("1")));
        let attribute_source = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("GIT_ATTR_SOURCE"))
            .and_then(|(_, value)| value);
        assert_eq!(
            attribute_source,
            Some(std::ffi::OsStr::new(&expected_attribute_source))
        );
        let args = command
            .get_args()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(
            args.iter()
                .any(|argument| argument == "core.quotePath=true")
        );
        assert!(args.iter().any(|argument| {
            argument == "core.attributesFile=/dev/null" || argument == "core.attributesFile=NUL"
        }));
        assert!(args.iter().any(|argument| argument == "diff.orderFile="));
        assert!(
            args.iter()
                .any(|argument| argument == "diff.interHunkContext=0")
        );
        for target in [
            "trace2.normalTarget=0",
            "trace2.perfTarget=0",
            "trace2.eventTarget=0",
        ] {
            assert!(args.iter().any(|argument| argument == target));
        }
        assert!(command.get_envs().all(|(key, _)| {
            let key = key.to_string_lossy();
            !key.starts_with("GIT_TRACE") && !key.starts_with("LD_") && !key.starts_with("DYLD_")
        }));
        Ok(())
    }

    #[test]
    fn git_local_config_rejects_includes_and_worktree_config() -> Result<()> {
        let (_temp, paths) = paths();
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        run_fixture_git(&paths.project_root, &["init", "-q"])?;
        let config_path = paths.project_root.join(".git/config");
        let original = fs::read_to_string(&config_path)?;
        fs::write(
            &config_path,
            format!("{original}\n[includeIf \"gitdir:/**\"]\n\tpath = ../outside-config\n"),
        )?;
        assert!(resolve_git_dir(&paths, None).is_err());

        fs::write(
            &config_path,
            format!("{original}\n[extensions]\n\tworktreeConfig = true\n"),
        )?;
        assert!(resolve_git_dir(&paths, None).is_err());
        Ok(())
    }

    #[test]
    fn git_local_config_prohibited_bytes_fail_without_echo() -> Result<()> {
        let (_temp, paths) = paths();
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        const CANARY: &str = "ghp_configcanarymustneverecho";
        run_fixture_git(&paths.project_root, &["init", "-q"])?;
        let config_path = paths.project_root.join(".git/config");
        let mut config = fs::read_to_string(&config_path)?;
        config.push_str(&format!("\n[fixture]\n\ttoken = {CANARY}\n"));
        fs::write(&config_path, &config)?;
        let error = resolve_git_dir(&paths, None)
            .expect_err("prohibited local config must fail before fingerprinting");
        let rendered = error.to_string();
        assert!(!rendered.contains(CANARY));
        assert!(!rendered.contains(&content_hash(config.as_bytes())));
        assert_eq!(fs::read_to_string(config_path)?, config);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn git_local_config_hard_links_are_rejected() -> Result<()> {
        let (temp, paths) = paths();
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        run_fixture_git(&paths.project_root, &["init", "-q"])?;
        let config_path = paths.project_root.join(".git/config");
        let linked = temp.path().join("linked-config");
        fs::copy(&config_path, &linked)?;
        fs::remove_file(&config_path)?;
        fs::hard_link(&linked, &config_path)?;
        assert!(resolve_git_dir(&paths, None).is_err());
        Ok(())
    }

    #[test]
    fn git_range_ignores_local_diff_order_file() -> Result<()> {
        let (temp, paths) = paths();
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        run_fixture_git(&paths.project_root, &["init", "-q"])?;
        run_fixture_git(
            &paths.project_root,
            &["config", "user.email", "fixture@example.test"],
        )?;
        run_fixture_git(&paths.project_root, &["config", "user.name", "Fixture"])?;
        fs::write(paths.project_root.join("a.md"), "before a\n")?;
        fs::write(paths.project_root.join("b.md"), "before b\n")?;
        run_fixture_git(&paths.project_root, &["add", "a.md", "b.md"])?;
        run_fixture_git(&paths.project_root, &["commit", "-qm", "base"])?;
        let base = fixture_git_stdout(&paths.project_root, &["rev-parse", "HEAD"])?;
        fs::write(paths.project_root.join("a.md"), "after a\n")?;
        fs::write(paths.project_root.join("b.md"), "after b\n")?;
        run_fixture_git(&paths.project_root, &["commit", "-qam", "head"])?;
        let head = fixture_git_stdout(&paths.project_root, &["rev-parse", "HEAD"])?;
        let order_file = temp.path().join("adversarial-order");
        fs::write(&order_file, "b.md\na.md\n")?;
        run_fixture_git(
            &paths.project_root,
            &[
                "config",
                "diff.orderFile",
                order_file
                    .to_str()
                    .context("order fixture path must be UTF-8")?,
            ],
        )?;
        let request = git_range_source(&base, &head);
        let first = load_capture_source(&paths, &request, &CaptureSourceInputs::new(), None)?;
        fs::remove_file(&order_file)?;
        let second = load_capture_source(&paths, &request, &CaptureSourceInputs::new(), None)?;
        assert_eq!(first.snapshot, second.snapshot);
        assert_eq!(first.documents[0].bytes, second.documents[0].bytes);
        let diff = std::str::from_utf8(&first.documents[0].bytes)?;
        assert!(diff.find("diff --git a/a.md b/a.md") < diff.find("diff --git a/b.md b/b.md"));
        Ok(())
    }

    #[test]
    fn git_commands_do_not_inherit_trace_environment() -> Result<()> {
        const CHILD: &str = "MEMZOI_GIT_TRACE_TEST_CHILD";
        const ROOT: &str = "MEMZOI_GIT_TRACE_TEST_ROOT";
        const TRACE: &str = "MEMZOI_GIT_TRACE_TEST_SENTINEL";
        const TRACE2: &str = "MEMZOI_GIT_TRACE2_TEST_SENTINEL";
        if env::var_os(CHILD).is_some() {
            let root = PathBuf::from(env::var_os(ROOT).context("trace child root is missing")?);
            let trace = PathBuf::from(env::var_os(TRACE).context("trace sentinel is missing")?);
            let trace2 = PathBuf::from(env::var_os(TRACE2).context("trace2 sentinel is missing")?);
            let project = root.join("repo");
            fs::create_dir_all(&project)?;
            run_fixture_git(&project, &["init", "-q"])?;
            run_fixture_git(&project, &["config", "user.email", "fixture@example.test"])?;
            run_fixture_git(&project, &["config", "user.name", "Fixture"])?;
            fs::write(project.join("note.md"), "before\n")?;
            run_fixture_git(&project, &["add", "note.md"])?;
            run_fixture_git(&project, &["commit", "-qm", "base"])?;
            let base = fixture_git_stdout(&project, &["rev-parse", "HEAD"])?;
            fs::write(project.join("note.md"), "after\n")?;
            run_fixture_git(&project, &["commit", "-qam", "head"])?;
            let head = fixture_git_stdout(&project, &["rev-parse", "HEAD"])?;
            for sentinel in [&trace, &trace2] {
                if sentinel.exists() {
                    fs::remove_file(sentinel)?;
                }
            }
            let paths = MemoryPaths::with_runtime_home(project, root.join("runtime"));
            load_capture_source(
                &paths,
                &git_range_source(&base, &head),
                &CaptureSourceInputs::new(),
                None,
            )?;
            assert!(!trace.exists());
            assert!(!trace2.exists());
            return Ok(());
        }
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let trace = temp.path().join("git-trace.log");
        let trace2 = temp.path().join("git-trace2.json");
        let output = Command::new(env::current_exe()?)
            .arg("--exact")
            .arg("capture::sources::tests::git_commands_do_not_inherit_trace_environment")
            .arg("--nocapture")
            .env(CHILD, "1")
            .env(ROOT, temp.path())
            .env(TRACE, &trace)
            .env(TRACE2, &trace2)
            .env("GIT_TRACE", &trace)
            .env("GIT_TRACE2_EVENT", &trace2)
            .output()?;
        assert!(
            output.status.success(),
            "trace child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!trace.exists());
        assert!(!trace2.exists());
        Ok(())
    }

    #[test]
    fn resolved_git_identity_rejects_same_path_repository_replacement() -> Result<()> {
        let (_temp, paths) = paths();
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        run_fixture_git(&paths.project_root, &["init", "-q"])?;
        let resolved = resolve_git_dir(&paths, None)?;
        fs::rename(
            paths.project_root.join(".git"),
            paths.project_root.join(".git-original"),
        )?;
        run_fixture_git(&paths.project_root, &["init", "-q"])?;
        let mut args = vec![OsString::from("version")];
        assert!(run_git_bounded_in(&resolved, &mut args, 256, None).is_err());
        Ok(())
    }

    #[test]
    fn git_range_rejects_ambient_info_attributes() -> Result<()> {
        let (_temp, paths) = paths();
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        run_fixture_git(&paths.project_root, &["init", "-q"])?;
        fs::write(
            paths.project_root.join(".git/info/attributes"),
            "*.md -diff\n",
        )?;
        let resolved = resolve_git_dir(&paths, None)?;
        assert!(reject_git_ambient_repository_files(&resolved).is_err());
        Ok(())
    }

    #[test]
    fn git_range_uses_named_head_nested_ignore_policy_with_negation() -> Result<()> {
        let (_temp, paths) = paths();
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        run_fixture_git(&paths.project_root, &["init", "-q"])?;
        run_fixture_git(
            &paths.project_root,
            &["config", "user.email", "fixture@example.test"],
        )?;
        run_fixture_git(&paths.project_root, &["config", "user.name", "Fixture"])?;
        fs::create_dir_all(paths.project_root.join("docs"))?;
        fs::write(paths.project_root.join(".gitignore"), "docs/*\n")?;
        fs::write(paths.project_root.join("docs/.gitignore"), "!keep.md\n")?;
        fs::write(paths.project_root.join("docs/keep.md"), "before\n")?;
        run_fixture_git(
            &paths.project_root,
            &["add", "-f", ".gitignore", "docs/.gitignore", "docs/keep.md"],
        )?;
        run_fixture_git(&paths.project_root, &["commit", "-qm", "base"])?;
        let base = fixture_git_stdout(&paths.project_root, &["rev-parse", "HEAD"])?;
        fs::write(paths.project_root.join("docs/keep.md"), "after\n")?;
        run_fixture_git(&paths.project_root, &["commit", "-qam", "head"])?;
        let head = fixture_git_stdout(&paths.project_root, &["rev-parse", "HEAD"])?;
        let request = source(
            CaptureSourceLocator::GitRange {
                repository: ".".to_owned(),
                base: format!("sha1:{base}"),
                head: format!("sha1:{head}"),
                merge_parent: "base_to_head".to_owned(),
                rename_detection: false,
                diff_format: "git-unified-v1".to_owned(),
            },
            "text/x-diff",
        );
        let loaded = load_capture_source(&paths, &request, &CaptureSourceInputs::new(), None)?;
        assert!(
            loaded
                .snapshot
                .policy_inputs
                .iter()
                .any(|input| input.path == "docs/.gitignore")
        );
        Ok(())
    }

    #[test]
    fn git_range_head_ignore_policy_prohibited_bytes_fail_without_echo() -> Result<()> {
        let (_temp, paths) = paths();
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        const CANARY: &str = "ghp_gitpolicycanarymustneverecho";
        let policy = format!("other.txt\n# {CANARY}\n");
        run_fixture_git(&paths.project_root, &["init", "-q"])?;
        run_fixture_git(
            &paths.project_root,
            &["config", "user.email", "fixture@example.test"],
        )?;
        run_fixture_git(&paths.project_root, &["config", "user.name", "Fixture"])?;
        fs::write(paths.project_root.join(".gitignore"), &policy)?;
        fs::write(paths.project_root.join("note.txt"), "before\n")?;
        run_fixture_git(&paths.project_root, &["add", ".gitignore", "note.txt"])?;
        run_fixture_git(&paths.project_root, &["commit", "-qm", "base"])?;
        let base = fixture_git_stdout(&paths.project_root, &["rev-parse", "HEAD"])?;
        fs::write(paths.project_root.join("note.txt"), "after\n")?;
        run_fixture_git(&paths.project_root, &["commit", "-qam", "head"])?;
        let head = fixture_git_stdout(&paths.project_root, &["rev-parse", "HEAD"])?;

        let error = load_capture_source(
            &paths,
            &git_range_source(&base, &head),
            &CaptureSourceInputs::new(),
            None,
        )
        .expect_err("prohibited head-tree ignore policy must fail before snapshotting");
        let rendered = error.to_string();
        assert!(!rendered.contains(CANARY));
        assert!(!rendered.contains(&content_hash(policy.as_bytes())));
        assert_eq!(
            fs::read_to_string(paths.project_root.join(".gitignore"))?,
            policy
        );
        Ok(())
    }

    #[test]
    fn git_range_rejects_path_ignored_by_named_head() -> Result<()> {
        let (_temp, paths) = paths();
        if Command::new("git").arg("--version").output().is_err() {
            return Ok(());
        }
        run_fixture_git(&paths.project_root, &["init", "-q"])?;
        run_fixture_git(
            &paths.project_root,
            &["config", "user.email", "fixture@example.test"],
        )?;
        run_fixture_git(&paths.project_root, &["config", "user.name", "Fixture"])?;
        fs::write(paths.project_root.join(".gitignore"), "note.txt\n")?;
        fs::write(paths.project_root.join("note.txt"), "before\n")?;
        run_fixture_git(
            &paths.project_root,
            &["add", "-f", ".gitignore", "note.txt"],
        )?;
        run_fixture_git(&paths.project_root, &["commit", "-qm", "base"])?;
        let base = fixture_git_stdout(&paths.project_root, &["rev-parse", "HEAD"])?;
        fs::write(paths.project_root.join("note.txt"), "after\n")?;
        run_fixture_git(&paths.project_root, &["commit", "-qam", "head"])?;
        let head = fixture_git_stdout(&paths.project_root, &["rev-parse", "HEAD"])?;
        let request = source(
            CaptureSourceLocator::GitRange {
                repository: ".".to_owned(),
                base: format!("sha1:{base}"),
                head: format!("sha1:{head}"),
                merge_parent: "base_to_head".to_owned(),
                rename_detection: false,
                diff_format: "git-unified-v1".to_owned(),
            },
            "text/x-diff",
        );
        assert!(load_capture_source(&paths, &request, &CaptureSourceInputs::new(), None).is_err());
        Ok(())
    }

    fn run_fixture_git(root: &Path, args: &[&str]) -> Result<()> {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()?;
        if !status.success() {
            bail!("failed to create Git source fixture");
        }
        Ok(())
    }

    fn fixture_git_stdout(root: &Path, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()?;
        if !output.status.success() {
            bail!("failed to inspect Git source fixture");
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }
}
