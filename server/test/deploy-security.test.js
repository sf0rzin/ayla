import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import test from "node:test";

async function deploymentFile(name) {
  return readFile(new URL(`../../deploy/ayla/${name}`, import.meta.url), "utf8");
}

test("database backups read every payload block, hash, and periodically restore", async () => {
  const script = await deploymentFile("backup-database.sh");
  assert.match(script, /pg_restore --exit-on-error --file \/dev\/null/);
  assert.match(script, /sha256sum --check --status/);
  assert.match(script, /flock -n 9/);
  assert.match(script, /promotion_in_progress/);
  assert.match(script, /createdb --username ayla/);
  assert.match(script, /pg_restore --exit-on-error --no-owner --no-privileges/);
  assert.match(script, /SELECT count\(\*\) FROM public\.users/);
});

test("Ayla reloads only its own nftables table", async () => {
  const rules = await deploymentFile("nftables.conf");
  assert.doesNotMatch(rules, /^\s*flush\s+ruleset\s*$/m);
  assert.match(rules, /^destroy table inet ayla_filter$/m);
  assert.match(rules, /^table inet ayla_filter \{$/m);
});

test("the real Docker build context is allowlisted", async () => {
  const ignore = await readFile(
    new URL("../Dockerfile.dockerignore", import.meta.url),
    "utf8",
  );
  assert.equal(ignore.split(/\r?\n/, 1)[0], "**");
  for (const required of ["!package.json", "!package-lock.json", "!server/package.json", "!server/src/**"]) {
    assert.match(ignore, new RegExp(`^${required.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`, "m"));
  }
});

test("the API uses a reconciled non-superuser PostgreSQL role", async () => {
  const [compose, bootstrap, server] = await Promise.all([
    deploymentFile("compose.yaml"),
    deploymentFile("bootstrap-database.sh"),
    readFile(new URL("../src/server.js", import.meta.url), "utf8"),
  ]);
  assert.match(compose, /database-init:/);
  assert.match(compose, /PGUSER: ayla_app/);
  assert.match(compose, /condition: service_completed_successfully/);
  assert.match(bootstrap, /ALTER ROLE ayla_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE/);
  assert.match(bootstrap, /CREATE TABLE IF NOT EXISTS public\.users/);
  assert.match(bootstrap, /ALTER TABLE public\.users OWNER TO ayla/);
  assert.match(bootstrap, /ALTER TABLE public\.sessions OWNER TO ayla/);
  assert.match(bootstrap, /REVOKE CREATE ON SCHEMA public FROM ayla_app/);
  assert.match(bootstrap, /GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public\.users, public\.sessions TO ayla_app/);
  assert.doesNotMatch(bootstrap, /GRANT\s+(?:USAGE,\s*)?CREATE\s+ON SCHEMA public TO ayla_app/);
  assert.doesNotMatch(server, /\bmigrate\s*\(/);
});

test("Forge workflow stays outside GitHub's executable workflow directory", async () => {
  const workflowsDirectory = new URL("../../.github/workflows/", import.meta.url);
  const activeWorkflows = await readdir(workflowsDirectory).catch((error) => {
    if (error?.code === "ENOENT") return [];
    throw error;
  });
  assert.deepEqual(activeWorkflows, []);

  const template = await readFile(
    new URL("../../.github/disabled-workflows/forge-build.yml", import.meta.url),
    "utf8",
  );
  assert.match(template, /github\.ref == 'refs\/heads\/main'/);
  assert.match(template, /github\.ref_protected/);
  assert.match(template, /ref: \$\{\{ github\.sha \}\}/);
  assert.match(template, /persist-credentials: false/);
});

test("self-hosted publication is locked and uses atomic unique paths", async () => {
  const publisher = await readFile(
    new URL("../../scripts/Publish-AylaSelfHostedRelease.remote.sh", import.meta.url),
    "utf8",
  );
  assert.match(publisher, /flock -x 9/);
  assert.match(publisher, /version_is_greater "\$version" "\$current_version"/);
  assert.match(publisher, /candidate_directory=.*\$nonce/);
  assert.match(publisher, /metadata_temporary=.*\$nonce/);
  assert.match(publisher, /mv -- "\$candidate_directory" "\$release_directory"/);
  assert.doesNotMatch(publisher, /\.latest\.json\.tmp/);
});
