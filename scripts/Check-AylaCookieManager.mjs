import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  cookieId,
  cookieUrl,
  exportJson,
  exportNetscape,
  normalizeDomain,
  parseImport,
  toSetDetails
} from "../extensions/ayla-cookie-manager/lib/cookies.js";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const extensionRoot = join(projectRoot, "extensions", "ayla-cookie-manager");
const manifest = JSON.parse(readFileSync(join(extensionRoot, "manifest.json"), "utf8"));

assert.equal(manifest.manifest_version, 3);
assert.equal(manifest.background.type, "module");
assert.ok(manifest.permissions.includes("cookies"));
assert.ok(manifest.permissions.includes("storage"));
assert.ok(manifest.host_permissions.includes("<all_urls>"));

for (const path of [
  manifest.background.service_worker,
  manifest.action.default_popup,
  "manager.html",
  "manager.js",
  "popup.js",
  "lib/cookies.js"
]) {
  readFileSync(join(extensionRoot, path));
}

function filesBelow(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    return entry.isDirectory() ? filesBelow(path) : [path];
  });
}

for (const path of filesBelow(extensionRoot).filter((path) => path.endsWith(".js"))) {
  execFileSync(process.execPath, ["--check", path], { stdio: "pipe" });
}

for (const path of filesBelow(extensionRoot).filter((path) => path.endsWith(".html"))) {
  const html = readFileSync(path, "utf8");
  assert.doesNotMatch(html, /<(?:script|link)[^>]+(?:src|href)=["']https?:/i, `${path} loads remote code`);
}

const cookie = {
  domain: ".example.com",
  hostOnly: false,
  name: "session",
  value: "synthetic",
  path: "/app",
  secure: true,
  httpOnly: true,
  sameSite: "lax",
  session: false,
  expirationDate: 2_000_000_000,
  storeId: "0",
  partitionKey: { topLevelSite: "https://top.example" }
};

assert.equal(normalizeDomain(cookie.domain), "example.com");
assert.equal(cookieUrl(cookie), "https://example.com/app");
assert.match(cookieId(cookie), /session/);
assert.equal(toSetDetails(cookie).domain, ".example.com");
assert.equal(toSetDetails(cookie).partitionKey.topLevelSite, "https://top.example");
assert.deepEqual(parseImport(exportJson([cookie]))[0], cookie);
const netscapeCookie = parseImport(exportNetscape([cookie]))[0];
assert.equal(netscapeCookie.domain, cookie.domain);
assert.equal(netscapeCookie.name, cookie.name);
assert.equal(netscapeCookie.value, cookie.value);
assert.equal(netscapeCookie.httpOnly, true);

console.log("Ayla Cookie Manager checks passed.");
