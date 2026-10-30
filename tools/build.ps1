<#
.SYNOPSIS
  Builds the NSIS installer and files it under builds\<version>-build.<NNNN>\.

.DESCRIPTION
  Every run produces a NEW folder under <repo>\builds; nothing already there is
  ever deleted or overwritten. The build number is one higher than the highest
  existing folder (and than builds\.last-build), so numbers are never reused even
  if an old folder is removed by hand.

  The cargo target directory stays on a local disk (default
  %USERPROFILE%\cargo-target\vram-gatekeeper): Windows refuses to run freshly
  built programs from a network drive. Only the finished outputs are copied into
  builds\.

  Steps: pre-flight checks, tools\fetch-engine.ps1 if an engine is missing,
  tools\ui.ps1 build, `cargo tauri build --ci`, then copy the installer and write
  manifest.json.

.PARAMETER TargetDir
  Local cargo target directory. Default: $env:CARGO_TARGET_DIR, else
  %USERPROFILE%\cargo-target\vram-gatekeeper.

.EXAMPLE
  pwsh tools/build.ps1
#>
[CmdletBinding()]
param(
    [string] $TargetDir = $(if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $env:USERPROFILE 'cargo-target\vram-gatekeeper' })
)

$ErrorActionPreference = 'Stop'
$started = Get-Date
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$buildsRoot = Join-Path $repo 'builds'
$tauriDir = Join-Path $repo 'app\src-tauri'

$conf = Get-Content (Join-Path $tauriDir 'tauri.conf.json') -Raw | ConvertFrom-Json
$version = $conf.version
$productName = $conf.productName
$engineTag = (Get-Content (Join-Path $PSScriptRoot 'engine-rules.json') -Raw | ConvertFrom-Json).tag

# --- Pick the next build number (never reuse one) ---------------------------
New-Item -ItemType Directory -Force $buildsRoot | Out-Null
$counterFile = Join-Path $buildsRoot '.last-build'
$last = 0
if (Test-Path $counterFile) { $last = [int](Get-Content $counterFile -Raw).Trim() }
Get-ChildItem $buildsRoot -Directory | ForEach-Object {
    if ($_.Name -match '-build\.(\d+)$') { $last = [Math]::Max($last, [int]$Matches[1]) }
}
$number = $last + 1
$buildId = '{0}-build.{1:D4}' -f $version, $number
$outDir = Join-Path $buildsRoot $buildId
if (Test-Path $outDir) { throw "$outDir already exists; refusing to touch an existing build." }

# --- Pre-flight -------------------------------------------------------------
$running = Get-Process -Name 'vram-gatekeeper*' -ErrorAction SilentlyContinue
if ($running) { throw "VRAM Gatekeeper is running (pid $($running.Id -join ', ')); close it so the link step can replace the exe." }

foreach ($id in 'vulkan', 'cuda') {
    if (-not (Test-Path (Join-Path $tauriDir "resources\engine\$id\llama-server.exe"))) {
        Write-Host "Engine '$id' is not staged; running fetch-engine.ps1"
        & (Join-Path $PSScriptRoot 'fetch-engine.ps1')
        if ($LASTEXITCODE) { throw "fetch-engine.ps1 failed ($LASTEXITCODE)" }
        break
    }
}

$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if ($env:PATH -notlike "*$cargoBin*") { $env:PATH = "$cargoBin;$env:PATH" }
$env:CARGO_TARGET_DIR = $TargetDir

# --- Build ------------------------------------------------------------------
Write-Host "== Build $buildId =="
& (Join-Path $PSScriptRoot 'ui.ps1') build
if ($LASTEXITCODE) { throw "UI build failed ($LASTEXITCODE)" }

Push-Location $tauriDir
try {
    cargo tauri build --ci
    if ($LASTEXITCODE) { throw "cargo tauri build failed ($LASTEXITCODE)" }
}
finally { Pop-Location }

# The bundler writes the same file name every time, so make sure this one is fresh.
$installer = Join-Path $TargetDir "release\bundle\nsis\${productName}_${version}_x64-setup.exe"
if (-not (Test-Path $installer)) { throw "Installer not found at $installer" }
$item = Get-Item $installer
if ($item.LastWriteTime -lt $started) { throw "Installer at $installer predates this build ($($item.LastWriteTime)); not copying a stale file." }

# --- File the result --------------------------------------------------------
New-Item -ItemType Directory $outDir | Out-Null
$installerName = "${productName}_${buildId}_x64-setup.exe"
Copy-Item $installer (Join-Path $outDir $installerName)
$hash = (Get-FileHash (Join-Path $outDir $installerName) -Algorithm SHA256).Hash.ToLowerInvariant()

[ordered]@{
    build_id         = $buildId
    build_number     = $number
    app_version      = $version
    built_at         = (Get-Date).ToString('o')
    installer        = $installerName
    installer_bytes  = $item.Length
    installer_sha256 = $hash
    llama_cpp_tag    = $engineTag
} | ConvertTo-Json | Set-Content (Join-Path $outDir 'manifest.json') -Encoding utf8

Set-Content $counterFile $number -Encoding ascii
Write-Host "Done: $outDir\$installerName"
