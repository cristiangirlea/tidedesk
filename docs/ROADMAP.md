# Roadmap

This is a planning list, not a promise of delivery, dates or future licensing terms.
Only what is under "Available now" is in a release; everything else is not available yet
and may change or be dropped. See [licensing](licensing.md) for the terms of each release.

Last brought up to date for v0.1.0-alpha.16.

## Available now (Windows alpha)

TideDesk is an experimental alpha for Windows x64. Every session runs directly between
the two computers; TideDesk never relays it. A host takes one viewer at a time.

**Connecting**

- One program, `tidedesk.exe`, on both computers, with one window and a sidebar: This
  computer, Connect, History, Settings and About. `tidedesk host` and `tidedesk view`
  still run either side alone, and `tidedesk host --headless` suits servers.
- Several sessions at once from one TideDesk, each in its own window, listed with its
  status and a Disconnect for each. (Since v0.1.0-alpha.16.)
- Connect by device ID or by address. TideDesk's connection service only introduces the
  two computers and never carries the session; it can be turned off. On the same local
  network a device ID works without the internet. (Experimental.)
- Direct internet connections: the router's port is opened by itself where the router
  allows it, and routers that change ports on one side usually get through. With such a
  router on both sides, the host's router opening its port, a forwarded port or a VPN
  is still needed. (Experimental.)
  See [internet access](internet-access.md).
- Keep both computers on the same release. From v0.1.0-alpha.16 on, TideDesk says which
  computer to update.

**Access and safety**

- An access code that changes after each session unless that is turned off, with
  lock-outs only for the address that guesses wrong.
- Disconnect ends the code that viewer used at once. (Since v0.1.0-alpha.15.)
- A saved password for your own computers, which neither computer ever sends, and
  trusted viewers that connect without a code. (Since v0.1.0-alpha.10.) The password
  works only from a computer that connected before. (Since v0.1.0-alpha.15.)
- A typed code or password shows as dots until you tick Show. (Since v0.1.0-alpha.15.)
- Host fingerprint pinning, a red dot on the tray icon while a viewer is connected, and
  a reason shown in the viewer when a session ends.
- Session history of the last 30 days, kept on the computer, where a changed or removed
  entry shows. Problems are listed on the computer; a report about one is sent only when
  you choose to, after reading it in full, and leaves out the other computer's name,
  file names and full internet addresses.
- Settings an administrator can set for every user, for example through Group Policy.
  (Since v0.1.0-alpha.12.)

**Picture, sound and input**

- Video encoded on the graphics card where it has an H.264 encoder (AMD, Intel or
  NVIDIA), falling back to software by itself; decoding with Windows' own decoder.
  (Since v0.1.0-alpha.8.)
- Bitrate that follows the screen's size, and scaling that keeps text readable at any
  window size.
- The host's sound on the viewer.
- Talk to the host: the viewer's microphone on the host's speakers, off on both sides,
  and nothing is captured until the host agrees. The viewer does not hear its own voice
  come back (with Windows 10 version 2004 or later on the host). (Since v0.1.0-alpha.15.)
- Keyboard and mouse control, two-way text clipboard sharing, safe mouse handoff,
  separate cursor indicators and changeable shortcuts. See
  [interaction controls](interaction-controls.md).
- A keyboard switch like the mouse one, and one shortcut that turns all input off to
  only watch. The host can allow or refuse the viewer's keyboard on its own.
  (Since v0.1.0-alpha.15.)
- The host's own TideDesk windows ignore the viewer's clicks and keys (since
  v0.1.0-alpha.15), and the viewer is told when that happens (since v0.1.0-alpha.16).
- Game Boost, opt-in: a 60 FPS target with lower buffering, switched live. (Experimental;
  see [Game Boost](game-boost.md).)

**Files and chat** (since v0.1.0-alpha.12)

- Copy files to the host by dropping them on the viewer's window, and send files back
  from the host. Files go directly between the two computers, never through a server.
- Send whole folders, and cancel a file on its way without ending the session. A list
  of files on their way, with size, progress and speed, on both computers.
  (Since v0.1.0-alpha.15.)
- Chat between the viewer and the host.

**Licence**

- Free for personal, non-commercial use. TideDesk Solo licenses work use and is activated
  under Settings, Licence (since v0.1.0-alpha.14). See
  [tidedesk.app/pricing](https://tidedesk.app/pricing/).

**Android viewer** (experimental, since v0.1.0-alpha.12)

- Connect by device ID or address with the access code; touch or touchpad control, zoom,
  saved computers and settings. Not yet: passwords, trusted viewers, files, chat and sound.
- The latest Android download came with v0.1.0-alpha.15 and connects to computers on
  that release. v0.1.0-alpha.16 has no Android download, and an earlier Android build
  cannot connect to it.

**Still to confirm**

- Two-computer, multi-monitor and mixed-DPI checks of recent changes, and two-computer
  gameplay and end-to-end latency for Game Boost.
- The executables are not code-signed yet; see the [code signing policy](code-signing-policy.md).

## Built, not in a release yet

Not in any release yet, and there is no download for it.

- **Linux:** the viewer, and the host on an X11 or a Wayland desktop, with screen, sound,
  keyboard and mouse. On Wayland the desktop asks the person at the computer first.

## Planned

Where an item has an issue, follow it (Subscribe, on its page) to hear when it ships.

- **Internet connections over IPv6**, alongside IPv4, still direct between the two
  computers. ([#3](https://github.com/cristiangirlea/tidedesk/issues/3))
- **A Linux download** beside the Windows one.

## Ideas, not scheduled

Say on the issue whether it would help you.

- **Updates inside the app for the Microsoft Store version**, never during a session.
  ([#4](https://github.com/cristiangirlea/tidedesk/issues/4))
- **Usage profiles:** Office, Gaming, Slow connection, Battery saver, set in one
  choice, within what the host allows.
  ([#5](https://github.com/cristiangirlea/tidedesk/issues/5))
- **A sharper picture when the viewer's window is larger** than the picture the host
  sends. ([#6](https://github.com/cristiangirlea/tidedesk/issues/6))

## Known issues

- Ctrl+Alt+C sent by some automation tools lowers the host's volume, and other letters
  with Ctrl or Alt act as media keys. Typing on the keyboard is not affected.
  ([#7](https://github.com/cristiangirlea/tidedesk/issues/7))

The release notes of each [release](https://github.com/cristiangirlea/tidedesk/releases)
list its known issues in full.

## Game mode: keyboard, mouse and streaming

The first gaming goal is fast, responsive picture and sound with keyboard and mouse.
Video on the graphics card and Game Boost are the start. Planned, not scheduled:

- Bitrate and frame rate that adapt to the connection, rather than a fixed promise.
- Relative mouse input for game cameras, with an easy way to release the mouse.
- Measurements that tell the network apart from the computers' own work, and checks on
  two computers before calling the mode ready for games.

Controllers and USB devices are outside this goal and will not delay it.

## Exploration, not scheduled

- Sharing USB devices over the session, starting with a tested list of devices, with
  approval for each device and a visible stop. Storage devices and security keys would
  never be shared by themselves.
- Game controllers, as an optional, separately installed part. Windows drivers have their
  own [signing requirements](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/driver-signing),
  and [MSIX packages do not install drivers](https://learn.microsoft.com/en-us/windows/msix/packaging-tool/tool-known-issues).
- Optional two-factor sign-in on the host, with no TideDesk account.
- Seeing and answering Windows permission prompts from the viewer, subject to a security
  review.
- Image clipboard, in-session monitor switching and the host's own cursor shapes.
- macOS, and an iOS viewer.
