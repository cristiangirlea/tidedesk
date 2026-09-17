[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = Split-Path $PSScriptRoot -Parent
Push-Location $repo
$variables = @('SIGNPATH_ENABLED', 'SIGNPATH_API_TOKEN', 'SIGNPATH_ORGANIZATION_ID',
    'SIGNPATH_PROJECT_SLUG', 'SIGNPATH_SIGNING_POLICY_SLUG')
$saved = @{}
function Assert-Fails([scriptblock]$Action, [string]$Message) {
    try { & $Action | Out-Null } catch {
        if ($_.Exception.Message -notlike "*$Message*") { throw }
        return
    }
    throw "Expected failure containing: $Message"
}
try {
    foreach ($name in $variables) {
        $saved[$name] = [Environment]::GetEnvironmentVariable($name)
        [Environment]::SetEnvironmentVariable($name, $null)
    }
    $metadata = cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed.' }
    $version = ($metadata.packages | Where-Object name -eq 'tidedesk-host').version
    $settings = ./tools/release-settings.ps1 -Tag "v$version"
    if ($settings.Signing -cne 'false') { throw 'Default must be unsigned.' }
    Assert-Fails { ./tools/release-settings.ps1 -Tag 'invalid' } 'Expected vMAJOR'
    Assert-Fails { ./tools/release-settings.ps1 -Tag 'v999.0.0' } 'does not match'
    $env:SIGNPATH_ENABLED = 'yes'
    Assert-Fails { ./tools/release-settings.ps1 -Tag "v$version" } 'must be true'
    $env:SIGNPATH_ENABLED = 'true'
    Assert-Fails { ./tools/release-settings.ps1 -Tag "v$version" } 'SIGNPATH_API_TOKEN is missing'
    foreach ($name in $variables | Where-Object { $_ -ne 'SIGNPATH_ENABLED' }) {
        [Environment]::SetEnvironmentVariable($name, 'test-placeholder')
    }
    $settings = ./tools/release-settings.ps1 -Tag "v$version-alpha.1"
    if ($settings.Signing -cne 'true') { throw 'Configured signing should be enabled.' }
    foreach ($name in $variables | Where-Object { $_ -ne 'SIGNPATH_ENABLED' }) {
        [Environment]::SetEnvironmentVariable($name, $null)
        Assert-Fails { ./tools/release-settings.ps1 -Tag "v$version" } "$name is missing"
        [Environment]::SetEnvironmentVariable($name, 'test-placeholder')
    }
    $env:SIGNPATH_ENABLED = 'false'
    if ((./tools/release-settings.ps1 -Tag "v$version").Signing -cne 'false') {
        throw 'Signing must require an explicit enable switch.'
    }

    # Exercise actual built executables and extract the final distributable.
    $binaryVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo(
        (Join-Path $repo 'target/release/tidedesk-host.exe')).ProductVersion
    $output = Join-Path $repo ("target/release-test-" + [guid]::NewGuid().ToString('N'))
    ./tools/package-release.ps1 -Version $binaryVersion -OutputDirectory $output
    $zipName = "tidedesk-$binaryVersion-windows-x64.zip"
    $zip = Join-Path $output $zipName
    $checksum = Get-Content -LiteralPath "$zip.sha256"
    $hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($checksum -cne "$hash  $zipName") { throw 'ZIP checksum mismatch.' }
    $extract = Join-Path $output 'extracted'
    Expand-Archive -LiteralPath $zip -DestinationPath $extract
    foreach ($file in @('tidedesk-host.exe', 'tidedesk-view.exe')) {
        $expected = (Get-FileHash -LiteralPath "target/release/$file").Hash
        $actual = (Get-FileHash -LiteralPath (Join-Path $extract $file)).Hash
        if ($expected -cne $actual) { throw "Archive changed $file." }
    }
    $readme = Get-Content -LiteralPath (Join-Path $extract 'README.txt') -Raw
    if ($readme -notmatch 'executables are unsigned' -or $readme -match '@VERSION@|@SIGNING@') {
        throw 'README has incorrect signing/version text.'
    }
    Assert-Fails {
        ./tools/package-release.ps1 -Version $binaryVersion -OutputDirectory $output
    } 'Output already exists'
    Assert-Fails {
        ./tools/package-release.ps1 -Version $binaryVersion -Signed -OutputDirectory "$output-signed"
    } 'valid timestamped Authenticode signature'
    Assert-Fails {
        ./tools/package-release.ps1 -Version '999.0.0' -OutputDirectory "$output-mismatch"
    } 'Unexpected product/version/filename'
    Write-Output 'Release tests passed: configuration gates, archive content, checksums and signed-mode rejection.'
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name]) }
    Pop-Location
}
