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
assert.equal(manifest.name, "Ayla Cookies for Microsoft Edge");
assert.equal(manifest.short_name, "Ayla Edge Cookies");
assert.equal(manifest.version, "0.2.0");
assert.equal(manifest.minimum_chrome_version, "130");
assert.equal(manifest.background.type, "module");
assert.ok(manifest.permissions.includes("cookies"));
assert.ok(manifest.permissions.includes("storage"));
assert.deepEqual(manifest.host_permissions, ["http://*/*", "https://*/*"]);
assert.equal(manifest.host_permissions.includes("<all_urls>"), false);

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

const managerHtml = readFileSync(join(extensionRoot, "manager.html"), "utf8");
const popupHtml = readFileSync(join(extensionRoot, "popup.html"), "utf8");
const backgroundSource = readFileSync(join(extensionRoot, "background.js"), "utf8");
const managerSource = readFileSync(join(extensionRoot, "manager.js"), "utf8");
const managerCss = readFileSync(join(extensionRoot, "manager.css"), "utf8");

assert.match(managerHtml, /id=["']import-open["'][^>]*>\s*Importar TXT\s*</i);
assert.match(managerHtml, /id=["']delete-all["'][^>]*>\s*Apagar tudo\s*</i);
assert.match(managerHtml, /id=["']import-file["'][^>]+accept=["'][^"']*\.txt/i);
assert.match(managerHtml, /<label[^>]+for=["']import-text["']/i);
assert.match(managerHtml, /id=["']export-warning["'][^>]+role=["']status["']/i);
assert.match(popupHtml, /id=["']import-txt["'][^>]*>\s*Importar cookies\.txt\s*</i);
assert.match(popupHtml, /id=["']delete-all["'][^>]*>\s*Apagar todos os cookies do Edge\s*</i);
assert.match(managerSource, /await Promise\.all\(\[/);
assert.match(managerSource, /resolveImportCookies\(report\.cookies\)/);
assert.match(managerSource, /const report = await analyzeImportText\(\)/);
assert.match(managerSource, /changes\[settingsStorageKey\(\)\]/);
assert.match(managerCss, /@media \(max-width: 760px\)/);

// Keep the callback channel explicit instead of depending on Promise-return behavior.
assert.match(backgroundSource, /onMessage\.addListener\(\(message,\s*_sender,\s*sendResponse\)/);
assert.match(backgroundSource, /sendResponse\(\{\s*ok:\s*true\s*\}\)/);
assert.match(backgroundSource, /return true;/);
assert.match(backgroundSource, /message\?\.type\s*!==\s*["']open-manager["']/);
assert.match(backgroundSource, /action\?\.type\s*===\s*["']import["']/);
assert.match(backgroundSource, /action\?\.type\s*===\s*["']delete-site["']/);

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
  partitionKey: { topLevelSite: "https://top.example", hasCrossSiteAncestor: true }
};

assert.equal(normalizeDomain(cookie.domain), "example.com");
assert.equal(cookieUrl(cookie), "https://example.com/app");
assert.match(cookieId(cookie), /session/);
assert.equal(toSetDetails(cookie).domain, ".example.com");
assert.equal(toSetDetails(cookie).partitionKey.topLevelSite, "https://top.example");
assert.equal(toSetDetails(cookie).partitionKey.hasCrossSiteAncestor, true);
assert.deepEqual(parseImport(exportJson([cookie]))[0], cookie);
assert.throws(
  () => exportNetscape([cookie]),
  /Cookies particionados não podem ser exportados em Netscape/
);
const cookieWithoutPartition = { ...cookie };
delete cookieWithoutPartition.partitionKey;
const netscapeCookie = parseImport(exportNetscape([cookieWithoutPartition]))[0];
assert.equal(netscapeCookie.domain, cookie.domain);
assert.equal(netscapeCookie.name, cookie.name);
assert.equal(netscapeCookie.value, cookie.value);
assert.equal(netscapeCookie.httpOnly, true);

console.log("Ayla Cookie Manager checks passed.");
