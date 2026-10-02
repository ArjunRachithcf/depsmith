<#
.SYNOPSIS
Install the depsmith executable from a GitHub release, verified against the
release's SHA256SUMS before anything is installed.

.EXAMPLE
irm https://github.com/ArjunRachithcf/depsmith/releases/latest/download/install.ps1 | iex

.EXAMPLE
pwsh install.ps1 -Version v0.1.0 -Prefix C:\Tools\depsmith

.PARAMETER Version
Release tag to install. Default: the latest release; with -Pre, the latest
release including pre-releases.

.PARAMETER Prefix
Directory for the executable. Default: %LOCALAPPDATA%\depsmith\bin
(~/.local/bin elsewhere). PATH is never changed; add it yourself.

.PARAMETER Target
Override the detected target triple.

.PARAMETER BaseUrl
Release download base, for tests and mirrors.
#>
param(
    [string]$Version = "",
    [switch]$Pre,
    [string]$Prefix = "",
    [string]$Target = "",
    [string]$BaseUrl = "https://github.com/ArjunRachithcf/depsmith/releases/download"
)
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$Repo = "ArjunRachithcf/depsmith"

function Fail([string]$Message) {
    [Console]::Error.WriteLine("depsmith install: $Message")
    exit 1
}

$windows = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [System.Runtime.InteropServices.OSPlatform]::Windows)
if (-not $Prefix) {
    $Prefix = if ($windows) { Join-Path $env:LOCALAPPDATA "depsmith\bin" }
              else { Join-Path $HOME ".local/bin" }
}
if (-not $Target) {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    if ($windows -and $arch -eq "X64") { $Target = "x86_64-pc-windows-msvc" }
    else { $Target = "$arch-unsupported" }
}
$supported = @("x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu",
               "x86_64-apple-darwin", "aarch64-apple-darwin")
if ($supported -notcontains $Target) {
    Fail "no prebuilt depsmith for $Target; install it with 'cargo install depsmith' or 'pip install depsmith'"
}
$local = $BaseUrl -match '^http://(127\.0\.0\.1|localhost):'
if (-not $local -and -not $BaseUrl.StartsWith("https://")) { Fail "only https URLs are fetched" }

if (-not $Version) {
    $api = "https://api.github.com/repos/$Repo/releases"
    try {
        $release = if ($Pre) { (Invoke-RestMethod "$api`?per_page=1")[0] }
                   else { Invoke-RestMethod "$api/latest" }
        $Version = $release.tag_name
    } catch { Fail "cannot find the latest release; pass -Version vX.Y.Z" }
}

$asset = "depsmith-$Target.tar.gz"
$work = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $file = Join-Path $work $asset
    $sums = Join-Path $work "SHA256SUMS"
    try {
        Invoke-WebRequest -UseBasicParsing "$BaseUrl/$Version/$asset" -OutFile $file
        Invoke-WebRequest -UseBasicParsing "$BaseUrl/$Version/SHA256SUMS" -OutFile $sums
    } catch { Fail "cannot download $asset or SHA256SUMS from ${Version}: $_" }

    $expected = Get-Content $sums | ForEach-Object {
        $fields = $_ -split '\s+', 2
        if ($fields.Count -eq 2 -and $fields[1].TrimStart('*') -eq $asset) { $fields[0] }
    } | Select-Object -First 1
    if (-not $expected) { Fail "SHA256SUMS of $Version has no entry for $asset" }
    $actual = (Get-FileHash -Algorithm SHA256 $file).Hash.ToLowerInvariant()
    if ($actual -ne $expected.ToLowerInvariant()) {
        Fail "checksum mismatch for $asset (expected $expected, got $actual); nothing installed"
    }

    $unpacked = Join-Path $work "unpacked"
    New-Item -ItemType Directory -Path $unpacked | Out-Null
    tar -xzf $file -C $unpacked
    if ($LASTEXITCODE -ne 0) { Fail "cannot unpack $asset" }
    $name = if ($Target -like "*windows*") { "depsmith.exe" } else { "depsmith" }
    $program = Join-Path $unpacked $name
    if (-not (Test-Path $program -PathType Leaf)) { Fail "$asset has no $name" }
    New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
    $destination = Join-Path $Prefix $name
    Copy-Item $program "$destination.new" -Force
    Move-Item "$destination.new" $destination -Force
    Write-Output "Installed depsmith $Version ($Target) to $destination"
    $paths = $env:PATH -split [System.IO.Path]::PathSeparator
    if ($paths -notcontains $Prefix) { Write-Output "Add $Prefix to your PATH to run depsmith." }
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
