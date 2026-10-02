<#
.SYNOPSIS
Install the depsmith executable from a GitHub release, verified against the
release's SHA256SUMS before anything is installed.

.DESCRIPTION
Piped through Invoke-Expression, options come from environment variables:
  $env:DEPSMITH_VERSION = "v0.1.0"; $env:DEPSMITH_PRE = "1"; $env:DEPSMITH_PREFIX = "C:\Tools"
  irm https://github.com/ArjunRachithcf/depsmith/releases/latest/download/install.ps1 | iex
Run as a file, use the parameters:
  pwsh install.ps1 -Version v0.1.0 -Prefix C:\Tools\depsmith

PATH is never changed; add the prefix yourself.

.PARAMETER Version
Release tag to install, such as v0.1.0. Default: the latest release.

.PARAMETER Pre
With no -Version, take the newest release including pre-releases.

.PARAMETER Prefix
Directory for the executable. Default: %LOCALAPPDATA%\depsmith\bin
(~/.local/bin elsewhere).

.PARAMETER Target
Override the detected platform.

.PARAMETER BaseUrl
Releases URL, for mirrors and tests.
#>
param(
    [string]$Version = $env:DEPSMITH_VERSION,
    [switch]$Pre,
    [string]$Prefix = $env:DEPSMITH_PREFIX,
    [string]$Target = "",
    [string]$BaseUrl = "https://github.com/ArjunRachithcf/depsmith/releases"
)

# Everything runs in this function so that, piped through iex, an error is
# thrown (not `exit`, which would close the session) and no preference or
# variable leaks into the caller's session.
function Install-Depsmith([string]$Version, [bool]$Pre, [string]$Prefix,
                          [string]$Target, [string]$BaseUrl) {
    $ErrorActionPreference = "Stop"
    $ProgressPreference = "SilentlyContinue"
    $api = if ($env:DEPSMITH_INSTALL_API) { $env:DEPSMITH_INSTALL_API }
           else { "https://api.github.com/repos/ArjunRachithcf/depsmith/releases" }
    # Windows PowerShell 5.1 may not offer TLS 1.2 by default.
    if ($PSVersionTable.PSVersion.Major -lt 6) {
        [Net.ServicePointManager]::SecurityProtocol =
            [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    }

    $windows = $PSVersionTable.PSVersion.Major -lt 6 -or $IsWindows
    if (-not $Prefix) {
        $Prefix = if ($windows) { Join-Path $env:LOCALAPPDATA "depsmith\bin" }
                  else { Join-Path $HOME ".local/bin" }
    }
    if (-not $Target) {
        $arch = if ($env:PROCESSOR_ARCHITECTURE) { $env:PROCESSOR_ARCHITECTURE } else { "unknown" }
        # Windows on Arm runs the x64 build under emulation.
        if ($windows -and @("AMD64", "ARM64") -contains $arch) { $Target = "x86_64-pc-windows-msvc" }
        else { $Target = "$arch-unsupported" }
    }
    $supported = @("x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu",
                   "x86_64-apple-darwin", "aarch64-apple-darwin")
    if ($supported -notcontains $Target) {
        throw "no prebuilt depsmith for $Target; install it with 'cargo install depsmith' or 'pip install depsmith'"
    }
    $local = $BaseUrl -match '^http://(127\.0\.0\.1|localhost):'
    if (-not $local -and -not $BaseUrl.StartsWith("https://")) { throw "only https URLs are fetched" }

    if (-not $Version -and $Pre) {
        try { $Version = (Invoke-RestMethod "$api`?per_page=1")[0].tag_name }
        catch { throw "cannot find the newest release (GitHub API rate limit?); pass -Version vX.Y.Z" }
    }
    $from = if ($Version) { "$BaseUrl/download/$Version" } else { "$BaseUrl/latest/download" }

    $asset = "depsmith-$Target.tar.gz"
    $work = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid())
    New-Item -ItemType Directory -Path $work | Out-Null
    try {
        $file = Join-Path $work $asset
        $sums = Join-Path $work "SHA256SUMS"
        try {
            Invoke-WebRequest -UseBasicParsing "$from/$asset" -OutFile $file
            Invoke-WebRequest -UseBasicParsing "$from/SHA256SUMS" -OutFile $sums
        } catch { throw "cannot download $asset or SHA256SUMS from ${from}: $_" }

        $expected = Get-Content $sums | ForEach-Object {
            $fields = $_ -split '\s+', 2
            if ($fields.Count -eq 2 -and $fields[1].TrimStart('*') -eq $asset) { $fields[0] }
        } | Select-Object -First 1
        if (-not $expected) { throw "SHA256SUMS has no entry for $asset" }
        $actual = (Get-FileHash -Algorithm SHA256 $file).Hash.ToLowerInvariant()
        if ($actual -ne $expected.ToLowerInvariant()) {
            throw "checksum mismatch for $asset (expected $expected, got $actual); nothing installed"
        }

        $unpacked = Join-Path $work "unpacked"
        New-Item -ItemType Directory -Path $unpacked | Out-Null
        # Windows' own bsdtar: a GNU tar from Git would read C:\ as a host.
        $tar = if ($windows -and $env:SystemRoot) { Join-Path $env:SystemRoot "System32\tar.exe" } else { "tar" }
        & $tar -xzf $file -C $unpacked
        if ($LASTEXITCODE -ne 0) { throw "cannot unpack $asset" }
        $name = if ($Target -like "*windows*") { "depsmith.exe" } else { "depsmith" }
        $program = Join-Path $unpacked $name
        if (-not (Test-Path $program -PathType Leaf)) { throw "$asset has no $name" }
        New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
        $destination = Join-Path $Prefix $name
        Copy-Item $program "$destination.new" -Force
        try { Move-Item "$destination.new" $destination -Force }
        catch {
            Remove-Item "$destination.new" -Force -ErrorAction SilentlyContinue
            throw "cannot replace $destination (is depsmith running?): $_"
        }
        $label = if ($Version) { $Version } else { "latest" }
        Write-Output "Installed depsmith $label ($Target) to $destination"
        $paths = $env:PATH -split [System.IO.Path]::PathSeparator
        if ($paths -notcontains $Prefix) { Write-Output "Add $Prefix to your PATH to run depsmith." }
    } finally {
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }
}

$preFromEnv = @("1", "true", "yes") -contains "$env:DEPSMITH_PRE".ToLowerInvariant()
Install-Depsmith -Version $Version -Pre ($Pre.IsPresent -or $preFromEnv) -Prefix $Prefix `
    -Target $Target -BaseUrl $BaseUrl
