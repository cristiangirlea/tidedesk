<img src="assets/tidedesk-256.png" alt="TideDesk" width="96" align="right">

# TideDesk

**Free, open-source remote desktop with sound.** See and control another computer, and hear
what it plays — with a small footprint and low latency.

> Status: **early alpha**. Windows → Windows works on a local network. Linux, Android and iOS
> are planned. See the [roadmap](docs/ROADMAP.md).

TideDesk is free for everyone, forever. If it's useful to you, you can
[sponsor its development](https://github.com/sponsors/cristiangirlea).

## Why another remote desktop?

Classic VNC tools such as TightVNC were designed for a different era: they poll the screen,
compress it on the CPU as zlib/JPEG tiles, send everything over one TCP connection, protect
it with an 8-character DES password — and carry no sound at all.

TideDesk takes a modern route:

| | TideDesk | Classic VNC |
|---|---|---|
| Screen capture | DXGI Desktop Duplication — the GPU reports only real changes | Polling / hooks |
| Video | H.264, screen-content tuned | zlib/JPEG tiles |
| Sound | ✅ System audio, Opus, 10 ms frames | ❌ |
| Transport | QUIC over UDP: video, input and audio never block each other | Single TCP stream |
| Encryption | TLS 1.3, always on | Optional / weak |
| Auth | Access code proven via a session-bound HMAC (never sent), brute-force lock-out, host fingerprint pinning | DES password |
| Idle cost | ~0% CPU — nothing is captured or encoded while the screen is still | Keeps polling |

### Measured (alpha, release build)

On an AMD Ryzen 9 9950X (16 cores), streaming a 3840×2160 display over loopback:

| | CPU | RAM |
|---|---|---|
| Host, no viewer connected | 0.0% | 10 MB |
| Host, streaming 4K | ~13% of **one** core | ~270 MB |
| Viewer, showing 4K | ~5% of one core | ~210 MB |

Encoding cost per frame (software H.264): 8 ms at 1080p, 18 ms at 1440p, 37 ms at 4K.
Hardware (GPU) encoding and lower 4K memory use are on the roadmap.

## Quick start

**On the computer you want to reach**, run `tidedesk-host`. A small window shows the
**access code** and this computer's addresses; TideDesk also sits in the notification area (tray).
Closing the window keeps it running there — quit from the tray menu.

**On the computer you are sitting at**, run `tidedesk-view`, type the address and access code,
and press **Connect**. Save computers you use often under **My computers** for one-click
connections; access codes you choose to remember are encrypted for your Windows account.

The first time you connect, the viewer shows the host's fingerprint — check it matches the host
window. The viewer remembers it and refuses to connect if it ever changes.

Windows Firewall will ask to allow `tidedesk-host` the first time; allow it on private networks.

### Host settings

The host's **Settings** tab covers the shared screen, frame rate, quality, sound, UDP port,
whether the window appears in the taskbar, starting hidden in the tray, and starting when you
sign in to Windows. Settings are saved in `%APPDATA%\TideDesk\host.toml`.

### Command line

Both programs still work from a terminal:

```
tidedesk-host --headless
tidedesk-view 192.168.1.50 --code K7QM-3XPA-WZ
```

| Host | |
|---|---|
| `--headless` | No window or tray; print status to the console |
| `--tray` | Start hidden in the tray |
| `--list-displays` / `--display N` | Choose which monitor to share |
| `--fps 60` | Frame rate cap |
| `--bitrate 8000` | Video bitrate in kbit/s |
| `--no-audio` | Don't share sound |
| `--new-code` | Replace the access code |
| `--stats` | Print fps, bitrate and encode time |

Host options override the saved settings for that run only.

| Viewer | |
|---|---|
| `--no-audio` | Don't play the host's sound |
| `--fingerprint "3F2A 91C0 …"` | Verify the host on the very first connection |
| `--stats` | Print fps, bitrate and decode time |

The access code can also be supplied through the `TIDEDESK_CODE` environment variable.

### Over the internet

Built-in relay support is coming. Until then, see [internet access](docs/internet-access.md) —
a free VPN such as Tailscale is the easiest and safest option.

## Current limitations

- Windows only, one viewer at a time.
- The host runs as a normal app: UAC prompts and the sign-in screen can't be seen or controlled.
- The remote mouse pointer shape isn't shown; your local pointer is used.
- No clipboard sync or file transfer yet.
- Shortcuts the viewer's Windows handles itself (Alt+Tab, Win key) stay local.

## Building from source

Requirements (Windows):

- [Rust](https://rustup.rs) (stable, MSVC toolchain)
- Visual Studio Build Tools with the C++ workload
- [CMake](https://cmake.org)
- [NASM](https://nasm.us) on `PATH` — optional but strongly recommended; without it the video
  encoder builds without its SIMD assembly and runs 3–4× slower (the build warns you).

```
cargo build --release
```

Binaries land in `target/release/`: `tidedesk-host.exe` and `tidedesk-view.exe`.

## Project layout

| Crate | Purpose |
|---|---|
| `tidedesk-core` | Platform-independent: wire protocol, auth, identity pinning, QUIC setup, audio helpers |
| `tidedesk-host` | Screen capture, H.264 encoding, audio capture, input injection |
| `tidedesk-view` | Window, H.264 decoding, audio playback, input capture |
| `third_party/egui_software_backend` | Vendored CPU renderer for the small app windows, with a repaint fix |

The app windows are drawn on the CPU with [egui](https://github.com/emilk/egui): an OpenGL or
Direct3D context alone can cost 150–500 MB on some graphics drivers, while each window here uses
about 10 MB. The remote screen itself is presented with `softbuffer`.

Platform-specific code sits behind small traits (`Capturer`, input backends), so Linux,
Android and iOS ports reuse the core and protocol unchanged.

## License

[GNU AGPL-3.0](LICENSE). You may use, study, share and modify TideDesk freely. If you
distribute a modified version — or run one as a network service — you must publish your
source under the same license.

Written by Cristian Girlea, with AI assistance.

The icon is generated by `tools/make_icon.py`.

The H.264 codec is [OpenH264](https://github.com/cisco/openh264) (BSD-2-Clause), built from
source. Cisco's H.264 patent coverage applies only to Cisco's own prebuilt binaries; if you
redistribute TideDesk builds, check what applies in your jurisdiction.
