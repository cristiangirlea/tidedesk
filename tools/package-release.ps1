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
# Binary distribution must carry the dependencies' license notices.
Copy-Item -LiteralPath (Join-Path $repo 'licenses') -Destination $stage -Recurse
& (Join-Path $repo 'tools/package-third-party-notices.ps1') -StageDirectory $stage | Out-Null
$signingText = if ($Signed) {
    'The executables have verified, timestamped Authenticode signatures. Windows reputation checks may still show a warning.'
} else {
    'The executables are unsigned. Windows SmartScreen may show an unknown-publisher warning.'
}
$template = Get-Content -LiteralPath (Join-Path $repo 'assets/release-README.txt') -Raw
$template.Replace('@VERSION@', $Version).Replace('@SIGNING@', $signingText) |
    Set-Content -LiteralPath (Join-Path $stage 'README.txt') -Encoding utf8NoBOM
$expected = @('LICENSE', 'README.txt', 'tidedesk-host.exe', 'tidedesk-view.exe')
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
$notes = @(
    "TideDesk ${Version}: remote desktop for Windows x64, free for personal, non-commercial use.",
    '',
    'License: TideDesk Personal Use Source License 1.0 (see LICENSE in the ZIP). Business use requires separate written permission. Previously published AGPL releases retain their original permissions.',
    '',
    "Download $stem and extract it. Run tidedesk-host.exe on the computer to reach and tidedesk-view.exe on the other computer.",
    '',
    $signingText,
    '',
    '**Experimental alpha — not a stable release.** Game Boost is opt-in and off by default. Real-world two-computer gameplay and end-to-end latency validation are still pending.',
    '',
    'New in this build: experimental direct internet connections, computer to computer, with no relay. The host shows its internet address, learned from public STUN servers (configurable in Host Settings, Internet). In the viewer, tick "Over the internet", connect to that address, and give the viewer''s address to the person at the host, who types it under "Viewer on another network" and presses Open. Both computers then open a path through their routers and the session runs directly between them. Symmetric NAT, common on mobile data, cannot be traversed: use a VPN such as Tailscale there. Hosts with a forwarded port connect without the manual step.',
    '',
    "[Internet access](https://github.com/cristiangirlea/tidedesk/blob/v$Version/docs/internet-access.md) and [design notes](https://github.com/cristiangirlea/tidedesk/blob/v$Version/docs/design/nat-traversal.md)",
    '',
    '**Keep host and viewer on the same release.** This build uses protocol v3: it connects to v0.1.0-alpha.3 and later on a local network, but not to the earlier v1/v2 alpha releases. Internet connections need both computers on this release.',
    '',
    'Also experimental: connect by device ID. With a rendezvous service set in Host Settings and Viewer Settings (TideDesk''s own service is being set up and will become the default), a viewer types the host''s device ID instead of anyone typing internet addresses, and headless hosts can be reached too. The service only introduces the two computers; the session still runs directly between them.',
    '',
    'Retained from v0.1.0-alpha.4: no Microsoft Visual C++ Redistributable needed, and third-party license notices in the licenses folder.',
    '',
    'Retained: experimental Game Boost, available in Viewer Settings or with Ctrl+Alt+G (customizable). Switch live without reconnecting: 60 FPS target, motion-oriented OpenH264 software encoding, a smaller pending decode queue and lower audio buffering. Turning Boost off restores the desktop profile. Actual FPS depends on both PCs and the connection; host resolution, bitrate and sharing permissions are unchanged.',
    '',
    'Keyboard and desktop/absolute mouse input only. Relative game-camera input, GPU video acceleration, controllers and USB forwarding are not implemented. Audio remains host system output to viewer only; no microphone forwarding or additional driver dependency.',
    '',
    "[Game Boost usage and limitations](https://github.com/cristiangirlea/tidedesk/blob/v$Version/docs/game-boost.md)",
    '',
    'Also retained: the viewer remembers each host window location and monitor. Sessions start at the host native pixel size, shrinking proportionally only when needed to fit the available screen. Neither display resolution is changed.',
    '',
    'Includes optional bidirectional text clipboard sharing, customizable clipboard/mouse shortcuts, safe mouse handoff, and separate host/viewer cursor indicators.',
    '',
    "[Clipboard and mouse controls](https://github.com/cristiangirlea/tidedesk/blob/v$Version/docs/interaction-controls.md)",
    '',
    'Alpha validation: automated checks pass; live multi-monitor and two-computer verification is still pending.',
    '',
    'Windows to Windows, one viewer at a time. Allow the host through Windows Firewall on private networks.',
    '',
    "[Code signing policy](https://github.com/cristiangirlea/tidedesk/blob/v$Version/docs/code-signing-policy.md)",
    '',
    'SHA-256:',
    '',
    '~~~text',
    "$hash  $stem"
) + $binaryHashes + @('~~~')
$notes | Set-Content -LiteralPath (Join-Path $output 'release-notes.md') -Encoding utf8NoBOM
Write-Output "Packaged $zipPath"
