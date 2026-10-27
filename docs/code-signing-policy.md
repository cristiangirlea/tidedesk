# Code signing policy

## Current status

TideDesk's published alpha executable, tidedesk.exe, is unsigned. Optional SignPath integration
is prepared but production signing is not active. Each release's notes state
whether that release's executable is signed.

The personal-use-only source license is not eligible for SignPath Foundation's
open-source program. Do not claim Foundation approval or submit this licensing
model as an OSI-approved open-source project. A separately authorized signing
service or Microsoft Store MSIX distribution must be evaluated before activation.
See https://signpath.org/terms.html.

The Microsoft Store package is the signed Windows build. The maintainer builds an
unsigned MSIX submission candidate by hand, from the tidedesk.exe in that release's ZIP,
and submits it by hand; nothing submits or publishes automatically. Microsoft signs the
Store package after certification; this does not sign the portable GitHub ZIP.

## Responsibility

Cristian Girlea (https://github.com/cristiangirlea) maintains TideDesk, reviews external
contributions and is the designated release-signing approver. Changes from other
contributors require maintainer review, including changes to dependencies and build scripts.

Release builds have no signing step; activating signing means adding one to the
release build. The SignPath integration prepared in the source repository's own workflow,
which does not make releases, runs on GitHub-hosted Windows runners. When signing is
activated, release signatures require manual approval in SignPath. The signing
configuration limits signing to the TideDesk executable, tidedesk.exe, and verifies
its product name, version and original filename. Enabling signing makes signature
verification mandatory before publication.

## How releases are built and checked

TideDesk's source code is kept in private repositories. Since v0.1.0-alpha.14, Windows
releases are built on the maintainer's own Windows computer, not on a GitHub-hosted
machine, from a private
build repository that pins the exact commit of the source it builds. Only the
maintainer tags and publishes a release.

The build repository has a release workflow for that computer. It refuses to build
unless the source carries the same release tag on exactly the pinned commit, checks the
release notes before the build and again once the checksums are filled in, checks the
formatting, lints with warnings as errors, runs the build repository's tests and builds
with the locked dependency versions. The source's own tests run in its CI on Linux for
every code change; Windows CI for the source is not set up yet. Packaging checks that tidedesk.exe's product name, version and file name match
the release. A published release is never overwritten. That workflow's Windows runner
is not set up yet: releases since v0.1.0-alpha.14 were built and published by the
maintainer by hand on that computer. Earlier releases were built by the source
repository's own workflow on GitHub-hosted Windows runners.

Each release's notes list the SHA-256 checksums of the ZIP and of tidedesk.exe, and a
`.sha256` file for the ZIP is attached to the release. The Microsoft Store package is
made from that same tidedesk.exe and signed by Microsoft.

## Privacy

TideDesk sends screen images, system audio, remote input and optionally clipboard text between computers selected
by the operator. The host accepts connections using an access code; the viewer verifies
the host identity. Saved settings and computer entries are stored locally. The current
application has no analytics, advertising or automatic upload service.

Clipboard sharing is disabled by default and requires both host permission and viewer
activation. Only new text copies after activation are shared; clipboard contents are
not saved to settings or logs.

To show the host's internet address, TideDesk Host asks public STUN servers (by
default `stun.l.google.com` and `stun.cloudflare.com`) which address and port its
router uses, about every 25 seconds while it runs. Each request is 20 bytes with no
content; the server learns the computer's public IP address and port, as any server it
contacts would. Turn this off, or name other servers, under Settings, Internet (or
with `discover_public_address = false` or `stun_servers` in `host.toml`).

When connecting over the internet, TideDesk Viewer asks the same STUN servers once for
its own address (`--stun` chooses others). The small punch packets that open the path
(42 bytes, no content) go only to the address the user typed, and the session then runs
directly between the two computers: TideDesk never relays it through a server.

By default, TideDesk Host registers with TideDesk's own connection service,
`rendezvous.tidedesk.app` (run by the maintainer): the service learns the host's device
ID, its certificate (which is public) and its public IP address and port, refreshed
about every 25 seconds, so that it can introduce viewers who ask for that ID, and the
host's local network addresses sealed with the access code, which it cannot read and
passes only to a viewer at the host's own public address. It keeps this in memory
only, logs counts rather than IDs or addresses, and never carries sessions, access
codes or anything else. Turn it off under Settings, Internet (or with
`rendezvous = false` in `host.toml`; `--no-rendezvous` for a headless host); another
service can be named with `rendezvous_server` there. A viewer connecting by device ID
asks the same service (or the one named in `viewer.toml`) for that ID, which tells the service the
viewer's public address and which ID it asked for; a viewer that never connects by ID
never contacts it. Using the service is subject to the [terms of use](terms-of-use.md).

A viewer connecting by device ID also broadcasts a 24-byte query for that ID on its
local network (UDP port 47800), so other computers on that network can see which ID
it looks for. TideDesk Host answers only queries for its own ID from computers on its
local network, with a packet of the same size; nothing of this leaves the local
network. Turn answering off under Settings, Network (or with `lan_discovery = false`
in `host.toml`).

Problems TideDesk runs into are kept on the computer, under History, Problems. A
report is sent only when the user presses "Send to TideDesk" after seeing all of it:
it goes, with TideDesk's version, to `report.tidedesk.app` (run by the maintainer),
which keeps the report text to look into the problem and answers with a reference.
The service sees the computer's public IP address while the report arrives, to limit
how many reports one address sends, and does not store it with the report. Access
codes are hidden in reports before they are shown. An organisation can send its
computers' reports to its own report service instead, or turn sending off
(`ReportServer`, `ReportServerFingerprint` and `Reports` under
`HKLM\SOFTWARE\Policies\TideDesk`); TideDesk then receives none of them.

TideDesk for Android is a viewer only: it shows a computer's screen (and, from
v0.1.0-alpha.17, plays its sound on the phone), and sends the phone's taps and keys to that computer; it shares nothing of the
phone itself. Besides the network, it asks only to show notifications: from v0.1.0-alpha.18, a
session left for another app is held for up to 3 minutes with a notification that says so and ends it, and
is never held where that notification cannot show. It reads whether the phone is on mobile
data only to ask before a session there. Connecting by device ID, it asks TideDesk's
connection service for that ID, which tells the service the phone's public IP address and
port and which ID it asked for; it then sends a few small packets with no content to the
address the service gave, and the session runs directly between the phone and the computer.
Connecting by address, it contacts that address only. It sends no STUN request and no query
on the local network. The computer sees the phone's name: the one chosen in the app's
Settings, or else the phone's own device name, and the phone's own certificate, made on the
phone, by which the computer knows it again (a dropped session's code keeps working for it a
few minutes). The app keeps in its own storage on the phone its settings, that certificate
and its key, the computers it connected to (each one's name, and its device ID or address)
and the fingerprint of the certificate of each computer it reached by address (from
v0.1.0-alpha.17, by device ID too). It never saves an access code: from v0.1.0-alpha.18,
Reconnect is offered for up to 4 minutes 30 seconds from the first end of a session, with the
code last typed, which the app holds in memory only, not in its storage, until Reconnect is no
longer offered, another code is used, or the app is closed. It keeps no logs,
sends no problem reports, and has no analytics or advertising. Settings, "Forget all
computers" removes the list of computers; uninstalling the app removes everything it kept.

Operators choose their network destinations. If they use a separate VPN or other
third-party network service, that service's privacy policy also applies.

## Checking a download

Use the SHA-256 file attached to the same release to check download integrity.
A checksum alone does not authenticate the publisher. For a signed release, check
tidedesk.exe's Digital Signatures tab in Windows file properties or use
Get-AuthenticodeSignature in PowerShell.

A valid signature identifies a signer and detects modification. Windows reputation
checks may still display warnings; a signature is not a guarantee of instant
SmartScreen acceptance.
