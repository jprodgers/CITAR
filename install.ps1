<#
.SYNOPSIS
    CITAR installer for Windows.

.DESCRIPTION
    & ([scriptblock]::Create((iwr -useb https://raw.githubusercontent.com/jprodgers/CITAR/main/install.ps1)))
    & ([scriptblock]::Create((iwr -useb https://raw.githubusercontent.com/jprodgers/CITAR/main/install.ps1))) -Worker

    Run as a script block like this, it takes parameters (iwr ... | iex passes none, and leaves the
    script's settings in your session) and ends without closing your PowerShell: $LASTEXITCODE is 1
    when it could not finish, 0 when it did.

    For most people the downloadable installer from the releases page is the better route: it needs
    no Python and adds a Start Menu entry. This script is for anyone who would rather have CITAR as
    a command they can run, upgrade and script.

    It checks that CITAR is built for this Python (an x64 CPython 3.11 or newer with the GIL),
    installs CITAR into its own virtual environment from the prebuilt wheel (never building it from
    source), puts `citar` on the PATH for the current user, and then runs the setup wizard. Nothing
    is installed system-wide and it never needs an administrator.

    With -Worker it installs the CITAR helper instead: the one program a machine needs to lend its
    models (and, if you choose, its spare CPU) to a CITAR server. No Python, no game: the helper
    built for Windows is downloaded with its .sha256, checked before anything is written, and put in
    the bin folder as citar-helper.exe. It updates itself when its server offers a signed update.

    CITAR_RELEASE_URL, when set, is where -Worker downloads from (by default
    https://github.com/jprodgers/CITAR/releases): an https address, or http to this machine only.

.PARAMETER Worker
    Install the CITAR helper (citar-helper.exe) instead of the game.

.PARAMETER Version
    Install a specific release instead of the newest.

.PARAMETER Prefix
    Where to install the game (default: %LOCALAPPDATA%\CITAR\app).

.PARAMETER BinDir
    Where the commands go (default: %LOCALAPPDATA%\CITAR\bin).

.PARAMETER NoPath
    Leave the user's PATH alone: nothing is written to the registry.

.PARAMETER From
    Install the game from a local wheel, or a folder of wheels, instead of PyPI.

.PARAMETER Python
    The Python to install the game with (default: the newest 3.11 or later the py launcher knows).

.PARAMETER NoSetup
    Install only; do not run the setup wizard.

.PARAMETER Uninstall
    Remove what this script installed (with -Worker: the helper only). Saved games and settings
    are left alone.
#>
[CmdletBinding(PositionalBinding = $false)]
param(
    [switch]$Worker,
    [string]$Version = "",
    [string]$Prefix = "",
    [string]$BinDir = "",
    [switch]$NoPath,
    [string]$From = "",
    [string]$Python = "",
    [switch]$NoSetup,
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"   # Windows PowerShell's progress bar slows a download many times over
$MinPython = [version]"3.11"
$ReleasesDefault = "https://github.com/jprodgers/CITAR/releases"

function Step($text) { Write-Host "==> " -ForegroundColor Cyan -NoNewline; Write-Host $text }
function Ok($text)   { Write-Host "ok   " -ForegroundColor Green -NoNewline; Write-Host $text }
function Warn($text) { Write-Host "warn " -ForegroundColor Yellow -NoNewline; Write-Host $text }
# Run as a file (-File, or .\install.ps1), the script ends with exit. Run as a script block (the one-liners above,
# or iwr ... | iex), exit would end the user's PowerShell itself, closing a console window before its message could
# be read: so there a failure throws $Stopped, which the catch at the end turns into $LASTEXITCODE 1, and an end
# returns. Nothing between Fail and that catch may catch $Stopped.
$AsFile = [bool]$PSCommandPath
$Stopped = "CITAR's installer stopped: the error above says why."
function Fail($text) {
    Write-Host "error " -ForegroundColor Red -NoNewline; Write-Host $text
    if ($AsFile) { exit 1 }
    throw $Stopped
}

# A native program, its standard error passing through: Windows PowerShell turns redirected standard error into
# errors, which "Stop" would make fatal.
function Invoke-Native([string]$Exe, [string[]]$Arguments) {
    $saved = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try { & $Exe @Arguments } finally { $ErrorActionPreference = $saved }
}

# ----------------------------------------------------------------------------- the facts
# What this machine and its Python are, as the checks read them. CITAR_INSTALL_TEST_FACTS ("name=value ...", for the
# tests) replaces any of the probed facts: system, machine, python (X.Y), implementation, python_platform (Python's
# sysconfig.get_platform(): win-amd64, win-arm64, win32) and free_threaded (0 or 1).
function Get-TestFact([string]$Name) {
    foreach ($pair in ("$env:CITAR_INSTALL_TEST_FACTS" -split '\s+')) {
        if ($pair.StartsWith("$Name=")) { return $pair.Substring($Name.Length + 1) }
    }
    return $null
}
function Fact([string]$Name, $Probed) {
    $given = Get-TestFact $Name
    if ($null -ne $given) { return $given }
    return "$Probed"
}

# One message for every platform CITAR is not built for (design/phase5-release.md P5.8).
function No-Prebuilt([string]$Why) {
    if ($Worker) {
        Fail @"
There is no prebuilt CITAR helper for this platform: $Why.
The helper builds from CITAR's source with a Rust toolchain (https://rustup.rs): in a clone of
https://github.com/jprodgers/CITAR, run  cargo build --release -p citar-helper
"@
    }
    Fail @"
There is no prebuilt CITAR for this platform: $Why.
'pip install citar' builds it from source, with a Rust toolchain (https://rustup.rs).
"@
}

# A folder named relative to here made absolute, its separators Windows' own and no trailing one: the shims name it,
# and the uninstall compares them with it.
function Full-Path([string]$Path) {
    $resolved = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
    return [IO.Path]::GetFullPath($resolved).TrimEnd('\', '/')
}

try {
    # ----------------------------------------------------------------------------- arguments
    $Version = $Version.TrimStart('v')
    if ($Version -and $Version -notmatch '^\d+\.\d+\.\d+(-(alpha|beta|rc)\.\d+)?$') {
        Fail "-Version $Version is not a CITAR release (X.Y.Z, or X.Y.Z-rc.N)"
    }
    if ($Worker -and $From) { Fail "-From installs the game from a wheel; -Worker downloads the helper: choose one" }

    $system = Fact "system" $(if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) { "Windows" } else { "other" })
    if ($system -ne "Windows") {
        Fail "This installer is for Windows. On Linux and macOS, run install.sh: https://github.com/jprodgers/CITAR"
    }

    if (-not $Prefix) { $Prefix = Join-Path $env:LOCALAPPDATA "CITAR\app" }
    $defaultBin = Join-Path $env:LOCALAPPDATA "CITAR\bin"
    if (-not $BinDir) { $BinDir = $defaultBin }
    $Prefix = Full-Path $Prefix
    $BinDir = Full-Path $BinDir
    if ($Prefix -eq (Full-Path $HOME) -or $Prefix -eq [IO.Path]::GetPathRoot($Prefix).TrimEnd('\')) {
        Fail "-Prefix $Prefix would hold more than CITAR: name a folder of its own"
    }

    # ----------------------------------------------------------------------------- PATH
    # The user's PATH as stored (HKCU\Environment), its %VARIABLES% unexpanded and its kind kept, so writing it back
    # changes nothing but the one entry; then Windows is told, so a new terminal sees it.
    function Get-UserPath {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment")
        try {
            if (-not $key) { return @{ Value = ""; Kind = [Microsoft.Win32.RegistryValueKind]::ExpandString } }
            $value = $key.GetValue("Path", "", [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
            if ($key.GetValueNames() -contains "Path") { $kind = $key.GetValueKind("Path") }
            return @{ Value = "$value"; Kind = $kind }
        } finally { if ($key) { $key.Close() } }
    }
    function Set-UserPath([string]$Value, $Kind) {
        $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey("Environment")
        try {
            # a PATH this script began (there was none) goes with its one entry, rather than staying as an empty value
            if ($Value) { $key.SetValue("Path", $Value, $Kind) } else { $key.DeleteValue("Path", $false) }
        } finally { $key.Close() }
        if (-not ("CitarInstaller.Broadcast" -as [type])) {
            Add-Type -Namespace CitarInstaller -Name Broadcast -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags,
    uint uTimeout, out UIntPtr lpdwResult);
'@
        }
        $result = [UIntPtr]::Zero
        [void][CitarInstaller.Broadcast]::SendMessageTimeout([IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, "Environment", 2, 5000,
            [ref]$result)
    }
    # The entry is appended as ";<dir>" (or alone), and taken away the same way, so an install and its uninstall leave
    # the value as it was, byte for byte.
    function Add-ToUserPath([string]$Dir) {
        if ($NoPath) {
            if (-not (($env:Path -split ';') -contains $Dir)) {
                Warn "$Dir is not on your PATH (-NoPath): run the commands from there, or add it to your PATH."
            }
            return
        }
        $path = Get-UserPath
        if (($path.Value -split ';') -contains $Dir) { return }
        Set-UserPath $(if ($path.Value) { $path.Value + ";" + $Dir } else { $Dir }) $path.Kind
        $env:Path = $env:Path + ";" + $Dir
        Warn "Added $Dir to your PATH. Open a new terminal for it to take effect everywhere."
    }
    function Remove-FromUserPath([string]$Dir) {
        if ($NoPath) { return }
        $path = Get-UserPath
        $value = $path.Value
        $entries = $value -split ';'
        if (-not ($entries -contains $Dir)) { return }
        if ($value -ieq $Dir) {
            $value = ""
        } elseif ($value.EndsWith(";" + $Dir, [StringComparison]::OrdinalIgnoreCase)) {
            $value = $value.Substring(0, $value.Length - $Dir.Length - 1)
        } else {
            # moved since: the first entry that is the folder goes, every other entry (empty ones too) stays
            $kept = New-Object System.Collections.Generic.List[string]
            $dropped = $false
            foreach ($entry in $entries) {
                if (-not $dropped -and $entry -ieq $Dir) { $dropped = $true } else { $kept.Add($entry) }
            }
            $value = $kept -join ';'
        }
        Set-UserPath $value $path.Kind
        Ok "Removed $Dir from your PATH."
    }

    # ----------------------------------------------------------------------------- uninstall
    # A prefix this script (or 0.1.5's) installed: its marker, or the game's venv. Nothing else is ever deleted.
    function Test-CitarPrefix([string]$Dir) {
        return (Test-Path -LiteralPath (Join-Path $Dir ".citar-installer")) -or
               (Test-Path -LiteralPath (Join-Path $Dir "Scripts\citar.exe"))
    }
    function Test-Empty([string]$Dir) {
        return -not (Test-Path -LiteralPath $Dir) -or -not (Get-ChildItem -LiteralPath $Dir -Force | Select-Object -First 1)
    }

    if ($Uninstall) {
        Step "Removing CITAR"
        $helperExe = Join-Path $BinDir "citar-helper.exe"
        if (Test-Path -LiteralPath $helperExe) {
            # The helper at sign-in is a Run value naming this binary: removing the file under it would leave a Run value
            # that starts nothing, so its own uninstall comes first. (The machine's service runs a copy of its own.)
            $run = Get-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "CITAR helper" `
                -ErrorAction SilentlyContinue
            if ($run -and "$($run.'CITAR helper')".IndexOf($helperExe, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
                Fail @"
The helper at $helperExe starts at sign-in (the Run value "CITAR helper"). Remove that first:
    & "$helperExe" service uninstall
then run this again. Nothing was removed.
"@
            }
        }
        if (-not $Worker) {
            if ((Test-Path -LiteralPath $Prefix) -and (Test-Empty $Prefix)) {
                Remove-Item -LiteralPath $Prefix -Force
            } elseif (Test-Path -LiteralPath $Prefix) {
                if (-not (Test-CitarPrefix $Prefix)) {
                    Fail "$Prefix does not hold a CITAR install (no .citar-installer, no Scripts\citar.exe): nothing was removed."
                }
                Remove-Item -LiteralPath $Prefix -Recurse -Force
                Ok "Removed the program from $Prefix."
            }
            # Only the shims that start the prefix's commands: a file of the same name from elsewhere is not ours.
            foreach ($name in @("citar", "citar-admin", "citar-mcp")) {
                $shim = Join-Path $BinDir "$name.cmd"
                if ((Test-Path -LiteralPath $shim) -and
                    (Get-Content -LiteralPath $shim -Raw).IndexOf("$Prefix\Scripts\", [StringComparison]::OrdinalIgnoreCase) -ge 0) {
                    Remove-Item -LiteralPath $shim -Force
                }
            }
        }
        # The binary and what its updates leave beside it (crates/citar-helper/src/update/files.rs): the binary the
        # last update replaced (.old), one rolled back while it ran (.failed), either set aside under a process id
        # (.old.<pid>, .failed.<pid>), an update's half-written file, and this script's own. The helper clears these
        # at its next start, and after an uninstall there is none; left, they would keep the bin folder (and its PATH
        # entry) for good.
        $helperFiles = @()
        if (Test-Path -LiteralPath $BinDir -PathType Container) {
            $helperFiles = @(Get-ChildItem -LiteralPath $BinDir -Force -File | Where-Object {
                $_.Name -in @("citar-helper.exe", "citar-helper.exe.old", "citar-helper.exe.failed",
                              ".citar-helper.exe.new", ".citar-helper.install") -or
                $_.Name -match '^citar-helper\.exe\.(old|failed)\.\d+$'
            })
        }
        if ($helperFiles) {
            $helperFiles | ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force }
            Ok "Removed citar-helper.exe and what its updates left from $BinDir."
        }
        if ((Test-Path -LiteralPath $BinDir) -and (Test-Empty $BinDir) -and $BinDir -eq $defaultBin) {
            Remove-Item -LiteralPath $BinDir -Force
        }
        if (Test-Empty $BinDir) { Remove-FromUserPath $BinDir }
        Write-Host ""
        Write-Host "Your games, settings and results were NOT deleted. They are in:"
        Write-Host "  $env:LOCALAPPDATA\CITAR"
        Write-Host "Delete that folder too if you want CITAR gone completely."
        if ($AsFile) { exit 0 }
        $global:LASTEXITCODE = 0
        return
    }

    # ----------------------------------------------------------------------------- the helper
    # CITAR_RELEASE_URL: https anywhere, or plain http to this machine only (a test's own server); never with a user name
    # in it. Refused before anything is requested.
    function Test-ReleaseUrl([string]$Url) {
        $uri = $null
        if (-not [Uri]::TryCreate($Url, [UriKind]::Absolute, [ref]$uri)) {
            Fail "Refused: CITAR_RELEASE_URL $Url is not an https:// address."
        }
        if ($uri.UserInfo) { Fail "Refused: CITAR_RELEASE_URL $Url names a user; give the address alone." }
        if ($uri.Scheme -eq "https") { return }
        if ($uri.Scheme -eq "http") {
            if ($uri.IsLoopback) { return }
            Fail @"
Refused: CITAR_RELEASE_URL $Url is plain http to $($uri.Host). Plain http is allowed to this machine only
(127.0.0.1, localhost or [::1]); use https.
"@
        }
        Fail "Refused: CITAR_RELEASE_URL $Url is not an https:// address."
    }

    # A download into the temporary folder; every redirect must stay https (GitHub sends a release's files from its own
    # storage), so the checksum comes as safely as the file.
    function Save-Download([string]$Url, [string]$OutFile) {
        try {
            $response = Invoke-WebRequest -Uri $Url -OutFile $OutFile -UseBasicParsing -PassThru -MaximumRedirection 5
        } catch {
            Fail "Could not download ${Url}: $($_.Exception.Message)"
        }
        $final = $null
        if ($response.BaseResponse.PSObject.Properties["ResponseUri"]) {
            $final = $response.BaseResponse.ResponseUri                          # Windows PowerShell 5.1
        } elseif ($response.BaseResponse.PSObject.Properties["RequestMessage"]) {
            $final = $response.BaseResponse.RequestMessage.RequestUri            # PowerShell 7
        }
        if ($final -and $final.AbsoluteUri -ne ([Uri]$Url).AbsoluteUri -and $final.Scheme -ne "https") {
            Fail "Refused: $Url was redirected to $final, which is not https. Nothing was installed."
        }
    }

    if ($Worker) {
        $machine = Fact "machine" $(if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE })
        switch ($machine) {
            "AMD64" { }
            "ARM64" { Warn "This helper is built for x64: on ARM64, Windows 11 runs it under its x64 emulation." }
            default { No-Prebuilt "Windows on $machine (the helper is built for x64)" }
        }
        $asset = "citar-helper-windows-x64.exe"
        $base = "$env:CITAR_RELEASE_URL"
        if (-not $base) { $base = $ReleasesDefault }
        $base = $base.TrimEnd('/')
        Test-ReleaseUrl $base
        $url = if ($Version) { "$base/download/v$Version/$asset" } else { "$base/latest/download/$asset" }
        # Windows PowerShell offers TLS 1.2 only when asked (this process only).
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        $tmp = Join-Path ([IO.Path]::GetTempPath()) ("citar-helper-" + [Guid]::NewGuid().ToString("N"))
        New-Item -ItemType Directory -Path $tmp | Out-Null
        try {
            Step "Downloading the CITAR helper: $url"
            Save-Download "$url.sha256" (Join-Path $tmp "$asset.sha256")
            Save-Download $url (Join-Path $tmp $asset)
            $line = Get-Content -LiteralPath (Join-Path $tmp "$asset.sha256") -TotalCount 1
            $want = ("$line".Trim() -split '\s+')[0].ToLowerInvariant()
            if ($want -notmatch '^[0-9a-f]{64}$') { Fail "Refused: $url.sha256 does not hold a SHA-256. Nothing was installed." }
            $got = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $tmp $asset)).Hash.ToLowerInvariant()
            if ($got -ne $want) {
                Fail "Refused: the download's SHA-256 is $got, and $url.sha256 says $want.`nNothing was installed."
            }
            Ok "SHA-256 $got, as its .sha256 says"
            New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
            $staged = Join-Path $BinDir ".citar-helper.install"
            $dest = Join-Path $BinDir "citar-helper.exe"
            Copy-Item -LiteralPath (Join-Path $tmp $asset) -Destination $staged -Force
            try {
                Move-Item -LiteralPath $staged -Destination $dest -Force
            } catch {
                Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
                Fail "$dest could not be replaced ($($_.Exception.Message)): stop the helper (or its service), then run this again."
            }
        } finally {
            Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
        }
        Ok "Installed $dest"
        Add-ToUserPath $BinDir
        $helper = if (($env:Path -split ';') -contains $BinDir) { "citar-helper" } else { "& `"$dest`"" }
        Write-Host ""
        Write-Host "Next, with a worker token from the Servers page of your CITAR server:"
        Write-Host "    $helper setup              # the server's address, the token, and whether to lend CPU"
        Write-Host "    $helper service install    # keep it running: started at sign-in, and again after an update or a crash"
        Write-Host "or run it now in this terminal:  $helper"
        if ($AsFile) { exit 0 }
        $global:LASTEXITCODE = 0
        return
    }

    # ----------------------------------------------------------------------------- python
    # Whether a Python can install CITAR's wheel: the message that says why not, or nothing. The wheel is x64 and abi3, so
    # an ARM64 Python is told to use an x64 one (Windows 11 on ARM runs it) or the installer, and a free-threaded build
    # (3.13t, 3.14t), which cannot load an abi3 extension, the regular build of the same version.
    $probe = "import struct, sys, sysconfig; print('%d.%d' % sys.version_info[:2], sys.implementation.name, " +
             "sysconfig.get_platform(), 1 if sysconfig.get_config_var('Py_GIL_DISABLED') else 0)"
    function Get-PythonProblem([string]$Exe, [string[]]$Pre) {
        $saved = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        try { $out = & $Exe @Pre -c $probe 2>$null } catch { $out = $null } finally { $ErrorActionPreference = $saved }
        $parts = "$out".Trim() -split '\s+'
        if ($LASTEXITCODE -ne 0 -or $parts.Count -ne 4) { return @{ Skip = $true; Text = "it does not run" } }
        $ver = Fact "python" $parts[0]
        $impl = Fact "implementation" $parts[1]
        $plat = Fact "python_platform" $parts[2]
        $free = Fact "free_threaded" $parts[3]
        $verOk = $false
        try { $verOk = [version]$ver -ge $MinPython } catch { }
        if (-not $verOk) { return @{ Skip = $true; Text = "it is Python $ver, and CITAR needs $MinPython or newer" } }
        if ($impl -ne "cpython") { return @{ Text = "no-prebuilt:$impl Python $ver (the builds are for CPython)" } }
        if ($free -ne "0") {
            return @{ Text = @"
$Exe is Python $ver built free-threaded (${ver}t), which cannot load CITAR's prebuilt engine: the
wheel is for CPython with the GIL. Use the regular build of Python $ver (py -$ver, not py -${ver}t), with -Python
if it is not found first.
"@ }
        }
        if ($plat -eq "win-arm64") {
            return @{ Text = @"
$Exe is Python $ver for ARM64, and CITAR's Windows wheel is built for x64. Install the x64 build of
Python $ver from https://www.python.org/downloads/windows/ (Windows 11 on ARM runs it), and give it with -Python;
or use the CITAR installer from https://github.com/jprodgers/CITAR/releases, which needs no Python.
"@ }
        }
        if ($plat -ne "win-amd64") { return @{ Text = "no-prebuilt:a Python for $plat (the Windows build is for x64)" } }
        return $null
    }
    function Stop-ForPython($Problem) {
        if ($Problem.Text.StartsWith("no-prebuilt:")) { No-Prebuilt $Problem.Text.Substring(12) }
        Fail $Problem.Text
    }

    Step "Looking for Python $MinPython or newer"
    $py = $null
    if ($Python) {
        if (-not (Get-Command $Python -ErrorAction SilentlyContinue)) { Fail "-Python ${Python}: no such program" }
        $problem = Get-PythonProblem $Python @()
        if ($problem) {
            if ($problem.Skip) { Fail "-Python ${Python}: $($problem.Text)." }
            Stop-ForPython $problem
        }
        $py = @{ Exe = $Python; Pre = @() }
    } else {
        # The newest Python on a machine is often not the first one on the PATH, so the py launcher is asked first: it
        # knows about every installation, including ones that were never added to PATH. The first that can install CITAR
        # is taken, else the first new enough one says why it cannot.
        $candidates = @()
        if (Get-Command py -ErrorAction SilentlyContinue) {
            foreach ($tag in @("-3.14", "-3.13", "-3.12", "-3.11", "-3")) { $candidates += , @("py", @($tag)) }
        }
        foreach ($name in @("python3", "python")) {
            if (Get-Command $name -ErrorAction SilentlyContinue) { $candidates += , @($name, @()) }
        }
        $firstProblem = $null
        foreach ($candidate in $candidates) {
            $problem = Get-PythonProblem $candidate[0] $candidate[1]
            if (-not $problem) { $py = @{ Exe = $candidate[0]; Pre = $candidate[1] }; break }
            if (-not $problem.Skip -and -not $firstProblem) { $firstProblem = $problem }
        }
        if (-not $py -and $firstProblem) { Stop-ForPython $firstProblem }
    }
    if (-not $py) {
        Fail @"
No Python $MinPython or newer was found.

Install it from https://www.python.org/downloads/ or with:
    winget install Python.Python.3.12

Tick "Add python.exe to PATH" in the installer, then run this script again.

If you would rather not install Python at all, download the CITAR installer from
https://github.com/jprodgers/CITAR/releases - it has everything built in.
"@
    }
    $pyArgs = @($py.Pre) + @("-c", "import sys; print('%d.%d.%d' % sys.version_info[:3])")
    Ok "Python $(& $py.Exe @pyArgs)"

    # ----------------------------------------------------------------------------- the wheel
    function Pep440([string]$V) { return $V -replace '-alpha\.', 'a' -replace '-beta\.', 'b' -replace '-rc\.', 'rc' }
    function No-Wheel([string]$Why) {
        Fail @"
${Why}: this installer installs CITAR's prebuilt wheels only.
'pip install' of a source distribution or a checkout builds CITAR from source, with a Rust toolchain
(https://rustup.rs).
"@
    }

    $wheelDir = $null
    $wheel = $null
    if ($From) {
        # A local wheel, or a folder of wheels: how CI tests this script before a release exists, and how an offline
        # install works. Never a source distribution or a checkout.
        if (-not (Test-Path -LiteralPath $From)) { Fail "No such file or folder: $From" }
        $From = Full-Path $From
        if (Test-Path -LiteralPath $From -PathType Container) {
            $wheelDir = $From
            $versions = @(Get-ChildItem -LiteralPath $wheelDir -Filter "citar-*.whl" -File |
                ForEach-Object { $_.Name.Split('-')[1] } | Sort-Object -Unique)
            if ($versions.Count -eq 0) {
                $sdists = @(Get-ChildItem -LiteralPath $wheelDir -File |
                    Where-Object { $_.Name -like "citar-*.tar.gz" -or $_.Name -like "citar-*.zip" } | ForEach-Object { $_.Name })
                $only = if ($sdists.Count) { " (only $($sdists -join ', '), a source distribution)" } else { "" }
                No-Wheel "$wheelDir holds no CITAR wheel$only"
            }
            if ($Version) {
                if (-not ($versions -contains (Pep440 $Version))) { Fail "$wheelDir holds CITAR $($versions -join ', '), not $Version" }
            } elseif ($versions.Count -gt 1) {
                Fail "$wheelDir holds CITAR $($versions -join ', '): name the one to install with -Version"
            } else {
                $Version = $versions[0]
            }
        } else {
            if (-not $From.EndsWith(".whl")) { No-Wheel "$From is not a wheel" }
            if ($Version -and (Split-Path -Leaf $From).Split('-')[1] -ne (Pep440 $Version)) { Fail "$From is not CITAR $Version" }
            $wheel = $From
        }
    }

    # ----------------------------------------------------------------------------- install
    if ((Test-Path -LiteralPath $Prefix) -and -not (Test-CitarPrefix $Prefix) -and -not (Test-Empty $Prefix)) {
        Fail "$Prefix is not empty and holds no CITAR install: name a new or empty folder with -Prefix."
    }
    Step "Installing CITAR into $Prefix"
    New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
    # The marker first, so an install that fails half way can be run again (or removed with -Uninstall).
    Set-Content -LiteralPath (Join-Path $Prefix ".citar-installer") -Value "installing with install.ps1" -Encoding ascii

    Invoke-Native $py.Exe (@($py.Pre) + @("-m", "venv", $Prefix))
    if ($LASTEXITCODE -ne 0) { Fail "Could not create a virtual environment in $Prefix" }

    $venvPython = Join-Path $Prefix "Scripts\python.exe"
    Invoke-Native $venvPython @("-m", "pip", "install", "--quiet", "--disable-pip-version-check", "--upgrade", "pip")
    if ($LASTEXITCODE -ne 0) { Fail "Could not bring pip up to date in $Prefix" }

    $extras = "all"
    if ($wheelDir) {
        # pip picks the wheel for this machine from the folder alone (no index), then installs it as a file below.
        $picked = Join-Path ([IO.Path]::GetTempPath()) ("citar-wheel-" + [Guid]::NewGuid().ToString("N"))
        Invoke-Native $venvPython @("-m", "pip", "download", "--quiet", "--disable-pip-version-check", "--no-deps",
            "--no-index", "--only-binary=:all:", "--find-links", $wheelDir, "--dest", $picked, "citar==$Version")
        if ($LASTEXITCODE -ne 0) { No-Wheel "$wheelDir holds no CITAR $Version wheel for this machine" }
        $wheel = (Get-ChildItem -LiteralPath $picked -Filter "citar-*.whl" | Select-Object -First 1).FullName
    }
    $spec = if ($wheel) { "$wheel[$extras]" } elseif ($Version) { "citar[$extras]==$Version" } else { "citar[$extras]" }

    # --only-binary=citar: CITAR from a wheel or not at all (its dependencies as pip chooses). Without it, a machine
    # with no wheel would download the source distribution and fail minutes later, building Rust.
    Step "Downloading CITAR and its dependencies"
    try {
        Invoke-Native $venvPython @("-m", "pip", "install", "--quiet", "--disable-pip-version-check", "--only-binary=citar", $spec)
        $code = $LASTEXITCODE
    } finally {
        if ($wheelDir -and $picked) { Remove-Item -LiteralPath $picked -Recurse -Force -ErrorAction SilentlyContinue }
    }
    if ($code -ne 0) {
        Fail @"
pip could not install $spec. If pip found no matching distribution, there is no prebuilt CITAR $Version
for this Python: 'pip install citar' builds it from source, with a Rust toolchain (https://rustup.rs).
"@
    }
    $installed = & (Join-Path $Prefix "Scripts\citar.exe") --version
    Set-Content -LiteralPath (Join-Path $Prefix ".citar-installer") -Encoding ascii `
        -Value @("installed by install.ps1: $installed", "bin: $BinDir")
    Ok "$installed installed"

    # ----------------------------------------------------------------------------- the commands
    # Shims rather than copies: an upgrade replaces the executables inside the venv, and a copy would
    # go on launching the version that was current when it was made.
    Step "Putting citar on your PATH"
    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    foreach ($name in @("citar", "citar-admin", "citar-mcp")) {
        $target = Join-Path $Prefix "Scripts\$name.exe"
        if (-not (Test-Path -LiteralPath $target)) { continue }
        $shim = Join-Path $BinDir "$name.cmd"
        "@echo off`r`n`"$target`" %*" | Set-Content -LiteralPath $shim -Encoding ascii
    }
    Ok "Linked into $BinDir"
    Add-ToUserPath $BinDir

    # ----------------------------------------------------------------------------- setup
    Write-Host ""
    if ($NoSetup) {
        Ok "Installed. Run 'citar setup' when you are ready."
        if ($AsFile) { exit 0 }
        $global:LASTEXITCODE = 0
        return
    }

    Step "Setting up"
    & (Join-Path $Prefix "Scripts\citar.exe") setup --local
    if ($AsFile) { exit $LASTEXITCODE }
} catch {
    # Fail, in a script block: its message is out, and the user's PowerShell goes on, $LASTEXITCODE 1. Anything else
    # is an error of its own, shown as PowerShell shows one.
    if ("$($_.Exception.Message)" -ne $Stopped) { throw }
    $global:LASTEXITCODE = 1
}
