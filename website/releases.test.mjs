import assert from "node:assert/strict";
import { test } from "node:test";
import { selectRelease, loadRelease, applyRelease } from "./dist/releases.mjs";
import { readFile } from "node:fs/promises";

function release(tag, date, prerelease = true) {
  const name = "tidedesk-" + tag.slice(1) + "-windows-x64.zip";
  const url = "https://github.com/cristiangirlea/tidedesk/releases/download/" + tag + "/" + name;
  return { tag_name: tag, published_at: date, prerelease, draft: false, assets: [
    { name, browser_download_url: url, size: 100, state: "uploaded" },
    { name: name + ".sha256", browser_download_url: url + ".sha256", size: 100, state: "uploaded" },
  ] };
}

test("selects newest published complete Windows release, including alpha", () => {
  const older = release("v0.1.0", "2026-01-01T00:00:00Z", false);
  const newer = release("v0.2.0-alpha.1", "2026-02-01T00:00:00Z");
  assert.equal(selectRelease([older, newer]).version, newer.tag_name);
});
test("ignores drafts, incomplete assets, invalid dates and foreign URLs", () => {
  const good = release("v0.1.0-alpha.2", "2026-01-01T00:00:00Z");
  const draft = { ...release("v0.2.0", "2026-02-01T00:00:00Z"), draft: true };
  const incomplete = release("v0.3.0", "2026-03-01T00:00:00Z"); incomplete.assets.pop();
  const foreign = release("v0.4.0", "2026-04-01T00:00:00Z"); foreign.assets[0].browser_download_url = "https://example.com/download.zip";
  const invalid = release("v0.5.0", "invalid");
  assert.equal(selectRelease([draft, incomplete, foreign, invalid, good]).version, good.tag_name);
  assert.equal(selectRelease({}), null);
  assert.equal(selectRelease([]), null);
});
test("API failures are surfaced for the Releases-page fallback", async () => {
  await assert.rejects(loadRelease(async () => ({ ok: false })));
  await assert.rejects(loadRelease(async () => ({ ok: true, json: async () => [] })));
  await assert.rejects(loadRelease(async () => { throw new Error("offline"); }));
});

test("page wires dynamic downloads with safe initial links and no pinned binary", async () => {
  const html = await readFile(new URL("./dist/index.html", import.meta.url), "utf8");
  assert.match(html, /<script type="module" src="\/releases\.mjs"><\/script>/);
  for (const attribute of ["data-download", "data-release-notes", "data-checksum"]) {
    assert.ok(html.includes(attribute + ' href="https://github.com/cristiangirlea/tidedesk/releases"'));
  }
  assert.ok(!html.includes("/releases/download/"));
  assert.ok(html.includes("data-release-version"));
});

test("selected release updates download, checksum, notes, version and matching license", () => {
  const selected = selectRelease([release("v0.3.0-alpha.2", "2026-03-01T00:00:00Z")]);
  const elements = Object.fromEntries(["[data-download]", "[data-release-notes]", "[data-checksum]", "[data-release-version]"].map((key) => [key, {}]));
  elements['script[type="application/ld+json"]'] = { textContent: '{"name":"TideDesk"}' };
  applyRelease({ querySelector: (key) => elements[key], querySelectorAll: (key) => [elements[key]] }, selected);
  assert.equal(elements["[data-download]"].href, selected.url);
  assert.equal(elements["[data-checksum]"].href, selected.checksum);
  assert.equal(elements["[data-release-notes]"].href, selected.notes);
  assert.match(elements["[data-release-version]"].textContent, /v0.3.0-alpha.2 · Pre-release/);
  const data = JSON.parse(elements['script[type="application/ld+json"]'].textContent);
  assert.equal(data.softwareVersion, "0.3.0-alpha.2");
  assert.equal(data.downloadUrl, selected.url);
  assert.equal(data.license, "https://github.com/cristiangirlea/tidedesk/blob/v0.3.0-alpha.2/LICENSE");
});

test("malformed release records cannot break release selection", () => {
  const valid = release("v0.1.0-alpha.2", "2026-01-01T00:00:00Z");
  valid.assets.unshift(null);
  assert.equal(selectRelease([null, {}, { ...valid, assets: {} }, valid]).version, valid.tag_name);
});
