#requires -Version 5.1
<#
.SYNOPSIS
    Stages everything the offline installer has to carry into `src-tauri\resources`.

.DESCRIPTION
    The Windows installer must work with no internet connection (`OILinux_PRD.md` §10.1), so QEMU,
    the verified Alpine base image, the WebView2 offline installer and the third-party licence
    inventory are copied next to the application. Development builds do not need any of this:
    `oi_core::vm::accelerator::locate_qemu` falls back to a system QEMU and
    `oi_core::storage::images::locate_base_image` looks in `vm\out`.
#>
[CmdletBinding()]
param(
    [string]$QemuDir = 'C:\Program Files\qemu',
    [string]$BaseImage = 'vm\out\base.qcow2',
    [string]$WebView2,
    [string]$Resources = 'src-tauri\resources'
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
Push-Location $repo
try {
    New-Item -ItemType Directory -Force -Path "$Resources\qemu", "$Resources\images", "$Resources\licenses" | Out-Null

    Write-Host "[stage] QEMU from $QemuDir"
    if (-not (Test-Path $QemuDir)) { throw "QEMU was not found at $QemuDir" }
    Copy-Item (Join-Path $QemuDir '*.exe') "$Resources\qemu\" -Force
    Copy-Item (Join-Path $QemuDir '*.dll') "$Resources\qemu\" -Force
    Copy-Item (Join-Path $QemuDir 'COPYING*') "$Resources\qemu\" -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path "$Resources\qemu\share" | Out-Null
    Copy-Item (Join-Path $QemuDir 'share\edk2-*') "$Resources\qemu\share\" -Force

    Write-Host "[stage] base image $BaseImage"
    if (-not (Test-Path $BaseImage)) { throw "the base image $BaseImage does not exist; build it first" }
    Copy-Item $BaseImage "$Resources\images\base.qcow2" -Force
    $hash = (Get-FileHash "$Resources\images\base.qcow2" -Algorithm SHA256).Hash.ToLower()
    Set-Content -Path "$Resources\images\base.qcow2.sha256" -Value $hash -NoNewline
    if (Test-Path "$BaseImage.sha256") {
        $expected = (Get-Content "$BaseImage.sha256" -Raw).Trim()
        if ($expected -and ($expected -ne $hash)) { throw "the base image hash changed while copying" }
    }

    if ($WebView2) {
        Write-Host "[stage] WebView2 offline installer"
        New-Item -ItemType Directory -Force -Path "$Resources\webview2" | Out-Null
        Copy-Item $WebView2 "$Resources\webview2\MicrosoftEdgeWebView2RuntimeInstallerX64.exe" -Force
    } else {
        Write-Warning 'No WebView2 offline installer was supplied; the bundle will not be able to install it offline.'
    }

    Write-Host "[stage] licence inventory"
    $inventory = Get-Content "$Resources\licenses\third-party.json" -Raw | ConvertFrom-Json
    $inventory.generated_by = 'scripts/stage-resources.ps1'
    $inventory | ConvertTo-Json -Depth 6 | Set-Content "$Resources\licenses\third-party.json" -Encoding utf8

    Write-Host "[stage] qemu version"
    $version = (& (Join-Path $QemuDir 'qemu-system-x86_64.exe') --version | Select-Object -First 1)
    Set-Content -Path "$Resources\qemu\VERSION.txt" -Value $version -Encoding utf8

    Write-Host "[stage] done: $Resources" -ForegroundColor Green
} finally {
    Pop-Location
}
