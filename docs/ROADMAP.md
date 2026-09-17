# Roadmap

This is a technical planning list, not a promise of delivery, dates or future
licensing terms. Features under "Planned" and "Exploration" are not available yet.
See [licensing](licensing.md) for development-branch and earlier-release terms.

## Available in the Windows alpha

- Windows-to-Windows screen, keyboard and mouse control; one viewer at a time.
- DXGI capture, OpenH264 software video, WASAPI system audio and Opus.
- QUIC/TLS, access-code authentication, throttling and host fingerprint pinning.
- Tray host with saved settings and a viewer with saved computers.
- Optional text clipboard sharing, independent host permissions and custom shortcuts.
- Separate host/viewer cursor indicators and safe mouse-control handoff.
- Native-size viewer startup when the display permits, with per-host window placement.

Real-world two-computer and multi-monitor validation of recent interaction/window
changes is still pending. No hardware encoder selector, permanent user-chosen
password, 2FA or paid-feature enforcement is implemented.

## Planned: Windows reliability and security

- Complete two-computer, multi-monitor and mixed-DPI validation.
- Optional permanent password alongside the random code, with a separate,
  reviewed authentication flow, strong-password checks and protected local storage.
- Optional local TOTP two-factor authentication and recovery codes. Design goal:
  no TideDesk account or central user database; protected verifier data stays on the host.
- Selectable hardware encoders where actually supported, retaining software fallback.
  Performance, hardware support and codec licensing need validation.
- Adaptive bitrate/frame rate, lower memory use and improved presentation.
- Native cursor shapes/visibility, in-session monitor switching and image clipboard.
- Evaluate signed distribution and MSIX/Store packaging.

## Exploration, not scheduled

- Direct internet connectivity, NAT traversal and relay options.
- Linux and mobile clients; macOS support.
- File transfer, session recording and multiple viewers.
- Team administration, shared device lists and ticketing integration.
- Windows service mode and secure-desktop support, subject to security review.
