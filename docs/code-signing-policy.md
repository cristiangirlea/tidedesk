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

Microsoft Store MSIX distribution is the planned package-signing route.
The release workflow can prepare an unsigned submission candidate once Store
identity variables are configured. It does not submit or publish automatically.
Microsoft signs the Store package after certification; this does not sign the
portable GitHub ZIP. [Setup and remaining validation](microsoft-store.md).

## Responsibility

Cristian Girlea (https://github.com/cristiangirlea) maintains TideDesk, reviews external
contributions and is the designated release-signing approver. Changes from other
contributors require maintainer review, including changes to dependencies and build scripts.

When signing is activated, release signatures require manual approval in SignPath.
Builds run on GitHub-hosted Windows runners from the public source repository. The
signing configuration limits signing to the TideDesk executable, tidedesk.exe, and verifies
its product name, version and original filename. Enabling signing makes signature
verification mandatory before publication.

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

By default, TideDesk Host registers with TideDesk's own rendezvous service,
`rendezvous.tidedesk.app` (run by the maintainer): the service learns the host's device
ID, its certificate (which is public) and its public IP address and port, refreshed
about every 25 seconds, so that it can introduce viewers who ask for that ID. It keeps
this in memory only, logs counts rather than IDs or addresses, and never carries
sessions, access codes or anything else. Turn it off, or name another service, under
Settings, Internet (or with `rendezvous = false` or `rendezvous_server` in `host.toml`;
`--no-rendezvous` for a headless host). A viewer connecting by device ID asks the same
service (or the one in Viewer Settings) for that ID, which tells the service the
viewer's public address and which ID it asked for; a viewer that never connects by ID
never contacts it.

A viewer connecting by device ID also broadcasts a 24-byte query for that ID on its
local network (UDP port 47800), so other computers on that network can see which ID
it looks for. TideDesk Host answers only queries for its own ID from computers on its
local network, with a packet of the same size; nothing of this leaves the local
network. Turn answering off under Settings, Network (or with `lan_discovery = false`
in `host.toml`).

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
