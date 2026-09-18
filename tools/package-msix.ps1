[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$PackageVersion,
    [Parameter(Mandatory)][string]$IdentityName,
    [Parameter(Mandatory)][string]$Publisher,
    [Parameter(Mandatory)][string]$PublisherDisplayName,
    [string]$BinaryDirectory = 'target/release',
    [string]$OutputDirectory = 'target/store',
    [string]$MakeAppx
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($Version -cnotmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$') {
    throw 'Invalid TideDesk version.'
}
if ($PackageVersion -cnotmatch '^[1-9]\d*\.(0|[1-9]\d*)\.(0|[1-9]\d*)\.0$') {
    throw 'Store version must have four numeric components, a nonzero major and final zero.'
}
foreach ($part in $PackageVersion.Split('.')) {
    if ([long]$part -gt 65535) { throw 'Store version components must fit in 16 bits.' }
}
if ($IdentityName -cnotmatch '^[A-Za-z0-9][A-Za-z0-9.-]{2,49}$') {
    throw 'Use the exact Package/Identity/Name from Partner Center (3-50 characters).'
}
if (-not $Publisher.StartsWith('CN=') -or $Publisher.Length -gt 8192) {
    throw 'Use the exact Package/Identity/Publisher from Partner Center.'
}
if ([string]::IsNullOrWhiteSpace($PublisherDisplayName) -or $PublisherDisplayName.Length -gt 256) {
    throw 'Use the publisher display name from Partner Center.'
}
$repo = Split-Path $PSScriptRoot -Parent
$binaryRoot = (Resolve-Path -LiteralPath $BinaryDirectory).Path
foreach ($file in @('tidedesk-host.exe', 'tidedesk-view.exe')) {
    $path = Join-Path $binaryRoot $file
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing $file" }
    $info = [Diagnostics.FileVersionInfo]::GetVersionInfo($path)
    if ($info.ProductName -cne 'TideDesk' -or $info.ProductVersion -cne $Version -or
        $info.OriginalFilename -cne $file) {
        throw "Unexpected product/version/filename metadata in $file."
    }
}
if (-not $MakeAppx) {
    $sdkRoot = Join-Path ([Environment]::GetFolderPath('ProgramFilesX86')) 'Windows Kits/10/bin'
    $candidates = @(Get-ChildItem -LiteralPath $sdkRoot -Directory |
        Where-Object { $_.Name -match '^10\.\d+\.\d+\.\d+$' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName 'x64/makeappx.exe' } |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf })
    if (-not $candidates.Count) { throw 'Install the Windows 10/11 SDK (MakeAppx.exe).' }
    $MakeAppx = $candidates[0]
}
$MakeAppx = (Resolve-Path -LiteralPath $MakeAppx).Path
$output = [IO.Path]::GetFullPath($OutputDirectory)
$stage = Join-Path $output 'stage'
$package = Join-Path $output "tidedesk-$PackageVersion-x64.msix"
foreach ($path in @($stage, $package, "$package.sha256")) {
    if (Test-Path -LiteralPath $path) { throw "Output already exists: $path" }
}
New-Item -ItemType Directory -Path (Join-Path $stage 'Assets') -Force | Out-Null
foreach ($file in @('tidedesk-host.exe', 'tidedesk-view.exe')) {
    Copy-Item -LiteralPath (Join-Path $binaryRoot $file) -Destination $stage
}
Copy-Item -LiteralPath (Join-Path $repo 'LICENSE') -Destination $stage
Copy-Item -LiteralPath (Join-Path $repo 'licenses') -Destination $stage -Recurse
& (Join-Path $repo 'tools/package-third-party-notices.ps1') -StageDirectory $stage
[xml]$manifest = Get-Content -LiteralPath (Join-Path $repo 'packaging/msix/AppxManifest.xml') -Raw
$manifest.Package.Identity.SetAttribute('Name', $IdentityName)
$manifest.Package.Identity.SetAttribute('Publisher', $Publisher)
$manifest.Package.Identity.SetAttribute('Version', $PackageVersion)
$manifest.Package.Properties.PublisherDisplayName = $PublisherDisplayName
$manifest.Save((Join-Path $stage 'AppxManifest.xml'))
# Generate the required sizes from the existing product icon.
Add-Type -AssemblyName System.Drawing
$source = [Drawing.Image]::FromFile((Join-Path $repo 'assets/tidedesk-256.png'))
try {
    foreach ($asset in @(
        @{ Name = 'StoreLogo.png'; Size = 50 },
        @{ Name = 'Square44x44Logo.png'; Size = 44 },
        @{ Name = 'Square150x150Logo.png'; Size = 150 }
    )) {
        $bitmap = [Drawing.Bitmap]::new($asset.Size, $asset.Size)
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $graphics.DrawImage($source, 0, 0, $asset.Size, $asset.Size)
            $bitmap.Save((Join-Path $stage "Assets/$($asset.Name)"), [Drawing.Imaging.ImageFormat]::Png)
        } finally {
            $graphics.Dispose()
            $bitmap.Dispose()
        }
    }
} finally { $source.Dispose() }
# Schema and semantic validation remain enabled (no /nv).
& $MakeAppx pack /d $stage /p $package /o /h SHA256
if ($LASTEXITCODE -ne 0) { throw 'MakeAppx validation/packaging failed.' }
$hash = (Get-FileHash -LiteralPath $package -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $([IO.Path]::GetFileName($package))" | Set-Content -LiteralPath "$package.sha256" -Encoding ascii
Write-Output "Prepared unsigned Store submission package: $package"
Write-Warning 'Not a signed installer. Complete docs/microsoft-store.md validation before submitting to Microsoft.'
