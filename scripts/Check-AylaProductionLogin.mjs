import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";

const assetsDirectory = new URL("../dist/assets/", import.meta.url);
const javascriptAssets = (await readdir(assetsDirectory))
  .filter((name) => name.endsWith(".js"));

assert.ok(javascriptAssets.length > 0, "the production build has no JavaScript asset");

const forbiddenDevelopmentMarkers = [
  "development_login_bypass_session",
  "development-local-test-session",
  "--skip-login",
  "test@local.invalid",
];

for (const asset of javascriptAssets) {
  const contents = await readFile(new URL(asset, assetsDirectory), "utf8");
  for (const marker of forbiddenDevelopmentMarkers) {
    assert.ok(
      !contents.includes(marker),
      `development login marker ${JSON.stringify(marker)} leaked into ${asset}`,
    );
  }
}

console.log("Production login bypass checks passed.");
