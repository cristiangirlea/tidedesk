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
- No Visual C++ Redistributable dependency; third-party license notices ship with builds.

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

## Experimental in v0.1.0-alpha.3: initial Game Boost

- Saved Viewer Settings button and customizable Ctrl+Alt+G shortcut, applied live.
- 60 FPS target, motion-oriented software encoding, bounded decode queue and
  smaller audio buffer, with return to the host's desktop FPS.
- Unchanged host resolution, bitrate and sharing permissions; input release on
  profile changes and coalesced mouse movement without reordering input barriers.
- Synthetic live-switching and codec regression tests. Two-computer gameplay,
  visual quality and end-to-end latency validation are still pending.

See [Game Boost](game-boost.md) for use and limitations. This is not a stable feature.

## Next: Game mode — keyboard, mouse and streaming

The first gaming milestone is fast, responsive screen/audio streaming with
keyboard and mouse input. Controller forwarding, USB passthrough and their
virtual-device drivers are outside this milestone and must not delay it.

- Use hardware encoding and decoding where supported, reduce unnecessary GPU/CPU
  frame copies, and retain a software fallback with its limitations made visible.
- Tune frame pacing, bounded video queues and audio buffering; adapt bitrate and
  frame rate to the available hardware and connection instead of promising a
  fixed frame rate or latency.
- Prioritize responsive keyboard input, including held keys and key combinations,
  with reliable input release when focus is lost or the session disconnects.
- Add relative mouse input for game-camera control while preserving local-host
  takeover, input release on disconnect and an easy way to release the viewer's
  mouse capture.
- Show useful performance measurements, distinguishing network round-trip time
  from capture, encoding and decoding time. Validate input-to-display latency and
  game compatibility on two computers before describing the mode as gaming-ready.

## Exploration, not scheduled

- General USB device passthrough over the authenticated connection, starting with
  an explicitly tested device allowlist rather than claiming support for every USB
  device. Require per-device approval and a visible stop-sharing control; document
  when a forwarded device becomes unavailable locally.
- Review USB passthrough isolation, driver maintenance, device ownership and safe
  disconnect/recovery before implementation. Keep this separate from controller
  emulation and ordinary file transfer; do not automatically forward storage
  devices or security keys.
- Direct internet connectivity, NAT traversal and relay options.
- Linux and mobile clients; macOS support.
- File transfer, session recording and multiple viewers.
- Team administration, shared device lists and ticketing integration.
- Windows service mode and secure-desktop support, subject to security review.

### Optional controller forwarding — later, not part of initial Game mode

- Consider gamepad forwarding with a compatible virtual controller on the host
  after the keyboard/mouse streaming milestone. This is also separate from generic
  USB device passthrough.
- Require explicit session permission, show which controller is being forwarded,
  and release its inputs and remove the virtual device when forwarding stops or
  the connection ends.
- Evaluate a maintained, appropriately licensed driver implementation and a tested
  controller compatibility list. Treat vibration and device-specific features as
  separately validated capabilities, not guarantees for every controller.
- Keep the driver component optional. Validate its installation, updates, removal
  and signing separately from the portable application and Store package.
  Windows kernel drivers have their own
  [signing requirements](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/driver-signing);
  [MSIX does not install drivers](https://learn.microsoft.com/en-us/windows/msix/packaging-tool/tool-known-issues).
