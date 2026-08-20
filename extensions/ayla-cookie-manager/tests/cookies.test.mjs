import assert from "node:assert/strict";
import { AsyncLocalStorage } from "node:async_hooks";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { createChromeMock, installNavigatorLocksMock, mockCookieId } from "./chrome-mock.mjs";

let moduleNumber = 0;

function syntheticCookie(overrides = {}) {
  return {
    domain: "example.test",
    hostOnly: true,
    httpOnly: true,
    name: "synthetic-session",
    path: "/",
    sameSite: "lax",
    secure: true,
    session: true,
    storeId: "0",
    value: "synthetic-value",
    ...overrides
  };
}

async function setup(options = {}) {
  const mock = createChromeMock(options);
  globalThis.chrome = mock.api;
  const module = await import(`../lib/cookies.js?test=${moduleNumber += 1}`);
  return { ...mock, module };
}

function installChromeContextRouter() {
  const previousDescriptor = Object.getOwnPropertyDescriptor(globalThis, "chrome");
  const contexts = new AsyncLocalStorage();
  Object.defineProperty(globalThis, "chrome", {
    configurable: true,
    get() { return contexts.getStore(); }
  });
  return {
    run(api, operation) { return contexts.run(api, operation); },
    restore() {
      if (previousDescriptor) Object.defineProperty(globalThis, "chrome", previousDescriptor);
      else delete globalThis.chrome;
    }
  };
}

test("purga snapshots legados do storage.local e reidrata somente do cookie jar", { concurrency: false }, async () => {
  const live = syntheticCookie({ value: "live-synthetic-value" });
  const legacy = syntheticCookie({ value: "legacy-synthetic-value" });
  const { control, localData, sessionData, module } = await setup({
    cookies: [live],
    local: { protectedCookies: [{ id: "untrusted-legacy-id", cookie: legacy }] }
  });

  const protectedRecords = await module.getProtectedCookies();

  assert.equal(protectedRecords.length, 1);
  assert.equal(protectedRecords[0].cookie.value, live.value);
  assert.equal(Object.hasOwn(localData, "protectedCookies"), false);
  assert.doesNotMatch(JSON.stringify(localData), /legacy-synthetic-value|live-synthetic-value|"value"/);
  assert.match(JSON.stringify(sessionData), /live-synthetic-value/);
  assert.deepEqual(control.accessLevels, { local: "TRUSTED_CONTEXTS", session: "TRUSTED_CONTEXTS" });
});

test("proteção anônima usa somente storage.session separado por contexto", { concurrency: false }, async () => {
  const incognitoCookie = syntheticCookie({ storeId: "1", value: "private-synthetic-value" });
  const { localData, sessionData, module } = await setup({ incognito: true, cookies: [incognitoCookie] });

  await module.protectCookies([incognitoCookie]);

  assert.doesNotMatch(JSON.stringify(localData), /private-synthetic-value|"value"/);
  assert.equal(localData.protectedCookieMetadataV2.records.length, 0);
  assert.match(JSON.stringify(sessionData["protectedCookieSnapshotsV2:incognito"]), /private-synthetic-value/);
  assert.equal(sessionData["protectedCookieSnapshotsV2:regular"], undefined);

  await module.purgeIncognitoProtectedStorage();
  assert.equal(sessionData["protectedCookieSnapshotsV2:incognito"], undefined);
});

test("protege e desprotege lotes com um único RMW por área", { concurrency: false }, async () => {
  const cookies = [
    syntheticCookie({ name: "one" }),
    syntheticCookie({ name: "two" }),
    syntheticCookie({ name: "three" })
  ];
  const { control, module } = await setup({ cookies });
  await module.getProtectedCookies();
  control.storageSetCount.local = 0;
  control.storageSetCount.session = 0;

  const protectedRecords = await module.protectCookies(cookies);
  assert.equal(protectedRecords.length, 3);
  assert.deepEqual(control.storageSetCount, { local: 1, session: 1 });

  control.storageSetCount.local = 0;
  control.storageSetCount.session = 0;
  const remaining = await module.unprotectCookies(cookies.slice(0, 2));
  assert.deepEqual(remaining.map((record) => record.cookie.name), ["three"]);
  assert.deepEqual(control.storageSetCount, { local: 1, session: 1 });
});

test("cache evita reler storage em eventos repetidos de cookie", { concurrency: false }, async () => {
  const cookie = syntheticCookie();
  const { control, module } = await setup({ cookies: [cookie] });
  await module.protectCookies([cookie]);
  control.storageGetCount.local = 0;
  control.storageGetCount.session = 0;

  for (let index = 0; index < 20; index += 1) {
    const records = await module.getProtectedCookies();
    assert.deepEqual(records.map((record) => record.id), [module.cookieId(cookie)]);
  }

  assert.deepEqual(control.storageGetCount, { local: 0, session: 0 });
});

test("storage.onChanged sincroniza caches de duas janelas sem nova leitura", { concurrency: false }, async () => {
  const cookie = syntheticCookie();
  const mock = createChromeMock({ cookies: [cookie] });
  globalThis.chrome = mock.api;
  const firstWindow = await import(`../lib/cookies.js?cache-window=${moduleNumber += 1}`);
  const secondWindow = await import(`../lib/cookies.js?cache-window=${moduleNumber += 1}`);
  await secondWindow.getProtectedCookies();
  await firstWindow.protectCookies([cookie]);
  const readsAfterProtection = { ...mock.control.storageGetCount };

  const synchronized = await secondWindow.getProtectedCookies();
  assert.deepEqual(synchronized.map((record) => record.id), [secondWindow.cookieId(cookie)]);
  assert.deepEqual(mock.control.storageGetCount, readsAfterProtection);

  await firstWindow.unprotectCookies([cookie]);
  const readsAfterRemoval = { ...mock.control.storageGetCount };
  assert.deepEqual(await secondWindow.getProtectedCookies(), []);
  assert.deepEqual(mock.control.storageGetCount, readsAfterRemoval);
});

test("listener regular ignora snapshots da sessão anônima", { concurrency: false }, async () => {
  const privateCookie = syntheticCookie({ storeId: "1", value: "private-cache-value" });
  const { api, control, module } = await setup();
  await module.getProtectedCookies();
  control.storageGetCount.local = 0;
  control.storageGetCount.session = 0;

  await api.storage.session.set({
    "protectedCookieSnapshotsV2:incognito": [{ id: module.cookieId(privateCookie), cookie: privateCookie }]
  });
  const regularRecords = await module.getProtectedCookies();

  assert.deepEqual(regularRecords, []);
  assert.deepEqual(control.storageGetCount, { local: 0, session: 0 });
});

test("mudança divergente de metadados locais invalida o cache regular", { concurrency: false }, async () => {
  const cookie = syntheticCookie();
  const { api, control, module } = await setup({ cookies: [cookie] });
  await module.protectCookies([cookie]);
  control.storageGetCount.local = 0;
  control.storageGetCount.session = 0;

  await api.storage.local.set({
    protectedCookieMetadataV2: { version: 2, records: [] }
  });
  const records = await module.getProtectedCookies();

  assert.deepEqual(records.map((record) => record.id), [module.cookieId(cookie)]);
  assert.ok(control.storageGetCount.local > 0);
  assert.ok(control.storageGetCount.session > 0);
});

test("colisão no destino é explícita e não altera nenhum cookie", { concurrency: false }, async () => {
  const original = syntheticCookie();
  const replacement = syntheticCookie({ domain: "destination.test", value: "replacement" });
  const collision = syntheticCookie({ domain: "destination.test", value: "existing" });
  const { control, jar, module } = await setup({ cookies: [original, collision] });

  await assert.rejects(
    module.replaceCookie(original, replacement),
    (error) => error.code === "COOKIE_DESTINATION_EXISTS"
  );
  assert.equal(control.operations.length, 0);
  assert.equal(jar.find((cookie) => mockCookieId(cookie) === mockCookieId(collision)).value, "existing");
  assert.equal(jar.find((cookie) => mockCookieId(cookie) === mockCookieId(original)).value, original.value);
});

test("falha ao gravar o destino preserva o original", { concurrency: false }, async () => {
  const original = syntheticCookie();
  const replacement = syntheticCookie({ domain: "destination.test", value: "replacement" });
  const { control, jar, module } = await setup({ cookies: [original] });
  control.failNextCookieSet = 1;

  await assert.rejects(
    module.replaceCookie(original, replacement),
    (error) => error.code === "COOKIE_TRANSACTION_REVERTED"
  );
  assert.equal(jar.length, 1);
  assert.equal(jar[0].value, original.value);
  assert.equal(jar[0].domain, original.domain);
});

test("falha ao remover a origem reverte cookie e proteção", { concurrency: false }, async () => {
  const original = syntheticCookie();
  const replacement = syntheticCookie({ domain: "destination.test", value: "replacement" });
  const { control, jar, module } = await setup({ cookies: [original] });
  await module.protectCookies([original]);
  control.blockRemoveIds.add(mockCookieId(original));

  await assert.rejects(
    module.replaceCookie(original, replacement, true),
    (error) => error.code === "COOKIE_TRANSACTION_REVERTED"
  );
  assert.equal(jar.some((cookie) => mockCookieId(cookie) === mockCookieId(replacement)), false);
  assert.equal(jar.find((cookie) => mockCookieId(cookie) === mockCookieId(original)).value, original.value);
  const protectedRecords = await module.getProtectedCookies();
  assert.deepEqual(protectedRecords.map((record) => record.id), [module.cookieId(original)]);
});

test("falha de quota ao mover proteção reverte o destino", { concurrency: false }, async () => {
  const original = syntheticCookie();
  const replacement = syntheticCookie({ domain: "destination.test", value: "replacement" });
  const { control, jar, module } = await setup({ cookies: [original] });
  await module.protectCookies([original]);
  control.failLocalSetOnce = true;

  await assert.rejects(
    module.replaceCookie(original, replacement, true),
    (error) => error.code === "COOKIE_TRANSACTION_REVERTED"
  );
  assert.equal(jar.some((cookie) => mockCookieId(cookie) === mockCookieId(replacement)), false);
  assert.equal(jar.find((cookie) => mockCookieId(cookie) === mockCookieId(original)).value, original.value);
  assert.deepEqual((await module.getProtectedCookies()).map((record) => record.id), [module.cookieId(original)]);
});

test("substituição bem-sucedida valida o destino antes de remover a origem", { concurrency: false }, async () => {
  const original = syntheticCookie();
  const replacement = syntheticCookie({ domain: "destination.test", value: "replacement" });
  const { control, jar, module } = await setup({ cookies: [original] });

  const saved = await module.replaceCookie(original, replacement);

  const setIndex = control.operations.indexOf(`set:${mockCookieId(replacement)}`);
  const removeIndex = control.operations.indexOf(`remove:${mockCookieId(original)}`);
  assert.ok(setIndex >= 0 && removeIndex > setIndex);
  assert.equal(saved.value, replacement.value);
  assert.deepEqual(jar.map((cookie) => cookie.domain), [replacement.domain]);
});

test("duas janelas não podem substituir origens distintas pelo mesmo destino", { concurrency: false }, async () => {
  const originalA = syntheticCookie({ domain: "source-a.test", name: "source-a" });
  const originalB = syntheticCookie({ domain: "source-b.test", name: "source-b" });
  const replacement = syntheticCookie({ domain: "destination.test", name: "merged", value: "same-target-value" });
  const mock = createChromeMock({ cookies: [originalA, originalB] });
  globalThis.chrome = mock.api;
  const restoreNavigator = installNavigatorLocksMock();

  try {
    const firstWindow = await import(`../lib/cookies.js?window=${moduleNumber += 1}`);
    const secondWindow = await import(`../lib/cookies.js?window=${moduleNumber += 1}`);
    const results = await Promise.allSettled([
      firstWindow.replaceCookie(originalA, replacement),
      secondWindow.replaceCookie(originalB, replacement)
    ]);

    assert.equal(results.filter((result) => result.status === "fulfilled").length, 1);
    const [rejected] = results.filter((result) => result.status === "rejected");
    assert.equal(rejected.reason.code, "COOKIE_DESTINATION_EXISTS");
    assert.equal(mock.jar.filter((cookie) => mockCookieId(cookie) === mockCookieId(replacement)).length, 1);
    assert.equal(mock.jar.filter((cookie) => [mockCookieId(originalA), mockCookieId(originalB)].includes(mockCookieId(cookie))).length, 1);
  } finally {
    restoreNavigator();
  }
});

test("parser Netscape tolera inválidos, preserva HttpOnly e aplica last-wins", { concurrency: false }, async () => {
  const { module } = await setup();
  const text = [
    "# Netscape HTTP Cookie File",
    "#HttpOnly_.example.test\tTRUE\t/\tTRUE\t4102444800\tsession\tfirst-value",
    "example.test\tMAYBE\t/\tTRUE\t0\tbad\tsensitive-invalid-value",
    "#HttpOnly_.example.test\tTRUE\t/\tTRUE\t4102444800\tsession\tlast-value"
  ].join("\n");

  const report = module.parseImportReport(text);

  assert.equal(report.sourceCount, 3);
  assert.equal(report.duplicates, 1);
  assert.equal(report.cookies.length, 1);
  assert.equal(report.cookies[0].httpOnly, true);
  assert.equal(report.cookies[0].hostOnly, false);
  assert.equal(report.cookies[0].domain, ".example.test");
  assert.equal(report.cookies[0].value, "last-value");
  assert.deepEqual(report.invalid, [{ line: 3, message: "Flag de subdomínios inválida." }]);
  assert.doesNotMatch(JSON.stringify(report.invalid), /sensitive-invalid-value/);
  assert.throws(() => module.parseImport(text), /1 cookie\(s\) inválido\(s\)/);
});

test("parser JSON deduplica válidos e não expõe valores rejeitados", { concurrency: false }, async () => {
  const { module } = await setup();
  const report = module.parseImportReport(JSON.stringify([
    syntheticCookie({ value: "json-first" }),
    { name: "missing-domain", value: "json-sensitive-invalid-value" },
    syntheticCookie({ value: "json-last" }),
    syntheticCookie({ name: "insecure-none", secure: false, sameSite: "no_restriction" }),
    syntheticCookie({ name: "insecure-partition", secure: false, partitionKey: { topLevelSite: "https://top.test" } })
  ]));

  assert.equal(report.sourceCount, 5);
  assert.equal(report.duplicates, 1);
  assert.equal(report.cookies.length, 1);
  assert.equal(report.cookies[0].value, "json-last");
  assert.equal(report.invalid.length, 3);
  assert.doesNotMatch(JSON.stringify(report.invalid), /json-sensitive-invalid-value/);
  assert.throws(() => module.parseImport(JSON.stringify([{ name: "missing-domain", value: "secret" }])), /inválido/);
});

test("importação é bounded, deduplica deterministicamente e atualiza proteção", { concurrency: false }, async () => {
  const protectedCookie = syntheticCookie({ name: "cookie-0", value: "protected-old" });
  const { control, jar, module } = await setup({ cookies: [protectedCookie] });
  await module.protectCookies([protectedCookie]);
  control.cookieSetDelayMs = 4;
  control.peakCookieSets = 0;
  const cookies = Array.from({ length: 30 }, (_, index) => syntheticCookie({
    name: `cookie-${index}`,
    value: `value-${index}`
  }));
  cookies.push(syntheticCookie({ name: "cookie-0", value: "protected-last" }));

  const result = await module.importCookies(cookies);

  assert.deepEqual(result, { imported: 30, failed: 0, duplicates: 1 });
  assert.ok(control.peakCookieSets > 1);
  assert.ok(control.peakCookieSets <= 12);
  assert.equal(jar.find((cookie) => cookie.name === "cookie-0").value, "protected-last");
  const protectedRecords = await module.getProtectedCookies();
  assert.equal(protectedRecords.length, 1);
  assert.equal(protectedRecords[0].cookie.value, "protected-last");
});

test("captura prévia de import grande enumera uma vez por store", { concurrency: false }, async () => {
  const { control, jar, module } = await setup();
  const cookies = Array.from({ length: 10_000 }, (_, index) => syntheticCookie({
    name: `bulk-${index}`,
    storeId: "0",
    value: `value-${index}`
  }));
  control.cookieGetAllCount = 0;
  control.cookieGetAllQueries.length = 0;

  const result = await module.importCookies(cookies);

  assert.deepEqual(result, { imported: 10_000, failed: 0, duplicates: 0 });
  assert.equal(jar.length, 10_000);
  assert.equal(control.cookieGetAllCount, 1);
  assert.deepEqual(control.cookieGetAllQueries, [{ storeId: "0", partitionKey: {} }]);
});

test("falha de metadata reverte import misto inteiro ao estado anterior", { concurrency: false }, async () => {
  const protectedCookie = syntheticCookie({ name: "protected", value: "protected-old" });
  const { control, jar, localData, sessionData, module } = await setup({ cookies: [protectedCookie] });
  await module.protectCookies([protectedCookie]);
  const previousJar = structuredClone(jar);
  const previousMetadata = structuredClone(localData.protectedCookieMetadataV2);
  const previousSnapshots = structuredClone(sessionData["protectedCookieSnapshotsV2:regular"]);
  control.cookieSetDelayMs = 4;
  control.peakCookieSets = 0;
  control.cookieGetAllCount = 0;
  control.cookieGetAllQueries.length = 0;
  control.failLocalSetOnce = true;

  await assert.rejects(
    module.importCookies([
      syntheticCookie({ name: "protected", value: "protected-new" }),
      syntheticCookie({ name: "brand-new", value: "brand-new-value", storeId: "2" })
    ]),
    (error) => error.code === "COOKIE_TRANSACTION_REVERTED" && error.rollbackErrors.length === 0
  );

  assert.ok(control.peakCookieSets >= 2);
  assert.ok(control.peakCookieSets <= 12);
  const captureQueries = control.cookieGetAllQueries.filter((query) => (
    Object.keys(query).length === 2
      && Object.prototype.hasOwnProperty.call(query, "storeId")
      && Object.prototype.hasOwnProperty.call(query, "partitionKey")
  ));
  assert.deepEqual(captureQueries.map((query) => query.storeId).sort(), ["0", "2"]);
  assert.deepEqual(jar, previousJar);
  assert.deepEqual(localData.protectedCookieMetadataV2, previousMetadata);
  assert.deepEqual(sessionData["protectedCookieSnapshotsV2:regular"], previousSnapshots);
  assert.deepEqual(await module.getProtectedCookies(), previousSnapshots);
});

test("getAllCookies e deleteAll incluem CHIPS, protegidos e limpam proteção", { concurrency: false }, async () => {
  const regular = syntheticCookie({ name: "regular" });
  const partitioned = syntheticCookie({
    name: "partitioned",
    partitionKey: { topLevelSite: "https://top.test", hasCrossSiteAncestor: true }
  });
  const { jar, localData, sessionData, module } = await setup({ cookies: [regular, partitioned] });
  assert.deepEqual((await chrome.cookies.getAll({})).map((cookie) => cookie.name), ["regular"]);
  assert.deepEqual((await module.getAllCookies()).map((cookie) => cookie.name), ["regular", "partitioned"]);
  await module.protectCookies([regular]);

  const result = await module.deleteAllCookies();

  assert.deepEqual(result, {
    total: 2,
    removed: 2,
    failed: 0,
    remaining: 0,
    protectionDisabled: true
  });
  assert.deepEqual(jar, []);
  assert.equal(localData[module.settingsStorageKey()].protectAgainstExternalDeletion, false);
  assert.deepEqual(localData.protectedCookieMetadataV2.records, []);
  assert.deepEqual(sessionData["protectedCookieSnapshotsV2:regular"], []);
  assert.deepEqual(await module.getProtectedCookies(), []);
});

test("deleteAll relata falha de remoção e cookie remanescente", { concurrency: false }, async () => {
  const removable = syntheticCookie({ name: "removable" });
  const blocked = syntheticCookie({ name: "blocked" });
  const { control, jar, module } = await setup({ cookies: [removable, blocked] });
  control.blockRemoveIds.add(mockCookieId(blocked));

  const result = await module.deleteAllCookies();

  assert.deepEqual(result, {
    total: 2,
    removed: 1,
    failed: 1,
    remaining: 1,
    protectionDisabled: true
  });
  assert.deepEqual(jar.map((cookie) => cookie.name), ["blocked"]);
});

test("removeUnprotectedCookies lê proteção dentro do lock entre janelas", { concurrency: false }, async () => {
  const removable = syntheticCookie({ name: "removable" });
  const newlyProtected = syntheticCookie({ name: "newly-protected" });
  const mock = createChromeMock({ cookies: [removable, newlyProtected] });
  globalThis.chrome = mock.api;
  const restoreNavigator = installNavigatorLocksMock();
  try {
    const firstWindow = await import(`../lib/cookies.js?remove-window=${moduleNumber += 1}`);
    const secondWindow = await import(`../lib/cookies.js?protect-window=${moduleNumber += 1}`);

    const [, result] = await Promise.all([
      secondWindow.protectCookies([newlyProtected]),
      firstWindow.removeUnprotectedCookies([removable, newlyProtected])
    ]);

    assert.deepEqual(result, { removed: 1, skipped: 1, failed: 0 });
    assert.deepEqual(mock.jar.map((cookie) => cookie.name), ["newly-protected"]);
    assert.deepEqual(
      (await firstWindow.getProtectedCookies()).map((record) => record.cookie.name),
      ["newly-protected"]
    );
  } finally {
    restoreNavigator();
  }
});

test("restore protegido revalida estado depois de unprotect e deleteAll", { concurrency: false }, async () => {
  const cookie = syntheticCookie();
  const mock = createChromeMock({ cookies: [cookie] });
  globalThis.chrome = mock.api;
  const restoreNavigator = installNavigatorLocksMock();
  try {
    const manager = await import(`../lib/cookies.js?manager-race=${moduleNumber += 1}`);
    const background = await import(`../lib/cookies.js?background-race=${moduleNumber += 1}`);
    await manager.protectCookies([cookie]);
    await manager.removeCookie(cookie);

    const [, restoredAfterUnprotect] = await Promise.all([
      manager.unprotectCookies([cookie]),
      background.restoreProtectedCookieIfEnabled(cookie)
    ]);
    assert.equal(restoredAfterUnprotect, false);
    assert.deepEqual(mock.jar, []);

    await manager.setCookie(cookie);
    await manager.protectCookies([cookie]);
    await manager.removeCookie(cookie);
    const [deleted, restoredAfterDeleteAll] = await Promise.all([
      manager.deleteAllCookies(),
      background.restoreProtectedCookieIfEnabled(cookie)
    ]);
    assert.equal(deleted.protectionDisabled, true);
    assert.equal(restoredAfterDeleteAll, false);
    assert.deepEqual(mock.jar, []);
    assert.deepEqual(await manager.getProtectedCookies(), []);
  } finally {
    restoreNavigator();
  }
});

test("settings patch RMW entre janelas não reativa proteção desabilitada", { concurrency: false }, async () => {
  const mock = createChromeMock();
  globalThis.chrome = mock.api;
  const restoreNavigator = installNavigatorLocksMock();
  try {
    const firstWindow = await import(`../lib/cookies.js?settings-a=${moduleNumber += 1}`);
    const secondWindow = await import(`../lib/cookies.js?settings-b=${moduleNumber += 1}`);
    const [disabled, autoClear] = await Promise.all([
      firstWindow.saveSettingsPatch({ protectAgainstExternalDeletion: false }),
      secondWindow.saveSettingsPatch({ autoClearOnStartup: true, ignoredSetting: true })
    ]);

    assert.equal(disabled.protectAgainstExternalDeletion, false);
    assert.equal(autoClear.autoClearOnStartup, true);
    assert.equal(autoClear.protectAgainstExternalDeletion, false);
    assert.equal(Object.hasOwn(mock.localData[firstWindow.settingsStorageKey()], "ignoredSetting"), false);
    await assert.rejects(
      firstWindow.saveSettingsPatch({ confirmDestructiveActions: "yes" }),
      /deve ser booleana/
    );
  } finally {
    restoreNavigator();
  }
});

test("settings migram e permanecem independentes entre regular e anônimo", { concurrency: false }, async () => {
  const sharedLocalData = {
    settings: {
      autoClearOnStartup: false,
      protectAgainstExternalDeletion: true,
      confirmDestructiveActions: false
    }
  };
  const regularCookie = syntheticCookie({ name: "regular-context" });
  const incognitoCookie = syntheticCookie({ name: "incognito-context", storeId: "1" });
  const regularMock = createChromeMock({ cookies: [regularCookie], sharedLocalData });
  const incognitoMock = createChromeMock({ incognito: true, cookies: [incognitoCookie], sharedLocalData });
  const router = installChromeContextRouter();
  try {
    const regular = await import(`../lib/cookies.js?settings-regular=${moduleNumber += 1}`);
    const incognito = await import(`../lib/cookies.js?settings-incognito=${moduleNumber += 1}`);
    assert.equal(router.run(regularMock.api, () => regular.settingsStorageKey()), "settingsV2:regular");
    assert.equal(router.run(incognitoMock.api, () => incognito.settingsStorageKey()), "settingsV2:incognito");

    const [regularFirst, incognitoFirst] = await Promise.all([
      router.run(regularMock.api, () => regular.saveSettingsPatch({ autoClearOnStartup: true })),
      router.run(incognitoMock.api, () => incognito.saveSettingsPatch({ confirmDestructiveActions: true }))
    ]);
    assert.deepEqual(regularFirst, {
      autoClearOnStartup: true,
      protectAgainstExternalDeletion: true,
      confirmDestructiveActions: false
    });
    assert.deepEqual(incognitoFirst, {
      autoClearOnStartup: false,
      protectAgainstExternalDeletion: true,
      confirmDestructiveActions: true
    });
    assert.equal(sharedLocalData.settings, undefined);

    const [deleted] = await Promise.all([
      router.run(incognitoMock.api, () => incognito.deleteAllCookies()),
      router.run(regularMock.api, () => regular.saveSettingsPatch({ confirmDestructiveActions: true }))
    ]);
    assert.equal(deleted.protectionDisabled, true);
    assert.deepEqual(incognitoMock.jar, []);
    assert.deepEqual(regularMock.jar, [regularCookie]);

    const [regularFinal, incognitoFinal] = await Promise.all([
      router.run(regularMock.api, () => regular.getSettings()),
      router.run(incognitoMock.api, () => incognito.getSettings())
    ]);
    assert.deepEqual(regularFinal, {
      autoClearOnStartup: true,
      protectAgainstExternalDeletion: true,
      confirmDestructiveActions: true
    });
    assert.deepEqual(incognitoFinal, {
      autoClearOnStartup: false,
      protectAgainstExternalDeletion: false,
      confirmDestructiveActions: true
    });
    assert.deepEqual(sharedLocalData["settingsV2:regular"], regularFinal);
    assert.deepEqual(sharedLocalData["settingsV2:incognito"], incognitoFinal);
  } finally {
    router.restore();
  }
});

test("import Netscape resolve store anônimo antes de dedupe e proteção", { concurrency: false }, async () => {
  const original = syntheticCookie({ storeId: "1", value: "incognito-old" });
  const { jar, module } = await setup({ incognito: true, cookies: [original] });
  await module.protectCookies([original]);
  const report = module.parseImportReport(
    "#HttpOnly_example.test\tFALSE\t/\tTRUE\t0\tsynthetic-session\tincognito-new"
  );
  assert.equal(report.cookies[0].storeId, undefined);

  const resolved = await module.resolveImportCookies(report.cookies);
  assert.equal(resolved[0].storeId, "1");
  const result = await module.importCookies(report.cookies);

  assert.deepEqual(result, { imported: 1, failed: 0, duplicates: 0 });
  assert.equal(jar.length, 1);
  assert.equal(jar[0].storeId, "1");
  assert.equal(jar[0].value, "incognito-new");
  assert.equal((await module.getProtectedCookies())[0].cookie.value, "incognito-new");
});

test("export Netscape recusa CHIPS sem gerar roundtrip destrutivo", { concurrency: false }, async () => {
  const { module } = await setup();
  const regular = syntheticCookie({ name: "regular" });
  const partitioned = syntheticCookie({
    name: "chips",
    partitionKey: { topLevelSite: "https://top.test" }
  });

  assert.match(module.exportNetscape([regular]), /\tregular\tsynthetic-value/);
  assert.throws(
    () => module.exportNetscape([regular, partitioned]),
    /particionados.*use JSON/i
  );
});

test("removeCookies conta retorno undefined como falha", { concurrency: false }, async () => {
  const cookie = syntheticCookie();
  const { control, jar, module } = await setup({ cookies: [cookie] });
  control.blockRemoveIds.add(mockCookieId(cookie));

  const result = await module.removeCookies([cookie]);

  assert.deepEqual(result, { removed: 0, skipped: 0, failed: 1 });
  assert.equal(jar.length, 1);
});

test("HTML expõe controles nomeados, ações separadas e confirmações acessíveis", { concurrency: false }, async () => {
  const managerHtml = await readFile(new URL("../manager.html", import.meta.url), "utf8");
  const popupHtml = await readFile(new URL("../popup.html", import.meta.url), "utf8");
  const managerJs = await readFile(new URL("../manager.js", import.meta.url), "utf8");

  assert.match(managerHtml, /<html lang="pt-BR">/);
  assert.match(popupHtml, /<html lang="pt-BR">/);
  assert.match(managerHtml, /id="protect-selected"/);
  assert.match(managerHtml, /id="unprotect-selected"/);
  assert.match(managerHtml, /aria-labelledby="setting-protection-label"/);
  assert.match(managerHtml, /id="confirm-dialog"[^>]+aria-labelledby/);
  assert.match(popupHtml, /id="confirm-dialog"[^>]+aria-labelledby/);
  assert.doesNotMatch(managerJs, /Promise\.all\(selected\.map/);
  assert.match(managerJs, /state\.selected\.clear\(\);\s*renderSummary\(\);\s*renderRows\(\);/);
});
