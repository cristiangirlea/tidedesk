# Windows releases

The Release workflow runs only when a v* tag is pushed. Publishing a tag authorizes
that workflow to create its GitHub release. Obtain the maintainer's approval before
pushing a branch, tagging or publishing.

## Build and archive

v0.1.0-alpha.5 adds **experimental** direct internet connections: hole punching
through both routers, never a relay (see [internet access](internet-access.md) and the
[design notes](design/nat-traversal.md)). It keeps protocol v3: v0.1.0-alpha.3 and later
still connect on a local network, while internet connections need both computers on
alpha.5; earlier v1/v2 builds cannot connect. Since v0.1.0-alpha.4 the executables link
the C runtime statically, so they need no Microsoft Visual C++ Redistributable, and the
archive ships third-party license notices. Game Boost, introduced in v0.1.0-alpha.3, remains **experimental**.
Publish it as a prerelease, not a stable release. Automated checks do not replace
two-computer validation. Audio remains host system output to viewer only.

Release jobs use GitHub-hosted Windows runners, NASM, Rust stable and Cargo.lock
(--locked). They run formatting, clippy, tests and a workspace release build. They do
not restore compiled-code caches. The tag's commit must be on main, and its numeric
version must match every workspace package. Supported tag examples are v0.1.0 and
v0.1.0-alpha.1.

The archive is tidedesk-VERSION-windows-x64.zip, containing these files at its root:

- tidedesk.exe (both sides: `tidedesk host`, `tidedesk view`)
- tidedesk-host.exe and tidedesk-view.exe (the same two modes; kept for one release)
- README.txt
- LICENSE
- licenses/: earlier-release AGPL text and third-party notices, generated from
  Cargo.lock by tools/package-third-party-notices.ps1 (index: licenses/third-party/INDEX.txt)

The workflow attaches the ZIP and ZIP.sha256 to the release. Release notes also contain
both executable hashes and a Code signing policy link. Hashes are calculated after
signing. Tags with a prerelease suffix produce a GitHub prerelease.

Packaging checks Windows product/version metadata and rejects stale output directories.
For local packaging, after building with NASM on PATH:

~~~powershell
$env:TIDEDESK_VERSION = '0.1.0-alpha.1'
cargo build --locked --release --workspace
./tools/package-release.ps1 -Version $env:TIDEDESK_VERSION -OutputDirectory target/local-package
~~~

The build embeds this version in both executables. Without TIDEDESK_VERSION it uses
the Cargo package version. Versions have three numeric components and an optional
prerelease suffix; Windows numeric version components must fit into 16 bits.

## Microsoft Store packages

The same release workflow can prepare a separate MSIX submission candidate.
See [Store setup, variables and validation](microsoft-store.md). Store signing
applies to that package, not to the portable ZIP. No Store submission or
publication is automated.

## Optional executable signing

The personal-use-only license is not eligible for SignPath Foundation's free
open-source program. The optional integration below is provider tooling, not
proof of eligibility or a signing entitlement. Leave it disabled until the
maintainer has arranged an appropriate service and verified its configuration.

Before approval, leave SIGNPATH_ENABLED unset or false. The workflow publishes
explicitly unsigned builds without requiring a token.

Under GitHub repository Settings > Secrets and variables > Actions, configure:

| Type | Name | Value |
| --- | --- | --- |
| Secret | SIGNPATH_API_TOKEN | Token for the CI submitter authorized by the signing policy |
| Variable | SIGNPATH_ENABLED | true only when production signing is approved and ready |
| Variable | SIGNPATH_ORGANIZATION_ID | Organization ID supplied by SignPath |
| Variable | SIGNPATH_PROJECT_SLUG | Exact project slug in SignPath |
| Variable | SIGNPATH_SIGNING_POLICY_SLUG | Exact production signing-policy slug |

Use .signpath/artifact-configuration.xml as the project's default artifact configuration.
It expects the GitHub artifact ZIP containing exactly the two root-level executables,
not a ZIP nested inside another ZIP. The workflow supplies its version parameter.

The official SignPath action is pinned to the reviewed v3 commit. The unsigned artifact
ID comes from upload-artifact, and signed files are downloaded into target/signed.
Those files, not the original build outputs, are packaged. Each must have a valid
timestamped Authenticode signature. A self-signed test policy will not pass this
production verification on a runner that does not trust that certificate.

Once enabled, missing settings, denied or timed-out signing, invalid signatures and
missing signed files all stop publication. There is no automatic unsigned fallback.
The signing action waits up to one hour for approval; the build job has a 90-minute limit.
If approval times out, investigate the request and rerun only when appropriate.

The build job has read access; only the separate publication job receives contents:write.
The standard GitHub token creates the release; no additional GitHub personal token is needed.
Existing releases are not overwritten. Never move or recreate a published tag to repair
a release; choose a new version after review.

## Repository safeguards

Private planning belongs in the locally excluded .private/ directory. Verify the
.git/info/exclude entry in every checkout; local excludes are not transferred by clone.
Build outputs, executable files, ZIPs and signing key containers are ignored.

Before any approved push, inspect git status, git diff and the outgoing commits for
private planning, credentials and binaries. Use Cristian Girlea
<cristiangirlea@gmail.com> as the local commit author.

The repository's GitHub settings also need owner-managed main-branch and release-tag
rulesets: restrict release-tag creation/update/deletion, block force pushes and deletion
on main, and require the Windows CI check. Configure review rules that fit the actual
maintainer team. Do not require a second person's approval if no second reviewer exists.

## References

- [Official SignPath GitHub integration and inputs](https://docs.signpath.io/trusted-build-systems/github)
- [Exact v3 action definition used here](https://github.com/SignPath/github-action-submit-signing-request/blob/f6d04783b4569d051e0c80105fe66e82819d0092/action.yml)
- [Artifact configuration examples](https://docs.signpath.io/artifact-configuration/examples)
- [Code signing policy](code-signing-policy.md)
