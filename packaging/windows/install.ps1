<#
.SYNOPSIS
    Register Feather with Windows: a candidate for the formats it can open, and
    an "Open with Feather" verb.

.DESCRIPTION
    Everything here is written under HKCU, so it needs no administrator rights
    and no installer. It registers Feather as a *candidate* — the user still
    chooses the default in Settings, which is what §7 of the plan says an
    application's job is.

.PARAMETER Binary
    Path to feather-viewer.exe. Defaults to one next to this script.

.PARAMETER Uninstall
    Remove everything this script wrote.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File install.ps1
#>
param(
    [string]$Binary = "",
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"

$ProgId  = "Feather.Document"
$AppName = "Feather"

# Only the formats the viewer can actually open. A file association is a
# promise; claiming more than the pipelines support is how a viewer earns a
# reputation for opening a black window.
$Extensions = @(
    ".pdf", ".epub",
    ".cbz",
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".tif", ".tiff"
)

function Set-RegistryValue {
    param([string]$Path, [string]$Name, [string]$Value)
    if (-not (Test-Path $Path)) {
        New-Item -Path $Path -Force | Out-Null
    }
    New-ItemProperty -Path $Path -Name $Name -Value $Value -PropertyType String -Force | Out-Null
}

if ($Uninstall) {
    Remove-Item -Path "HKCU:\Software\Classes\$ProgId" -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -Path "HKCU:\Software\Classes\Applications\feather-viewer.exe" -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -Path "HKCU:\Software\Feather" -Recurse -Force -ErrorAction SilentlyContinue
    foreach ($ext in $Extensions) {
        Remove-ItemProperty -Path "HKCU:\Software\Classes\$ext\OpenWithProgids" -Name $ProgId -ErrorAction SilentlyContinue
    }
    Write-Host "Feather unregistered."
    exit 0
}

if (-not $Binary) {
    $here = Split-Path -Parent $MyInvocation.MyCommand.Path
    foreach ($candidate in @(
        (Join-Path $here "feather-viewer.exe"),
        (Join-Path $here "..\..\target\release\feather-viewer.exe")
    )) {
        if (Test-Path $candidate) { $Binary = (Resolve-Path $candidate).Path; break }
    }
}

if (-not $Binary -or -not (Test-Path $Binary)) {
    Write-Error "feather: could not find feather-viewer.exe; pass -Binary PATH"
    exit 1
}

$Binary = (Resolve-Path $Binary).Path
$Command = "`"$Binary`" `"%1`""

# The ProgID: what a file of this type opens with.
Set-RegistryValue "HKCU:\Software\Classes\$ProgId" "(Default)" "Feather document"
Set-RegistryValue "HKCU:\Software\Classes\$ProgId" "FriendlyTypeName" "Feather document"
Set-RegistryValue "HKCU:\Software\Classes\$ProgId\DefaultIcon" "(Default)" "$Binary,0"
Set-RegistryValue "HKCU:\Software\Classes\$ProgId\shell\open\command" "(Default)" $Command
Set-RegistryValue "HKCU:\Software\Classes\$ProgId\shell\open" "FriendlyAppName" $AppName

# Applications\: how "Open with" launches us for an arbitrary file.
Set-RegistryValue "HKCU:\Software\Classes\Applications\feather-viewer.exe" "FriendlyAppName" $AppName
Set-RegistryValue "HKCU:\Software\Classes\Applications\feather-viewer.exe\DefaultIcon" "(Default)" "$Binary,0"
Set-RegistryValue "HKCU:\Software\Classes\Applications\feather-viewer.exe\shell\open\command" "(Default)" $Command

# Capabilities + RegisteredApplications: how we appear in Settings > Default apps.
$capabilities = "HKCU:\Software\Feather\Capabilities"
Set-RegistryValue $capabilities "ApplicationName" $AppName
Set-RegistryValue $capabilities "ApplicationDescription" "A read-only document viewer that opens before you blink"
Set-RegistryValue $capabilities "ApplicationIcon" "$Binary,0"
foreach ($ext in $Extensions) {
    Set-RegistryValue "$capabilities\FileAssociations" $ext $ProgId
}
Set-RegistryValue "HKCU:\Software\RegisteredApplications" $AppName "Software\Feather\Capabilities"

# Candidate only: adds us to the "Open with" list, does not steal the default.
foreach ($ext in $Extensions) {
    Set-RegistryValue "HKCU:\Software\Classes\$ext\OpenWithProgids" $ProgId ""
}

Write-Host "Feather registered as a candidate for $($Extensions.Count) file types."
Write-Host "To make it the default: Settings > Apps > Default apps > Feather."
Write-Host "(this script will not do that for you — §7 of the plan)"
