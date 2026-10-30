<#
.SYNOPSIS
  Runs UI tooling (install, check, lint, build, dev) for app/ui.

.DESCRIPTION
  Windows will not run the native programs inside node_modules (esbuild, rolldown,
  the Tauri CLI) when they live on a network drive, and junctions or symlinks to a
  local disk cannot be created there without admin rights. So when the repository
  is on a network drive, this script mirrors app/ui to a local folder, runs npm
  there, and copies `dist` back. On a normal local checkout it simply runs npm in
  place.

.PARAMETER Task
  install | check | lint | build | dev

.PARAMETER Mirror
  Local folder used when the repo is on a network drive.
  Default: %USERPROFILE%\gatekeeper-dev\ui-mirror (or $env:GATEKEEPER_UI_MIRROR).

.EXAMPLE
  pwsh tools/ui.ps1 build
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('install', 'check', 'lint', 'build', 'dev')]
    [string] $Task = 'build',
    [string] $Mirror = $(if ($env:GATEKEEPER_UI_MIRROR) { $env:GATEKEEPER_UI_MIRROR } else { Join-Path $env:USERPROFILE 'gatekeeper-dev\ui-mirror' })
)

$ErrorActionPreference = 'Stop'
$src = (Resolve-Path (Join-Path $PSScriptRoot '..\app\ui')).Path
$drive = (Get-Item $src).PSDrive
$onNetwork = $drive -and $drive.DisplayRoot -like '\\*'

function Sync-Robocopy([string] $from, [string] $to, [string[]] $excludeDirs) {
    New-Item -ItemType Directory -Force $to | Out-Null
    # /MIR mirrors; excluded directories are neither copied nor deleted. Exit codes 0-7 mean success.
    robocopy $from $to /MIR /XD @excludeDirs /NFL /NDL /NJH /NJS /NP | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "robocopy failed ($LASTEXITCODE) copying $from to $to" }
}

$work = $src
if ($onNetwork) {
    Write-Host "Repo is on a network drive; building in local mirror $Mirror"
    Sync-Robocopy $src $Mirror @('node_modules', 'dist')
    $work = $Mirror
}

Push-Location $work
try {
    if ($Task -eq 'install' -or -not (Test-Path 'node_modules')) {
        npm install
        if ($LASTEXITCODE -ne 0) { throw 'npm install failed' }
        if ($onNetwork) { Copy-Item (Join-Path $work 'package-lock.json') $src -Force }
    }
    if ($Task -ne 'install') {
        npm run $Task
        if ($LASTEXITCODE -ne 0) { throw "npm run $Task failed" }
    }
}
finally {
    Pop-Location
}

if ($onNetwork -and $Task -eq 'build') {
    Sync-Robocopy (Join-Path $work 'dist') (Join-Path $src 'dist') @()
    Write-Host "Copied dist back to $src\dist"
}
