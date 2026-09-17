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
$files = @('tidedesk-host.exe', 'tidedesk-view.exe')
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
$signingText = if ($Signed) {
    'The executables have verified, timestamped Authenticode signatures. Windows reputation checks may still show a warning.'
} else {
    'The executables are unsigned. Windows SmartScreen may show an unknown-publisher warning.'
}
$template = Get-Content -LiteralPath (Join-Path $repo 'assets/release-README.txt') -Raw
$template.Replace('@VERSION@', $Version).Replace('@SIGNING@', $signingText) |
    Set-Content -LiteralPath (Join-Path $stage 'README.txt') -Encoding utf8NoBOM
$expected = @('LICENSE', 'README.txt', 'tidedesk-host.exe', 'tidedesk-view.exe')
Compress-Archive -LiteralPath ($expected | ForEach-Object { Join-Path $stage $_ }) -DestinationPath $zipPath
$archive = [IO.Compression.ZipFile]::OpenRead($zipPath)
try {
    $actual = @($archive.Entries | ForEach-Object { $_.FullName } | Sort-Object)
    if (Compare-Object $expected $actual -CaseSensitive) { throw 'Unexpected ZIP layout.' }
} finally { $archive.Dispose() }
$hash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $stem" | Set-Content -LiteralPath "$zipPath.sha256" -Encoding ascii
$binaryHashes = $files | ForEach-Object {
    $digest = (Get-FileHash -LiteralPath (Join-Path $stage $_) -Algorithm SHA256).Hash.ToLowerInvariant()
    "$digest  $_"
}
$notes = @(
    "TideDesk ${Version}: free, open-source remote desktop with sound for Windows x64.",
    '',
    "Download $stem and extract it. Run tidedesk-host.exe on the computer to reach and tidedesk-view.exe on the other computer.",
    '',
    $signingText,
    '',
    'Windows to Windows, one viewer at a time. Allow the host through Windows Firewall on private networks.',
    '',
    '[Code signing policy](https://github.com/cristiangirlea/tidedesk/blob/main/docs/code-signing-policy.md)',
    '',
    'SHA-256:',
    '',
    '~~~text',
    "$hash  $stem"
) + $binaryHashes + @('~~~')
$notes | Set-Content -LiteralPath (Join-Path $output 'release-notes.md') -Encoding utf8NoBOM
Write-Output "Packaged $zipPath"
