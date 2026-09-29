[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Version,
    [string]$BinaryDirectory = 'target/release',
    [string]$OutputDirectory = 'target/dist',
    [switch]$Signed
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($Version -cnotmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$') {
    throw 'Invalid release version.'
}
$repo = Split-Path $PSScriptRoot -Parent
$files = @('tidedesk.exe')
$binaryRoot = (Resolve-Path -LiteralPath $BinaryDirectory).Path
foreach ($file in $files) {
    $path = Join-Path $binaryRoot $file
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing $file" }
    $info = [Diagnostics.FileVersionInfo]::GetVersionInfo($path)
    if ($info.ProductName -cne 'TideDesk' -or $info.ProductVersion -cne $Version -or
        $info.OriginalFilename -cne $file) {
        throw "Unexpected product/version/filename metadata in $file."
    }
    if ($Signed) {
        $signature = Get-AuthenticodeSignature -LiteralPath $path
        if ($signature.Status -ne 'Valid' -or $null -eq $signature.TimeStamperCertificate) {
            throw "$file must have a valid timestamped Authenticode signature."
        }
    }
}

$output = [IO.Path]::GetFullPath($OutputDirectory)
$stem = "tidedesk-$Version-windows-x64.zip"
$zipPath = Join-Path $output $stem
$stage = Join-Path $output "stage-$Version"
# Never overwrite an earlier package or mix stale files into a release.
foreach ($path in @($stage, $zipPath, "$zipPath.sha256", (Join-Path $output 'release-notes.md'))) {
    if (Test-Path -LiteralPath $path) { throw "Output already exists: $path" }
}
New-Item -ItemType Directory -Path $stage -Force | Out-Null
foreach ($file in $files) { Copy-Item -LiteralPath (Join-Path $binaryRoot $file) -Destination $stage }
Copy-Item -LiteralPath (Join-Path $repo 'LICENSE') -Destination $stage
# Binary distribution must carry the dependencies' license notices.
Copy-Item -LiteralPath (Join-Path $repo 'licenses') -Destination $stage -Recurse
& (Join-Path $repo 'tools/package-third-party-notices.ps1') -StageDirectory $stage | Out-Null
$signingText = if ($Signed) {
    'tidedesk.exe has a verified, timestamped Authenticode signature. Windows reputation checks may still show a warning.'
} else {
    'tidedesk.exe is unsigned. Windows SmartScreen may show an unknown-publisher warning.'
}
$template = Get-Content -LiteralPath (Join-Path $repo 'assets/release-README.txt') -Raw
$template.Replace('@VERSION@', $Version).Replace('@SIGNING@', $signingText) |
    Set-Content -LiteralPath (Join-Path $stage 'README.txt') -Encoding utf8NoBOM
$expected = @('LICENSE', 'README.txt', 'tidedesk.exe')
Add-Type -AssemblyName System.IO.Compression.FileSystem
# CreateFromDirectory writes '/'-separated entry names on both PowerShell editions.
[IO.Compression.ZipFile]::CreateFromDirectory($stage, $zipPath, [IO.Compression.CompressionLevel]::Optimal, $false)
$archive = [IO.Compression.ZipFile]::OpenRead($zipPath)
try {
    $names = @($archive.Entries | ForEach-Object { $_.FullName })
    $root = @($names | Where-Object { $_ -notlike 'licenses/*' } | Sort-Object)
    if (Compare-Object $expected $root -CaseSensitive) { throw 'Unexpected ZIP layout.' }
    foreach ($required in @('licenses/AGPL-3.0-only.txt', 'licenses/third-party/INDEX.txt')) {
        if ($names -cnotcontains $required) { throw "ZIP is missing $required." }
    }
    if ($names | Where-Object { $_ -like '*\*' }) { throw 'ZIP entry names must use forward slashes.' }
} finally { $archive.Dispose() }
$hash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $stem" | Set-Content -LiteralPath "$zipPath.sha256" -Encoding ascii
$binaryHashes = $files | ForEach-Object {
    $digest = (Get-FileHash -LiteralPath (Join-Path $stage $_) -Algorithm SHA256).Hash.ToLowerInvariant()
    "$digest  $_"
}
# The notes' text lives in assets/release-notes.md, next to the README's.
$hashes = (@("$hash  $stem") + $binaryHashes) -join "`n"
$notes = (Get-Content -LiteralPath (Join-Path $repo 'assets/release-notes.md') -Raw).
    Replace('@VERSION@', $Version).Replace('@ZIP@', $stem).
    Replace('@SIGNING@', $signingText).Replace('@HASHES@', $hashes)
$notes | Set-Content -LiteralPath (Join-Path $output 'release-notes.md') -Encoding utf8NoBOM -NoNewline
Write-Output "Packaged $zipPath"
