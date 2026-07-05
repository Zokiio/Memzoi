[CmdletBinding()]
param(
    [string]$Ref = $env:MEMZOI_REF,
    [string]$InstallDir = $env:MEMZOI_INSTALL_DIR,
    [string]$RepoUrl = $env:MEMZOI_REPO_URL,
    [string]$DownloadBase = $env:MEMZOI_DOWNLOAD_BASE,
    [string]$ReleaseApiBase = $env:MEMZOI_RELEASE_API_BASE
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

if ([string]::IsNullOrWhiteSpace($Ref)) {
    $Ref = "latest"
}

if ([string]::IsNullOrWhiteSpace($RepoUrl)) {
    $RepoUrl = "https://github.com/Zokiio/Memzoi.git"
}

if ([string]::IsNullOrWhiteSpace($DownloadBase)) {
    $DownloadBase = "https://github.com/Zokiio/Memzoi/releases/download"
}

if ([string]::IsNullOrWhiteSpace($ReleaseApiBase)) {
    $ReleaseApiBase = "https://api.github.com/repos/Zokiio/Memzoi/releases"
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    if ($env:CARGO_INSTALL_ROOT) {
        $InstallDir = Join-Path $env:CARGO_INSTALL_ROOT "bin"
    } elseif ($env:LOCALAPPDATA) {
        $InstallDir = Join-Path $env:LOCALAPPDATA "Programs\Memzoi\bin"
    } else {
        $InstallDir = Join-Path $HOME ".local\bin"
    }
}

$BinDir = $InstallDir
$RepoRoot = $null
$ResolvedRef = $null

if ((Test-Path "crates/memzoi-cli") -and (Test-Path "crates/memzoi-mcp")) {
    $RepoRoot = (Get-Location).Path
} elseif ($PSCommandPath) {
    $ScriptDir = Split-Path -Parent $PSCommandPath
    $CandidateRoot = Resolve-Path (Join-Path $ScriptDir "..") -ErrorAction SilentlyContinue
    if ($CandidateRoot -and
        (Test-Path (Join-Path $CandidateRoot "crates/memzoi-cli")) -and
        (Test-Path (Join-Path $CandidateRoot "crates/memzoi-mcp"))) {
        $RepoRoot = $CandidateRoot.Path
    }
}

function Invoke-Cargo {
    param([string[]]$Arguments)

    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "cargo is required for source installs. Install Rust from https://rustup.rs/ and re-run this script, or use a release tag with binary assets."
    }

    Write-Host "+ cargo $($Arguments -join ' ')"
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}

function Install-PathPackage {
    param([string]$PackagePath)

    Invoke-Cargo -Arguments @("install", "--path", $PackagePath, "--locked")
}

function Install-GitPackage {
    param([string]$Package)

    if ($ResolvedRef -eq "main" -or $ResolvedRef -eq "master") {
        Invoke-Cargo -Arguments @("install", "--git", $RepoUrl, "--branch", $ResolvedRef, $Package, "--locked")
    } else {
        Invoke-Cargo -Arguments @("install", "--git", $RepoUrl, "--tag", $ResolvedRef, $Package, "--locked")
    }
}

function Resolve-Ref {
    if ($Ref -eq "main" -or $Ref -eq "master") {
        return $Ref
    }

    if ($Ref -eq "latest") {
        try {
            $releaseMetadata = Invoke-RestMethod -Uri "$ReleaseApiBase/latest"
        } catch {
            throw "Could not fetch latest Memzoi release metadata. GitHub API may be unavailable or rate limited. $($_.Exception.Message)"
        }

        if (-not $releaseMetadata.tag_name) {
            throw "Could not resolve latest Memzoi release tag."
        }

        return [string]$releaseMetadata.tag_name
    }

    if ($Ref.StartsWith("v")) {
        return $Ref
    }

    if ($Ref -match "^[0-9]") {
        return "v$Ref"
    }

    return $Ref
}

function Get-TargetTriple {
    $Architecture = $env:PROCESSOR_ARCHITECTURE
    if ($Architecture -eq "AMD64" -or $Architecture -eq "x86_64") {
        return "x86_64-pc-windows-msvc"
    }

    throw "unsupported Windows platform architecture: $Architecture"
}

function Get-Sha256FromManifest {
    param([string]$ManifestPath)

    foreach ($line in Get-Content -LiteralPath $ManifestPath) {
        $match = [regex]::Match($line, "^\s*([0-9a-fA-F]{64})\b")
        if ($match.Success) {
            return $match.Groups[1].Value.ToLowerInvariant()
        }
    }

    throw "Could not read SHA-256 checksum from $ManifestPath."
}

function Test-ArchiveDigest {
    param(
        [string]$ArchivePath,
        [string]$ManifestPath
    )

    $expectedDigest = Get-Sha256FromManifest -ManifestPath $ManifestPath
    $actualDigest = (Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualDigest -ne $expectedDigest) {
        throw "Downloaded Memzoi archive checksum did not match. Expected $expectedDigest but got $actualDigest."
    }
}

function Install-ReleaseBinaries {
    $Target = Get-TargetTriple
    $Archive = "memzoi-$ResolvedRef-$Target.zip"
    $Url = "$DownloadBase/$ResolvedRef/$Archive"
    $ChecksumUrl = "$Url.sha256"
    $TempDir = Join-Path ([System.IO.Path]::GetTempPath()) "memzoi-install-$([System.Guid]::NewGuid())"
    New-Item -ItemType Directory -Force -Path $TempDir | Out-Null

    try {
        $ArchivePath = Join-Path $TempDir $Archive
        $ChecksumPath = "$ArchivePath.sha256"
        Write-Host "+ download $Url"
        Invoke-WebRequest -Uri $Url -OutFile $ArchivePath
        Write-Host "+ download $ChecksumUrl"
        Invoke-WebRequest -Uri $ChecksumUrl -OutFile $ChecksumPath
        Test-ArchiveDigest -ArchivePath $ArchivePath -ManifestPath $ChecksumPath
        Expand-Archive -Path $ArchivePath -DestinationPath $TempDir -Force
        New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
        Copy-Item -Path (Join-Path $TempDir "memzoi.exe") -Destination (Join-Path $BinDir "memzoi.exe") -Force
        Copy-Item -Path (Join-Path $TempDir "memzoi-mcp.exe") -Destination (Join-Path $BinDir "memzoi-mcp.exe") -Force
    } catch {
        throw "no release binary found for $ResolvedRef on this platform: $($_.Exception.Message)"
    } finally {
        Remove-Item -Recurse -Force -Path $TempDir -ErrorAction SilentlyContinue
    }
}

function Test-PathContains {
    param(
        [string]$PathValue,
        [string]$Entry
    )

    if ([string]::IsNullOrWhiteSpace($PathValue)) {
        return $false
    }

    $needle = $Entry.TrimEnd("\")
    foreach ($segment in ($PathValue -split ";")) {
        if ([string]::IsNullOrWhiteSpace($segment)) {
            continue
        }
        if ($segment.TrimEnd("\") -ieq $needle) {
            return $true
        }
    }

    return $false
}

function Add-ToUserPath {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not (Test-PathContains -PathValue $userPath -Entry $BinDir)) {
        if ([string]::IsNullOrWhiteSpace($userPath)) {
            $newUserPath = $BinDir
        } else {
            $newUserPath = "$BinDir;$userPath"
        }
        [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
        Write-Host "PATH updated for future PowerShell sessions."
    }

    if (-not (Test-PathContains -PathValue $env:Path -Entry $BinDir)) {
        $env:Path = "$BinDir;$env:Path"
    }
}

if ($RepoRoot) {
    Install-PathPackage (Join-Path $RepoRoot "crates/memzoi-cli")
    Install-PathPackage (Join-Path $RepoRoot "crates/memzoi-mcp")
} else {
    $ResolvedRef = Resolve-Ref
    if ($ResolvedRef -eq "main" -or $ResolvedRef -eq "master") {
        Install-GitPackage "memzoi-cli"
        Install-GitPackage "memzoi-mcp"
    } else {
        Install-ReleaseBinaries
    }
}

$Memzoi = Join-Path $BinDir "memzoi.exe"
$MemzoiMcp = Join-Path $BinDir "memzoi-mcp.exe"

Write-Host "+ memzoi --version"
& $Memzoi --version

Write-Host "+ memzoi-mcp --version"
& $MemzoiMcp --version

Add-ToUserPath

Write-Host ""
Write-Host "Installed Memzoi."
Write-Host ""
Write-Host "Next:"
Write-Host "  memzoi init"
Write-Host "  memzoi doctor"
Write-Host "  memzoi quickstart --apply-sample"
