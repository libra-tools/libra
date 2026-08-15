param(
    [Parameter(Mandatory = $true)]
    [string]$RepoRoot
)

$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    throw "install alias Windows smoke: $Message"
}

function Get-FreePort {
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    try {
        $listener.Start()
        return ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    }
    finally {
        $listener.Stop()
    }
}

function Start-ReleaseServer([string]$BinaryPath) {
    $port = Get-FreePort
    $server = Start-Job -ArgumentList $port, $BinaryPath -ScriptBlock {
        param($Port, $Source)

        $listener = [Net.HttpListener]::new()
        $listener.Prefixes.Add("http://127.0.0.1:$Port/")
        $listener.Start()
        try {
            # The scenario performs five installs. Exiting after the fifth
            # response keeps cleanup from blocking on a pending GetContext().
            $requests = 0
            while ($requests -lt 5) {
                $context = $listener.GetContext()
                $requests++
                try {
                    if ($context.Request.Url.AbsolutePath -match '^/libra/releases/v[0-9]+\.[0-9]+\.[0-9]+[A-Za-z0-9.+-]*/libra-windows-amd64\.exe$') {
                        $context.Response.StatusCode = 200
                        $context.Response.ContentType = "application/octet-stream"
                        $context.Response.ContentLength64 = (Get-Item -LiteralPath $Source).Length
                        $input = [IO.File]::OpenRead($Source)
                        try {
                            $input.CopyTo($context.Response.OutputStream)
                        }
                        finally {
                            $input.Dispose()
                        }
                    }
                    else {
                        $context.Response.StatusCode = 404
                    }
                }
                finally {
                    $context.Response.Close()
                }
            }
        }
        finally {
            $listener.Stop()
            $listener.Close()
        }
    }
    Start-Sleep -Milliseconds 250
    if ($server.State -ne "Running") {
        Receive-Job -Job $server | Out-String | ForEach-Object { Fail "release server did not start: $_" }
    }
    return @{ Job = $server; BaseUrl = "http://127.0.0.1:$port" }
}

$work = Join-Path ([IO.Path]::GetTempPath()) "libra-install-alias-$([guid]::NewGuid().ToString('N'))"
$previousLocalAppData = $env:LOCALAPPDATA
$server = $null

try {
    $installer = Join-Path $RepoRoot "install.ps1"
    $source = Join-Path $env:SystemRoot "System32\cmd.exe"
    if (-not (Test-Path -LiteralPath $installer)) {
        Fail "missing installer: $installer"
    }
    if (-not (Test-Path -LiteralPath $source)) {
        Fail "missing Windows test executable: $source"
    }

    # The installer only requires a PE payload. cmd.exe keeps the local
    # release server small while still exercising the generated lba.cmd.
    $sourceVersion = "0.0.0"
    $server = Start-ReleaseServer $source

    function Invoke-Installer([string]$InstallDirectory, [string]$AppDataDirectory, [string[]]$ExtraArguments) {
        $env:LOCALAPPDATA = $AppDataDirectory
        $arguments = @(
            "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $installer,
            "-Version", $sourceVersion,
            "-InstallDir", $InstallDirectory,
            "-DownloadBaseUrl", $server.BaseUrl,
            "-NoModifyPath"
        ) + $ExtraArguments
        & powershell.exe @arguments
        if ($LASTEXITCODE -ne 0) {
            Fail "installer failed for $InstallDirectory"
        }
    }

    $installDir = Join-Path $work "managed"
    $appDataDir = Join-Path $work "managed-appdata"
    Invoke-Installer $installDir $appDataDir @()
    $shim = Join-Path $appDataDir "Microsoft\WindowsApps\lba.cmd"
    $libraVersion = (& (Join-Path $installDir "libra.exe") --version | Out-String).Trim()
    $lbaVersion = (& $shim --version | Out-String).Trim()
    if ($libraVersion -ne $lbaVersion) {
        Fail "lba --version differs from libra --version"
    }

    $foreignCmdDir = Join-Path $work "foreign-cmd"
    $foreignCmdAppData = Join-Path $work "foreign-cmd-appdata"
    $foreignCmdShim = Join-Path $foreignCmdAppData "Microsoft\WindowsApps\lba.cmd"
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $foreignCmdShim) | Out-Null
    $foreignCmdContent = "@echo foreign`r`n"
    [IO.File]::WriteAllText($foreignCmdShim, $foreignCmdContent)
    Invoke-Installer $foreignCmdDir $foreignCmdAppData @()
    if ([IO.File]::ReadAllText($foreignCmdShim) -ne $foreignCmdContent) {
        Fail "foreign lba.cmd was overwritten"
    }

    $foreignPs1Dir = Join-Path $work "foreign-ps1"
    $foreignPs1AppData = Join-Path $work "foreign-ps1-appdata"
    $foreignPs1Shim = Join-Path $foreignPs1AppData "Microsoft\WindowsApps\lba.ps1"
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $foreignPs1Shim) | Out-Null
    $foreignPs1Content = "Write-Output 'foreign'`r`n"
    [IO.File]::WriteAllText($foreignPs1Shim, $foreignPs1Content)
    Invoke-Installer $foreignPs1Dir $foreignPs1AppData @()
    if ([IO.File]::ReadAllText($foreignPs1Shim) -ne $foreignPs1Content) {
        Fail "foreign lba.ps1 was overwritten"
    }

    $noAliasDir = Join-Path $work "no-alias"
    $noAliasAppData = Join-Path $work "no-alias-appdata"
    Invoke-Installer $noAliasDir $noAliasAppData @("-NoAlias")
    if (Test-Path -LiteralPath (Join-Path $noAliasAppData "Microsoft\WindowsApps\lba.cmd")) {
        Fail "-NoAlias created lba.cmd"
    }

    $envAliasDir = Join-Path $work "env-no-alias"
    $envAliasAppData = Join-Path $work "env-no-alias-appdata"
    $previousNoAlias = $env:LIBRA_NO_ALIAS
    try {
        $env:LIBRA_NO_ALIAS = "1"
        Invoke-Installer $envAliasDir $envAliasAppData @()
    }
    finally {
        $env:LIBRA_NO_ALIAS = $previousNoAlias
    }
    if (Test-Path -LiteralPath (Join-Path $envAliasAppData "Microsoft\WindowsApps\lba.cmd")) {
        Fail "LIBRA_NO_ALIAS=1 created lba.cmd"
    }

    Write-Output "install alias Windows smoke: ok"
}
finally {
    $env:LOCALAPPDATA = $previousLocalAppData
    if ($null -ne $server) {
        Stop-Job -Job $server.Job -ErrorAction SilentlyContinue
        Remove-Job -Job $server.Job -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $work) {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}
