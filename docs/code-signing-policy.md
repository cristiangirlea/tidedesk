# Code signing policy

## Current status

TideDesk's published alpha executables are unsigned. SignPath integration is prepared;
Foundation approval and production signing are not yet active. Each release's notes
state whether that release's executables are signed.

## Responsibility

Cristian Girlea (https://github.com/cristiangirlea) maintains TideDesk, reviews external
contributions and is the designated release-signing approver. Changes from other
contributors require maintainer review, including changes to dependencies and build scripts.

When signing is activated, release signatures require manual approval in SignPath.
Builds run on GitHub-hosted Windows runners from the public source repository. The
signing configuration limits signing to the two TideDesk executables and verifies
their product name, version and original filenames. Enabling signing makes signature
verification mandatory before publication.

## Privacy

TideDesk sends screen images, system audio, remote input and optionally clipboard text between computers selected
by the operator. The host accepts connections using an access code; the viewer verifies
the host identity. Saved settings and computer entries are stored locally. The current
application has no analytics, advertising or automatic upload service.

Clipboard sharing is disabled by default and requires both host permission and viewer
activation. Only new text copies after activation are shared; clipboard contents are
not saved to settings or logs.

Operators choose their network destinations. If they use a separate VPN or other
third-party network service, that service's privacy policy also applies.

## Checking a download

Use the SHA-256 file attached to the same release to check download integrity.
A checksum alone does not authenticate the publisher. For a signed release, check
both executables' Digital Signatures tab in Windows file properties or use
Get-AuthenticodeSignature in PowerShell.

A valid signature identifies a signer and detects modification. Windows reputation
checks may still display warnings; a signature is not a guarantee of instant
SmartScreen acceptance.
