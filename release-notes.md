TideDesk @VERSION@: remote desktop for Windows x64, free for personal, non-commercial use.

License: TideDesk Personal Use Source License 1.0 (see LICENSE in the ZIP). Business use requires separate written permission: a TideDesk Solo subscription from https://tidedesk.app/pricing/ is that permission (the TideDesk Commercial License 1.0 at https://tidedesk.app/terms/). Previously published AGPL releases retain their original permissions.

Download @ZIP@ and extract it. Run tidedesk.exe on both computers.

@SIGNING@

**Experimental alpha, not a stable release.** Game Boost is opt-in and off by default. Two-computer gameplay and end-to-end latency validation are still pending.

## New in this release

### Files

- **See what is on its way.** Each file sent or received is listed with its size, how much has gone, the speed, and where it was saved, or why not. The host lists them on the Share tab, under the viewer, with **Clear** for the files that are done; **Open Downloads\TideDesk** sits beside **Send files**. The viewer shows them in a **Files** window, which opens by itself when a file starts going either way. (#196)
- **Cancel a file on its way.** Each file still going, or waiting its turn behind another, has **Cancel** in the list of files. Only that file stops; the other files and the session go on. The list then says where it was cancelled, and nothing of it is left in Downloads\TideDesk. (#200)
- **Send a folder.** Drop one on the viewer's window, or press **Send a folder** on the host: it arrives in Downloads\TideDesk with everything in it, listed as one line. Both computers need this release, as with every change to what the two computers tell each other: an older one says "protocol mismatch". (#201)
- **Two files of the same name sent at the same moment both keep theirs.** Before, one could rarely replace the other, while both senders were told it arrived. The second is now saved as "name (2)". (#183)

### Input

- **Keyboard off, and all input off.** The viewer's keyboard now has a switch like the mouse: **Ctrl+Alt+K** (changeable) keeps keys on your computer, and **Ctrl+Alt+I** turns mouse and keyboard off together, or back on, so one press lets you only watch. The host decides too: **Settings**, **Live session permissions** has **Allow viewer keyboard control** next to the mouse one, and **Allow viewer input** over both. A key held when control goes off is released on the host. Keys and mouse that the host does not allow show in the viewer's title as "blocked by host". (#191)
- Both computers need this release for the keyboard switch: it changes what the two tell each other about input, so this build speaks protocol v5 and an older release says "protocol mismatch".
- **The host's TideDesk windows are the host's own.** They are on the shared screen, but the viewer's clicks and key presses no longer reach them. The person at the host uses them as before. (#210)

### Sound

- **Talk to the host.** The viewer's microphone can play on the host's speakers, so the person at the viewer can talk to the person at the host. It is off at both ends: **Ctrl+Alt+V** (changeable) or **Talk to the host** in the viewer's Settings, and **Play the viewer's microphone** in the host's **Live session permissions**. Nothing is captured until the host has agreed, and the viewer's title says when the microphone is on. The voice goes directly between the two computers, like the host's sound. (#192)
- **No echo.** The host's shared sound now leaves TideDesk's own playback out, so the viewer does not hear its own voice come back. This needs Windows 10 version 2004 or later on the host; older Windows shares the whole output as before.
- **Sound that stops during a session.** The host now tells the viewer when its sound stops, and the viewer goes on as in a session without sound. The host's log says so too. Both computers need this release. (#267)
- The Microsoft Store version asks once for the microphone, as Windows does for every app that uses one.

### Connections

- **Another TideDesk computer behind the host's router no longer gets in the way.** Over the internet, the viewer tries a few ports near the host's, and another TideDesk computer there could answer in the host's place. The viewer then waited 20 seconds or stopped at that computer's fingerprint. It now checks the computer that answered, and goes to the host's own address when it is not the host. (#237)
- **Steadier when strangers knock.** A computer that connects to the host but has not given the access code or password can open only the one connection it needs to sign in until it does, so many of them at once use far less of the host's memory. Files and sound start as before once the viewer is let in. (#213)

### Access

- **The saved password works only from a computer that connected before.** The host now checks this itself: a computer that never came in with the access code or as a trusted viewer is told to use the access code once. The host starts its list with this release, so a computer that is not a trusted viewer uses the code once more before the password works for it again. Where an administrator has turned the access code off, the password works as before. (#211)
- **Disconnect means gone.** When you press **Disconnect** (or **Disconnect viewer** in the tray), turn off **Accept new connections**, or quit, the code that viewer used stops working at once. After a dropped connection the old code still works for five minutes, but now only for the viewer that was connected, so nobody else can use it. The Android viewer shows no certificate yet, so after a dropped connection it needs the new code. (#209)
- **Guessing from many computers slows down instead of locking you out.** A wrong access code or password counts from the moment it is tried, so many tries at once from one address get no further than a few. When many addresses guess at once, each try waits its turn instead of the host turning your own computers away, and computers that came in before, by their certificate or from an address used in the last month, never wait. (#212)

### Chat and names

- **Names read as they are.** Hidden characters that change how the other computer's name reads are left out, and a name is at most 64 characters. They are left out of chat messages too, and of the names of files on the host's list. The chat keeps its last 500 messages and leaves out any sent faster than ten a second, and TideDesk's log file now stays small while TideDesk runs, not only when it starts. (#216)

## Introduced in v0.1.0-alpha.14

### TideDesk Solo: a licence for work, activated in Settings

- **Settings, Licence.** TideDesk Solo (€99 a year or €11.90 a month, bought at https://tidedesk.app/pricing/) comes as a licence key that starts with `AL-`. Paste it under **Settings**, **Licence** and press **Activate**: this computer asks TideDesk's licence service (licence.tidedesk.app), which signs its answer, and the licence is saved on this computer. About then shows **SOLO**, business use is licensed, the session history shows every session with search and a CSV export, and a company-managed computer is licensed for work.
- **Up to 5 of your computers.** The page shows how many are in use. **Deactivate this computer** frees its place; so does **Free this place** on your licences page at https://tidedesk.app/licence/, which lists the computers and signs you in by an emailed link. Setting up the first 5 places is free; after that, activating on another computer is a move, allowed once every 7 days, and a computer that has not checked in for 45 days frees its place by itself.
- **Checked weekly, works offline.** The saved licence holds for 30 days, and TideDesk checks it again about once a week while sharing runs. A cancelled or refunded licence lapses at the next check; a computer that stays offline keeps working until its saved licence runs out.
- **Switch to yearly** from a monthly licence: **Settings**, **Licence**, **Switch to yearly…**, then **Confirm the switch**. Nothing is charged at the switch; the yearly price is charged at the end of the current month. Within 45 days of buying monthly, the switch gives the months already paid plus 14 more for the price of 12.
- **What is sent:** the licence key, this computer's public key and name, and the time, signed with a key sealed for this Windows account. Nothing about your sessions. See https://tidedesk.app/privacy/.
- The licence text pasted under About in earlier releases still works as before.

### Viewing

- Game Boost's text in Settings, Viewing no longer says "Software encoding".

## Known issues

- Automation tools that mark every key as an extended one (Python's `uiautomation`) send some letters with Ctrl or Alt as media keys: Ctrl+Alt+C turns the host's volume down. (#66)
- A password or a trust needs the host on v0.1.0-alpha.10 or later; a code works with every host from v0.1.0-alpha.3 on.
- Ctrl+Alt+T does not bring an open chat window to the front.
- The Android viewer is experimental: it types only what a US keyboard can, and signs in with the access code only. An Android viewer needs a build from this release: earlier ones speak an older protocol, and the host says "protocol mismatch".
- A Solo licence key activates on this release and later. Earlier releases take only a licence text under About.
- The microphone uses the viewer's default microphone and the host's default speakers; neither can be chosen in TideDesk yet. The Android viewer does not send a voice.

## Good to know

A device ID also works on the same local network, even without the internet: the viewer asks its own network too, and the host answers directly. Two computers behind one router also find each other through the host's local addresses, which it gives the connection service sealed with its access code. The host encodes video on the graphics card where it has an H.264 encoder (AMD, Intel or NVIDIA) and falls back to Windows' software encoder by itself.

**Keep host and viewer on the same release.** This build uses protocol v5, and both computers need that same version: a release with another one, older or newer, says "protocol mismatch". Update both computers together.

Experimental direct internet connections, computer to computer, with no relay. The host shows its internet address, learned from public STUN servers (can be turned off in Host Settings, Internet). In the viewer, tick "Over the internet", connect to that address, and give the viewer's address to the person at the host, who types it under "Viewer on another network" and presses Open. Both computers then open a path through their routers and the session runs directly between them. Behind a symmetric NAT (common on mobile data) on one side, TideDesk guesses the next ports and usually gets through; with one on both sides, let the host's router open its port (Settings, Network) or use a VPN such as Tailscale. Hosts whose router opened the port, or with a forwarded port, connect without the manual step.

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
