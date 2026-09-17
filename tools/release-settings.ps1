[CmdletBinding()]
param([Parameter(Mandatory)][string]$Tag)

$ErrorActionPreference = 'Stop'
if ($Tag -cnotmatch '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$') {
    throw 'Expected vMAJOR.MINOR.PATCH with an optional prerelease suffix.'
}
$version = $Tag.Substring(1)
$metadata = cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw 'Cannot read Cargo workspace metadata.' }
$baseVersion = $version.Split('-')[0]
foreach ($package in $metadata.packages) {
    if ($package.id -in $metadata.workspace_members -and $package.version -ne $baseVersion) {
        throw "Tag base version $baseVersion does not match $($package.name) $($package.version)."
    }
}

$enabled = $env:SIGNPATH_ENABLED
if ($enabled -and $enabled -cnotin @('true', 'false')) {
    throw 'SIGNPATH_ENABLED must be true, false, or unset.'
}
$signing = $enabled -ceq 'true'
if ($signing) {
    foreach ($name in @('SIGNPATH_API_TOKEN', 'SIGNPATH_ORGANIZATION_ID',
        'SIGNPATH_PROJECT_SLUG', 'SIGNPATH_SIGNING_POLICY_SLUG')) {
        if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($name))) {
            throw "Signing enabled but $name is missing."
        }
    }
}
[pscustomobject]@{ Version = $version; Signing = $signing.ToString().ToLowerInvariant() }
