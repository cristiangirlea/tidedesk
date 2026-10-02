TideDesk @VERSION@: remote desktop for Windows x64, free for personal, non-commercial use.

License: TideDesk Personal Use Source License 1.0 (see LICENSE in the ZIP). Business use requires separate written permission. Previously published AGPL releases retain their original permissions.

Download @ZIP@ and extract it. Run tidedesk.exe on both computers.

@SIGNING@

**Experimental alpha, not a stable release.** Game Boost is opt-in and off by default. Two-computer gameplay and end-to-end latency validation are still pending.

## New in this release

### Files and chat, free

- **Copy files to the host:** drop files on the viewer's window. Each lands in `Downloads\TideDesk` on the host, and the viewer's title says when it is saved. **And back:** the host's Share tab has **Send files to the viewer...**, which opens Windows' file picker; the files land in `Downloads\TideDesk` on the viewer's computer. Files go directly between the two computers, below the picture, input and sound in priority, never through a server. Names are cleaned, nothing is overwritten, and a file only appears once it arrived whole. Each side can refuse files in its settings. Folders are not sent yet. (#98, #100)
- **Chat:** Ctrl+Alt+T in the viewer (changeable) opens a small chat window, which also opens by itself when the host writes; the host chats from its Share tab, with nothing popping up on its screen. (#102)
- Files and chat need both computers on this release; with an older one the viewer says so, and everything else works as before.

### Sessions

- **A session that ends says why.** The viewer's window stays open with the last picture, and its title says why the session ended; nothing appears on the host's screen. The reason also goes to `tidedesk.log` in `%APPDATA%\TideDesk`. (#60)

### Licences

- **About shows a licence, and takes one.** Business use is licensed with a short signed text: paste it under About, **Add a licence**. It is checked on the computer itself: no account, nothing sent anywhere. Personal use stays free. (#86)
- **Session history, for everyone.** Settings shows who connected to this computer in the last 30 days: when, how long, from where, how they were let in and why the session ended. It stays on the computer, sealed for the Windows account, and each entry is chained to the one before, so a changed or removed entry shows. Older sessions are kept; a licence that includes the session history shows all of them, with search and a CSV export. (#88, #107)
- **Company computers.** A computer managed by an organisation (joined to a domain or to Microsoft Entra ID, or in device management) shows a notice that TideDesk will need a licence there for work. Nothing is limited in this release. A home lab that runs its own domain can declare its computer personal on the Share tab. (#90, #96, #104)

## Introduced in v0.1.0-alpha.11

- **An About tab with the version**, a button that copies it, and one that opens the releases page. (#82)

## Known issues

- Automation tools that mark every key as an extended one (Python's `uiautomation`) send some letters with Ctrl or Alt as media keys: Ctrl+Alt+C turns the host's volume down. (#66)
- A password or a trust needs the host on v0.1.0-alpha.10 or later; a code works with every host from v0.1.0-alpha.3 on.
- Folders are not sent yet, and Ctrl+Alt+T does not bring an open chat window to the front.

## Good to know

A device ID also works on the same local network, even without the internet: the viewer asks its own network too, and the host answers directly. Two computers behind one router also find each other through the host's local addresses, which it gives the connection service sealed with its access code. The host encodes video on the graphics card where it has an H.264 encoder (AMD, Intel or NVIDIA) and falls back to Windows' software encoder by itself.

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
