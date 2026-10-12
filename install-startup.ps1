<#
.SYNOPSIS
    Adds (or removes) a Startup shortcut so Mic Hopper launches at login.

.EXAMPLE
    .\install-startup.ps1            # create shortcut to target\release\mic-hopper.exe
    .\install-startup.ps1 -Remove    # delete the shortcut
    .\install-startup.ps1 -Exe "C:\Tools\mic-hopper.exe"   # point at a different build
#>
[CmdletBinding()]
param(
    [string]$Exe,
    [switch]$Remove
)

$ErrorActionPreference = "Stop"

if (-not $Exe) {
    $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    $Exe = Join-Path $scriptDir "target\release\mic-hopper.exe"
}

$startupDir = [Environment]::GetFolderPath("Startup")
$linkPath = Join-Path $startupDir "Mic Hopper.lnk"

if ($Remove) {
    if (Test-Path -LiteralPath $linkPath) {
        Remove-Item -LiteralPath $linkPath
        Write-Host "Removed $linkPath"
    } else {
        Write-Host "No startup shortcut found at $linkPath"
    }
    return
}

$Exe = (Resolve-Path -LiteralPath $Exe).Path
$exeDir = Split-Path -Parent $Exe

if (-not (Test-Path -LiteralPath (Join-Path $exeDir "mic-hopper.toml"))) {
    Write-Warning "No mic-hopper.toml next to $Exe; the app reads its config from there."
}

$shell = New-Object -ComObject WScript.Shell
$link = $shell.CreateShortcut($linkPath)
$link.TargetPath = $Exe
$link.WorkingDirectory = $exeDir
$link.WindowStyle = 7            # start minimized; the app hides its console anyway
$link.Description = "Mic Hopper - automatic microphone switcher"
$link.IconLocation = "$Exe,0"
$link.Save()

Write-Host "Created $linkPath"
Write-Host "  -> $Exe"
Write-Host "Mic Hopper will start at next login. Run it now with: & `"$Exe`""
