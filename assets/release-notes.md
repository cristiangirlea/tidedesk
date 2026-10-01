TideDesk @VERSION@: remote desktop for Windows x64, free for personal, non-commercial use.

License: TideDesk Personal Use Source License 1.0 (see LICENSE in the ZIP). Business use requires separate written permission. Previously published AGPL releases retain their original permissions.

Download @ZIP@ and extract it. Run tidedesk.exe on both computers.

@SIGNING@

**Experimental alpha, not a stable release.** Game Boost is opt-in and off by default. Two-computer gameplay and end-to-end latency validation are still pending.

## New in this release

### Access

- **Your own computers without the code.** Set a password under the access code on the host (or `tidedesk host --set-password`). A viewer that has connected to that computer before, or reaches it by device ID, types the password where the code goes. Neither computer sends the password: both prove to each other that they know it, and the host keeps only a key derived from it. Both computers need this release.
- **Invite a viewer.** During a session, press **Trust this viewer** on the host: that viewer then connects with the code left empty, until you remove it from **Trusted viewers**. Each viewer has its own certificate, whose key never leaves that computer. Without a window: `tidedesk view --my-fingerprint` on the viewer, `tidedesk host --trust-viewer FINGERPRINT` on the host.
- **A new access code after each session.** When a session ends the host makes a new code; the old one still works for five minutes, so a dropped connection comes straight back. A switch under the code turns this off; a host started with `--headless` keeps its code. A code remembered under My computers works until the host makes a new one.
- **Wrong codes lock out only the address that sent them.** Someone guessing no longer locks you out too: each address gets five tries (a wrong password counts double), then waits that grow up to an hour. The host window names the addresses it refuses.

### The app

- **The Settings shortcut can be changed**, like the clipboard, mouse and Game Boost ones; with it moved, Ctrl+Alt+S reaches the host. (#69)
- **Settings opened during a session opens over it**, on the same monitor, instead of wherever Windows puts a new window. (#73)
- **Simpler settings.** The fields for naming STUN servers and a connection service are gone from Host and Viewer Settings; servers named in the settings files or on the command line still apply.
- **Cleaner text.** Window titles and labels use a bar or a colon instead of long dashes (`OBLIVION5080U9 | TideDesk`). (#70)

## Introduced in v0.1.0-alpha.9

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

## Known issues

- When a session ends, the viewer's window closes without saying why. (#60)
- Automation tools that mark every key as an extended one (Python's `uiautomation`) send some letters with Ctrl or Alt as media keys: Ctrl+Alt+C turns the host's volume down. (#66)
- A password or a trust needs the host on this release; a code works with every host from v0.1.0-alpha.3 on.

## Good to know

A device ID also works on the same local network, even without the internet: the viewer asks its own network too, and the host answers directly. The host encodes video on the graphics card where it has an H.264 encoder (AMD, Intel or NVIDIA) and falls back to Windows' software encoder by itself.

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
