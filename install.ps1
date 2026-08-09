<#
.SYNOPSIS
  Installs Libra on Windows.

.DESCRIPTION
  Default install is per-user and does not require administrator privileges.
  Override the version with -Version or LIBRA_VERSION.
#>

[CmdletBinding()]
param(
    [string]$Version = $env:LIBRA_VERSION,
    [string]$InstallDir = "",
    [string]$DownloadBaseUrl = "https://download.libra.tools",
    [switch]$NoModifyPath,
    # Skip the optional `lba` shorthand. Mirrors install.sh's --no-alias.
    [switch]$NoAlias
)

$ErrorActionPreference = "Stop"

# One of the release version surfaces. `compat_version_surface_sync` pins it
# to Cargo.toml: this value is substituted verbatim into the download URL, so
# a stale value silently installs an old binary when -Version is not given.
$DefaultVersion = "v0.19.111"
$ExeName = "libra.exe"
$ReleaseAsset = "libra-windows-amd64.exe"

# Marker written into every shim this installer generates. It is how a
# reinstall recognises ITS OWN shim (so rewriting is idempotent) and how it
# recognises a file it did not write (so an unrelated `lba` on the user's
# PATH is never clobbered).
$ShimMarker = "@rem libra-managed-shim"

function Write-Info {
    param([string]$Message)
    Write-Host "[libra-install] $Message" -ForegroundColor Cyan
}

function Ensure-Directory {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) {
        New-Item -ItemType Directory -Path $Path -Force | Out-Null
    }
}

function Add-UserPath {
    param([string]$PathToAdd)

    if ($NoModifyPath) {
        Write-Info "Skipping PATH update because -NoModifyPath was set."
        return
    }

    $normalizedPath = $PathToAdd.TrimEnd("\")
    $currentUserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $userPathItems = @()

    if (-not [string]::IsNullOrWhiteSpace($currentUserPath)) {
        $userPathItems = $currentUserPath -split ";" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    }

    $alreadyInUserPath = $false
    foreach ($item in $userPathItems) {
        if ($item.TrimEnd("\") -ieq $normalizedPath) {
            $alreadyInUserPath = $true
            break
        }
    }

    if (-not $alreadyInUserPath) {
        $nextUserPath = (@($normalizedPath) + $userPathItems) -join ";"
        [Environment]::SetEnvironmentVariable("Path", $nextUserPath, "User")
        Write-Info "Added to user PATH: $normalizedPath"
    } else {
        Write-Info "PATH already contains: $normalizedPath"
    }

    if (($env:Path -split ";" | ForEach-Object { $_.TrimEnd("\") }) -notcontains $normalizedPath) {
        $env:Path = "$normalizedPath;$env:Path"
    }
}

function Install-CmdShim {
    param(
        [string]$TargetExe,
        [string]$InstallBin
    )

    $shimDirs = @(
        (Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps"),
        (Join-Path $env:USERPROFILE ".local\bin")
    )

    foreach ($dir in $shimDirs) {
        try {
            Ensure-Directory $dir
            $probe = Join-Path $dir ".libra-write-test"
            Set-Content -LiteralPath $probe -Value "ok" -Encoding ASCII -Force
            Remove-Item -LiteralPath $probe -Force -ErrorAction SilentlyContinue

            $shimPath = Join-Path $dir "libra.cmd"
            Set-Content -LiteralPath $shimPath -Encoding ASCII -Force -Value @(
                "@echo off",
                $ShimMarker,
                "`"$TargetExe`" %*"
            )
            Write-Info "Installed CMD shim: $shimPath"
            return $dir
        } catch {
            continue
        }
    }

    Write-Info "Could not create a CMD shim. Reopen your terminal if libra is not found immediately."
    Write-Info "Manual PATH fallback: set PATH=%PATH%;$InstallBin"
    return $null
}

# The optional `lba` shorthand, the Windows counterpart of install.sh's
# `lba -> libra` symlink (plan-20260714 PD-10).
#
# Safety contract, matching the POSIX installer:
# - an existing `lba.*` this installer did not write is NEVER overwritten,
#   because `lba` is short enough to belong to something else;
# - rewriting our own shim is idempotent, so reinstalling is a no-op;
# - opting out via -NoAlias / LIBRA_NO_ALIAS is honoured;
# - failure to create the shim warns, it does not fail the installation.
function Install-LbaAlias {
    param(
        [string]$TargetExe,
        [string]$ShimDir
    )

    if ($NoAlias -or $env:LIBRA_NO_ALIAS -eq "1") {
        Write-Info "lba alias: disabled"
        return
    }
    if ([string]::IsNullOrWhiteSpace($ShimDir)) {
        Write-Info "lba alias: skipped - no writable shim directory"
        return
    }

    # Any spelling Windows would resolve ahead of, or instead of, our shim.
    foreach ($ext in @(".exe", ".bat", ".ps1")) {
        $rival = Join-Path $ShimDir "lba$ext"
        if (Test-Path -LiteralPath $rival) {
            Write-Info "lba alias: skipped - $rival already exists and was not installed by libra"
            return
        }
    }

    $aliasPath = Join-Path $ShimDir "lba.cmd"
    if (Test-Path -LiteralPath $aliasPath) {
        $existing = ""
        try {
            $existing = Get-Content -LiteralPath $aliasPath -Raw -ErrorAction Stop
        } catch {
            $existing = ""
        }
        if ($existing -notmatch [regex]::Escape($ShimMarker)) {
            Write-Info "lba alias: skipped - $aliasPath already exists and was not installed by libra"
            return
        }
    }

    try {
        Set-Content -LiteralPath $aliasPath -Encoding ASCII -Force -Value @(
            "@echo off",
            $ShimMarker,
            "`"$TargetExe`" %*"
        )
        Write-Info "lba alias: $aliasPath -> $TargetExe"
    } catch {
        Write-Info "lba alias: skipped - could not write $aliasPath"
    }
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $Version = $DefaultVersion
}
if ($Version -notmatch "^v") {
    $Version = "v$Version"
}

$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -notin @("AMD64", "X64")) {
    throw "Unsupported Windows architecture '$arch'. Libra currently provides a Windows amd64 installer only."
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = Join-Path $env:LOCALAPPDATA "libra\bin"
}

$DownloadBaseUrl = $DownloadBaseUrl.TrimEnd("/")
$downloadUrl = "$DownloadBaseUrl/libra/releases/$Version/$ReleaseAsset"
$tempDir = Join-Path $env:TEMP "libra-install"
$tempExe = Join-Path $tempDir $ReleaseAsset
$targetExe = Join-Path $InstallDir $ExeName

Write-Info "Target version: $Version"
Write-Info "Downloading: $downloadUrl"

Ensure-Directory $tempDir
Ensure-Directory $InstallDir

try {
    $ProgressPreference = "SilentlyContinue"
    Invoke-WebRequest -Uri $downloadUrl -OutFile $tempExe -UseBasicParsing

    if (-not (Test-Path -LiteralPath $tempExe)) {
        throw "Download failed: $downloadUrl"
    }

    Move-Item -LiteralPath $tempExe -Destination $targetExe -Force
    Write-Info "Installed to: $targetExe"

    Add-UserPath $InstallDir
    $shimDir = Install-CmdShim -TargetExe $targetExe -InstallBin $InstallDir
    Install-LbaAlias -TargetExe $targetExe -ShimDir $shimDir

    Write-Info "Installation complete."
    Write-Info "Run: libra --version"
} finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
