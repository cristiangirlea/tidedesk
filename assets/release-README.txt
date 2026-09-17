TideDesk @VERSION@ (Windows x64)
Remote access to your own computers. Free for personal, non-commercial use.
https://github.com/cristiangirlea/tidedesk

tidedesk-host.exe  Run on the computer you want to reach. It shows an access code
                   and this computer's address, and lives in the tray.
tidedesk-view.exe  Run on the computer you are sitting at. Enter the address and
                   access code, or save computers under "My computers".

Viewer Settings: text clipboard off by default; mouse control on by default.
In the focused remote window, Ctrl+Alt+C toggles clipboard, Ctrl+Alt+M toggles mouse
control, and Ctrl+Alt+S opens settings. Toggle shortcuts are customizable.
Clipboard sharing must also be allowed in Host Settings; it shares new text copies.
An amber arrow shows the host pointer; a separate crosshair is your local pointer.
The viewer remembers each host window position and monitor. Sessions open at native
host pixel size, shrinking only to fit the available screen. Display resolutions
are not changed. Resize or maximize manually if desired.
With mouse control off, the host arrow still updates and your crosshair stays independent.
Native host cursor shapes and hidden-cursor states are not mirrored yet.
After host mouse movement, the first viewer movement only aligns the viewer pointer
to the host's current position. The next movement controls the host from there.
Update BOTH executables: protocol v2 cannot connect to the original alpha's v1.

No installation needed. Allow the host through Windows Firewall on private networks.
@SIGNING@
Code signing policy:
https://github.com/cristiangirlea/tidedesk/blob/main/docs/code-signing-policy.md

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
Dependency licenses and the vendored renderer's license notices remain in the
source repository and corresponding dependency source distributions.
