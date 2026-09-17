# Clipboard and mouse controls

These controls use protocol v2. Update both the host and viewer together.
The original v0.1.0-alpha executables use v1 and cannot connect to this build.

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

Open Settings in the viewer launcher, or press Ctrl+Alt+S while the remote window is
focused. Choose the desired options and press Save settings. Changes are picked up
by open sessions and stored in %APPDATA%\TideDesk\viewer.toml.

| Control | Default | Default shortcut |
| --- | --- | --- |
| Bidirectional text clipboard | Off | Ctrl+Alt+C |
| Host mouse control | On | Ctrl+Alt+M |

Each toggle shortcut can use Ctrl and/or Alt, optional Shift, and a letter or F1-F12.
The shortcuts must differ; Ctrl+Alt+S is reserved for settings. Shortcuts only act
while the remote window is focused. Holding a shortcut does not repeatedly toggle it.
The remote window title shows the current state and configured shortcuts.

## Host permissions

In Host Settings, Live session permissions contains Allow text clipboard sharing
(off by default) and Allow viewer mouse control (on by default). These permissions
apply to an existing connection. The viewer cannot override a host denial.

Clipboard sharing needs to be enabled at both ends. It shares new text copies made
after activation, in either direction; existing clipboard contents are not uploaded
when enabling it. Text is limited to 48 KiB of UTF-8. Images, files and rich formatting
are not transferred. Clipboard contents are never written to logs or settings.
After copying text, allow a brief synchronization delay before pasting on the other
computer. Disabling sharing leaves the last copied text in each OS clipboard.

Turning mouse control off stops movement, clicks and scrolling. Keyboard forwarding
is independent. Held remote mouse buttons are released when control is disabled,
the pointer leaves the image, focus is lost or the session disconnects.

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
- With mouse control off, move the host pointer over a stationary desktop: only the
  amber arrow follows it. Move the viewer crosshair independently and verify the
  host pointer stays put. Repeat with control denied in Host Settings.
- Verify both cursors after resizing, at all four image edges and with different
  display scaling on the two computers; the amber marker must stay inside the image.
- On a multi-monitor host, move the pointer off the shared display and confirm the viewer
  cannot pull it back.
