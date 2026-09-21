<#
.SYNOPSIS
    CITAR installer for Windows.

.DESCRIPTION
    iwr -useb https://raw.githubusercontent.com/jprodgers/CITAR/main/install.ps1 | iex

    For most people the downloadable installer from the releases page is the better route: it needs
    no Python and adds a Start Menu entry. This script is for anyone who would rather have CITAR as
    a command they can run, upgrade and script.

    It installs CITAR into its own virtual environment under %LOCALAPPDATA%, puts `citar` on the
    PATH for the current user, and then runs the setup wizard. Nothing is installed system-wide and
    it never needs an administrator.

.PARAMETER Worker
    Install and configure the worker agent, which serves this machine's models to a CITAR server
    somewhere else.

.PARAMETER Version
    Install a specific release instead of the newest.

.PARAMETER NoSetup
    Install only; do not run the setup wizard.

.PARAMETER Uninstall
    Remove what this script installed. Saved games and settings are left alone.
#>
[CmdletBinding()]
param(
    [switch]$Worker,
    [string]$Version = "",
    [switch]$NoSetup,
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
$MinPython = [version]"3.11"
$Prefix = Join-Path $env:LOCALAPPDATA "CITAR\app"
$ShimDir = Join-Path $env:LOCALAPPDATA "CITAR\bin"

function Step($text) { Write-Host "==> " -ForegroundColor Cyan -NoNewline; Write-Host $text }
function Ok($text)   { Write-Host "ok   " -ForegroundColor Green -NoNewline; Write-Host $text }
function Warn($text) { Write-Host "warn " -ForegroundColor Yellow -NoNewline; Write-Host $text }
function Fail($text) { Write-Host "error " -ForegroundColor Red -NoNewline; Write-Host $text; exit 1 }

# ----------------------------------------------------------------------------- uninstall
if ($Uninstall) {
    Step "Removing CITAR"
    if (Test-Path $Prefix) { Remove-Item -Recurse -Force $Prefix }
    if (Test-Path $ShimDir) { Remove-Item -Recurse -Force $ShimDir }
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath -and $userPath.Contains($ShimDir)) {
        $cleaned = ($userPath -split ';' | Where-Object { $_ -and $_ -ne $ShimDir }) -join ';'
        [Environment]::SetEnvironmentVariable("Path", $cleaned, "User")
    }
    Ok "Removed the program."
    Write-Host ""
    Write-Host "Your games, settings and results were NOT deleted. They are in:"
    Write-Host "  $env:LOCALAPPDATA\CITAR"
    Write-Host "Delete that folder too if you want CITAR gone completely."
    exit 0
}

# ----------------------------------------------------------------------------- python
# The newest Python on a machine is often not the first one on the PATH, so the py launcher is
# asked first: it knows about every installation, including ones that were never added to PATH.
Step "Looking for Python $MinPython or newer"
$python = $null
$candidates = @()
if (Get-Command py -ErrorAction SilentlyContinue) {
    foreach ($tag in @("-3.14", "-3.13", "-3.12", "-3.11", "-3")) { $candidates += ,@("py", $tag) }
}
foreach ($name in @("python3", "python")) {
    if (Get-Command $name -ErrorAction SilentlyContinue) { $candidates += ,@($name, $null) }
}

foreach ($candidate in $candidates) {
    $exe, $arg = $candidate
    try {
        $args = @()
        if ($arg) { $args += $arg }
        $args += @("-c", "import sys; print('%d.%d' % sys.version_info[:2])")
        $reported = & $exe @args 2>$null
        if ($LASTEXITCODE -eq 0 -and [version]$reported -ge $MinPython) {
            $python = @{ Exe = $exe; Arg = $arg; Version = $reported }
            break
        }
    } catch { }
}

if (-not $python) {
    Fail @"
No Python $MinPython or newer was found.

Install it from https://www.python.org/downloads/ or with:
    winget install Python.Python.3.12

Tick "Add python.exe to PATH" in the installer, then run this script again.

If you would rather not install Python at all, download the CITAR installer from
https://github.com/jprodgers/CITAR/releases - it has everything built in.
"@
}
Ok "Python $($python.Version)"

# ----------------------------------------------------------------------------- install
Step "Installing CITAR into $Prefix"
New-Item -ItemType Directory -Force -Path $Prefix | Out-Null

$pythonArgs = @()
if ($python.Arg) { $pythonArgs += $python.Arg }
& $python.Exe @pythonArgs -m venv $Prefix
if ($LASTEXITCODE -ne 0) { Fail "Could not create a virtual environment in $Prefix" }

$venvPython = Join-Path $Prefix "Scripts\python.exe"
& $venvPython -m pip install --quiet --upgrade pip wheel

$extras = if ($Worker) { "worker,openai" } else { "all" }
$spec = if ($Version) { "citar[$extras]==$Version" } else { "citar[$extras]" }

Step "Downloading CITAR and its dependencies"
& $venvPython -m pip install --quiet $spec
if ($LASTEXITCODE -ne 0) { Fail "pip could not install $spec" }
Ok (& (Join-Path $Prefix "Scripts\citar.exe") --version)

# ----------------------------------------------------------------------------- PATH
# Shims rather than copies: an upgrade replaces the executables inside the venv, and a copy would
# go on launching the version that was current when it was made.
Step "Putting citar on your PATH"
New-Item -ItemType Directory -Force -Path $ShimDir | Out-Null
foreach ($name in @("citar", "citar-admin", "citar-worker", "citar-mcp")) {
    $target = Join-Path $Prefix "Scripts\$name.exe"
    if (-not (Test-Path $target)) { continue }
    $shim = Join-Path $ShimDir "$name.cmd"
    "@echo off`r`n`"$target`" %*" | Set-Content -Path $shim -Encoding ascii
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not $userPath) { $userPath = "" }
if (-not ($userPath -split ';' | Where-Object { $_ -eq $ShimDir })) {
    [Environment]::SetEnvironmentVariable("Path", ($userPath.TrimEnd(';') + ";" + $ShimDir).Trim(';'), "User")
    $env:Path = $env:Path + ";" + $ShimDir
    Warn "Added $ShimDir to your PATH. Open a new terminal for it to take effect everywhere."
}
Ok "Linked into $ShimDir"

# ----------------------------------------------------------------------------- setup
Write-Host ""
if ($NoSetup) {
    Ok "Installed. Run 'citar setup' when you are ready."
    exit 0
}

Step "Setting up"
$setupFlag = if ($Worker) { "--worker" } else { "--local" }
& (Join-Path $Prefix "Scripts\citar.exe") setup $setupFlag
exit $LASTEXITCODE
