# Clipboard, mouse, keyboard and microphone controls

TideDesk uses protocol v6, since v0.1.0-alpha.16. From v0.1.0-alpha.17 on, a release that
brings a new protocol version still connects to a release of the version before it, at the
older of the two; update both computers together until that has been tried on two computers.
Today the other computer still needs v6: a build with another version (earlier ones use v1 to
v5) cannot connect, and a v6 build says which computer to update.

## Viewer settings

### Session window placement and size

Each host's last normal window position and monitor are remembered locally when
the session closes or disconnects. Reconnecting opens at that position, adjusted
to stay visible if the monitor layout has changed. A disconnected monitor falls
back to an available screen. On a first connection, the monitor under the local
mouse pointer is used when available.

Every session starts at the host's current pixel dimensions, not a previously
stretched window size. For example, a 2560 x 1600 host opens with a 2560 x 1600
image area on a 3840 x 2160 viewer display. The title bar and borders are extra.
If that does not fit in the monitor's usable area, the image is reduced
proportionally. Neither computer's display resolution is changed. You can still
resize or maximize the window manually during the session; reopening starts at
native size (or fitted down) again, using the last normal window location.

Positions are stored separately from controls in
%APPDATA%\TideDesk\window-FINGERPRINT.toml, keyed by the host's certificate identity.
This keeps different hosts separate and retains their positions after an IP change.

### Sharing controls

Open Settings in the viewer launcher, or press its shortcut (Ctrl+Alt+S unless changed)
while the remote window is focused. Changes are saved as you make them, picked up by open sessions at once, and
stored in %APPDATA%\TideDesk\viewer.toml. A shortcut that clashes with another is
shown with the reason and saved once it is valid.

| Control | Default | Default shortcut |
| --- | --- | --- |
| Bidirectional text clipboard | Off | Ctrl+Alt+C |
| Host mouse control | On | Ctrl+Alt+M |
| Host keyboard control | On | Ctrl+Alt+K |
| All input (mouse and keyboard) | | Ctrl+Alt+I |
| Microphone to the host | Off | Ctrl+Alt+V |
| Open Viewer Settings | | Ctrl+Alt+S |

All input turns mouse and keyboard off together while either is on, and both on
when both are off: one press to only watch, one press to take over again. With the
keyboard off, keys pressed in the remote window stay on this computer; the
shortcuts in this table still work. Settings files from before these shortcuts keep
the ones they chose; Keyboard, All input and Microphone take the first free keys.

Each shortcut can use Ctrl and/or Alt, optional Shift, and a letter or F1-F12.
The shortcuts must differ; with Settings on another one, Ctrl+Alt+S reaches the host. Shortcuts only act
while the remote window is focused. Holding a shortcut does not repeatedly toggle it.
The remote window title shows the current state and configured shortcuts.
When nothing has come from the host for three seconds, the title says
"No answer from the host for 3 s" and counts on; the picture is the last one
received. The session goes on if the host answers again, as after a network that
failed for a moment, and ends after about 20 seconds if it does not.

## Host permissions

In Host Settings, Live session permissions contains Allow text clipboard sharing
(off by default) and Allow viewer input (on by default), with Allow viewer keyboard
control and Allow viewer mouse control under it: the first switches both, and shows
a dash while only one of the two is allowed. These permissions apply to an existing
connection. The viewer cannot override a host denial; its title says "waiting/blocked
by host" for a control the host does not allow.

Clipboard sharing needs to be enabled at both ends. It shares new text copies made
after activation, in either direction; existing clipboard contents are not uploaded
when enabling it. Text is limited to 48 KiB of UTF-8. Images, files and rich formatting
are not transferred. Clipboard contents are never written to logs or settings.
After copying text, allow a brief synchronization delay before pasting on the other
computer. Disabling sharing leaves the last copied text in each OS clipboard.

Turning mouse control off stops movement, clicks and scrolling. Keyboard control
is independent: off, the host drops every key the viewer sends. Held remote mouse
buttons and keys are released when the control is disabled at either end, the pointer
leaves the image, focus is lost or the session disconnects.

The host's own TideDesk windows are on the shared screen, but they are the host's: the
viewer's clicks and wheel over them, and its key presses while one is in front, do not
reach them. The pointer still moves over them, and keys held from before are still let
go. To go on elsewhere, the viewer clicks another window; when TideDesk fills the whole
screen, the person at the host moves it aside. Only the viewer's input is kept out: the
person at the host, and the tools they use there, such as the on-screen keyboard, work
in TideDesk as before.

Linux is built but not in a release yet (see the [roadmap](ROADMAP.md)). A Linux host on
a Wayland desktop cannot tell which window the viewer's input reaches:
such a desktop does not say. There the host's TideDesk window is locked while the viewer
has mouse or keyboard control, a line in it says why, and only Disconnect works in it
until the control ends. The host's pointer is drawn into the picture rather than marked
with the amber arrow, and the host's own mouse movements do not take the pointer back
from the viewer.

## Microphone

The person at the viewer can talk to the person at the host: the viewer's default
microphone plays on the host's default speakers. It is off at both ends by default.
The viewer turns it on with Talk to the host in Viewer Settings, or Ctrl+Alt+V; the
host allows it with Play the viewer's microphone under Live session permissions.

Nothing is captured until the host has answered that it plays the voice. The viewer's
title says "Microphone on" only then, and "waiting/blocked by host" before. The
microphone stops when either side turns it off and when the session ends, and it keeps
going while the remote window is in the background, as a call would. Talk to the host
is a setting like the others: it stays on for the next session, with any host, and the
voice goes there once that host allows it.

The voice is mono Opus at 32 kbit/s in 10 ms packets, sent as QUIC datagrams like the
host's sound: never through a server, never stored. A packet that arrives late is
dropped. The host plays it through a short jitter buffer.

The host's shared sound leaves TideDesk's own playback out, so the viewer does not hear
its own voice come back. This uses Windows' process loopback (Windows 10 version 2004
and later). On older Windows the host's sound is the whole output device, and the voice
comes back as an echo; `tidedesk.log` on the host says so. Setting the environment
variable `TIDEDESK_SOUND=device` on the host forces that older capture, for comparing.
Leaving TideDesk out also leaves out the programs it starts: a viewer session opened
from the host's TideDesk, or a mail program it opened for a problem report, is not in
the host's shared sound while it runs.

The Microsoft Store version asks once whether TideDesk may use the microphone, as
Windows does for every packaged app. The ZIP version is a desktop program: Windows'
setting "Let desktop apps access your microphone" (Settings, Privacy and security,
Microphone) decides. When the microphone cannot open, the viewer's title says why;
when Windows gives it silence instead, the host hears nothing.

## Keys from tools

The host gets the keys of the viewer's keyboard by their place on it (hardware scan
codes), so its own keyboard layout decides what they type. Automation and accessibility
tools, and pens that write, make key events of other kinds:

- A key named by its virtual-key code is sent as the key that Windows gives for it.
  The arrows, Home, End, Page Up, Page Down, Insert and Delete are sent as those keys,
  also when they come from the number pad with NumLock off on the viewer's keyboard:
  the host's NumLock does not turn them into digits. A program on the host that tells
  the number pad's keys from the arrows, as some games do, gets the number pad's keys
  while NumLock is on at the viewer.
- A character given as text is typed with the keys that make it on the viewer's
  keyboard layout, with Shift or AltGr as needed. It comes out the same where the
  host's layout is the same. A character that no key of the viewer's layout makes
  is not sent.

## Two distinct cursors

The amber arrow inside the remote image marks the host's current pointer position.
The viewer's local pointer is a crosshair. This distinction remains visible with
mouse control either on or off; it is not a separate setting.

With control off, moving the local crosshair never moves the host pointer. Moving
the host mouse updates the amber arrow without moving the local crosshair. Updates
also arrive while the desktop image is otherwise stationary. Denying mouse control
in the host settings does not hide its pointer position from an authenticated viewer.
The marker disappears when the host pointer leaves the shared display.

The arrow is a position indicator, not the host application's native cursor shape;
text-selection, resize, busy and hidden-cursor states are not mirrored yet.

## Mouse handoff rule

This behavior always applies when mouse control is enabled; it has no separate switch:

1. A physical mouse movement on the host does not move the viewer's local pointer.
2. On the next viewer movement, the viewer requests the host's current pointer position.
3. The viewer aligns its own pointer to that location. The initiating movement and
   the generated repositioning event are not sent as host mouse movement.
4. Subsequent viewer movement controls the host from that position.

A click says where it is meant. One made before the handoff, as when a tablet, a pen
or a tool puts the pointer somewhere and clicks at once, is not lost: it waits for the
host's answer, and then the host's pointer goes to the click while the viewer's stays.
This includes the click that brings the remote window to the front: it is made on the
host too, as in other remote desktop viewers. A click is forgotten if the pointer leaves
the image before the host has answered.

The host rejects events carrying an outdated pointer epoch, including events already
in transit when someone moves the host mouse. Switching control back on, refocusing,
or resizing also requires a fresh handoff. If the host pointer is on another monitor
outside the shared display, it is not pulled back; move it onto the shared display first.

## Two-computer verification

Use two machines with the new host and viewer. A loopback session cannot prove this
behavior because both applications would share the same operating-system cursor
and clipboard.

- On a 4K viewer, connect to a 2560 x 1600 host and verify the initial image area
  is exactly 2560 x 1600 physical pixels (plus window borders), with no enlargement.
- Move the session to a secondary monitor, close it, and reconnect. Verify its
  location is restored, including monitors left of or above the primary display.
- Repeat with different Windows display scaling, a smaller viewer display, a
  disconnected secondary monitor, and after maximizing or minimizing the session.
- Connect to a second host and confirm its saved location remains independent.

- Verify both defaults and the host's independent permission switches.
- Turn the keyboard off with Ctrl+Alt+K while holding a letter: the host stops
  repeating it. Type: nothing arrives. Ctrl+Alt+K again, type again: it arrives.
  Repeat with Allow viewer keyboard control off at the host: the title says so.
- Ctrl+Alt+I with both on: nothing moves or types on the host, and the title says
  Mouse off and Keyboard off. Ctrl+Alt+I again: both work. With only the mouse off,
  Ctrl+Alt+I turns the keyboard off too.
- At the host, Allow viewer input off: the viewer only watches; both boxes under it
  clear. One of the two on again: the top box shows a dash.
- With the host's TideDesk window open on the shared screen, click its buttons and
  boxes from the viewer, scroll it, and type while it is in front: nothing changes. Click
  another window and type: that works. At the host, the same clicks work.
- Microphone: Ctrl+Alt+V at the viewer with the host's permission off: the title says
  "Microphone waiting/blocked by host" and nothing plays. Allow it at the host: the
  viewer's voice plays there within a moment, and the title says "Microphone on".
- With the host playing music and sharing its sound, talk at the viewer: the person at
  the host hears the voice, and the viewer hears the music but not its own voice.
- Ctrl+Alt+V again, then end the session: the voice stops each time, and Windows'
  microphone indicator on the viewer goes away.
- Enable clipboard at both ends; copy a new Unicode string in each direction and paste.
  Verify old clipboard text was not transferred merely by enabling sharing.
- Disable via the shortcut and confirm new copies stay local; re-enable and copy again.
- Change both shortcuts, save, use them in the active session, and restart the viewer
  to check persistence. Hold each shortcut to check it toggles only once.
- Move the host mouse while the viewer pointer is still; the viewer pointer must stay.
  Move the viewer once: only its pointer should align. Then move again to control the host.
- Repeat during rapid alternating movement, while dragging, after toggling mouse control,
  after focus loss and after resizing a letterboxed remote window.
- Disable mouse control while holding a button; ensure the host is not left dragging.
- With a pen, a tablet or a tool that puts the pointer somewhere and clicks at once:
  the first click lands on the host where it was made, also right after the remote
  window got the focus and after the host's own mouse moved.
- With mouse control off, move the host pointer over a stationary desktop: only the
  amber arrow follows it. Move the viewer crosshair independently and verify the
  host pointer stays put. Repeat with control denied in Host Settings.
- Verify both cursors after resizing, at all four image edges and with different
  display scaling on the two computers; the amber marker must stay inside the image.
- On a multi-monitor host, move the pointer off the shared display and confirm the viewer
  cannot pull it back.
