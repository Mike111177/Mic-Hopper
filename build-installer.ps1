<#
.SYNOPSIS
    Builds the release binary and packages it into dist\MikeHopper-Setup-<version>.exe.

.EXAMPLE
    .\build-installer.ps1
    .\build-installer.ps1 -SkipBuild    # reuse the existing target\release build

.NOTES
    Requires NSIS 3:  winget install NSIS.NSIS
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path

if (-not $SkipBuild) {
    cargo build --release --manifest-path (Join-Path $root "Cargo.toml")
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

$version = (Select-String -Path (Join-Path $root "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value

$makensis = Get-Command makensis -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
if (-not $makensis) {
    foreach ($candidate in @("${env:ProgramFiles(x86)}\NSIS\makensis.exe", "$env:ProgramFiles\NSIS\makensis.exe")) {
        if (Test-Path -LiteralPath $candidate) { $makensis = $candidate; break }
    }
}
if (-not $makensis) { throw "makensis not found. Install NSIS: winget install NSIS.NSIS" }

New-Item -ItemType Directory -Path (Join-Path $root "dist") -Force | Out-Null

Push-Location $root
try {
    & $makensis /V2 "/DVERSION=$version" installer.nsi
    if ($LASTEXITCODE -ne 0) { throw "makensis failed" }
} finally {
    Pop-Location
}

Write-Host "Built dist\MikeHopper-Setup-$version.exe"
