[CmdletBinding()]
param([Parameter(Mandatory)][string]$StageDirectory)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = Split-Path $PSScriptRoot -Parent
$stage = (Resolve-Path -LiteralPath $StageDirectory).Path
$destination = Join-Path $stage 'licenses/third-party'
if (Test-Path -LiteralPath $destination) { throw "Notices already exist: $destination" }

Push-Location $repo
try {
    $metadataText = & cargo metadata --locked --offline --format-version 1 --filter-platform x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Could not read locked Windows dependency metadata.' }
} finally {
    Pop-Location
}
$metadata = ($metadataText -join "`n") | ConvertFrom-Json
$packages = @($metadata.packages | Where-Object { $_.source -like 'registry+*' } | Sort-Object name, version)
if ($packages.Count -lt 1) { throw 'No registry dependencies found.' }

# Some crates publish the license expression but omit its text from the crate archive.
# These fallback texts are from dependencies already present in the locked graph.
$boostCrate = $packages | Where-Object { $_.name -eq 'error-code' } | Select-Object -First 1
if (-not $boostCrate) { throw 'Missing BSL-1.0 fallback source.' }
$boostText = Join-Path (Split-Path $boostCrate.manifest_path -Parent) 'LICENSE'
$apacheText = Join-Path $repo 'third_party/egui_software_backend/LICENSE-APACHE'

New-Item -ItemType Directory -Path $destination | Out-Null
$index = [Collections.Generic.List[string]]::new()
$index.Add('Third-party notices for the TideDesk Windows x64 package')
$index.Add('Generated from the exact Cargo.lock dependency graph at packaging time.')
$index.Add('An entry may include build-time dependencies; extra notices are intentional.')
$index.Add('License expressions below come from each dependency Cargo manifest.')
$index.Add('Package | Version | SPDX expression | License files in this directory')

foreach ($package in $packages) {
    $source = Split-Path $package.manifest_path -Parent
    $folder = Join-Path $destination "$($package.name)-$($package.version)"
    New-Item -ItemType Directory -Path $folder | Out-Null
    $files = @(Get-ChildItem -LiteralPath $source -File | Where-Object {
        $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE|COPYRIGHT)([.-]|$)'
    })
    foreach ($file in $files) { Copy-Item -LiteralPath $file.FullName -Destination $folder }

    switch ($package.name) {
        'epaint_default_fonts' {
            $fontFolder = Join-Path $source 'fonts'
            foreach ($file in @(Get-ChildItem -LiteralPath $fontFolder -Filter '*.txt' -File)) {
                Copy-Item -LiteralPath $file.FullName -Destination $folder
            }
        }
        'openh264-sys2' {
            Copy-Item -LiteralPath (Join-Path $source 'upstream/LICENSE') -Destination (Join-Path $folder 'OpenH264-LICENSE')
        }
        'openh264' {
            $native = $packages | Where-Object { $_.name -eq 'openh264-sys2' -and $_.version -eq $package.version } | Select-Object -First 1
            if (-not $native) { throw 'Missing matching OpenH264 source license.' }
            Copy-Item -LiteralPath (Join-Path (Split-Path $native.manifest_path -Parent) 'upstream/LICENSE') -Destination (Join-Path $folder 'OpenH264-LICENSE')
        }
        'opusic-sys' {
            Copy-Item -LiteralPath (Join-Path $source 'opus/COPYING') -Destination (Join-Path $folder 'Opus-COPYING')
        }
        'ring' {
            Copy-Item -LiteralPath (Join-Path $source 'third_party/fiat/LICENSE') -Destination (Join-Path $folder 'fiat-LICENSE')
            Copy-Item -LiteralPath (Join-Path $source 'src/polyfill/once_cell/LICENSE-MIT') -Destination (Join-Path $folder 'once_cell-LICENSE-MIT')
            Copy-Item -LiteralPath (Join-Path $source 'src/polyfill/once_cell/LICENSE-APACHE') -Destination (Join-Path $folder 'once_cell-LICENSE-APACHE')
        }
    }

    if (-not @(Get-ChildItem -LiteralPath $folder -File).Count) {
        if ($package.name -eq 'clipboard-win' -and $package.license -eq 'BSL-1.0') {
            Copy-Item -LiteralPath $boostText -Destination (Join-Path $folder 'LICENSE-BSL-1.0')
        } elseif ($package.license -match 'Apache-2.0') {
            Copy-Item -LiteralPath $apacheText -Destination (Join-Path $folder 'LICENSE-APACHE-2.0')
        } else {
            throw "No distributable license text found for $($package.name) $($package.version) [$($package.license)]."
        }
    }
    $names = @(Get-ChildItem -LiteralPath $folder -File | Sort-Object Name | ForEach-Object Name)
    $index.Add("$($package.name) | $($package.version) | $($package.license) | $($names -join ', ')")
}

$vendored = Join-Path $destination 'egui_software_backend-vendored'
New-Item -ItemType Directory -Path $vendored | Out-Null
Copy-Item -LiteralPath (Join-Path $repo 'third_party/egui_software_backend/LICENSE-MIT') -Destination $vendored
Copy-Item -LiteralPath $apacheText -Destination $vendored
$index.Add('egui_software_backend | vendored | MIT OR Apache-2.0 | LICENSE-APACHE, LICENSE-MIT')
$index | Set-Content -LiteralPath (Join-Path $destination 'INDEX.txt') -Encoding utf8
Write-Output "Bundled notices for $($packages.Count) locked registry packages and the vendored backend."
