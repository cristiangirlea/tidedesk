<#
Checks what the release notes say, before a release is published from them.
Run it on release-notes.md (the template), or on the notes the release build
filled in (with -Filled), on Windows PowerShell 5.1 or PowerShell 7:

    ./tools/check-release-notes.ps1 -Path release-notes.md
    ./tools/check-release-notes.ps1 -Path target/dist/release-notes.md -Filled -ProtocolVersion 5
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Path,
    # The notes were filled in by the release build: no placeholder may be left.
    [switch]$Filled,
    # The protocol the release speaks, when known: the notes must name it. Given
    # empty (a release step that found no version), it is refused, not skipped.
    [ValidateRange(1, 65535)][int]$ProtocolVersion
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$text = [IO.File]::ReadAllText((Resolve-Path -LiteralPath $Path).Path)
$problems = New-Object Collections.Generic.List[string]
function Require([bool]$ok, [string]$problem) { if (-not $ok) { $problems.Add($problem) } }

$placeholders = @('@VERSION@', '@ZIP@', '@SIGNING@', '@HASHES@')
$found = @([regex]::Matches($text, '@[A-Z]+@') | ForEach-Object { $_.Value } | Sort-Object -Unique)
if ($Filled) {
    Require ($found.Count -eq 0) "Unfilled placeholders: $($found -join ', ')."
} else {
    foreach ($name in $placeholders) { Require ($found -ccontains $name) "The template must keep $name." }
    foreach ($name in $found) { Require ($placeholders -ccontains $name) "Unknown placeholder $name." }
}

Require ($text -match '(?m)^## New in this release\r?$') 'The changes must start with "## New in this release".'
Require ($text -match '(?m)^## Known issues\r?$') 'The notes must keep a "## Known issues" section.'
Require ($text -match 'Business use requires separate written permission' -and $text -notmatch 'free, open-source') `
    'The licence lines are stale.'
Require ($text -match 'terms-of-use\.md') 'The notes must link the terms of use.'
Require ($text -match '(?is)Game Boost.*experimental|experimental.*Game Boost' -and $text -match 'Ctrl\+Alt\+G') `
    'The notes must describe experimental Game Boost and its shortcut.'
Require ($text -match 'direct internet' -and $text -notmatch 'relay support is coming') `
    'The notes must describe direct internet connections and promise no relay.'
Require ($text -match 'device ID' -and $text -match 'same local network') `
    'The notes must say device IDs also work on the same local network.'
Require ($text -match 'local addresses') `
    'The notes must say a viewer behind the same router tries the host''s local addresses.'
Require ($text -match 'graphics card' -and $text -notmatch 'no GPU encoding|GPU video acceleration') `
    'The notes must describe encoding on the graphics card.'
if ($PSBoundParameters.ContainsKey('ProtocolVersion')) {
    # Earlier releases' sections may name older protocols; what this build uses must be this one.
    $uses = @([regex]::Matches($text, '(?:uses|speaks) protocol v(\d+)') | ForEach-Object { [int]$_.Groups[1].Value } |
        Sort-Object -Unique)
    Require ($uses.Count -gt 0 -and @($uses | Where-Object { $_ -ne $ProtocolVersion }).Count -eq 0) `
        "The notes must say this build uses protocol v$ProtocolVersion (they say: $($uses -join ', '))."
}

if ($problems.Count -gt 0) { throw ("Release notes check failed:`n- " + ($problems -join "`n- ")) }
Write-Output "Release notes check passed: $Path"
