#requires -Version 5.1
<#
.SYNOPSIS
    Builds the Alpine base image that ships with OI Linux.

.DESCRIPTION
    Boots the official Alpine "standard" ISO's kernel with a serial console, installs the pinned
    package list and the `vm\rootfs` overlay onto a fresh qcow2 disk, then publishes the result as
    `vm\out\base.qcow2` with its SHA-256 sidecar.

    The script is deliberately literal about the sequence (`vm\scripts\install-base.sh` does the
    work inside the guest) so that a failed build can be re-run and inspected: the guest's
    installation log is fetched back to `<OutDir>\install.log`.

    This is the Windows equivalent of `vm\build.sh`, which does the same job on Linux CI.

.PARAMETER Iso
    Path of the Alpine standard ISO. Defaults to a copy in `D:\` and then to `vm\cache\`.

.PARAMETER WorkDir
    Scratch directory for the build (disk image, payload). Large files belong on a data drive.
#>
[CmdletBinding()]
param(
    [string]$Iso,
    [string]$WorkDir = 'D:\oi-image-build',
    [string]$OutDir,
    [string]$Qemu = 'C:\Program Files\qemu\qemu-system-x86_64.exe',
    [string]$QemuImg = 'C:\Program Files\qemu\qemu-img.exe',
    [int]$VmMemoryMiB = 2048,
    [int]$VmCpus = 2,
    [int]$SerialPort = 44600,
    [string]$Mirror = 'https://mirrors.tuna.tsinghua.edu.cn/alpine',
    [string]$Branch = 'v3.24',
    [int]$DiskGiB = 8,
    [switch]$KeepWorkDir
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$repo = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) { $OutDir = Join-Path $repo 'vm\out' }

function Write-Step($message) { Write-Host "`n[build] $message" -ForegroundColor Cyan }
function Fail($message) { Write-Host "`n[build] ERROR: $message" -ForegroundColor Red; exit 1 }

if (-not (Test-Path $Qemu)) { Fail "QEMU was not found at $Qemu." }
if (-not $Iso) {
    $candidates = @(
        'D:\alpine-standard-3.24.2-x86_64.iso',
        (Join-Path $repo 'vm\cache\alpine-standard-3.24.2-x86_64.iso')
    )
    $Iso = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $Iso -or -not (Test-Path $Iso)) {
    Fail "the Alpine standard ISO was not found; pass -Iso <path>."
}
$Iso = (Resolve-Path $Iso).Path

New-Item -ItemType Directory -Force -Path $WorkDir, $OutDir | Out-Null
$images = Join-Path $WorkDir 'images'
New-Item -ItemType Directory -Force -Path $images | Out-Null

Write-Step "building the host helper"
Push-Location $repo
try { & cargo build -p oi-core --bin oitool --quiet } finally { Pop-Location }
$oitool = Join-Path $repo 'target\debug\oitool.exe'
if (-not (Test-Path $oitool)) { Fail 'the oitool helper could not be built.' }

Write-Step "mounting the ISO to extract the kernel"
$mounted = Mount-DiskImage -ImagePath $Iso -PassThru
$volume = $mounted | Get-Volume
$drive = if ($volume.DriveLetter) { "$($volume.DriveLetter):" } else { $null }
if (-not $drive) {
    Dismount-DiskImage -ImagePath $Iso | Out-Null
    Fail 'the ISO could not be mounted (no drive letter).'
}
$kernel = Join-Path $images 'vmlinuz-lts'
$initrd = Join-Path $images 'initramfs-lts'
try {
    Copy-Item (Join-Path $drive 'boot\vmlinuz-lts') $kernel -Force
    Copy-Item (Join-Path $drive 'boot\initramfs-lts') $initrd -Force
} finally {
    Dismount-DiskImage -ImagePath $Iso | Out-Null
}

Write-Step "creating the target disk"
$target = Join-Path $images 'base-building.qcow2'
Remove-Item $target -ErrorAction SilentlyContinue
& $QemuImg create -f qcow2 $target "$($DiskGiB)G" | Out-Null
if ($LASTEXITCODE -ne 0) { Fail 'qemu-img create failed.' }

Write-Step "packing the installer payload"
$payload = Join-Path $WorkDir 'oi-payload.tar.gz'
Remove-Item $payload -ErrorAction SilentlyContinue
& tar -czf $payload -C (Join-Path $repo 'vm') rootfs scripts/install-base.sh
if ($LASTEXITCODE -ne 0) { Fail 'the payload could not be packed.' }

Write-Step "booting the live medium (TCG; this is the slow part)"
$serialLog = Join-Path $WorkDir 'serial.log'
$qemuArgs = @(
    '-name', 'oi-image-build',
    '-accel', 'tcg,thread=multi',
    '-m', "$VmMemoryMiB", '-smp', "$VmCpus",
    '-kernel', $kernel, '-initrd', $initrd,
    '-append', 'modules=loop,squashfs,sd-mod,usb-storage alpine_dev=cdrom console=tty0 console=ttyS0,115200',
    '-cdrom', $Iso,
    '-drive', "file=$target,format=qcow2,if=virtio,cache=writeback",
    '-netdev', 'user,id=oinet',
    '-device', 'virtio-net-pci,netdev=oinet',
    '-device', 'virtio-rng-pci',
    '-serial', "tcp:127.0.0.1:$SerialPort,server=on,wait=off",
    '-display', 'none', '-monitor', 'none', '-rtc', 'base=utc'
)

# Quote one argument the way Windows expects, for the .NET Framework fallback below.
function Quote-Argument([string]$value) {
    if ($value -eq '') { return '""' }
    if ($value -notmatch '[\s"]') { return $value }
    $builder = New-Object System.Text.StringBuilder
    [void]$builder.Append('"')
    $backslashes = 0
    foreach ($ch in $value.ToCharArray()) {
        if ($ch -eq '\') { $backslashes++; continue }
        if ($ch -eq '"') {
            [void]$builder.Append('\' * (2 * $backslashes + 1))
            [void]$builder.Append('"')
        } else {
            [void]$builder.Append('\' * $backslashes)
            [void]$builder.Append($ch)
        }
        $backslashes = 0
    }
    [void]$builder.Append('\' * (2 * $backslashes))
    [void]$builder.Append('"')
    return $builder.ToString()
}

# `Start-Process -ArgumentList` joins arguments with spaces and cannot represent a path with
# spaces (the firmware, the ISO and the work directory all can contain one), so the process is
# launched through .NET, which quotes each argument itself. Windows PowerShell runs on .NET
# Framework, which predates `ArgumentList`, hence the manual quoting fallback.
#
# QEMU's own stdout/stderr are deliberately *not* redirected: an unread pipe would eventually
# block the emulator, and QEMU's diagnostics belong in the build log anyway.
$startInfo = New-Object System.Diagnostics.ProcessStartInfo
$startInfo.FileName = $Qemu
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
if ($startInfo.PSObject.Properties.Name -contains 'ArgumentList') {
    foreach ($argument in $qemuArgs) { $startInfo.ArgumentList.Add([string]$argument) }
} else {
    $startInfo.Arguments = (($qemuArgs | ForEach-Object { Quote-Argument ([string]$_) }) -join ' ')
}
$qemuProcess = [System.Diagnostics.Process]::Start($startInfo)
if (-not $qemuProcess) { Fail 'QEMU did not start.' }

$driver = Join-Path $repo 'vm\scripts\build-image.py'
try {
    Write-Step "installing (one serial session: login, upload, install, power off)"
    & python $driver --port $SerialPort --payload $payload --mirror $Mirror --branch $Branch --transcript $serialLog | Tee-Object -FilePath (Join-Path $OutDir 'install.log') | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Fail 'the installation did not finish; see the install log and the serial transcript.'
    }
} finally {
    if ($qemuProcess) {
        try {
            if (-not $qemuProcess.HasExited) {
                $qemuProcess.Kill($true)
                $qemuProcess.WaitForExit(15000) | Out-Null
            }
        } catch { }
        $qemuProcess.Dispose()
    }
}

Write-Step "publishing the base image"
$output = Join-Path $OutDir 'base.qcow2'
Remove-Item $output -ErrorAction SilentlyContinue
& $QemuImg convert -O qcow2 $target $output
if ($LASTEXITCODE -ne 0) { Fail 'qemu-img convert failed.' }
& $QemuImg check $output
if ($LASTEXITCODE -ne 0) { Fail 'the produced image is damaged (qemu-img check).' }

$hash = (& $oitool sha256 $output) -split '\s+' | Select-Object -First 1
Set-Content -Path "$output.sha256" -Value $hash -NoNewline
Write-Host "[build] base image: $output"
Write-Host "[build] sha256    : $hash"

if (-not $KeepWorkDir) {
    Remove-Item -Recurse -Force $images -ErrorAction SilentlyContinue
    Remove-Item -Force $payload -ErrorAction SilentlyContinue
}
Write-Host "`n[build] done. Run the acceptance tests with: cargo run --bin ntfs-share (in integration-tests)" -ForegroundColor Green
