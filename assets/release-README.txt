TideDesk @VERSION@ (Windows x64)
Remote access to your own computers. Free for personal, non-commercial use.
https://github.com/cristiangirlea/tidedesk

tidedesk.exe       One program, one window. Run it on both computers.
                   "Share this computer" shows the access code, this computer's
                   addresses and its device ID; TideDesk keeps sharing from the
                   notification area (tray) when the window is closed.
                   "Connect to a computer" takes a device ID or an address plus
                   the access code, and keeps your saved computers.
tidedesk-host.exe  The same as "tidedesk host".   Both remain in this release
tidedesk-view.exe  The same as "tidedesk view".   for existing shortcuts and
                                                 autostart entries.

Viewer Settings: text clipboard off by default; mouse control on by default.
In the focused remote window, Ctrl+Alt+C toggles clipboard, Ctrl+Alt+M toggles mouse
control, Ctrl+Alt+G toggles Game Boost, and Ctrl+Alt+S opens settings.
Toggle shortcuts are customizable.
Game Boost is EXPERIMENTAL and off by default. Its Settings button applies live,
targeting 60 FPS with smaller audio/video buffers; actual performance varies.
Switch it off to restore desktop settings. Host resolution and bitrate are unchanged.
Software encoding and desktop mouse only: no GPU encoding or relative game-camera input.
Audio is host system output to viewer only; microphone forwarding is not supported.
Clipboard sharing must also be allowed in Host Settings; it shares new text copies.
An amber arrow shows the host pointer; a separate crosshair is your local pointer.
The viewer remembers each host window position and monitor. Sessions open at native
host pixel size, shrinking only to fit the available screen. Display resolutions
are not changed. Resize or maximize manually if desired.
With mouse control off, the host arrow still updates and your crosshair stays independent.
Native host cursor shapes and hidden-cursor states are not mirrored yet.
After host mouse movement, the first viewer movement only aligns the viewer pointer
to the host's current position. The next movement controls the host from there.
Keep BOTH computers on the same release. Protocol v3 connects to v0.1.0-alpha.3
and later, not to the earlier v1/v2 alphas.
This alpha is for testing, not production-critical remote access. Real-world
two-computer gameplay and end-to-end latency validation are still pending.

No installation needed, and no Visual C++ Redistributable is required.
Allow the host through Windows Firewall on private networks.
@SIGNING@
Code signing policy:
https://github.com/cristiangirlea/tidedesk/blob/main/docs/code-signing-policy.md

Direct internet connections are EXPERIMENTAL. The host shows its internet address.
In the viewer, tick "Over the internet", connect to that address, and give the viewer's
address to the person at the host, who types it under "Viewer on another network" and
presses Open. The session runs directly between the two computers: TideDesk never relays.
Symmetric NAT (common on mobile data) prevents a direct path; use a VPN such as Tailscale.
Addresses are looked up from public STUN servers, which see the public IP address and port.
A viewer can instead connect by the host's device ID (TD-XXXX-XXXX-XXXX-XXXX), headless
hosts too: TideDesk's rendezvous service (rendezvous.tidedesk.app) introduces the two
computers and never carries the session. Hosts register with it unless that is turned off
in Host Settings; another service can be named in Host and Viewer Settings.

Windows to Windows, one viewer at a time. For internet connections, see:
https://github.com/cristiangirlea/tidedesk/blob/main/docs/internet-access.md

To remove: disable "Start with Windows" in Host Settings if enabled, quit the host
from its tray menu, close the viewer, then delete the extracted folder.
Optional: remove %APPDATA%\TideDesk to erase saved settings, computers and identities.
Remove any Windows Firewall exception you created for TideDesk.

Licensed under the TideDesk Personal Use Source License 1.0 (see LICENSE).
Business use, resale and paid customer support require separate written permission.
Earlier AGPL releases retain their original permissions; these terms are not retroactive.
Source for this version: https://github.com/cristiangirlea/tidedesk/tree/v@VERSION@
Third-party license notices for the components built into these executables are in
the licenses folder (see licenses/third-party/INDEX.txt).
