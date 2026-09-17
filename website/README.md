# TideDesk website

Static product page for https://tidedesk.cristiangirlea.ro/, separate from the CV website and its Cloudflare Pages project.

## Deployment

Publish only website/dist to a new, separate Cloudflare Pages project. No build command, dependencies or application binaries are needed. For a Git-connected project, use website as the root directory and dist as the output directory. Select the reviewed production branch.

Add tidedesk.cristiangirlea.ro in that project's Custom domains settings before configuring its DNS record. Do not change the apex or www records, or reuse the CV deployment project.

Official instructions: https://developers.cloudflare.com/pages/configuration/custom-domains/

## Checks and maintenance

- Preview desktop and mobile widths, keyboard focus, FAQ expansion and download links.
- Deploy only dist; this README is not part of the site.
- Keep the visible version, ZIP/checksum URLs and SoftwareApplication metadata aligned.
- The download module selects the newest published complete Windows ZIP plus checksum from the public GitHub Releases API, including pre-releases. GitHub's releases/latest excludes pre-releases.
- Initial/no-JavaScript/API-failure links open GitHub Releases, never a stale binary. Release-specific metadata uses the selected tag's license, not main's newer license.
- Run node --test website/releases.test.mjs before deployment.
- Update signing claims only after signed releases have actually been verified.
- Keep canonical, sitemap and robots URLs consistent with the production domain.
- No analytics scripts, external fonts, runtime dependencies or cookies are added.
- The browser fetches public release metadata from GitHub. This third-party request is disclosed in the privacy section.
- After publication, the domain owner can submit the sitemap and request indexing in Google Search Console. Indexing and ranking are not guaranteed.
