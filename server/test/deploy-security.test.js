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

test("the Forge template stays outside GitHub's executable workflow directory", async () => {
  // O template do Forge continua desabilitado. A pre-condicao que docs/forge-build.md
  // exige para reabilita-lo e sobre o RUNNER SELF-HOSTED, e ela nao foi atendida:
  // "keep the runner group scoped only to this repository".
  const workflowsDirectory = new URL("../../.github/workflows/", import.meta.url);
  const activeWorkflows = await readdir(workflowsDirectory).catch((error) => {
    if (error?.code === "ENOENT") return [];
    throw error;
  });
  assert.ok(
    !activeWorkflows.includes("forge-build.yml"),
    "forge-build.yml exige runner self-hosted e nao pode virar workflow ativo",
  );

  const template = await readFile(
    new URL("../../.github/disabled-workflows/forge-build.yml", import.meta.url),
    "utf8",
  );
  assert.match(template, /github\.ref == 'refs\/heads\/main'/);
  assert.match(template, /github\.ref_protected/);
  assert.match(template, /ref: \$\{\{ github\.sha \}\}/);
  assert.match(template, /persist-credentials: false/);
});

test("every active workflow is ephemeral, SHA-pinned and scoped to this repository", async () => {
  // Este teste substitui o antigo `assert.deepEqual(activeWorkflows, [])`.
  //
  // Aquele assert existia porque a unica automacao prevista era o forge-build,
  // que aloca um runner SELF-HOSTED persistente; docs/forge-build.md e explicito
  // sobre o risco ("Anyone able to alter workflow policy or administer the runner
  // remains outside what repository files can prevent") e lista a pre-condicao:
  // runner isolado/efemero, main protegido, dispatch restrito.
  //
  // O release.yml usa runner hospedado pela GitHub, que e efemero por construcao,
  // entao a condicao de isolamento passa a ser satisfeita pelo proprio provedor.
  // O que este arquivo AINDA pode verificar sao as propriedades que dependem do
  // YAML - e e isso que os asserts abaixo travam. As demais (main protegido,
  // dispatch restrito a mantenedores) sao controles externos e continuam sendo
  // responsabilidade do administrador do repositorio.
  const workflowsDirectory = new URL("../../.github/workflows/", import.meta.url);
  const activeWorkflows = await readdir(workflowsDirectory).catch((error) => {
    if (error?.code === "ENOENT") return [];
    throw error;
  });

  for (const name of activeWorkflows) {
    const workflow = await readFile(new URL(name, workflowsDirectory), "utf8");
    const where = `.github/workflows/${name}`;

    // Nenhum runner self-hosted: e o risco que motivou a proibicao original.
    assert.doesNotMatch(workflow, /runs-on:\s*\[?[^\n]*self-hosted/, `${where} usa runner self-hosted`);

    // Escopo ao repositorio. O contexto github.repository preserva a caixa real
    // do slug, entao a comparacao e sensivel a maiusculas.
    assert.match(workflow, /github\.repository == 'sf0rzin\/ayla'/, `${where} nao esta escopado a sf0rzin/ayla`);

    // Checkout do SHA exato e sem deixar credencial na arvore.
    assert.match(workflow, /ref: \$\{\{ (needs\.[a-z]+\.outputs\.sha|github\.sha) \}\}/, `${where} nao fixa o SHA no checkout`);
    assert.match(workflow, /persist-credentials: false/, `${where} nao desliga persist-credentials`);

    // Actions de terceiros fixadas por SHA, nao por tag movel.
    for (const [, ref] of workflow.matchAll(/^\s*uses:\s*([^\s#]+)/gm)) {
      if (ref.startsWith("./")) continue;
      assert.match(ref, /@[0-9a-f]{40}$/, `${where} usa ${ref} sem fixar SHA`);
    }
  }
});

test("the signing key is only reachable from a gated job that runs no dependency code", async () => {
  // A chave de assinatura do updater instala software na maquina de todo usuario
  // com installMode passive. Duas propriedades sustentam o isolamento dela, e as
  // duas vivem no YAML:
  //   1. o job que ve o segredo declara um Environment (onde se exige revisor);
  //   2. esse job nao roda script de dependencia (npm ci --ignore-scripts), porque
  //      um postinstall pode escrever em $GITHUB_ENV e o runner aplica isso aos
  //      passos seguintes - inclusive ao passo que assina.
  const release = await readFile(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  ).catch((error) => {
    if (error?.code === "ENOENT") return null;
    throw error;
  });
  if (release === null) return; // sem pipeline de release, nada a verificar

  assert.match(release, /environment:\s*release-signing/);
  assert.match(release, /TAURI_SIGNING_PRIVATE_KEY/);
  assert.match(release, /npm ci --ignore-scripts/);

  // A validacao pesada (npm ci com scripts, npm run check) fica num job proprio,
  // sem environment e portanto sem acesso aos secrets daquele environment.
  assert.match(release, /^\s{2}validate:/m, "o job validate precisa existir e ser separado do build");

  // O manifesto tem de sair com a URL publica final: o agente da VM recusa
  // qualquer outra e o canal nunca avancaria.
  assert.match(release, /https:\/\/yl\.xyne\.gg\/updates\/releases\//);
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
