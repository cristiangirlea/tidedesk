# Licensing

The current development source is offered under the TideDesk Personal Use Source
License 1.1 in the root LICENSE file. This is a custom source-available license,
not AGPL and not an OSI-approved open-source license.

## Summary

- Individuals may run, inspect, compile and modify it for personal,
  non-commercial purposes, including unpaid help to family and friends.
- Business use, including internal organizational IT support, requires separate
  written permission: a licence file issued by the Licensor, or another agreement.
  So do resale, paid customer support and product integration.
- On a computer managed by an organisation (joined to a domain or an organisational
  identity service, or enrolled in device management), any lawful use, including
  business use, is allowed for 14 days from first use on that computer and then for
  up to 8 hours of sessions a month, as TideDesk counts them there (section 3a).
- Copies may be shared without payment for personal use, with license notices
  intact and modifications clearly identified.
- The LICENSE text is authoritative. No paid feature, subscription, commercial
  agreement or future feature is included merely by downloading the source.

## Terms of use

The [terms of use](terms-of-use.md) add rules for using TideDesk with other people and
for TideDesk's connection service (rendezvous.tidedesk.app), such as connecting only to
computers you may use and the operator's right to block misuse. They do not change the
license. A new installation asks for both to be accepted before it shares anything;
an installation used before the terms says that they exist and keeps working.

## Previously published releases

Versions v0.1.0-alpha, v0.1.0-alpha.1 and v0.1.0-alpha.2 and their corresponding
source were published under GNU AGPL-3.0-only. Those grants remain in place.
The former license is preserved in licenses/AGPL-3.0-only.txt and the release tags.
This change does not revoke rights to copies or source already offered under AGPL,
nor prevent continued use or development of those versions under AGPL.

Releases up to v0.1.0-alpha.11 were offered under version 1.0 of the TideDesk
Personal Use Source License; version 1.1 adds section 3a and says that a signed
licence file is a separate written license.

Check the license included with the particular release you download, rather than
assuming that the license on the development branch applies to every release.

## Dependencies

Third-party components keep their own licenses; the personal-use restriction is
not a relicensing of their code. The vendored egui_software_backend retains its
MIT and Apache-2.0 notices. Their notice, source-availability and other
obligations must be met when distributing a build. A Cargo metadata inventory
is not a complete legal audit.

Rust crates are checked in CI against [deny.toml](../deny.toml), which lists
the licenses TideDesk may ship: permissive ones, the egui fonts' licenses
(OFL-1.1, Ubuntu Font Licence), and MPL-2.0 for option-ext only, which Linux
builds use unmodified (its source is on crates.io). GPL, LGPL and AGPL code is
not allowed. Where a crate offers a choice, TideDesk uses the permissive one,
such as self_cell under Apache-2.0. Every ZIP and MSIX carries the crates'
license texts in `licenses/third-party`, generated from `Cargo.lock`; the app
opens them from the About tab, **Third-party notices**.

The license grants no rights in third-party patents (LICENSE, section 5).

## Roadmap

The roadmap records intentions, not a contractual promise or a delivery schedule.
TideDesk checks licence files offline (About, **Add a licence**); there is no
account, activation service or usage reporting. A payment system is not part of the
source.
