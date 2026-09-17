# Roadmap

TideDesk stays free and open source. Sponsorship funds development; it never unlocks
features in the software itself.

## ✅ 0.1 — Windows → Windows on a LAN (alpha)

- DXGI Desktop Duplication capture; idle screens cost nothing
- H.264 (OpenH264) encode/decode with latest-frame-wins backpressure
- System audio via WASAPI loopback → Opus → QUIC datagrams, jitter buffer with loss concealment
- Keyboard (scancodes) and mouse injection, stuck-key protection
- QUIC + TLS 1.3, BBR congestion control
- Access-code auth bound to the TLS session, brute-force lock-out, fingerprint pinning

## 0.2 — Windows polish

- **Hardware encoding** via Media Foundation (NVIDIA NVENC, Intel Quick Sync, AMD AMF),
  software fallback kept. Brings 4K down to a few % CPU and removes the patent question for
  distributed builds.
- Adaptive bitrate and frame rate from measured network conditions
- Text clipboard sync with settings and shortcuts implemented; two-computer validation pending
- Image clipboard sync
- Mouse-control settings and shortcuts with host-position handoff implemented; two-computer validation pending
- Independent amber host-position marker and local viewer crosshair implemented; two-computer validation pending
- Native remote cursor shapes and visibility (text, resize, busy, hidden)
- Switch monitors during a session, or view all at once
- Capture Alt+Tab / Win-key combinations in the viewer
- Lower memory at 4K (decode straight into the presentation buffer)
- GPU presentation in the viewer
- ✅ GUI: tray host with settings, viewer with saved computers (done early)
- Windows service mode: control UAC prompts and the sign-in screen
- Signed installer and portable builds

## 0.3 — Across the internet

- Open-source **rendezvous and relay server** anyone can self-host for free
- Direct peer-to-peer connection through NAT (UDP hole punching) whenever possible, relay only
  as fallback
- Step-by-step guides for running your own relay on a small VPS or home server

## 0.4 — Linux

- Host: PipeWire screen capture through the desktop portal (Wayland and X11), PipeWire/PulseAudio
  monitor for sound, input via `uinput`/libei
- Viewer: same `winit` + audio stack as Windows

## 0.5 — Mobile

- Android viewer (touch → mouse gestures, on-screen keyboard)
- iOS viewer (requires macOS for building and signing)
- Android host (MediaProjection) considered later

## Later

- macOS host and viewer
- File transfer
- Multiple simultaneous viewers, view-only mode
- Session recording
