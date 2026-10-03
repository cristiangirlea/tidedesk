# TideDesk viewer for Android

A light Android app that connects to computers sharing their screen with TideDesk. It views
and controls them; the phone shares nothing.

## How it is built, and why (battery first)

| Part | What | Why |
|---|---|---|
| Screens | Kotlin with Android's own views: no Compose, no AppCompat, no libraries | Smallest app, fastest start. |
| Video | MediaCodec, the phone's hardware H.264 decoder, drawing straight onto a `SurfaceView` | Frames never pass through the CPU. TideDesk sends constrained-baseline H.264 with SPS/PPS in front of every keyframe, which every phone decodes in hardware. |
| Connection | TideDesk's Rust core, through `crates/tidedesk-android` (JNI) | One implementation of the protocol, the encryption, device IDs and hole punching, shared with the desktop. |

More choices that keep the battery in mind:
- **Nothing polls.** The decoder runs in asynchronous mode and says when a buffer is free and when a frame is ready. The feeder thread waits for the next frame.
- **Back-pressure.** Frames queue only four deep, so a slow decoder slows the host down (it skips captures) instead of filling memory.
- **Nothing in the background.** Leaving the session screen ends the session. The screen stays on only while a session is shown.

The release app is about 2.4 MB, nearly all of it the Rust library.

## Build

Needs the Android SDK with NDK 27.3.13750724, the Rust target `aarch64-linux-android`, and
`cargo-ndk` (`cargo install cargo-ndk`). Then, from this folder:

```
gradle assembleDebug        # or assembleRelease
```

The `cargoBuild` task builds the Rust library into `app/src/main/jniLibs` first. Install with
`adb install app/build/outputs/apk/debug/app-debug.apk`.

## What it does now

- **Connecting:**
  - by device ID, through TideDesk's connection service, with hole punching (including guessing the port behind a symmetric router);
  - by address on the same network, trusting the computer on first use as the desktop viewer does.
- **The access code.**
- **Touch:** tap to click, long press to right-click, drag with one finger, scroll with two.

## Not yet

- The saved password and trusted-viewer sign-in.
- The keyboard, saved computers, the clipboard, files, chat and sound.
- "Over the internet" by typed address.
- No CI job builds the app yet.

The Rust side's tests (`cargo test -p tidedesk-android`) run a session against a fake host on
any computer.
