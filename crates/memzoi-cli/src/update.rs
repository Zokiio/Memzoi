use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use semver::Version;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::output::print_json;

const DOWNLOAD_BASE: &str = "https://github.com/Zokiio/Memzoi/releases/download";
const RELEASE_API_BASE: &str = "https://api.github.com/repos/Zokiio/Memzoi/releases";
const REPO_URL: &str = "https://github.com/Zokiio/Memzoi.git";
const INSTALL_SH_URL: &str =
    "https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.sh";
const INSTALL_PS1_URL: &str =
    "https://raw.githubusercontent.com/Zokiio/Memzoi/main/scripts/install.ps1";

#[cfg(windows)]
const MEMZOI_BIN: &str = "memzoi.exe";
#[cfg(not(windows))]
const MEMZOI_BIN: &str = "memzoi";

#[cfg(windows)]
const MEMZOI_MCP_BIN: &str = "memzoi-mcp.exe";
#[cfg(not(windows))]
const MEMZOI_MCP_BIN: &str = "memzoi-mcp";

pub(crate) fn update_command(check_only: bool, reference: &str, as_json: bool) -> Result<()> {
    let report = run_update(UpdateOptions {
        check_only,
        reference,
    });

    if as_json {
        print_json(&report.to_json())?;
    } else {
        report.print_human();
    }

    if report.status.is_failure() {
        bail!(
            "{}",
            report.message.as_deref().unwrap_or(report.status.as_str())
        );
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct UpdateOptions<'a> {
    check_only: bool,
    reference: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateStatus {
    UpToDate,
    UpdateAvailable,
    Updated,
    Unsupported,
    InvalidRef,
    DownloadFailed,
    ChecksumMismatch,
    RollbackFailed,
}

impl UpdateStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::UpToDate => "up_to_date",
            Self::UpdateAvailable => "update_available",
            Self::Updated => "updated",
            Self::Unsupported => "unsupported",
            Self::InvalidRef => "invalid_ref",
            Self::DownloadFailed => "download_failed",
            Self::ChecksumMismatch => "checksum_mismatch",
            Self::RollbackFailed => "rollback_failed",
        }
    }

    fn is_failure(self) -> bool {
        matches!(
            self,
            Self::Unsupported
                | Self::InvalidRef
                | Self::DownloadFailed
                | Self::ChecksumMismatch
                | Self::RollbackFailed
        )
    }
}

#[derive(Debug, Clone)]
struct UpdateReport {
    status: UpdateStatus,
    current_version: Version,
    target_version: Option<Version>,
    target_ref: Option<String>,
    check_only: bool,
    updated: bool,
    apply_supported: bool,
    install_dir: Option<PathBuf>,
    manual_command: Option<String>,
    message: Option<String>,
}

impl UpdateReport {
    fn to_json(&self) -> Value {
        json!({
            "status": self.status.as_str(),
            "current_version": self.current_version.to_string(),
            "target_version": self.target_version.as_ref().map(ToString::to_string),
            "target_ref": self.target_ref,
            "check_only": self.check_only,
            "updated": self.updated,
            "apply_supported": self.apply_supported,
            "install_dir": self.install_dir,
            "manual_command": self.manual_command,
            "message": self.message,
        })
    }

    fn print_human(&self) {
        match self.status {
            UpdateStatus::UpToDate => {
                println!("Memzoi is up to date ({})", self.current_version);
            }
            UpdateStatus::UpdateAvailable => {
                let target = self
                    .target_version
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown".to_owned());
                println!(
                    "Memzoi {target} is available (current {})",
                    self.current_version
                );
                if !self.apply_supported {
                    println!("This install cannot be updated automatically.");
                    if let Some(command) = &self.manual_command {
                        println!("Use: {command}");
                    }
                }
            }
            UpdateStatus::Updated => {
                let target = self
                    .target_version
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown".to_owned());
                println!("Updated Memzoi from {} to {target}", self.current_version);
            }
            UpdateStatus::Unsupported
            | UpdateStatus::InvalidRef
            | UpdateStatus::DownloadFailed
            | UpdateStatus::ChecksumMismatch
            | UpdateStatus::RollbackFailed => {
                if let Some(message) = &self.message {
                    println!("{message}");
                }
                if let Some(command) = &self.manual_command {
                    println!("Use: {command}");
                }
            }
        }
    }
}

fn run_update(options: UpdateOptions<'_>) -> UpdateReport {
    let current_version = current_version();
    let install = InstallInfo::detect_current();

    let requested_ref = match RequestedRef::parse(options.reference) {
        Ok(requested_ref) => requested_ref,
        Err(message) => {
            return report(
                UpdateStatus::InvalidRef,
                current_version,
                None,
                options,
                false,
                &install,
                Some(message),
            );
        }
    };

    if !options.check_only && !install.apply_supported {
        if let RequestedRef::Exact { tag, version } = &requested_ref {
            let target = TargetInfo {
                tag: tag.clone(),
                version: version.clone(),
            };
            if target.version < current_version {
                return report(
                    UpdateStatus::InvalidRef,
                    current_version,
                    Some(target),
                    options,
                    false,
                    &install,
                    Some("target release is older than the installed Memzoi version".to_owned()),
                );
            }
            if target.version == current_version {
                return report(
                    UpdateStatus::UpToDate,
                    current_version,
                    Some(target),
                    options,
                    false,
                    &install,
                    None,
                );
            }

            return report(
                UpdateStatus::Unsupported,
                current_version,
                Some(target),
                options,
                false,
                &install,
                Some(install.message()),
            );
        }

        return report(
            UpdateStatus::Unsupported,
            current_version,
            None,
            options,
            false,
            &install,
            Some(install.message()),
        );
    }

    let resolved_ref = match resolve_requested_ref(requested_ref) {
        Ok(resolved_ref) => resolved_ref,
        Err(ResolveError::InvalidRef(message)) => {
            return report(
                UpdateStatus::InvalidRef,
                current_version,
                None,
                options,
                false,
                &install,
                Some(message),
            );
        }
        Err(ResolveError::Download(message)) => {
            return report(
                UpdateStatus::DownloadFailed,
                current_version,
                None,
                options,
                false,
                &install,
                Some(message),
            );
        }
    };

    if resolved_ref.version < current_version {
        if resolved_ref.from_latest {
            return report(
                UpdateStatus::UpToDate,
                current_version,
                Some(TargetInfo::from(&resolved_ref)),
                options,
                false,
                &install,
                None,
            );
        }

        return report(
            UpdateStatus::InvalidRef,
            current_version,
            Some(TargetInfo::from(&resolved_ref)),
            options,
            false,
            &install,
            Some("target release is older than the installed Memzoi version".to_owned()),
        );
    }

    if resolved_ref.version == current_version {
        return report(
            UpdateStatus::UpToDate,
            current_version,
            Some(TargetInfo::from(&resolved_ref)),
            options,
            false,
            &install,
            None,
        );
    }

    if options.check_only {
        return report(
            UpdateStatus::UpdateAvailable,
            current_version,
            Some(TargetInfo::from(&resolved_ref)),
            options,
            false,
            &install,
            None,
        );
    }

    if !install.apply_supported {
        return report(
            UpdateStatus::Unsupported,
            current_version,
            Some(TargetInfo::from(&resolved_ref)),
            options,
            false,
            &install,
            Some(install.message()),
        );
    }

    match apply_release_update(&install, &resolved_ref.tag) {
        Ok(()) => report(
            UpdateStatus::Updated,
            current_version,
            Some(TargetInfo::from(&resolved_ref)),
            options,
            true,
            &install,
            None,
        ),
        Err(ApplyError::Download(message)) => report(
            UpdateStatus::DownloadFailed,
            current_version,
            Some(TargetInfo::from(&resolved_ref)),
            options,
            false,
            &install,
            Some(message),
        ),
        Err(ApplyError::Checksum(message)) => report(
            UpdateStatus::ChecksumMismatch,
            current_version,
            Some(TargetInfo::from(&resolved_ref)),
            options,
            false,
            &install,
            Some(message),
        ),
        Err(ApplyError::Rollback(message)) => report(
            UpdateStatus::RollbackFailed,
            current_version,
            Some(TargetInfo::from(&resolved_ref)),
            options,
            false,
            &install,
            Some(message),
        ),
    }
}

fn report(
    status: UpdateStatus,
    current_version: Version,
    target: Option<TargetInfo>,
    options: UpdateOptions<'_>,
    updated: bool,
    install: &InstallInfo,
    message: Option<String>,
) -> UpdateReport {
    let target_ref = target.as_ref().map(|target| target.tag.as_str());
    let manual_command = if status == UpdateStatus::UpdateAvailable && install.apply_supported {
        None
    } else if matches!(
        status,
        UpdateStatus::UpdateAvailable | UpdateStatus::Unsupported
    ) {
        install.manual_command(target_ref)
    } else {
        None
    };

    UpdateReport {
        status,
        current_version,
        target_version: target.as_ref().map(|target| target.version.clone()),
        target_ref: target.map(|target| target.tag),
        check_only: options.check_only,
        updated,
        apply_supported: install.apply_supported,
        install_dir: install.install_dir.clone(),
        manual_command,
        message,
    }
}

fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version should be valid semver")
}

#[derive(Debug, Clone)]
enum RequestedRef {
    Latest,
    Exact { tag: String, version: Version },
}

impl RequestedRef {
    fn parse(input: &str) -> std::result::Result<Self, String> {
        let trimmed = input.trim();
        if trimmed.is_empty() || trimmed == "latest" {
            return Ok(Self::Latest);
        }

        normalize_exact_ref(trimmed).map(|(tag, version)| Self::Exact { tag, version })
    }
}

#[derive(Debug, Clone)]
struct ResolvedRef {
    tag: String,
    version: Version,
    from_latest: bool,
}

#[derive(Debug, Clone)]
struct TargetInfo {
    tag: String,
    version: Version,
}

impl From<&ResolvedRef> for TargetInfo {
    fn from(resolved_ref: &ResolvedRef) -> Self {
        Self {
            tag: resolved_ref.tag.clone(),
            version: resolved_ref.version.clone(),
        }
    }
}

#[derive(Debug, Clone)]
enum ResolveError {
    InvalidRef(String),
    Download(String),
}

fn resolve_requested_ref(
    requested_ref: RequestedRef,
) -> std::result::Result<ResolvedRef, ResolveError> {
    match requested_ref {
        RequestedRef::Latest => resolve_latest_ref(),
        RequestedRef::Exact { tag, version } => Ok(ResolvedRef {
            tag,
            version,
            from_latest: false,
        }),
    }
}

fn resolve_latest_ref() -> std::result::Result<ResolvedRef, ResolveError> {
    let api_base = env_value("MEMZOI_RELEASE_API_BASE", RELEASE_API_BASE);
    let url = format!("{}/latest", api_base.trim_end_matches('/'));
    let bytes = http_get_bytes(&url).map_err(|error| {
        ResolveError::Download(format!(
            "could not fetch latest Memzoi release metadata: {error}"
        ))
    })?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        ResolveError::Download(format!(
            "could not parse latest Memzoi release metadata: {error}"
        ))
    })?;
    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ResolveError::Download(
                "latest Memzoi release metadata did not include tag_name".to_owned(),
            )
        })?;
    let (tag, version) = normalize_exact_ref(tag).map_err(ResolveError::InvalidRef)?;

    Ok(ResolvedRef {
        tag,
        version,
        from_latest: true,
    })
}

fn normalize_exact_ref(input: &str) -> std::result::Result<(String, Version), String> {
    if input == "main" || input == "master" {
        return Err("branch refs are not supported by memzoi update".to_owned());
    }
    if input.contains("://") || input.contains('/') || input.contains('\\') {
        return Err("release ref must be a version tag, not a URL, path, or branch".to_owned());
    }
    if looks_like_sha(input) {
        return Err("commit SHAs are not supported by memzoi update".to_owned());
    }

    let tag = if input.starts_with('v') {
        input.to_owned()
    } else if input.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        format!("v{input}")
    } else {
        return Err("release ref must be latest, vX.Y.Z, or X.Y.Z".to_owned());
    };

    let raw_version = tag
        .strip_prefix('v')
        .ok_or_else(|| "release ref must start with v".to_owned())?;
    let version = Version::parse(raw_version)
        .map_err(|_| "release ref must be a valid semver tag like v1.2.3".to_owned())?;
    if !version.pre.is_empty() {
        return Err("prerelease tags are not installed by memzoi update".to_owned());
    }
    if !version.build.is_empty() {
        return Err("build metadata tags are not installed by memzoi update".to_owned());
    }
    if tag != format!("v{version}") {
        return Err("release ref must be normalized as vX.Y.Z".to_owned());
    }

    Ok((tag, version))
}

fn looks_like_sha(input: &str) -> bool {
    input.len() >= 7 && input.chars().all(|character| character.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallKind {
    ReleaseBinary,
    SourceBuild,
    CargoInstall,
    PackageManaged,
    Windows,
    MissingSibling,
    UnsupportedPath,
}

#[derive(Debug, Clone)]
struct InstallInfo {
    install_dir: Option<PathBuf>,
    apply_supported: bool,
    kind: InstallKind,
}

impl InstallInfo {
    fn detect_current() -> Self {
        match std::env::current_exe() {
            Ok(current_exe) => {
                let mut install = classify_install_path(&current_exe);
                if install.apply_supported
                    && (run_binary_version(&current_exe).is_err()
                        || install
                            .install_dir
                            .as_ref()
                            .map(|install_dir| {
                                run_binary_version(&install_dir.join(MEMZOI_MCP_BIN)).is_err()
                            })
                            .unwrap_or(true))
                {
                    install.apply_supported = false;
                    install.kind = InstallKind::UnsupportedPath;
                }
                install
            }
            Err(_) => Self {
                install_dir: None,
                apply_supported: false,
                kind: InstallKind::UnsupportedPath,
            },
        }
    }

    fn message(&self) -> String {
        match self.kind {
            InstallKind::ReleaseBinary => "this Memzoi install is updateable".to_owned(),
            InstallKind::SourceBuild => {
                "memzoi update does not apply updates to source checkout builds".to_owned()
            }
            InstallKind::CargoInstall => {
                "memzoi update does not apply updates to Cargo-installed binaries".to_owned()
            }
            InstallKind::PackageManaged => {
                "memzoi update does not apply updates to package-managed binaries".to_owned()
            }
            InstallKind::Windows => {
                "memzoi update can check for updates on Windows, but automatic replacement is not implemented yet".to_owned()
            }
            InstallKind::MissingSibling => {
                "memzoi update requires memzoi and memzoi-mcp to be sibling binaries".to_owned()
            }
            InstallKind::UnsupportedPath => {
                "this Memzoi install path is not supported for automatic updates".to_owned()
            }
        }
    }

    fn manual_command(&self, target_ref: Option<&str>) -> Option<String> {
        let target_ref = target_ref.unwrap_or("vX.Y.Z");
        match self.kind {
            InstallKind::SourceBuild => Some("git pull && make install".to_owned()),
            InstallKind::CargoInstall => Some(format!(
                "cargo install --git {REPO_URL} --tag {target_ref} memzoi-cli --locked && cargo install --git {REPO_URL} --tag {target_ref} memzoi-mcp --locked"
            )),
            InstallKind::PackageManaged => {
                Some("use your package manager to update Memzoi".to_owned())
            }
            InstallKind::Windows => Some(format!(
                "powershell -ExecutionPolicy Bypass -c \"$env:MEMZOI_REF='{target_ref}'; irm {INSTALL_PS1_URL} | iex\""
            )),
            InstallKind::MissingSibling
            | InstallKind::UnsupportedPath
            | InstallKind::ReleaseBinary => Some(format!(
                "curl -fsSL {INSTALL_SH_URL} | MEMZOI_REF={target_ref} sh"
            )),
        }
    }
}

fn classify_install_path(current_exe: &Path) -> InstallInfo {
    let install_dir = current_exe.parent().map(Path::to_path_buf);
    let Some(install_dir_path) = install_dir.as_deref() else {
        return InstallInfo {
            install_dir,
            apply_supported: false,
            kind: InstallKind::UnsupportedPath,
        };
    };

    if current_exe.file_name() != Some(OsStr::new(MEMZOI_BIN)) {
        return InstallInfo {
            install_dir,
            apply_supported: false,
            kind: InstallKind::UnsupportedPath,
        };
    }

    if path_has_component(current_exe, "target") || is_under_source_checkout(current_exe) {
        return InstallInfo {
            install_dir,
            apply_supported: false,
            kind: InstallKind::SourceBuild,
        };
    }

    if is_cargo_bin_path(current_exe) {
        return InstallInfo {
            install_dir,
            apply_supported: false,
            kind: InstallKind::CargoInstall,
        };
    }

    if is_package_managed_path(current_exe) {
        return InstallInfo {
            install_dir,
            apply_supported: false,
            kind: InstallKind::PackageManaged,
        };
    }

    if cfg!(windows) {
        return InstallInfo {
            install_dir,
            apply_supported: false,
            kind: InstallKind::Windows,
        };
    }

    if install_dir_path.join(MEMZOI_MCP_BIN).metadata().is_err() {
        return InstallInfo {
            install_dir,
            apply_supported: false,
            kind: InstallKind::MissingSibling,
        };
    }

    if platform_archive_target().is_none() || !directory_may_be_writable(install_dir_path) {
        return InstallInfo {
            install_dir,
            apply_supported: false,
            kind: InstallKind::UnsupportedPath,
        };
    }

    InstallInfo {
        install_dir,
        apply_supported: true,
        kind: InstallKind::ReleaseBinary,
    }
}

fn path_has_component(path: &Path, name: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == OsStr::new(name))
}

fn is_under_source_checkout(path: &Path) -> bool {
    path.ancestors().any(|ancestor| {
        ancestor.join("crates/memzoi-cli").is_dir() && ancestor.join("crates/memzoi-mcp").is_dir()
    })
}

fn is_cargo_bin_path(path: &Path) -> bool {
    let Some(home) = home_dir() else {
        return false;
    };
    path.starts_with(home.join(".cargo").join("bin"))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn is_package_managed_path(path: &Path) -> bool {
    let path_string = path.to_string_lossy();
    path_string.starts_with("/nix/store/")
        || path_string.contains("/Cellar/")
        || path_string.starts_with("/opt/homebrew/")
        || path_string.starts_with("/usr/bin/")
        || path_string.starts_with("/bin/")
        || path_string.starts_with("/usr/sbin/")
        || path_string.starts_with("/sbin/")
}

fn directory_may_be_writable(path: &Path) -> bool {
    probe_directory_writable(path).is_ok()
}

fn apply_release_update(
    install: &InstallInfo,
    target_ref: &str,
) -> std::result::Result<(), ApplyError> {
    let Some(install_dir) = install.install_dir.as_deref() else {
        return Err(ApplyError::Download(
            "could not detect install directory".to_owned(),
        ));
    };
    ensure_install_dir_writable(install_dir)
        .map_err(|error| ApplyError::Download(error.to_string()))?;

    let target = platform_archive_target().ok_or_else(|| {
        ApplyError::Download("no Memzoi release binary target for this platform".to_owned())
    })?;
    let archive_name = format!("memzoi-{target_ref}-{target}.tar.gz");
    let download_base = env_value("MEMZOI_DOWNLOAD_BASE", DOWNLOAD_BASE);
    let archive_url = format!(
        "{}/{target_ref}/{archive_name}",
        download_base.trim_end_matches('/')
    );
    let checksum_url = format!("{archive_url}.sha256");

    let archive_bytes = http_get_bytes(&archive_url).map_err(|error| {
        ApplyError::Download(format!("failed to download {archive_url}: {error}"))
    })?;
    let checksum_bytes = http_get_bytes(&checksum_url).map_err(|error| {
        ApplyError::Download(format!("failed to download {checksum_url}: {error}"))
    })?;
    let checksum_manifest = std::str::from_utf8(&checksum_bytes).map_err(|error| {
        ApplyError::Checksum(format!("checksum manifest was not UTF-8: {error}"))
    })?;
    let expected_checksum = parse_sha256_manifest(checksum_manifest)
        .map_err(|error| ApplyError::Checksum(error.to_string()))?;
    verify_archive_checksum(&archive_bytes, &expected_checksum)
        .map_err(|error| ApplyError::Checksum(error.to_string()))?;

    let temp_dir = TempDir::new().map_err(|error| {
        ApplyError::Download(format!("failed to create update temp directory: {error}"))
    })?;
    let extracted = unpack_unix_archive(&archive_bytes, temp_dir.path())
        .map_err(|error| ApplyError::Download(error.to_string()))?;
    run_binary_version(&extracted.memzoi).map_err(|error| {
        ApplyError::Download(format!(
            "downloaded memzoi binary failed --version: {error}"
        ))
    })?;
    run_binary_version(&extracted.memzoi_mcp).map_err(|error| {
        ApplyError::Download(format!(
            "downloaded memzoi-mcp binary failed --version: {error}"
        ))
    })?;

    replace_installed_binaries(&extracted, install_dir).map_err(|error| match error {
        ReplaceError::RolledBack(message) => ApplyError::Download(message),
        ReplaceError::RollbackFailed(message) => ApplyError::Rollback(message),
    })?;

    Ok(())
}

fn env_value(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn http_get_bytes(url: &str) -> Result<Vec<u8>> {
    let response = ureq::get(url)
        .set("User-Agent", concat!("memzoi/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|error| anyhow!("{error}"))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed reading response body from {url}"))?;
    Ok(bytes)
}

fn parse_sha256_manifest(manifest: &str) -> Result<String> {
    for line in manifest.lines() {
        let Some(first_field) = line.split_whitespace().next() else {
            continue;
        };
        if first_field.len() == 64
            && first_field
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Ok(first_field.to_ascii_lowercase());
        }
    }

    bail!("could not read SHA-256 checksum from manifest")
}

fn verify_archive_checksum(archive_bytes: &[u8], expected_checksum: &str) -> Result<()> {
    let mut hasher = Sha256::new();
    hasher.update(archive_bytes);
    let actual_checksum = format!("{:x}", hasher.finalize());
    if actual_checksum != expected_checksum {
        bail!(
            "downloaded Memzoi archive checksum did not match: expected {expected_checksum}, got {actual_checksum}"
        );
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct ExtractedBinaries {
    memzoi: PathBuf,
    memzoi_mcp: PathBuf,
}

fn unpack_unix_archive(archive_bytes: &[u8], destination: &Path) -> Result<ExtractedBinaries> {
    let decoder = GzDecoder::new(Cursor::new(archive_bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut found = HashSet::new();

    for entry in archive.entries().context("failed to read Memzoi archive")? {
        let mut entry = entry.context("failed to read Memzoi archive entry")?;
        if !entry.header().entry_type().is_file() {
            bail!("Memzoi archive contained a non-file entry");
        }
        let entry_path = entry
            .path()
            .context("failed to read Memzoi archive entry path")?;
        let file_name = clean_single_component(&entry_path)?;
        if file_name != MEMZOI_BIN && file_name != MEMZOI_MCP_BIN {
            bail!("Memzoi archive contained unexpected file: {file_name}");
        }
        if !found.insert(file_name.clone()) {
            bail!("Memzoi archive contained duplicate file: {file_name}");
        }
        entry
            .unpack(destination.join(file_name))
            .context("failed to unpack Memzoi archive entry")?;
    }

    if !found.contains(MEMZOI_BIN) || !found.contains(MEMZOI_MCP_BIN) || found.len() != 2 {
        bail!("Memzoi archive did not contain exactly memzoi and memzoi-mcp");
    }

    Ok(ExtractedBinaries {
        memzoi: destination.join(MEMZOI_BIN),
        memzoi_mcp: destination.join(MEMZOI_MCP_BIN),
    })
}

fn clean_single_component(path: &Path) -> Result<String> {
    let mut components = path
        .components()
        .filter(|component| !matches!(component, Component::CurDir));
    let Some(Component::Normal(file_name)) = components.next() else {
        bail!("Memzoi archive contained an unsafe path");
    };
    if components.next().is_some() {
        bail!("Memzoi archive contained a nested or unsafe path");
    }

    Ok(file_name.to_string_lossy().into_owned())
}

fn run_binary_version(path: &Path) -> Result<()> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .with_context(|| format!("failed to run {}", path.display()))?;
    if !output.status.success() {
        bail!("{} --version exited with {}", path.display(), output.status);
    }

    Ok(())
}

fn ensure_install_dir_writable(install_dir: &Path) -> Result<()> {
    probe_directory_writable(install_dir).with_context(|| {
        format!(
            "install directory is not writable: {}",
            install_dir.display()
        )
    })
}

fn probe_directory_writable(path: &Path) -> Result<()> {
    let probe = tempfile::Builder::new()
        .prefix(".memzoi-update-write-test-")
        .tempfile_in(path)
        .with_context(|| format!("failed to create write probe in {}", path.display()))?;
    probe
        .close()
        .with_context(|| format!("failed to remove write probe in {}", path.display()))?;
    Ok(())
}

#[derive(Debug)]
enum ApplyError {
    Download(String),
    Checksum(String),
    Rollback(String),
}

#[derive(Debug)]
enum ReplaceError {
    RolledBack(String),
    RollbackFailed(String),
}

#[cfg(unix)]
fn replace_installed_binaries(
    extracted: &ExtractedBinaries,
    install_dir: &Path,
) -> std::result::Result<(), ReplaceError> {
    use std::os::unix::fs::PermissionsExt;

    let pid = std::process::id();
    let replacements = [
        FileReplacement {
            source: prepare_staged_binary(
                &extracted.memzoi,
                &install_dir.join(MEMZOI_BIN),
                &install_dir.join(format!(".memzoi-update-new-{pid}-{MEMZOI_BIN}")),
            )?,
            dest: install_dir.join(MEMZOI_BIN),
            backup: install_dir.join(format!(".memzoi-update-backup-{pid}-{MEMZOI_BIN}")),
        },
        FileReplacement {
            source: prepare_staged_binary(
                &extracted.memzoi_mcp,
                &install_dir.join(MEMZOI_MCP_BIN),
                &install_dir.join(format!(".memzoi-update-new-{pid}-{MEMZOI_MCP_BIN}")),
            )?,
            dest: install_dir.join(MEMZOI_MCP_BIN),
            backup: install_dir.join(format!(".memzoi-update-backup-{pid}-{MEMZOI_MCP_BIN}")),
        },
    ];

    for replacement in &replacements {
        let existing_mode = fs::metadata(&replacement.dest)
            .map_err(|error| {
                ReplaceError::RolledBack(format!(
                    "failed to read {} permissions before update: {error}",
                    replacement.dest.display()
                ))
            })?
            .permissions()
            .mode();
        let mode = if existing_mode & 0o111 == 0 {
            existing_mode | 0o755
        } else {
            existing_mode
        };
        fs::set_permissions(&replacement.source, fs::Permissions::from_mode(mode)).map_err(
            |error| {
                ReplaceError::RolledBack(format!(
                    "failed to set staged binary permissions for {}: {error}",
                    replacement.source.display()
                ))
            },
        )?;
    }

    let backups = replace_files_transaction(&replacements)?;
    if let Err(error) = run_binary_version(&install_dir.join(MEMZOI_BIN))
        .and_then(|_| run_binary_version(&install_dir.join(MEMZOI_MCP_BIN)))
    {
        rollback_replacements(&backups)?;
        return Err(ReplaceError::RolledBack(format!(
            "installed Memzoi binaries failed --version after update: {error}; rolled back"
        )));
    }

    for (backup, _) in backups {
        let _ = fs::remove_file(backup);
    }

    Ok(())
}

#[cfg(not(unix))]
fn replace_installed_binaries(
    _extracted: &ExtractedBinaries,
    _install_dir: &Path,
) -> std::result::Result<(), ReplaceError> {
    Err(ReplaceError::RolledBack(
        "automatic update replacement is only implemented on Unix platforms".to_owned(),
    ))
}

#[cfg(unix)]
fn prepare_staged_binary(
    source: &Path,
    dest: &Path,
    staged: &Path,
) -> std::result::Result<PathBuf, ReplaceError> {
    if staged.exists() {
        fs::remove_file(staged).map_err(|error| {
            ReplaceError::RolledBack(format!(
                "failed to remove stale staged binary {}: {error}",
                staged.display()
            ))
        })?;
    }
    if dest.exists() {
        fs::copy(source, staged).map_err(|error| {
            ReplaceError::RolledBack(format!(
                "failed to stage update binary {}: {error}",
                staged.display()
            ))
        })?;
        Ok(staged.to_path_buf())
    } else {
        Err(ReplaceError::RolledBack(format!(
            "installed binary is missing: {}",
            dest.display()
        )))
    }
}

#[cfg(unix)]
#[derive(Debug, Clone)]
struct FileReplacement {
    source: PathBuf,
    dest: PathBuf,
    backup: PathBuf,
}

#[cfg(unix)]
fn replace_files_transaction(
    replacements: &[FileReplacement],
) -> std::result::Result<Vec<(PathBuf, PathBuf)>, ReplaceError> {
    let mut moved = Vec::new();

    for replacement in replacements {
        if replacement.backup.exists() {
            fs::remove_file(&replacement.backup).map_err(|error| {
                ReplaceError::RolledBack(format!(
                    "failed to remove stale backup {}: {error}",
                    replacement.backup.display()
                ))
            })?;
        }

        if let Err(error) = fs::rename(&replacement.dest, &replacement.backup) {
            rollback_replacements(&moved)?;
            return Err(ReplaceError::RolledBack(format!(
                "failed to back up {}: {error}; rolled back",
                replacement.dest.display()
            )));
        }
        moved.push((replacement.backup.clone(), replacement.dest.clone()));

        if let Err(error) = fs::rename(&replacement.source, &replacement.dest) {
            rollback_replacements(&moved)?;
            return Err(ReplaceError::RolledBack(format!(
                "failed to install {}: {error}; rolled back",
                replacement.dest.display()
            )));
        }
    }

    Ok(moved)
}

#[cfg(unix)]
fn rollback_replacements(moved: &[(PathBuf, PathBuf)]) -> std::result::Result<(), ReplaceError> {
    let mut failures = Vec::new();
    for (backup, dest) in moved.iter().rev() {
        if dest.exists() {
            if let Err(error) = fs::remove_file(dest) {
                failures.push(format!(
                    "failed to remove partial {}: {error}",
                    dest.display()
                ));
                continue;
            }
        }
        if let Err(error) = fs::rename(backup, dest) {
            failures.push(format!(
                "failed to restore {} from {}: {error}",
                dest.display(),
                backup.display()
            ));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(ReplaceError::RollbackFailed(failures.join("; ")))
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn platform_archive_target() -> Option<&'static str> {
    Some("x86_64-unknown-linux-gnu")
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
fn platform_archive_target() -> Option<&'static str> {
    Some("x86_64-apple-darwin")
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn platform_archive_target() -> Option<&'static str> {
    Some("aarch64-apple-darwin")
}

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
)))]
fn platform_archive_target() -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_stable_release_refs() {
        let RequestedRef::Latest = RequestedRef::parse("latest").expect("latest ref") else {
            panic!("latest should parse as latest");
        };
        assert_eq!(
            normalize_exact_ref("1.2.3").expect("numeric tag").0,
            "v1.2.3"
        );
        assert_eq!(
            normalize_exact_ref("v1.2.3").expect("v tag").1,
            Version::parse("1.2.3").expect("semver")
        );
    }

    #[test]
    fn rejects_branch_sha_prerelease_and_malformed_refs() {
        for invalid_ref in [
            "main",
            "master",
            "abc1234",
            "https://example.com/memzoi",
            "feature/update",
            "v1.2.3-alpha.1",
            "v1.2",
            "release-v1.2.3",
        ] {
            assert!(
                RequestedRef::parse(invalid_ref).is_err(),
                "{invalid_ref} should be rejected"
            );
        }
    }

    #[test]
    fn explicit_downgrade_reports_invalid_ref_without_network() {
        let current = current_version();
        if current.patch == 0 {
            return;
        }
        let older = format!("v{}.{}.{}", current.major, current.minor, current.patch - 1);
        let report = run_update(UpdateOptions {
            check_only: true,
            reference: &older,
        });

        assert_eq!(report.status, UpdateStatus::InvalidRef);
    }

    #[test]
    fn semver_comparison_does_not_string_compare() {
        let newer = Version::parse("0.1.10").expect("newer");
        let older = Version::parse("0.1.9").expect("older");

        assert!(newer > older);
    }

    #[test]
    fn parses_sha256_manifest_first_field() {
        let digest = "ABCDEFabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234";
        let parsed = parse_sha256_manifest(&format!("{digest}  memzoi.tar.gz\n"))
            .expect("manifest should parse");

        assert_eq!(parsed, digest.to_ascii_lowercase());
    }

    #[test]
    fn rejects_archive_path_traversal() {
        assert!(clean_single_component(Path::new("../memzoi")).is_err());

        let temp = tempfile::tempdir().expect("temp dir");
        let archive_bytes = archive_with_entries(&[("nested/memzoi", b"bad".as_slice())]);

        let error = unpack_unix_archive(&archive_bytes, temp.path()).expect_err("unsafe archive");

        assert!(error.to_string().contains("nested or unsafe path"));
    }

    #[test]
    fn classifies_source_cargo_missing_sibling_and_release_paths() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("target/debug").join(MEMZOI_BIN);
        fs::create_dir_all(source_path.parent().expect("source parent")).expect("source parent");
        fs::write(&source_path, b"").expect("source binary");
        assert_eq!(
            classify_install_path(&source_path).kind,
            InstallKind::SourceBuild
        );

        if let Some(home) = home_dir() {
            let cargo_path = home.join(".cargo/bin").join(MEMZOI_BIN);
            assert_eq!(
                classify_install_path(&cargo_path).kind,
                InstallKind::CargoInstall
            );
        }

        let missing_sibling = temp.path().join("bin").join(MEMZOI_BIN);
        fs::create_dir_all(missing_sibling.parent().expect("bin parent")).expect("bin parent");
        fs::write(&missing_sibling, b"").expect("memzoi binary");
        assert_eq!(
            classify_install_path(&missing_sibling).kind,
            InstallKind::MissingSibling
        );

        let release_dir = temp.path().join("release-bin");
        fs::create_dir_all(&release_dir).expect("release dir");
        let memzoi = release_dir.join(MEMZOI_BIN);
        let mcp = release_dir.join(MEMZOI_MCP_BIN);
        fs::write(&memzoi, b"").expect("memzoi binary");
        fs::write(&mcp, b"").expect("mcp binary");
        let release = classify_install_path(&memzoi);
        if platform_archive_target().is_some() && !cfg!(windows) {
            assert_eq!(release.kind, InstallKind::ReleaseBinary);
            assert!(release.apply_supported);
        }
    }

    #[cfg(unix)]
    #[test]
    fn replacement_transaction_rolls_back_after_partial_failure() {
        let temp = tempfile::tempdir().expect("temp dir");
        let one_dest = temp.path().join("one");
        let two_dest = temp.path().join("two");
        let one_source = temp.path().join("one.new");
        let missing_two_source = temp.path().join("two.new");
        let one_backup = temp.path().join("one.backup");
        let two_backup = temp.path().join("two.backup");
        fs::write(&one_dest, b"old-one").expect("one dest");
        fs::write(&two_dest, b"old-two").expect("two dest");
        fs::write(&one_source, b"new-one").expect("one source");

        let result = replace_files_transaction(&[
            FileReplacement {
                source: one_source,
                dest: one_dest.clone(),
                backup: one_backup,
            },
            FileReplacement {
                source: missing_two_source,
                dest: two_dest.clone(),
                backup: two_backup,
            },
        ]);

        assert!(matches!(result, Err(ReplaceError::RolledBack(_))));
        assert_eq!(fs::read(&one_dest).expect("one restored"), b"old-one");
        assert_eq!(fs::read(&two_dest).expect("two restored"), b"old-two");
    }

    fn archive_with_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive_bytes = Vec::new();
        {
            let encoder =
                flate2::write::GzEncoder::new(&mut archive_bytes, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            for (path, body) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder
                    .append_data(&mut header, path, *body)
                    .expect("append test archive entry");
            }
            builder.finish().expect("finish archive");
        }
        archive_bytes
    }
}
