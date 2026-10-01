TideDesk @VERSION@: remote desktop for Windows x64, free for personal, non-commercial use.

License: TideDesk Personal Use Source License 1.0 (see LICENSE in the ZIP). Business use requires separate written permission. Previously published AGPL releases retain their original permissions.

Download @ZIP@ and extract it. Run tidedesk.exe on both computers.

@SIGNING@

**Experimental alpha, not a stable release.** Game Boost is opt-in and off by default. Two-computer gameplay and end-to-end latency validation are still pending.

## New in this release

### Sessions that last

- **A permission prompt on the host no longer ends the session.** When Windows asked "Do you want to allow this app to make changes?" on the host, or showed the lock screen, the viewer was disconnected. Now the picture stands still while the prompt is up, the viewer says that the host's pointer is outside the shared screen, and the session goes on once the prompt is answered at the host. The prompt itself still cannot be seen or answered from the viewer. Programs that ask for permission when they start (installers, Docker Desktop) ended sessions this way. (#58)
- **A host that has gone is noticed within seconds.** After three seconds without an answer, the viewer's title says "No answer from the host" and counts up; it goes away when the host answers again. The session is still given up only after 20 seconds, so a network that fails for a moment does not end it. (#42)

### Video

- **Intel graphics reach the frame rate.** On Intel's encoder each picture took 31 ms, whatever its size: Windows' timers tick every 15.6 ms unless a program asks for finer ones, and the encoder waited on them. The host now asks while it encodes on a graphics card. On a laptop with Intel graphics at 2560x1600, a moving screen went from 32 to 60 frames a second, and from 45 to 14 ms between the screen and the encoded picture. (#43, #65)
- **The graphics card's encoder gets the next picture while it works on the last** where that helps, and one picture at a time where the card takes them in turn anyway, which it finds out by itself in the first seconds. (#43)

### Mouse and keyboard

- **The first click after the pointer jumps reaches the host**, where it was made: tablets, pens and tools that put the pointer somewhere and click at once lost that click. (#41)
- **Keys from tools and on-screen keyboards work**: keys that arrive without a hardware scan code, including arrows and the viewer's own shortcuts, were ignored. Text a tool types as characters is typed on the host with the keys that make it. (#40)

### Connecting

- **Two computers behind one router find each other without broadcasts.** The host gives its local addresses to the connection service, sealed with its access code so that the service cannot read them. A viewer at the same internet address opens them with the code and connects directly, before it tries a path through the router. Hosts and viewers from earlier releases keep working. (#44)

### For scripts

- **Output sent to a file arrives there.** `tidedesk host --headless --stats > host.log` from a terminal or a batch file wrote nothing to the file. (#62)

## Introduced in v0.1.0-alpha.8

### A sharper picture

- **Text stays readable at any window size.** When the remote screen does not fit the window pixel for pixel, the viewer blends neighbouring pixels instead of skipping or repeating them. Thin strokes no longer vanish when the picture shrinks, and they keep even widths when it is enlarged by a fraction. Whole-number enlargements (2x, 3x) still repeat pixels, which is the crispest. (#38, #39)
- **The bitrate follows the screen's size.** The host's bitrate is Automatic by default: about 6 Mbit/s at 1920x1080, 11 at 2560x1440 and 12 at 2560x1600, within 4 to 20 Mbit/s. Before, every screen streamed at 4 Mbit/s, which broke moving pictures into blocks on large screens. A bitrate chosen in Settings is kept, and Settings shows what Automatic picked.

### Video on the graphics card, through Windows' own codecs

- **The host encodes on the graphics card** where the card has an H.264 encoder (AMD, Intel and NVIDIA, through Windows' Media Foundation). The captured screen stays on the card from capture to encoder. On the test PC at 2560x1440, the host's processor work per frame went from 17-22 ms with the software encoder to 1.5-1.7 ms.
- **It falls back by itself.** Without a usable encoder on the card, or for a size the card refuses (hardware encoders stop at 4096 columns), Windows' software encoder takes over, and OpenH264 after that. An encoder that fails during a session is replaced from the next frame, starting with a keyframe, instead of the video stopping.
- **The viewer decodes with Windows' own H.264 decoder**, with OpenH264 as the fallback where Windows has none (Windows N without the Media Feature Pack). Turning the decoded picture into pixels is 30 to 40 percent faster.
- **Nothing changes on the wire.** The stream format is the same (protocol v3), so this release still talks to earlier ones.
- To choose by hand, set the environment variable `TIDEDESK_CODEC` to `software` (leave the graphics card out) or `openh264` before starting TideDesk.

### Connecting

- **A device ID works on the same local network, even without the internet.** While it asks the connection service, the viewer also asks its own network, and the host answers directly; the session then takes the local path. Host Settings, Network can turn this off.
- **tidedesk.exe is the only program.** tidedesk-host.exe and tidedesk-view.exe, kept in v0.1.0-alpha.7 for existing shortcuts, are no longer included. A start-up entry that ran tidedesk-host.exe starts tidedesk.exe from its first run; shortcuts to the old programs need to point to tidedesk.exe. `tidedesk host` and `tidedesk view` still run either side alone (`tidedesk host --headless` for servers).

### The app

- **Viewer settings save as you change them.** There is no Save button. An edit that cannot be saved yet, such as a shortcut that clashes with another, says why in red until it is fixed.
- **Terms of use.** A new installation asks to accept the [terms of use](https://github.com/cristiangirlea/tidedesk/blob/v@VERSION@/docs/terms-of-use.md) before it shares anything; installations from earlier releases keep working and show a notice. Settings, About has the version, the license, the terms, the privacy notes and the third-party notices.
- **A warning where scams happen.** Under the access code, the Share tab says to give the code only to someone you know and trust.

## Known issues

- Starting some programs on the host has been reported to end the session: Docker Desktop, Zoom, Iriun Webcam, an application download. Those that ask Windows for permission are fixed in this release (#58); the others are not yet confirmed. A host started with `tidedesk host --headless --stats > host.log` writes in its log why a session ended. (#59)
- When a session ends, the viewer's window closes without saying why. (#60)
- Automation tools that mark every key as an extended one (Python's `uiautomation`) send some letters with Ctrl or Alt as media keys: Ctrl+Alt+C turns the host's volume down. (#66)

## Good to know

**Keep host and viewer on the same release.** This build uses protocol v3: it connects to v0.1.0-alpha.3 and later on a local network, but not to the earlier v1/v2 alpha releases. Internet connections need both computers on this release.

Experimental direct internet connections, computer to computer, with no relay. The host shows its internet address, learned from public STUN servers (can be turned off in Host Settings, Internet). In the viewer, tick "Over the internet", connect to that address, and give the viewer's address to the person at the host, who types it under "Viewer on another network" and presses Open. Both computers then open a path through their routers and the session runs directly between them. Symmetric NAT, common on mobile data, cannot be traversed: use a VPN such as Tailscale there. Hosts with a forwarded port connect without the manual step.

Also experimental: connect by device ID. A viewer types the host's device ID instead of anyone typing internet addresses, and headless hosts can be reached too. Hosts register with TideDesk's connection service (rendezvous.tidedesk.app) unless that is turned off in Host Settings; it only introduces the two computers and never carries the session. What it sees is in the privacy notes of the code signing policy.

[Internet access](https://github.com/cristiangirlea/tidedesk/blob/v@VERSION@/docs/internet-access.md) and [design notes](https://github.com/cristiangirlea/tidedesk/blob/v@VERSION@/docs/design/nat-traversal.md)

Experimental Game Boost, in Viewer Settings or with Ctrl+Alt+G (customizable), switches live without reconnecting: a 60 FPS target, a smaller pending decode queue and lower audio buffering. Turning Boost off restores the desktop profile. Actual FPS depends on both PCs and the connection; host resolution, bitrate and sharing permissions are unchanged.

[Game Boost usage and limitations](https://github.com/cristiangirlea/tidedesk/blob/v@VERSION@/docs/game-boost.md)

Keyboard and desktop/absolute mouse input only. Relative game-camera input, controllers and USB forwarding are not implemented. The viewer decodes on the processor. Audio remains host system output to viewer only; no microphone forwarding or additional driver dependency.

The viewer remembers each host's window location and monitor. Sessions start at the host's native pixel size and shrink proportionally only when needed to fit the available screen. Neither display resolution is changed. Optional two-way text clipboard sharing, customizable clipboard and mouse shortcuts, safe mouse handoff, and separate host and viewer cursor indicators are included.

[Clipboard and mouse controls](https://github.com/cristiangirlea/tidedesk/blob/v@VERSION@/docs/interaction-controls.md)

No Microsoft Visual C++ Redistributable is needed, and third-party license notices are in the licenses folder.

Alpha validation: automated checks pass; live multi-monitor and two-computer verification is still pending.

Windows to Windows, one viewer at a time. Allow the host through Windows Firewall on private networks.

[Code signing policy](https://github.com/cristiangirlea/tidedesk/blob/v@VERSION@/docs/code-signing-policy.md) and [terms of use](https://github.com/cristiangirlea/tidedesk/blob/v@VERSION@/docs/terms-of-use.md), which a new installation asks to accept before it shares anything.

SHA-256:

~~~text
@HASHES@
~~~
