const repository = "https://github.com/cristiangirlea/tidedesk";
const api = "https://api.github.com/repos/cristiangirlea/tidedesk/releases?per_page=100";

// Pre-releases count: GitHub's /releases/latest excludes them.
export function selectRelease(releases) {
  if (!Array.isArray(releases)) return null;
  return releases
    .filter((release) => release && !release.draft && Number.isFinite(Date.parse(release.published_at)) && /^v\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(release.tag_name) && Array.isArray(release.assets))
    .sort((a, b) => Date.parse(b.published_at) - Date.parse(a.published_at))
    .map((release) => {
      const name = "tidedesk-" + release.tag_name.slice(1) + "-windows-x64.zip";
      const expected = repository + "/releases/download/" + release.tag_name + "/" + name;
      const zip = release.assets.find((asset) => asset && asset.name === name && asset.state === "uploaded" && asset.size > 0 && asset.browser_download_url === expected);
      const checksum = release.assets.find((asset) => asset && asset.name === name + ".sha256" && asset.state === "uploaded" && asset.size > 0 && asset.browser_download_url === expected + ".sha256");
      return zip && checksum && Number.isFinite(Date.parse(release.published_at))
        ? { version: release.tag_name, url: expected, checksum: expected + ".sha256", notes: repository + "/releases/tag/" + release.tag_name, prerelease: release.prerelease }
        : null;
    })
    .find(Boolean) ?? null;
}

export async function loadRelease(fetcher = fetch) {
  const response = await fetcher(api, {
    headers: { Accept: "application/vnd.github+json" },
    signal: AbortSignal.timeout(5000),
    cache: "no-store",
  });
  if (!response.ok) throw new Error("Release information unavailable");
  const release = selectRelease(await response.json());
  if (!release) throw new Error("No complete Windows release");
  return release;
}

export function applyRelease(document, release) {
    document.querySelectorAll("[data-download]").forEach((link) => { link.href = release.url; });
    document.querySelector("[data-release-notes]").href = release.notes;
    document.querySelector("[data-checksum]").href = release.checksum;
    document.querySelector("[data-release-version]").textContent =
      release.version + (release.prerelease ? " · Pre-release" : "") + " · Windows x64 · Portable ZIP";
    const metadata = document.querySelector('script[type="application/ld+json"]');
    const data = JSON.parse(metadata.textContent);
    data.softwareVersion = release.version.slice(1);
    data.downloadUrl = release.url;
    data.license = repository + "/blob/" + release.version + "/LICENSE";
    metadata.textContent = JSON.stringify(data);
}

if (typeof document !== "undefined") {
  loadRelease().then((release) => applyRelease(document, release)).catch(() => {
    // The links already point to Releases, never to a silently stale binary.
    document.querySelector("[data-release-version]").textContent =
      "Choose the newest Windows ZIP on GitHub Releases.";
  });
}
