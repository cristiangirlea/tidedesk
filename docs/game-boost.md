# Game Boost

Introduced in **v0.1.0-alpha.3 as an experimental feature**, off by default.
This is not a stable release or a claim of gaming-ready latency.
Update **both** executables together: protocol v3
rejects connections from the earlier v1/v2 builds.

## Use

Open Viewer Settings from the launcher or with **Ctrl+Alt+S** while connected.
The **Game Boost** button saves and applies the mode immediately to open sessions.
**Ctrl+Alt+G** toggles it while the remote window is focused; its shortcut can be
changed in Settings. An older custom binding using that shortcut is preserved,
with Boost assigned another available binding.

Boost is off by default. Its saved value is reused on the next connection.
The window title says "applying" until the host acknowledges a frame produced
with the requested profile. Use the same button/shortcut to return to Desktop.
Settings are shared by viewer sessions on this Windows account.

## Implemented behavior

- Host encoding targets 60 FPS with OpenH264's real-time camera/video preset,
  low complexity, four encoding threads and frame skipping for bitrate control.
  Desktop mode restores the connection's original FPS, screen-content preset
  and two encoding threads. Rapid changes are coalesced with a 250 ms minimum
  between encoder reconfigurations.
- A new encoder starts each profile change with a keyframe and codec headers.
  No display mode switch, fixed resolution, scaling change or reconnection is used.
  The original host bitrate setting is preserved; Boost does not raise bandwidth
  automatically. A low bitrate can reduce motion quality.
- The viewer keeps at most one pending encoded frame in Boost after draining any
  existing desktop backlog (plus a frame being decoded and network buffers).
  Every dependent H.264 frame is decoded in order. When behind, unnecessary RGB
  conversions are skipped, not the decoder's reference frames.
- Audio prebuffering changes from 40 to 20 ms, with a trim threshold of 60 rather
  than 150 ms. Existing buffered audio is trimmed when enabling Boost. Smaller
  buffers may cause more underruns on an unstable connection.
- Repaint notifications are coalesced. Adjacent queued absolute mouse movements
  use the latest position, but never cross keyboard, button, scroll, permission
  or pointer-epoch boundaries. Held keys and mouse buttons are released when
  changing profiles; normal focus-loss/disconnect release remains in place.
- Host mouse/clipboard permissions, safe local-host takeover, cursor indicators,
  authentication and fingerprint checks are unchanged.

The FPS label is a **target**, not measured throughput. Audio buffer targets are
**not** end-to-end latency. Existing --stats output reports local encode/decode
work and throughput; it does not measure input-to-display latency.

## Limitations

This is a software-streaming Boost preset with keyboard and **desktop/absolute**
mouse input, not a complete replacement for a dedicated game streamer. Relative
mouse capture for FPS game cameras, GPU encoding/decoding, automatic congestion
adaptation and in-window latency statistics still need implementation.
No controller or USB passthrough is included.

Native 4K at 60 FPS is not guaranteed. Some games may reject injected input or
use capture paths not supported by the current desktop capture. No anti-cheat
bypass or elevated input driver is installed.

## Validation

Automated coverage includes protocol serialization, old settings migration,
shortcut conflicts, stale acknowledgements, permission preservation, held-key
release, audio trimming/restoration and ordered bounded video queues. A synthetic
static capture runs through the production host encode loop and a real decoder,
switching profiles live without resizing.

Before releasing, test on **two Windows computers**:

1. Connect both new builds, including with clipboard and host mouse permission off.
   Toggle Boost with the button and shortcut; permissions must remain unchanged.
2. Toggle during a moving scene and on a static desktop. The title must acknowledge
   60 FPS target, then the original desktop FPS when disabled. Check resolution
   and remembered viewer window placement are unchanged.
3. Hold W or a mouse button, toggle mode or move focus away, and verify no stuck
   input. Move the physical host mouse and verify the no-snap handoff still works.
4. Exercise keyboard plus mouse in a desktop-pointer-compatible game, in windowed
   and borderless modes. Record actual FPS/encode/decode work with --stats.
   Do not treat an FPS game needing relative input as supported.
5. Test 1080p, 2560x1600 and 4K, wired and Wi-Fi, and a temporary network stall.
   Check audio recovery and motion quality. Compare actual input-to-display
   latency externally before publishing performance claims.
6. Disable Boost, reconnect, and restart the viewer: saved state and desktop
   restoration must work. Older builds must fail with a clear protocol mismatch.

Two-computer visual/gameplay and end-to-end latency validation is still pending.
