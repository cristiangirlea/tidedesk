# Microsoft Store distribution (MSIX)

Status: TideDesk is registered as an MSIX app in Partner Center and is still
in draft. Packaging is prepared; no published Store listing, certification or
Store-signed download is available yet. The separate GitHub ZIP remains unsigned
unless the optional executable-signing service is enabled.

## Registered package identity

These public values are copied from the owner's Partner Center product identity.
Keep the identity stable across updates; do not substitute a test publisher.

| Field | Value |
| --- | --- |
| Package/Identity/Name | CristianGirlea.TideDesk |
| Package/Identity/Publisher | CN=6B74E324-6BD2-49EA-90C8-CB25E2C0631C |
| Package/Properties/PublisherDisplayName | Cristian Girlea |
| Package family name | CristianGirlea.TideDesk_pj1n3tb47e7ka |
| Store ID | 9PLH1HWXHB3Q |

The Store ID is reserved, not evidence of a live listing or Microsoft signature.
Keep MSIX_ENABLED=false until the required pre-submission checks below pass.

## One-time owner setup

1. Open https://storedeveloper.microsoft.com/ and complete the appropriate
   Individual or Company account onboarding yourself. Use Microsoft's current
   account-type criteria; a personal-use edition does not itself determine the
   publisher's business status. Do not share identity documents or login codes.
2. In Partner Center > Apps and games, reserve the TideDesk app name (subject to
   availability) and choose packaged MSIX distribution, not the MSI/EXE route.
   The manifest also names its Start entries TideDesk Host and TideDesk Viewer:
   reserve these additional names for the same product and verify name validation
   on upload. If a name is unavailable, adjust the manifest before submission.
3. Under the app's Product management > Product identity, copy the exact
   Package/Identity/Name, Package/Identity/Publisher and publisher display name.
   These are public package metadata, not account passwords or certificate keys.
4. In GitHub > repository Settings > Secrets and variables > Actions > Variables,
   add the settings below. Leave MSIX_ENABLED absent/false until validation and
   the identity values are ready. No Microsoft API secret is needed for manual
   Partner Center upload. SignPath is not required for Store package signing.

| Variable | Value |
| --- | --- |
| MSIX_ENABLED | true to build a Store candidate on each release tag |
| MSIX_IDENTITY_NAME | Exact Package/Identity/Name from Partner Center |
| MSIX_PUBLISHER | Exact Package/Identity/Publisher, including CN= |
| MSIX_PUBLISHER_DISPLAY_NAME | Exact publisher display name |

Never invent identity values for a real submission. TideDesk.TestOnly is used
only in local/CI validation and must not be submitted to the Store.

## Each release

After the normal review, merge and authorized tag push, Release builds and tests
the Rust workspace with NASM. With MSIX_ENABLED=true it also creates an unsigned
MSIX and checksum in the microsoft-store-submission Actions artifact (30-day
retention). Download that artifact promptly and retain the approved candidate.
It is not attached to the public GitHub release or presented as an installer.

Store package versions are 1.0.<Release workflow run number>.0, separate from
the user-facing TideDesk version embedded in the executables. This supports
alpha updates without mapping semver prerelease text into MSIX's numeric version.
Reruns have the same package version. Do not submit different packages at the
same version; create a new reviewed release. Before renaming/replacing the
workflow or reaching build 65535, advance the version prefix deliberately so
the next Store version remains greater than every published package.

Upload the candidate to a new Partner Center submission, update release notes,
review the validation results, and submit for certification. Microsoft signs
the package during publication after approval. This workflow does not upload,
submit, accept agreements, or publish to the Store automatically. Store updates
are separate from GitHub releases; portable ZIP users still update manually.

## Required checks before the first submission

- Review the custom license and ownership of original code; previously granted
  AGPL permissions remain unchanged. The license draft is not legal advice.
- Audit all binary dependencies and include their required license/copyright
  notices, including native OpenH264/Opus and embedded fonts. The current
  licenses directory is not a complete third-party notice bundle. Do not submit
  or distribute the candidate until that audit and packaging are complete.
- Use an isolated test machine/VM for development signing and installation.
  Do not install a test certificate into a production trust store. The unsigned
  candidate is not a double-click installer for end users.
- Run Windows App Certification Kit against the installed package. MakeAppx
  schema validation alone is not certification.
- Test both Start-menu entries on supported Windows x64 systems (manifest
  minimum Windows 10 build 19041), as a standard user. Test screen, audio,
  clipboard, remote input, tray behavior, disconnect and two-computer use.
- Test first-run Windows Firewall access, reconnect after package updates and
  blocking access on untrusted networks. Never disable the firewall.
- Test settings/access-code persistence through updates, then uninstall.
  MSIX can redirect application data: do not promise automatic migration from
  the portable ZIP or retention after uninstall.
- Startup is opt-in. Packaged builds use the manifest startup task; portable
  builds keep the per-user Run key. Test enabling/disabling, sign-in behavior,
  the "Start hidden in the tray" setting, and disabling through Task Manager.
  A user/policy-disabled startup task must not be silently re-enabled.
- Prepare real screenshots, description, age ratings, support contact, a live
  app privacy-policy URL and reviewed custom license terms for the listing.
  Do not advertise passwords, 2FA, hardware encoding or a relay as implemented.
- Explain the restricted runFullTrust capability in certification notes:
  TideDesk is a user-launched Win32 desktop app using DXGI screen capture,
  WASAPI audio and permitted keyboard/mouse input for an authenticated peer.
  It does not install a service/driver or request elevation; UAC and Windows
  login-screen control are unsupported. Approval is Microsoft's decision.

## Local packaging

Build with NASM on PATH and TIDEDESK_VERSION set to the intended release version.
Use PowerShell 7 and the x64 MakeAppx from the Windows 10/11 SDK.

~~~powershell
$settings = @{
    Version = '0.1.0-alpha.2-dev' # must match both executable product versions
    PackageVersion = '1.0.1.0'   # strictly above the last Store package
    IdentityName = $env:MSIX_IDENTITY_NAME
    Publisher = $env:MSIX_PUBLISHER
    PublisherDisplayName = $env:MSIX_PUBLISHER_DISPLAY_NAME
}
./tools/package-msix.ps1 @settings
./tools/test-msix.ps1 # test identity only; never installs or submits
~~~

Output directories must be new; packages are never overwritten. No signing
certificate, Store account credentials or publication permissions are requested.

## Microsoft documentation

- [Account setup](https://learn.microsoft.com/en-us/windows/apps/publish/partner-center/open-a-developer-account)
- [Package requirements, identity and version rules](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/app-package-requirements)
- [Additional app names](https://learn.microsoft.com/en-us/windows/apps/publish/partner-center/pwa/manage-app-name-reservations)
- [Manual packaging](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-manual-conversion)
- [Desktop compatibility checks](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-prepare)
- [Packaged startup tasks](https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.startuptask)
