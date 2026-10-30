<#
.SYNOPSIS
  Downloads, verifies and extracts the pinned llama.cpp engines that ship in the
  installer (ADR-0011, spec §25.2).

.DESCRIPTION
  Reads tools/engine-rules.json, downloads each listed asset from the GitHub
  release, checks its SHA-256 against the pinned value, and extracts every
  engine into <OutDir>\<engine id>\ (llama-server.exe and its DLLs side by side).
  Downloads are cached, so re-running is cheap. A hash mismatch stops the build.

.PARAMETER Engines
  Which engine ids to stage. Default: vulkan, cuda.

.PARAMETER OutDir
  Where engines are extracted. Default: app/src-tauri/resources/engine.
  NOTE: Windows may refuse to run programs from a network drive. For local
  testing pass a folder on a local disk.

.PARAMETER CacheDir
  Where downloaded zips are kept. Default: %LOCALAPPDATA%\vram-gatekeeper-build-cache.

.PARAMETER UpdatePins
  Instead of staging, query the GitHub API for -Tag, and rewrite the sha256
  values in engine-rules.json from the digests GitHub publishes. Review the diff.

.PARAMETER Tag
  Release tag to pin when using -UpdatePins.

.EXAMPLE
  pwsh tools/fetch-engine.ps1 -OutDir C:\gatekeeper-dev\engine
#>
[CmdletBinding()]
param(
    [string[]] $Engines = @('vulkan', 'cuda'),
    [string] $OutDir = (Join-Path $PSScriptRoot '..\app\src-tauri\resources\engine'),
    [string] $CacheDir = (Join-Path $env:LOCALAPPDATA 'vram-gatekeeper-build-cache'),
    [string] $Rules = (Join-Path $PSScriptRoot 'engine-rules.json'),
    [switch] $UpdatePins,
    [string] $Tag
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is much faster without the progress bar.

$pin = Get-Content $Rules -Raw | ConvertFrom-Json
# `pwsh -File` passes "a,b" as one string, so split it.
$Engines = @($Engines | ForEach-Object { $_ -split ',' } | Where-Object { $_ })

function Get-AssetName([string] $pattern) { return $pattern.Replace('{tag}', $pin.tag) }

if ($UpdatePins) {
    if (-not $Tag) { throw 'Pass -Tag <release tag> with -UpdatePins.' }
    $pin.tag = $Tag
    $release = Invoke-RestMethod -Headers @{ 'User-Agent' = 'vram-gatekeeper-build' } `
        -Uri "https://api.github.com/repos/$($pin.repo)/releases/tags/$Tag"
    foreach ($engine in $pin.engines) {
        foreach ($file in $engine.files) {
            $name = Get-AssetName $file.name
            $asset = $release.assets | Where-Object { $_.name -eq $name }
            if (-not $asset) { throw "Release $Tag has no asset named '$name'. Update the pattern in engine-rules.json." }
            if (-not $asset.digest -or -not $asset.digest.StartsWith('sha256:')) { throw "No sha256 digest published for $name." }
            $file.sha256 = $asset.digest.Substring('sha256:'.Length)
            Write-Host "pinned $name  $($file.sha256)"
        }
    }
    $pin | ConvertTo-Json -Depth 10 | Set-Content $Rules -Encoding utf8
    Write-Host "Updated $Rules. Review the diff, then re-run without -UpdatePins."
    return
}

New-Item -ItemType Directory -Force $CacheDir | Out-Null
New-Item -ItemType Directory -Force $OutDir | Out-Null

foreach ($id in $Engines) {
    $engine = $pin.engines | Where-Object { $_.id -eq $id }
    if (-not $engine) { throw "Unknown engine '$id'. Known: $(($pin.engines | ForEach-Object { $_.id }) -join ', ')" }

    $target = Join-Path $OutDir $id
    $stamp = Join-Path $target '.staged'
    $wanted = "$($pin.tag)|" + (($engine.files | ForEach-Object { $_.sha256 }) -join ',')
    if ((Test-Path $stamp) -and ((Get-Content $stamp -Raw).Trim() -eq $wanted)) {
        Write-Host "[$id] already staged ($($pin.tag))"
        continue
    }
    if (Test-Path $target) { Remove-Item -Recurse -Force $target }
    New-Item -ItemType Directory -Force $target | Out-Null

    foreach ($file in $engine.files) {
        $name = Get-AssetName $file.name
        $zip = Join-Path $CacheDir $name
        $hash = if (Test-Path $zip) { (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant() } else { '' }
        if ($hash -ne $file.sha256) {
            $url = "https://github.com/$($pin.repo)/releases/download/$($pin.tag)/$name"
            Write-Host "[$id] downloading $name"
            Invoke-WebRequest -Uri $url -OutFile $zip -Headers @{ 'User-Agent' = 'vram-gatekeeper-build' }
            $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        if ($hash -ne $file.sha256) {
            Remove-Item -Force $zip
            throw "SHA-256 mismatch for $name`n  expected $($file.sha256)`n  actual   $hash"
        }
        Write-Host "[$id] verified $name"
        Expand-Archive -Path $zip -DestinationPath $target -Force
    }

    if (-not (Test-Path (Join-Path $target 'llama-server.exe'))) {
        throw "[$id] llama-server.exe is missing after extraction. The release layout may have changed; check engine-rules.json."
    }
    Set-Content -Path $stamp -Value $wanted -Encoding ascii
    Write-Host "[$id] staged at $target"
}

Write-Host "Done. Engines in $OutDir"
