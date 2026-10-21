<#
Tests tools/check-release-notes.ps1 on release-notes.md and on changed copies of it.
Run before merging a change to either, on Windows PowerShell 5.1 or PowerShell 7:

    ./tools/test-check-release-notes.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = Split-Path $PSScriptRoot -Parent
$check = Join-Path $repo 'tools/check-release-notes.ps1'
$notes = Join-Path $repo 'release-notes.md'
$tmp = Join-Path ([IO.Path]::GetTempPath()) ('release-notes-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null

function Write-Notes([string]$name, [string]$text) {
    $path = Join-Path $tmp $name
    [IO.File]::WriteAllText($path, $text)
    $path
}
function Assert-Fails([string]$text, [hashtable]$options, [string]$message) {
    $path = Write-Notes 'case.md' $text
    try { & $check -Path $path @options | Out-Null } catch {
        if ($_.Exception.Message -notlike "*$message*") { throw "Expected '$message', got: $($_.Exception.Message)" }
        return
    }
    throw "Expected a failure containing: $message"
}

try {
    $template = [IO.File]::ReadAllText($notes)
    # The protocol the template says this build uses; it changes with releases.
    $protocol = [int][regex]::Match($template, '(?:uses|speaks) protocol v(\d+)').Groups[1].Value
    if ($protocol -lt 2) { throw 'The template names no protocol this build uses.' }
    & $check -Path $notes | Out-Null
    & $check -Path $notes -ProtocolVersion $protocol | Out-Null
    $filled = $template.Replace('@VERSION@', '0.1.0-alpha.99').Replace('@ZIP@', 'tidedesk.zip').
        Replace('@SIGNING@', 'Signing text.').Replace('@HASHES@', 'hashes')
    & $check -Path (Write-Notes 'filled.md' $filled) -Filled -ProtocolVersion $protocol | Out-Null

    Assert-Fails $template @{ ProtocolVersion = $protocol - 1 } "uses protocol v$($protocol - 1)"
    Assert-Fails $template @{ ProtocolVersion = '' } 'ProtocolVersion'
    Assert-Fails $template @{ Filled = $true } 'Unfilled placeholders'
    Assert-Fails $filled @{} 'must keep @VERSION@'
    Assert-Fails ($template + ' @FOO@') @{} 'Unknown placeholder @FOO@'
    Assert-Fails $template.Replace('## New in this release', '## New') @{} 'New in this release'
    Assert-Fails $template.Replace('## Known issues', '## Issues') @{} 'Known issues'
    Assert-Fails $template.Replace('same local network', 'LAN') @{} 'same local network'
    Assert-Fails $template.Replace('local addresses', 'addresses') @{} 'local addresses'
    Assert-Fails ($template + ' relay support is coming') @{} 'promise no relay'
    Assert-Fails $template.Replace('Ctrl+Alt+G', 'a key') @{} 'Game Boost'
    Assert-Fails $template.Replace('terms-of-use.md', 'terms.md') @{} 'terms of use'
    Assert-Fails $template.Replace('graphics card', 'GPU') @{} 'graphics card'
    Assert-Fails ($template + ' Audio remains host system output; no microphone forwarding.') @{} 'deny the microphone'
    Assert-Fails $template.Replace('Business use requires separate written permission', 'x') @{} 'licence lines'
    Write-Output 'Release notes check tests passed.'
} finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force
}
