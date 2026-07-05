[CmdletBinding()]
param(
    [string]$Ref = $env:MEMZOI_REF,
    [string]$RepoUrl = $env:MEMZOI_REPO_URL,
    [string]$DownloadBase = $env:MEMZOI_DOWNLOAD_BASE
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

if ([string]::IsNullOrWhiteSpace($Ref)) {
    $Ref = "v0.1.0"
}

if ([string]::IsNullOrWhiteSpace($RepoUrl)) {
    $RepoUrl = "https://github.com/Zokiio/Memzoi.git"
}

if ([string]::IsNullOrWhiteSpace($DownloadBase)) {
    $DownloadBase = "https://github.com/Zokiio/Memzoi/releases/download"
}

if ($env:CARGO_INSTALL_ROOT) {
    $InstallRoot = $env:CARGO_INSTALL_ROOT
} elseif ($env:CARGO_HOME) {
    $InstallRoot = $env:CARGO_HOME
} else {
    $InstallRoot = Join-Path $HOME ".cargo"
}

$BinDir = Join-Path $InstallRoot "bin"
$RepoRoot = $null

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

    if ($Ref -eq "main" -or $Ref -eq "master") {
        Invoke-Cargo -Arguments @("install", "--git", $RepoUrl, "--branch", $Ref, $Package, "--locked")
    } else {
        Invoke-Cargo -Arguments @("install", "--git", $RepoUrl, "--tag", $Ref, $Package, "--locked")
    }
}

function Get-TargetTriple {
    $Architecture = $env:PROCESSOR_ARCHITECTURE
    if ($Architecture -eq "AMD64" -or $Architecture -eq "x86_64") {
        return "x86_64-pc-windows-msvc"
    }

    throw "unsupported Windows platform architecture: $Architecture"
}

function Install-ReleaseBinaries {
    $Target = Get-TargetTriple
    $Archive = "memzoi-$Ref-$Target.zip"
    $Url = "$DownloadBase/$Ref/$Archive"
    $TempDir = Join-Path ([System.IO.Path]::GetTempPath()) "memzoi-install-$([System.Guid]::NewGuid())"
    New-Item -ItemType Directory -Force -Path $TempDir | Out-Null

    try {
        $ArchivePath = Join-Path $TempDir $Archive
        Write-Host "+ download $Url"
        Invoke-WebRequest -Uri $Url -OutFile $ArchivePath
        Expand-Archive -Path $ArchivePath -DestinationPath $TempDir -Force
        New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
        Copy-Item -Path (Join-Path $TempDir "memzoi.exe") -Destination (Join-Path $BinDir "memzoi.exe") -Force
        Copy-Item -Path (Join-Path $TempDir "memzoi-mcp.exe") -Destination (Join-Path $BinDir "memzoi-mcp.exe") -Force
    } catch {
        throw "no release binary found for $Ref on this platform: $($_.Exception.Message)"
    } finally {
        Remove-Item -Recurse -Force -Path $TempDir -ErrorAction SilentlyContinue
    }
}

if ($RepoRoot) {
    Install-PathPackage (Join-Path $RepoRoot "crates/memzoi-cli")
    Install-PathPackage (Join-Path $RepoRoot "crates/memzoi-mcp")
} elseif ($Ref -eq "main" -or $Ref -eq "master") {
    Install-GitPackage "memzoi-cli"
    Install-GitPackage "memzoi-mcp"
} else {
    Install-ReleaseBinaries
}

$Memzoi = Join-Path $BinDir "memzoi.exe"
$MemzoiMcp = Join-Path $BinDir "memzoi-mcp.exe"

Write-Host "+ memzoi --version"
& $Memzoi --version

Write-Host "+ memzoi-mcp --version"
& $MemzoiMcp --version

New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
Set-Content -Path (Join-Path $BinDir "agent-memory.cmd") -Value "@echo off`r`n`"%~dp0memzoi.exe`" %*`r`n"
Set-Content -Path (Join-Path $BinDir "agent-memory-mcp.cmd") -Value "@echo off`r`n`"%~dp0memzoi-mcp.exe`" %*`r`n"

$PathEntries = $env:Path -split [IO.Path]::PathSeparator
if ($PathEntries -notcontains $BinDir) {
    Write-Host ""
    Write-Host "Note: $BinDir is not on PATH in this shell."
    Write-Host "Add it to PATH before running memzoi from a new terminal."
}

Write-Host ""
Write-Host "Installed Memzoi."
Write-Host ""
Write-Host "Compatibility aliases are also installed: agent-memory, agent-memory-mcp."
Write-Host ""
Write-Host "Next:"
Write-Host "  memzoi init"
Write-Host "  memzoi doctor"
Write-Host "  memzoi quickstart --apply-sample"
