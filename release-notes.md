TideDesk @VERSION@: remote desktop for Windows x64, free for personal, non-commercial use.

License: TideDesk Personal Use Source License 1.0 (see LICENSE in the ZIP). Business use requires separate written permission: a TideDesk Solo subscription from https://tidedesk.app/pricing/ is that permission (the TideDesk Commercial License 1.0 at https://tidedesk.app/terms/). Previously published AGPL releases retain their original permissions.

Download @ZIP@ and extract it. Run tidedesk.exe on both computers.

@SIGNING@

**Experimental alpha, not a stable release.** Game Boost is opt-in and off by default. Two-computer gameplay and end-to-end latency validation are still pending.

## New in this release

### The same protocol as alpha.16

- **Still protocol v6.** This release speaks the same protocol as v0.1.0-alpha.16, so the two should connect. That has not been tried on two computers yet, so update both together, as before.

### Languages

- **TideDesk in eight languages.** **Settings**, **Language** offers English, Romanian, German, Spanish, French, Italian, Portuguese (Brazil) and Polish, each by its own name, besides the language Windows uses. The change applies at once, without a restart: the windows, the host's pages and Settings, History, the tray and the messages TideDesk shows. A session, chat or files window that is already open changes language when it opens again. Technical details in an error stay in English. (#244, #245, #246, #248, #249)

### Window

- **One size on every screen.** TideDesk's window keeps the same size in pixels when it moves between screens with different scaling. It is chosen at start-up to fit every screen, and changes only when you resize it or when a screen is too small for it. On a screen at a high scale, what the window shows is drawn bigger and scrolls inside it. Before, the window kept one size in points, so its size in pixels changed when it moved to a screen with another scale. (#435, #439)

### Connections

- **A device ID stays with the computer you met.** After the first connection by device ID, the viewer accepts only that computer's certificate for that ID. Another computer answering to the same ID is refused before the access code or password is used. (#219)
- **Why a silent session ended.** When the host ends a session it has heard nothing from for 30 seconds, the viewer now says "the host heard nothing from this viewer and ended the session" instead of "the connection was lost". (#364)
- **A certificate library updated for a security advisory.** TideDesk now uses rustls-webpki 0.103.16, which fixes RUSTSEC-2026-0382. (#454)

### Sessions

- **Recently connected keeps up.** With several sessions open, a computer is listed under **Recently connected** as soon as its session connects, and a computer saved by one session is no longer dropped when another session saves at the same moment. (#379, #418)
- **A damaged list of known computers is kept.** When `known_hosts.txt` cannot be read, TideDesk now says "reading known_hosts.txt" and remembers no new computer until the file is fixed or deleted. Before, it was silently replaced by a new list. (#418)
- **Closing TideDesk's window during a session** no longer makes a session stop with an internal error, and a session's end is reported once. (#382)

### Sharing

- **A better hint when sharing cannot start.** TideDesk says another TideDesk may be using the port only when the port is the problem. Otherwise it suggests starting TideDesk again, and sending a report from **History**, **Problems** if that keeps happening. (#348)

### Access

- **Codes typed in a terminal stay hidden.** `tidedesk view` asking for an access code or password, and `tidedesk host --set-password`, no longer show what is typed. (#357)

### Android viewer

- **The computer's sound.** The Android viewer plays the host's sound. **Sound from the computer** in its Settings turns it off; a change applies from the next session. (#451)
- **In the phone's language**, in the same eight languages. (#247, #248, #249)
- **Device IDs remembered.** Like the desktop viewer, it keeps the certificate it met for each device ID and refuses another. (#219)

## Introduced in v0.1.0-alpha.16

### Update both computers

- **This build speaks protocol v6.** A new message lets the host tell the viewer about its own window (see **Input**), so both computers need this release. An older one is told which computer to update. (#395)

### Sessions

- **Several sessions at once.** Connect to several computers from one TideDesk: its window lists every open session with its status and a **Disconnect** for each, and each session has its own window. Connecting again to a computer that already has a session open brings that session's window to the front instead of opening a second one. TideDesk's window no longer minimises when a session starts. (#383)

### Input

- **Told when TideDesk's window on the host keeps your input out.** The host's own TideDesk window does not take the viewer's clicks and keys. Until now the viewer was not told: the first time it happens in a session, the viewer's title says "TideDesk's own window on the host does not take your clicks or keys". (#395)

## Introduced in v0.1.0-alpha.15

### Once, after updating

- **A new access code, and trusted viewers to trust again.** On Windows the access code is now kept sealed for your Windows account, and the list of trusted viewers carries a signature. A code or a list from an earlier release cannot be checked, so the host makes a new code once and trusts nobody from the old list. Each viewer signs in with the new code, and the person at the host presses **Trust this viewer** to trust it again. Going back to an earlier release changes the code once more, and so does coming back to this one. (#227)

### Files

- **See what is on its way.** Each file sent or received is listed with its size, how much has gone, the speed, and where it was saved, or why not. The host lists them on the Share tab, under the viewer, with **Clear** for the files that are done; **Open Downloads\TideDesk** sits beside **Send files**. The viewer shows them in a **Files** window, which opens by itself when a file starts going either way. (#196)
- **Cancel a file on its way.** Each file still going, or waiting its turn behind another, has **Cancel** in the list of files. Only that file stops; the other files and the session go on. The list then says where it was cancelled, and nothing of it is left in Downloads\TideDesk. (#200)
- **Send a folder.** Drop one on the viewer's window, or press **Send a folder** on the host: it arrives in Downloads\TideDesk with everything in it, listed as one line. Both computers need this release, as with every change to what the two computers tell each other: an older one says "protocol mismatch". (#201)
- **The sender sees the name the folder was saved under.** When the name is already taken there, the folder is saved as "Photos (2)", and the sender's list now says so. (#298)
- **Two files of the same name sent at the same moment both keep theirs.** Before, one could rarely replace the other, while both senders were told it arrived. The second is now saved as "name (2)". (#183)

### Input

- **Keyboard off, and all input off.** The viewer's keyboard now has a switch like the mouse: **Ctrl+Alt+K** (changeable) keeps keys on your computer, and **Ctrl+Alt+I** turns mouse and keyboard off together, or back on, so one press lets you only watch. The host decides too: **Settings**, **Live session permissions** has **Allow viewer keyboard control** next to the mouse one, and **Allow viewer input** over both. A key held when control goes off is released on the host. Keys and mouse that the host does not allow show in the viewer's title as "blocked by host". (#191)
- Both computers need this release for the keyboard switch: it changes what the two tell each other about input, so alpha.15 moved to protocol v5 and an older release says "protocol mismatch".
- **The host's TideDesk windows are the host's own.** They are on the shared screen, but the viewer's clicks and key presses no longer reach them. The person at the host uses them as before. (#210)

### Sound

- **Talk to the host.** The viewer's microphone can play on the host's speakers, so the person at the viewer can talk to the person at the host. It is off at both ends: **Ctrl+Alt+V** (changeable) or **Talk to the host** in the viewer's Settings, and **Play the viewer's microphone** in the host's **Live session permissions**. Nothing is captured until the host has agreed, and the viewer's title says when the microphone is on. The voice goes directly between the two computers, like the host's sound. (#192)
- **No echo.** The host's shared sound now leaves TideDesk's own playback out, so the viewer does not hear its own voice come back. This needs Windows 10 version 2004 or later on the host; older Windows shares the whole output as before.
- **Sound that stops during a session.** The host now tells the viewer when its sound stops, and the viewer goes on as in a session without sound, also when the sound stops with an error. The host's log says so too. Both computers need this release. (#267, #290)
- **A microphone that stops is shown off.** When the viewer's microphone cannot be opened, or stops during a session, the viewer turns it off and says why in the viewer's title: "Microphone off: it could not be opened" or "Microphone off: it stopped". Turn it on again with its switch. (#301)
- The Microsoft Store version asks once for the microphone, as Windows does for every app that uses one.

### Connections

- **Back in after the host locks.** When the host computer locked or went to the sign-in screen during a session, the viewer lost it, and the host then refused every new attempt with "host already has a viewer connected" until someone pressed Disconnect at the host. Now a viewer that has been silent for 30 seconds gives its place to the one that comes back, and a viewer whose session ended closes its connection at once. (#359)
- **Sharing repairs its own keys.** After an older TideDesk copy had run on the same computer, sharing could stop starting with a "keys may not be consistent" error. TideDesk now repairs this itself and keeps this computer's identity when it can. Remove old TideDesk folders you no longer use: an old copy started again gives this computer a new ID. (#347)
- **Another TideDesk computer behind the host's router no longer gets in the way.** Over the internet, the viewer tries a few ports near the host's, and another TideDesk computer there could answer in the host's place. The viewer then waited 20 seconds or stopped at that computer's fingerprint. It now checks the computer that answered, and goes to the host's own address when it is not the host. It then keeps that address open while the connection starts, and a connection by device ID also tries the host's own address before giving up. (#237, #260)
- **A host met before at another address is asked about.** Over the internet, a host known only by another address is no longer trusted silently: TideDesk asks once, and remembers the answer. Two computers behind one router are no longer taken for each other without asking, and no password or code is sent before the host is confirmed. From a script with no terminal, such a host is refused until it is given with `--fingerprint`. (#259, #289)
- **Router ports are never opened for good.** TideDesk never asks a router to keep its port open permanently: a router that can only do that gets no mapping, and a port left open after a crash is closed at the next start, or ends by itself when its time runs out. (#225)
- **Steadier when strangers knock.** A computer that connects to the host but has not given the access code or password can open only the one connection it needs to sign in until it does, so many of them at once use far less of the host's memory. Files and sound start as before once the viewer is let in. The host also takes fewer such connections at once and gives each less memory until it signs in. Computers that came in before still get through. (#213, #279)

### Access

- **A typed code or password is hidden.** In the viewer, an access code or password shows as dots while it is typed, both to connect and for a saved computer: tick **Show** under the field to see it. The host's fields for a new password have **Show password**. (#355)
- **The saved password works only from a computer that connected before.** The host now checks this itself: a computer that never came in with the access code or as a trusted viewer is told to use the access code once. The host starts its list with this release, so every computer, trusted viewers included (see **Once, after updating**), uses the code once more before the password works for it again. Where an administrator has turned the access code off, the password works as before. (#211)
- **Disconnect means gone.** When you press **Disconnect** (or **Disconnect viewer** in the tray), turn off **Accept new connections**, or quit, the code that viewer used stops working at once. After a dropped connection the old code still works for five minutes from the first drop, but now only for the viewer that was connected, so nobody else can use it. The Android viewer shows no certificate yet, so after a dropped connection it needs the new code. (#209)
- **Guessing from many computers slows down instead of locking you out.** A wrong access code or password counts from the moment it is tried, so many tries at once from one address get no further than a few. When many addresses guess at once, each try waits its turn instead of the host turning your own computers away, and computers that came in before, by their certificate or from an address used in the last month, never wait. A computer that came in before is counted by its certificate, so another one at the same internet address cannot lock it out. (#212, #277)
- **Libraries only from Windows' own folder.** TideDesk loads the system's libraries only from Windows' System32 folder, so a file placed next to it cannot be run in its place. (#227)

### Problem reports

- **Reports leave out who and where.** A report sent with **Send to TideDesk** no longer carries the other computer's name, the names of files sent or received, or full internet addresses: only the first part of an address is kept. (#226)

### Android viewer

- **Steadier sessions.** Leaving a session no longer closes the app now and then. Switching between light and dark mode during a session now ends the session instead of closing the app. (#224, #310)

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

- The Licence page, and a company's profile and joining text, stay in English when another language is chosen under **Settings**, **Language**.
- Automation tools that mark every key as an extended one (Python's `uiautomation`) send some letters with Ctrl or Alt as media keys: Ctrl+Alt+C turns the host's volume down. (#66)
- A password or a trust needs the host on v0.1.0-alpha.10 or later; a code works with every host from v0.1.0-alpha.3 on.
- Ctrl+Alt+T does not bring an open chat window to the front.
- The Android viewer is experimental: it types only what a US keyboard can, and signs in with the access code only. An Android viewer needs a build from v0.1.0-alpha.16 or later: earlier ones speak an older protocol, and the host says "protocol mismatch". Its settings can be changed only between sessions, and leaving the session screen ends the session.
- A Solo licence key activates on this release and later. Earlier releases take only a licence text under About.
- The microphone uses the viewer's default microphone and the host's default speakers; neither can be chosen in TideDesk yet. The Android viewer does not send a voice.

## Good to know

A device ID also works on the same local network, even without the internet: the viewer asks its own network too, and the host answers directly. Two computers behind one router also find each other through the host's local addresses, which it gives the connection service sealed with its access code. The host encodes video on the graphics card where it has an H.264 encoder (AMD, Intel or NVIDIA) and falls back to Windows' software encoder by itself.

**Keep host and viewer on the same release.** This build uses protocol v6, and both computers need that same version. This release then says which computer to update; an earlier one says "protocol mismatch". Update both computers together. (#370)

Experimental direct internet connections, computer to computer, with no relay. The host shows its internet address, learned from public STUN servers (can be turned off in Host Settings, Internet). In the viewer, tick "Over the internet", connect to that address, and give the viewer's address to the person at the host, who types it under "Viewer on another network" and presses Open. Both computers then open a path through their routers and the session runs directly between them. Behind a symmetric NAT (common on mobile data) on one side, TideDesk guesses the next ports and usually gets through; with one on both sides, let the host's router open its port (Settings, Network) or use a VPN such as Tailscale. Hosts whose router opened the port, or with a forwarded port, connect without the manual step.

Also experimental: connect by device ID. A viewer types the host's device ID instead of anyone typing internet addresses, and headless hosts can be reached too. Hosts register with TideDesk's connection service (rendezvous.tidedesk.app) unless that is turned off in Host Settings; it only introduces the two computers and never carries the session. What it sees is in the privacy notes of the code signing policy.

[Internet access](https://github.com/cristiangirlea/tidedesk/blob/v@VERSION@/docs/internet-access.md) and [design notes](https://github.com/cristiangirlea/tidedesk/blob/v@VERSION@/docs/design/nat-traversal.md)

Experimental Game Boost, in Viewer Settings or with Ctrl+Alt+G (customizable), switches live without reconnecting: a 60 FPS target, a smaller pending decode queue and lower audio buffering. Turning Boost off restores the desktop profile. Actual FPS depends on both PCs and the connection; host resolution, bitrate and sharing permissions are unchanged.

[Game Boost usage and limitations](https://github.com/cristiangirlea/tidedesk/blob/v@VERSION@/docs/game-boost.md)

Keyboard and desktop/absolute mouse input only. Relative game-camera input, controllers and USB forwarding are not implemented. The viewer decodes on the processor. The host's sound plays on the viewer, and with Talk to the host the viewer's microphone plays on the host; no additional driver is needed.

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
