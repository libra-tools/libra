# install.ps1 smoke harness — twenty-two scenarios (plan-20260821 A1-05).
#
#   pwsh -NoProfile -File tests/data/install-smoke/run.ps1
#
# Mirrors run.sh: the production installer is COPIED and only its
# clearly-marked trust/origin constants are rewritten (test public key,
# local fixture server); the production file must stay byte-identical.
# The verifier-unavailable scenarios do not exist here: the Ed25519
# verifier ships inside the script (ADR-UP01-06).
#
# Requires: pwsh 7+, python3 (fixture HTTP server).

$ErrorActionPreference = "Stop"

$SmokeDir = $PSScriptRoot
$RepoRoot = Resolve-Path (Join-Path $SmokeDir "../../..")
$Installer = Join-Path $RepoRoot "install.ps1"
$Fixtures = Join-Path $SmokeDir "fixtures"

$Work = Join-Path ([System.IO.Path]::GetTempPath()) "libra-ps1-smoke-$([guid]::NewGuid().ToString('n'))"
New-Item -ItemType Directory -Path $Work -Force | Out-Null
$Server = $null
$OriginalBytes = [IO.File]::ReadAllBytes($Installer)

function Cleanup {
    if ($null -ne $Server -and -not $Server.HasExited) { $Server.Kill() }
    Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue
}

function Fail([string]$Message) {
    Write-Error "FAIL: $Message" -ErrorAction Continue
    Cleanup
    exit 1
}

try {
    # ── fixture server ──────────────────────────────────────────────────────
    $DocRoot = Join-Path $Work "docroot"
    New-Item -ItemType Directory -Path $DocRoot -Force | Out-Null
    Copy-Item -Recurse (Join-Path $Fixtures "tree/*") $DocRoot -Force
    $PortFile = Join-Path $Work "port"
    $serverScript = @"
import http.server, socketserver, sys, os
os.chdir(sys.argv[1])
handler = http.server.SimpleHTTPRequestHandler
handler.log_message = lambda *a, **k: None
socketserver.TCPServer.allow_reuse_address = True
httpd = socketserver.TCPServer(("127.0.0.1", 0), handler)
open(sys.argv[2], "w").write(str(httpd.server_address[1]))
httpd.serve_forever()
"@
    $serverPath = Join-Path $Work "server.py"
    Set-Content -LiteralPath $serverPath -Value $serverScript
    $Server = Start-Process -FilePath "python3" -ArgumentList @($serverPath, $DocRoot, $PortFile) -PassThru -NoNewWindow
    for ($i = 0; $i -lt 20 -and -not (Test-Path $PortFile); $i++) { Start-Sleep -Milliseconds 300 }
    if (-not (Test-Path $PortFile)) { Fail "fixture server did not report a port" }
    $Base = "http://127.0.0.1:$(Get-Content $PortFile)"

    # ── prepared installer copy ─────────────────────────────────────────────
    $copy = Get-Content -Raw $Installer
    function Rewrite([string]$From, [string]$To) {
        $script:copy = $copy
        if (($copy -split [regex]::Escape($From)).Count -ne 2) { Fail "marker drift: $From" }
        $script:copy = $copy.Replace($From, $To)
    }
    Rewrite '$ReleaseManifestKeyId = "libra-release-1"' '$ReleaseManifestKeyId = "libra-release-test-1"'
    Rewrite '$ReleaseManifestPublicKeyHex = "68aa00ea9358d455645010d811d40702b3f67cec4bdff52d3d4fb8107afaeed3"' '$ReleaseManifestPublicKeyHex = "a8a00ded13ddafaad525fabddc13efc717b29ebed50cd6d653196057fa8f8a43"'
    Rewrite '$ReleaseManifestOrigin = "https://download.libra.tools"' ('$ReleaseManifestOrigin = "' + $Base + '"')
    # The test keypair's validity window (fixtures are signed inside it).
    Rewrite '$ReleaseManifestKeyNotBefore = "2026-08-31T11:09:55Z"' '$ReleaseManifestKeyNotBefore = "2026-01-01T00:00:00Z"'
    Rewrite '$ReleaseManifestKeyNotAfter = "2027-08-31T00:00:00Z"' '$ReleaseManifestKeyNotAfter = "2028-01-01T00:00:00Z"'
    Rewrite '[string]$DownloadBaseUrl = "https://download.libra.tools",' ('[string]$DownloadBaseUrl = "' + $Base + '",')
    $copy = $copy -replace '\$DefaultVersion = "v[0-9][0-9.]*"', '$DefaultVersion = "v9.9.8"'
    $CopyPath = Join-Path $Work "install-copy.ps1"
    Set-Content -LiteralPath $CopyPath -Value $copy

    # ── scenario runner ─────────────────────────────────────────────────────
    $script:ScenariosRun = 0
    function Run-Scenario([string]$Name, [string]$Manifest, [string]$Expect, [string]$ExpectInstalled, [string]$Needle, [hashtable]$ExtraEnv = @{}) {
        $stableDir = Join-Path $DocRoot "libra/releases/stable"
        Remove-Item -LiteralPath (Join-Path $stableDir "manifest-v1.json") -Force -ErrorAction SilentlyContinue
        if ($Manifest -ne "-none-") {
            New-Item -ItemType Directory -Path $stableDir -Force | Out-Null
            Copy-Item (Join-Path $Fixtures $Manifest) (Join-Path $stableDir "manifest-v1.json") -Force
        }

        $scenarioHome = Join-Path $Work "home-$Name"
        New-Item -ItemType Directory -Path $scenarioHome -Force | Out-Null
        $installDir = Join-Path $scenarioHome "bin"

        $envBackup = @{}
        $scenarioEnv = @{
            PROCESSOR_ARCHITECTURE = "AMD64"
            LOCALAPPDATA           = $scenarioHome
            USERPROFILE            = $scenarioHome
            TEMP                   = (Join-Path $scenarioHome "tmp")
            LIBRA_VERSION          = $null
            LIBRA_ALLOW_FALLBACK   = $null
            LIBRA_NO_ALIAS         = "1"
        }
        # Hashtable '+' throws on duplicate keys; ExtraEnv must OVERRIDE.
        foreach ($key in $ExtraEnv.Keys) { $scenarioEnv[$key] = $ExtraEnv[$key] }
        foreach ($key in $scenarioEnv.Keys) {
            $envBackup[$key] = [Environment]::GetEnvironmentVariable($key)
            [Environment]::SetEnvironmentVariable($key, $scenarioEnv[$key])
        }
        New-Item -ItemType Directory -Path (Join-Path $scenarioHome "tmp") -Force | Out-Null

        $failed = $false
        $output = ""
        try {
            $output = & pwsh -NoProfile -File $CopyPath -InstallDir $installDir -NoModifyPath 2>&1 | Out-String
        } catch {
            $failed = $true
            $output += $_.Exception.Message
        }
        if ($LASTEXITCODE -ne 0) { $failed = $true }
        foreach ($key in $envBackup.Keys) {
            [Environment]::SetEnvironmentVariable($key, $envBackup[$key])
        }

        if ($Expect -eq "ok" -and $failed) { Fail "${Name}: expected success`n$output" }
        if ($Expect -eq "fail" -and -not $failed) { Fail "${Name}: expected failure`n$output" }
        $installed = Test-Path (Join-Path $installDir "libra.exe")
        if ($ExpectInstalled -eq "yes" -and -not $installed) { Fail "${Name}: expected an installed libra.exe`n$output" }
        if ($ExpectInstalled -eq "no" -and $installed) { Fail "${Name}: a binary landed on a fail-closed path`n$output" }
        # Error records wrap at console width and ConciseView inserts "|"
        # gutter decorations between fragments; strip both so multi-word
        # needles match regardless of where the renderer broke the line.
        $flatOutput = ($output -replace '[|]', ' ') -replace '\s+', ' '
        if ($flatOutput -notmatch [regex]::Escape($Needle)) { Fail "${Name}: output does not mention '$Needle'`n$output" }
        $script:ScenariosRun++
        Write-Host "ok: $Name"
    }

    Run-Scenario "valid" "manifest-valid.json" "ok" "yes" "Signed stable manifest verified"
    # The verified install records signed provenance for `libra upgrade`.
    $validMarker = Join-Path $Work "home-valid/bin/.libra-official-install.json"
    if (-not (Test-Path -LiteralPath $validMarker)) { Fail "valid: official-install marker missing" }
    $markerText = Get-Content -Raw -LiteralPath $validMarker
    if ($markerText -cnotmatch '"install_source":"official_signed_manifest"') { Fail "valid: marker lacks official install_source" }
    if ($markerText -cnotmatch '"version":"9.9.9"') { Fail "valid: marker version wrong" }
    Run-Scenario "bad-signature" "manifest-bad-signature.json" "fail" "no" "SIGNATURE VERIFICATION FAILED"
    Run-Scenario "sha-mismatch" "manifest-sha-mismatch.json" "fail" "no" "sha256 mismatch against the SIGNED manifest"
    Run-Scenario "size-mismatch" "manifest-size-mismatch.json" "fail" "no" "does not match the signed size"
    Run-Scenario "expired" "manifest-expired.json" "fail" "no" "is expired"
    Run-Scenario "paused" "manifest-paused.json" "fail" "no" "PAUSED"
    Run-Scenario "revoked" "manifest-revoked.json" "fail" "no" "REVOKED"
    Run-Scenario "stale-replay" "manifest-stale-replay.json" "fail" "no" "older than this installer's baseline"
    Run-Scenario "tampered-payload" "manifest-tampered-payload.json" "fail" "no" "SIGNATURE VERIFICATION FAILED"
    Run-Scenario "zero-size" "manifest-zero-size.json" "fail" "no" "outside (0, 128 MiB]"
    Run-Scenario "future-min-key" "manifest-future-min-key.json" "fail" "no" "min_key_generation"
    Run-Scenario "key-window" "manifest-key-window.json" "fail" "no" "validity window"
    Run-Scenario "noncanonical" "manifest-noncanonical.json" "fail" "no" "canonical serialization"
    Run-Scenario "bad-calendar" "manifest-bad-calendar.json" "fail" "no" "2026-09-31"
    Run-Scenario "huge-min-key" "manifest-huge-min-key.json" "fail" "no" "canonical serialization"
    Run-Scenario "trailing-artifact" "manifest-trailing-artifact.json" "fail" "no" "canonical serialization"
    Run-Scenario "pretty-envelope" "manifest-pretty-envelope.json" "ok" "yes" "Signed stable manifest verified"
    Run-Scenario "undersized" "manifest-undersized.json" "fail" "no" "does not match the signed size"
    Run-Scenario "multiline-payload" "manifest-multiline-payload.json" "fail" "no" "canonical serialization"
    Run-Scenario "huge-semver" "manifest-huge-semver.json" "fail" "no" "not canonical X.Y.Z"
    Run-Scenario "transition-404" "-none-" "fail" "no" "signature chain is not enabled yet"
    Run-Scenario "transition-404-fallback" "-none-" "ok" "yes" "proceeding UNVERIFIED" @{ LIBRA_ALLOW_FALLBACK = "1" }
    if (Test-Path -LiteralPath (Join-Path $Work "home-transition-404-fallback/bin/.libra-official-install.json")) {
        Fail "transition-404-fallback: an UNVERIFIED install must not carry the official marker"
    }

    # ── production file untouched ───────────────────────────────────────────
    $after = [IO.File]::ReadAllBytes($Installer)
    if (-not [System.Linq.Enumerable]::SequenceEqual($OriginalBytes, $after)) {
        Fail "the production install.ps1 was modified by the harness"
    }
    if ($ScenariosRun -ne 22) { Fail "expected 22 scenarios, ran $ScenariosRun" }
    Write-Host "install.ps1 smoke: all $ScenariosRun scenarios passed"
} finally {
    Cleanup
}
