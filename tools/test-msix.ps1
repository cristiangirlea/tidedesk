$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = Split-Path $PSScriptRoot -Parent
$binaryRoot = Join-Path $repo 'target/release'
$version = [Diagnostics.FileVersionInfo]::GetVersionInfo((Join-Path $binaryRoot 'tidedesk.exe')).ProductVersion
$output = Join-Path $repo ('target/msix-test-' + [guid]::NewGuid().ToString('N'))
$settings = @{
    Version = $version
    PackageVersion = '1.0.1.0'
    IdentityName = 'TideDesk.TestOnly'
    Publisher = 'CN=TideDesk Packaging Test'
    PublisherDisplayName = 'Test & Validation'
    BinaryDirectory = $binaryRoot
    OutputDirectory = $output
}
function Assert-Rejected([hashtable]$Overrides) {
    $argsCopy = $settings.Clone()
    foreach ($key in $Overrides.Keys) { $argsCopy[$key] = $Overrides[$key] }
    $rejected = $false
    try { & (Join-Path $PSScriptRoot 'package-msix.ps1') @argsCopy | Out-Null }
    catch { $rejected = $true }
    if (-not $rejected) { throw "Expected rejection: $($Overrides | ConvertTo-Json -Compress)" }
}
foreach ($invalid in @('0.1.0.0', '1.0.1.1', '1.0.65536.0', '1.0.01.0', '1.0.1-alpha')) {
    Assert-Rejected @{ PackageVersion = $invalid }
}
Assert-Rejected @{ IdentityName = '../invalid' }
Assert-Rejected @{ Publisher = 'not a publisher' }
Assert-Rejected @{ PublisherDisplayName = ' ' }
Assert-Rejected @{ Version = '99.99.99' }
& (Join-Path $PSScriptRoot 'package-msix.ps1') @settings
$package = Join-Path $output 'tidedesk-1.0.1.0-x64.msix'
$hash = (Get-FileHash -LiteralPath $package -Algorithm SHA256).Hash.ToLowerInvariant()
if ((Get-Content -LiteralPath "$package.sha256" -Raw).Trim() -cne "$hash  tidedesk-1.0.1.0-x64.msix") {
    throw 'Incorrect MSIX checksum.'
}
$archive = [IO.Compression.ZipFile]::OpenRead($package)
try {
    foreach ($file in @('tidedesk.exe', 'LICENSE')) {
        $entry = $archive.GetEntry($file)
        if (-not $entry) { throw "Missing $file in MSIX" }
        $stream = $entry.Open()
        $sha = [Security.Cryptography.SHA256]::Create()
        try { $actual = [Convert]::ToHexString($sha.ComputeHash($stream)) }
        finally { $sha.Dispose(); $stream.Dispose() }
        $source = if ($file -eq 'LICENSE') { Join-Path $repo $file } else { Join-Path $binaryRoot $file }
        if ($actual -cne (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash) {
            throw "$file was changed while packaging."
        }
    }
    if ($archive.GetEntry('AppxSignature.p7x')) { throw 'Test package must be unsigned.' }
    $reader = [IO.StreamReader]::new($archive.GetEntry('AppxManifest.xml').Open())
    try { [xml]$manifest = $reader.ReadToEnd() } finally { $reader.Dispose() }
    if ($manifest.Package.Identity.Name -cne $settings.IdentityName -or
        $manifest.Package.Identity.Version -cne $settings.PackageVersion -or
        $manifest.Package.Identity.Publisher -cne $settings.Publisher -or
        $manifest.Package.Properties.PublisherDisplayName -cne $settings.PublisherDisplayName) {
        throw 'Manifest identity differs from input.'
    }
    # One window, one Start entry, named like the product (Store policy 10.1.1.11 is moot with one).
    if (@($manifest.Package.Applications.Application).Count -ne 1) { throw 'Expected one Start entry.' }
    $names = @($manifest.SelectNodes("//*[local-name()='VisualElements']") | ForEach-Object { $_.DisplayName })
    if ($names.Count -ne 1 -or $names[0] -cne 'TideDesk') { throw "The Start entry must be 'TideDesk', not: $($names -join ', ')" }
    $startup = $manifest.SelectSingleNode("//*[local-name()='StartupTask']")
    if ($startup.TaskId -cne 'TideDeskHost' -or $startup.Enabled -cne 'false') { throw 'Startup must be opt-in.' }
    # One program: the entry and the startup task launch tidedesk.exe plain, which opens the one window.
    $uap10 = 'http://schemas.microsoft.com/appx/manifest/uap/windows10/10'
    $apps = @($manifest.SelectNodes("//*[local-name()='Application']"))
    $startupExtension = $manifest.SelectSingleNode("//*[local-name()='Extension' and @Category='windows.startupTask']")
    foreach ($node in $apps + @($startupExtension)) {
        if ($node.Executable -cne 'tidedesk.exe') { throw "Every entry must launch tidedesk.exe, not $($node.Executable)." }
        if ($node.HasAttribute('Parameters', $uap10)) { throw 'tidedesk.exe is launched with no mode word.' }
    }
    if (-not $archive.GetEntry('Assets/Square44x44Logo.png')) { throw 'Missing tile asset.' }
    foreach ($notice in @(
        'licenses/third-party/INDEX.txt',
        'licenses/third-party/openh264-sys2-0.9.8/OpenH264-LICENSE',
        'licenses/third-party/opusic-sys-0.7.5/Opus-COPYING',
        'licenses/third-party/epaint_default_fonts-0.34.3/OFL.txt',
        'licenses/third-party/epaint_default_fonts-0.34.3/UFL.txt',
        'licenses/third-party/egui_software_backend-vendored/LICENSE-MIT'
    )) {
        if (-not $archive.GetEntry($notice)) { throw "Missing third-party notice: $notice" }
    }
} finally { $archive.Dispose() }
Assert-Rejected @{}
Write-Output 'MSIX packaging checks passed. Installation and Store certification are separate gates.'
