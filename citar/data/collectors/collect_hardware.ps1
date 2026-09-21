# CITAR hardware collector for Windows machines without Python.
#   powershell -ExecutionPolicy Bypass -File collect_hardware.ps1 > my-machine.json
# Then import the JSON on CITAR's Servers page. (With Python installed, collect_hardware.py gathers a bit more.)
$ErrorActionPreference = 'SilentlyContinue'

function GB($bytes) { if ($bytes) { [math]::Round([double]$bytes / 1GB, 1) } else { $null } }

$cpu = @(Get-CimInstance Win32_Processor)
$cs = Get-CimInstance Win32_ComputerSystem
$chassis = @((Get-CimInstance Win32_SystemEnclosure).ChassisTypes)
$battery = @(Get-CimInstance Win32_Battery).Count
$laptopTypes = 8, 9, 10, 14, 30, 31, 32
$form = if ($battery -gt 0 -or ($chassis | Where-Object { $laptopTypes -contains $_ })) { 'laptop' } else { 'desktop' }

$vram = @{}
Get-ItemProperty 'HKLM:\SYSTEM\ControlSet001\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0*' | ForEach-Object {
    if ($_.DriverDesc -and $_.'HardwareInformation.qwMemorySize') { $vram[$_.DriverDesc] = GB $_.'HardwareInformation.qwMemorySize' }
}

$gpus = @()
$smi = Get-Command nvidia-smi -ErrorAction SilentlyContinue
if ($smi) {
    & $smi.Source --query-gpu=name,memory.total,driver_version,power.limit,power.draw --format=csv,noheader,nounits | ForEach-Object {
        $p = $_.Split(',') | ForEach-Object { $_.Trim() }
        $lim = 0.0; $now = 0.0
        $gpus += [ordered]@{ name = $p[0]; vendor = 'NVIDIA'; vram_gb = [math]::Round([double]$p[1] / 1024, 1); driver = $p[2]
            power_limit_w = $(if ([double]::TryParse($p[3], [ref]$lim)) { $lim } else { $null })
            power_now_w = $(if ([double]::TryParse($p[4], [ref]$now)) { $now } else { $null }); power_readable = $true }
    }
}
Get-CimInstance Win32_VideoController | ForEach-Object {
    $name = $_.Name
    if (-not $name -or $name -match 'Basic Display|Remote') { return }
    if ($gpus | Where-Object { $_.name -eq $name }) { return }
    $vendor = $_.AdapterCompatibility
    if ($vendor -match 'NVIDIA') { $vendor = 'NVIDIA' } elseif ($vendor -match 'AMD|Advanced Micro') { $vendor = 'AMD' } elseif ($vendor -match 'Intel') { $vendor = 'Intel' }
    $v = $vram[$name]; if (-not $v) { $v = GB $_.AdapterRAM }
    $gpus += [ordered]@{ name = $name; vendor = $vendor; vram_gb = $v; driver = $_.DriverVersion; power_readable = $false }
}

$disks = @(Get-PhysicalDisk | ForEach-Object {
    $mt = switch ($_.MediaType) { 3 { 'HDD' } 4 { 'SSD' } 'HDD' { 'HDD' } 'SSD' { 'SSD' } default { $null } }
    $bus = switch ($_.BusType) { 17 { 'NVMe' } 11 { 'SATA' } 7 { 'USB' } default { "$($_.BusType)" } }
    [ordered]@{ name = $_.FriendlyName; type = $mt; bus = $bus; size_gb = GB $_.Size }
})
$volumes = @(Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3' | ForEach-Object {
    [ordered]@{ mount = "$($_.DeviceID)\"; fs = $_.FileSystem; total_gb = GB $_.Size; free_gb = GB $_.FreeSpace }
})

$runtimes = @()
$lms = "$env:USERPROFILE\.lmstudio\bin\lms.exe"
if (Test-Path $lms) {
    $models = @()
    try {
        (& $lms ls --json | ConvertFrom-Json) | Where-Object { $_.type -ne 'embedding' } | ForEach-Object {
            $models += [ordered]@{ key = $_.modelKey; size_gb = GB $_.sizeBytes; params = $_.paramsString; arch = $_.architecture
                max_context = $_.maxContextLength; device = $(if ($_.deviceIdentifier) { $_.deviceIdentifier } else { 'local' }) }
        }
    } catch {}
    $runtimes += [ordered]@{ name = 'LM Studio'; cli = $lms; models = $models }
}

$threads = ($cpu | Measure-Object NumberOfLogicalProcessors -Sum).Sum
$cpuName = "$($cpu[0].Name)".Trim()
if ($form -eq 'laptop') { $idle = 12; $cpuMax = $(if ($cpuName -match '\d{4,5}HX?\b') { 55 } else { 25 }) }
else { $idle = 55; $cpuMax = $(if ($threads -le 8) { 65 } elseif ($threads -le 24) { 125 } else { 200 }) }
$gpuMax = 0
foreach ($g in $gpus) {
    if ($g.power_limit_w) { $gpuMax += 0.85 * $g.power_limit_w }
    elseif (($g.vendor -eq 'NVIDIA' -or $g.vendor -eq 'AMD') -and $g.vram_gb -ge 6) { $gpuMax += $(if ($form -eq 'laptop') { 80 } else { 220 }) }
}

$out = [ordered]@{
    format = 'citar-hardware'; collector_version = 1; collector = 'powershell'
    collected_at = (Get-Date -Format 'yyyy-MM-ddTHH:mm:ss'); hostname = $env:COMPUTERNAME
    os = [ordered]@{ system = 'Windows'; release = (Get-CimInstance Win32_OperatingSystem).Caption; version = [Environment]::OSVersion.Version.ToString(); arch = $env:PROCESSOR_ARCHITECTURE }
    system = [ordered]@{ form = $form; manufacturer = $cs.Manufacturer; model = $cs.Model }
    cpu = [ordered]@{ model = $cpuName; sockets = $cpu.Count; cores = ($cpu | Measure-Object NumberOfCores -Sum).Sum; threads = $threads
        max_ghz = [math]::Round($cpu[0].MaxClockSpeed / 1000, 2) }
    memory = [ordered]@{ ram_gb = GB $cs.TotalPhysicalMemory; unified = $false
        modules = @(Get-CimInstance Win32_PhysicalMemory | ForEach-Object { [ordered]@{ gb = GB $_.Capacity; mhz = $_.Speed; part = "$($_.PartNumber)".Trim() } }) }
    gpus = $gpus; physical_disks = $disks; volumes = $volumes; runtimes = $runtimes
    power = [ordered]@{ gpu_sampling = [bool]$smi
        suggested = [ordered]@{ idle_w = $idle; cpu_max_w = $cpuMax; gpu_max_w = [math]::Round($gpuMax); source = 'guess'
            note = 'Estimated from the hardware class; measure idle and busy wall power with a plug-in meter for better numbers.' } }
}
$out | ConvertTo-Json -Depth 6
