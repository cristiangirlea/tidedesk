TideDesk @VERSION@: remote desktop for Windows x64, free for personal, non-commercial use.

License: TideDesk Personal Use Source License 1.0 (see LICENSE in the ZIP). Business use requires separate written permission: a TideDesk Solo subscription from https://tidedesk.app/pricing/ is that permission (the TideDesk Commercial License 1.0 at https://tidedesk.app/terms/). Previously published AGPL releases retain their original permissions.

Download @ZIP@ and extract it. Run tidedesk.exe on both computers.

@SIGNING@

**Experimental alpha, not a stable release.** Game Boost is opt-in and off by default. Two-computer gameplay and end-to-end latency validation are still pending.

## New in this release

### The same protocol as alpha.16 and alpha.17

- **Still protocol v6.** This release speaks protocol v6, like v0.1.0-alpha.16 and v0.1.0-alpha.17, so it should connect to both. Two computers on different releases have not been tried together yet, so update both together, as before. Releases from v0.1.0-alpha.15 back use an older protocol and are still told which computer to update. (#459)
- **Ready for the next protocol.** Host and viewer can now speak a range of protocol versions and agree on the older of the two sides' newest. This release's range holds v6 only, so nothing changes yet. A later release that moves to a new protocol can then still reach this one at v6. (#459)

### Protected settings

- **Windows' administrator prompt guards the settings that let someone in.** Trusting a viewer, setting the saved password, unticking **New code after each session** and sharing when started at sign-in now show Windows' administrator prompt. **Yes** keeps the change; **No** keeps nothing. On a standard account, the prompt asks for an administrator's password. These settings are kept where only an administrator can change them, so a viewer using the host's mouse and keyboard cannot give itself lasting access: its clicks cannot reach the prompt. (#518)
- **Taking access away applies at once.** Removing a viewer or the password, turning new codes back on and turning start-up off take effect straight away; the prompt only makes them last. (#518)
- **Only when TideDesk is installed.** Windows is asked only when tidedesk.exe is in a folder users cannot change, such as Program Files. A copy run from another folder, such as Downloads, saves changes as before, and its window says to install it to protect them. (#518)
- **Refused during a session where Windows would not stop the viewer.** While a viewer is connected, changes that let more in are refused when the administrator prompt is turned off, when Windows elevates without asking, or when TideDesk runs as an administrator. The window says why. (#518)
- **Your settings move over when you choose.** Trusted viewers, the password and these settings from an earlier release keep working until the first confirmed change, or **Protect them**, moves them. Once they are protected, a host started at sign-in shares only if that was confirmed at the prompt. `--trust-viewer`, `--untrust-viewer`, `--set-password` and `--remove-password` ask in the same way. (#518)

### Connections

- **Hosts check the connection service's replies.** A host now accepts only replies that really come from the connection service, so a forged reply can no longer point it at another computer or take it off the service. It still works with the connection service as it was: the host then falls back to the earlier way after about three seconds. (#220)
- **Fewer introductions per viewer.** One viewer network can ask a host for at most 10 connections a minute through the connection service. The host guesses a viewer's next ports only when the connection service has checked that viewer's address. (#220)
- **One address cannot hold every place to sign in.** An address, or one IPv6 network, can hold only 2 of the 32 connections still setting up encryption, so strangers no longer keep everyone else out until they time out. Computers that came in before are not limited. (#327)
- **Less memory held for strangers.** Until a viewer is let in, the host drops the picture and sound packets it sends, instead of keeping up to 256 KB of them for each connection. (#326)

### Sessions

- **The same plain words in TideDesk's window.** When a session ends, TideDesk's window now gives the same plain reason as the session's own window, not the technical one. The technical reason stays in the log. (#421)
- **One session per computer, however it is typed.** Typing a computer's address another way, with or without the default port or with a dot at the end of its name, brings its open session to the front instead of opening a second one. A question before connecting shows one way to say no instead of two. (#381)
- **A stuck session gives way.** A viewer that knocks while the session in place has been silent for 30 seconds now gets in, even when that session cannot end by itself. The old session is stopped: none of its input, clipboard, chat or files reach the new one, and keys it held down are released. History says the viewer stopped answering. (#363)

### Files

- **Windows warns before a received program runs.** A file received by the viewer or the host, alone or in a folder, is marked as downloaded from the internet, as a browser marks a download. Windows then asks before running it. On a drive that cannot hold the mark (FAT, exFAT, some network shares) the file is saved unmarked, as browsers do. (#509)
- **Received names cannot disguise a file.** Characters that change how a name reads are left out of the name a received file or folder is saved under. A file whose name was made to show as `photoexe.jpg` is saved as `photogpj.exe`, which is what it is. (#510)
- **Size limits.** Each side refuses a file larger than the largest file it takes: a new setting under **Allow files**, 4 GB unless you change it. A file that would leave the disk less than a tenth of its free space is refused too. At most eight files of a session are on their way at once; the next one waits its turn and can be cancelled while it waits. An older release on the other side shows these refusals in English. (#515)
- **No settings, no files.** When Viewer Settings cannot be read, the viewer refuses files from the host instead of taking them all. The host hears that the viewer does not accept files. (#513)
- **A file never replaces another of the same name**, also on drives without hard links, such as FAT and exFAT. The later one is saved as "name (2)". (#514)
- **Folders whose names clash stay apart.** Received folders whose names come out the same once saved, such as `a?b` and `a*b`, or `Docs` and `docs`, used to share one folder. The second is now numbered, as a folder sent on its own is. (#296)
- **After a session ends**, a file dropped on its window is refused with "Could not send ‹name›: the session ended". A file cut off by the end no longer adds a notice on top of "Session ended", and a `send` from a script says the session ended instead of reporting success. (#366)

### Clipboard

- **Asked once for each computer.** Before it shares the clipboard with a host, the viewer asks "Share your clipboard with ‹computer›?". Until you say yes, its title says "Clipboard off for this computer". A no is remembered: the clipboard shortcut, or turning sharing on in Settings, asks again, and Settings can forget every answer. (#223)
- **Only what you copy in the session's window.** What you copy is shared only while the session's window is in front. A copy made in another window goes when you come back, the last one only. This is a new option, on by default. (#223)
- **Password managers' copies stay on your computer.** On Windows, a copy its app marks as private, as password managers do, is never sent, by the viewer or the host. (#223)

### Access

- **A password cannot start or end with a space.** It could never be typed in, because the viewer trims what is typed. Setting one now says "Don't start or end the password with a space." (#413)
- **A computer met before waits three minutes at most.** After too many wrong codes or passwords from a computer the host has met before, it waits at most three minutes instead of up to an hour, so someone with a copy of its key cannot keep its owner out for long. Strangers keep the long wait. The host keeps count for at most 256 such computers; past that, a new one is counted by its address, as a stranger is. (#309)

### Security

- **Two libraries updated.** TideDesk now uses tokio 1.53.3, which fixes RUSTSEC-2026-0386, and clipboard-win 5.4.3, which replaces a withdrawn version and checks the bounds of copied web content. (#476)
- **Damaged signatures are refused.** A licence file, a saved computer or a pasted key whose signature holds characters that are not hex digits is now refused with "the signature is not readable", instead of stopping TideDesk. Sealed files and the session history take hex digits only too. (#452, #472, #475)

### Problem reports

- **Errors that may carry the other computer's words are left out too.** A send or save error, or the reason the other computer gave for ending a session, shows in a report as "[error, may hold the other side's text]". The other computer can no longer fake where its text starts or ends in the log. (#315)

### Picture and sound

- **A fresh picture on a still screen.** When a viewer asks for a whole picture, after a decoder error or when the Android viewer comes back to a paused session, the host sends it at once, even if nothing on its screen changes. Before, the viewer waited for the next change. (#473)
- **The microphone stops at once.** Voice still queued when you turn the microphone off, or when the session ends, is dropped instead of sent. (#331)

### Android viewer

- **Leave for another app and come back.** Going to another app no longer ends the session: it is paused for three minutes, with a notification that says so and has **End**. At 2:30 it says the session ends in 30 seconds. Coming back carries on; nothing is decoded or played while paused. Back and **Disconnect** still end the session at once. (#319)
- **Notifications needed for the pause.** The app asks once for notifications, at the first session, on Android 13 and later. With notifications off, leaving ends the session as before: a pause nobody can see is not allowed. (#319)
- **Reconnect.** For 4 minutes 30 seconds after a session ends, the connect screen offers "Reconnect to ‹computer›" with the same code. The host still takes that code from this phone for 5 minutes after a dropped session. The code is held in memory only, never saved. If the host refuses it, type its current code. (#319)
- **The phone shows hosts a certificate of its own**, made on first use and kept in the app, so a host knows it again after a dropped session. (#319)
- **The Keyboard button closes the keyboard too.** Ctrl, Alt, Win and Shift held from the keys row are let go whenever the keyboard closes. (#320)
- **Forgotten computers go at once.** After **Forget all computers** in Settings, the connect screen no longer shows them. (#462)
- **Steadier when a session ends.** An update arriving just after the end no longer touches the closed session screen. (#344)

### Reliability

- **This computer keeps one identity.** When TideDesk sets aside identity files that do not match, it keeps every earlier copy, and never writes a new identity over files it could not move. A certificate or key that cannot be read stops the start instead of giving this computer a new identity. Two TideDesks starting at once end with one identity, and a plain copy of the host's key left beside the sealed one is removed. (#349, #350, #351)
- **Trusted viewers kept when two TideDesks save at once.** When the key that signs the list of trusted viewers could not be read, two TideDesk processes saving together could leave a list that trusted nobody. They now take turns and use the same new key. (#333)
- **The host's window opens on a first start.** With a new settings folder, it could stop at once with a font error. (#464)
- **Router ports cleaned up in more cases.** A port left open on the router after a crash is closed at the next start, even when port mapping is now off. A mapping whose answer was lost stays written down so it can be closed later. With no settings folder, TideDesk asks the router for nothing. A mapping with no lifetime, which a router reads as "remove", is never asked for. (#330, #336, #329)
- **No temporary files left behind** when saving a sealed file or the router record fails. (#334)

## Introduced in v0.1.0-alpha.17

### The same protocol as alpha.16

- **Still protocol v6.** This release speaks the same protocol as v0.1.0-alpha.16, so the two should connect. That has not been tried on two computers yet, so update both together, as before.

### Languages

- **TideDesk in eight languages.** **Settings**, **Language** offers English, Romanian, German, Spanish, French, Italian, Portuguese (Brazil) and Polish, each by its own name, besides the language Windows uses. The change applies at once, without a restart: the windows, the host's pages and Settings (**Licence** and **Organisation** included), the **Computers** page, History, the tray and the messages TideDesk shows. A session, chat or files window that is already open changes language when it opens again. Technical details in an error stay in English. (#244, #245, #246, #248, #249)

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

- Automation tools that mark every key as an extended one (Python's `uiautomation`) send some letters with Ctrl or Alt as media keys: Ctrl+Alt+C turns the host's volume down. (#66)
- A password or a trust needs the host on v0.1.0-alpha.10 or later; a code works with every host from v0.1.0-alpha.3 on.
- Ctrl+Alt+T does not bring an open chat window to the front.
- The Android viewer is experimental: it types only what a US keyboard can, and signs in with the access code only. An Android viewer needs a build from v0.1.0-alpha.16 or later: earlier ones speak an older protocol, and the host says "protocol mismatch". Its settings can be changed only between sessions. Back ends the session, and leaving it for another app ends it after three minutes.
- A Solo licence key activates on this release and later. Earlier releases take only a licence text under About.
- Protected settings have not been tried in the Microsoft Store version. Some ways to keep access still bypass them, such as an older TideDesk copy on the same computer.
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
