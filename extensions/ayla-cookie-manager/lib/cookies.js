const LEGACY_PROTECTED_KEY = "protectedCookies";
const PROTECTED_METADATA_KEY = "protectedCookieMetadataV2";
const PROTECTED_MIGRATION_KEY = "protectedCookieMigrationV2";
const PROTECTED_SESSION_PREFIX = "protectedCookieSnapshotsV2";
const LEGACY_SETTINGS_KEY = "settings";
const SETTINGS_PREFIX = "settingsV2";
const STORAGE_SCHEMA_VERSION = 2;
const COOKIE_MUTATION_CONCURRENCY = 12;

const queues = new Map();
const volatileSession = new Map();
const protectionCache = new Map();
let storageAccessPromise;
let migrationPromise;
let storageChangeListenerInstalled = false;

export const DEFAULT_SETTINGS = Object.freeze({
  autoClearOnStartup: false,
  protectAgainstExternalDeletion: true,
  confirmDestructiveActions: true
});
const SETTING_KEYS = new Set(Object.keys(DEFAULT_SETTINGS));

export class CookieConflictError extends Error {
  constructor() {
    super("Já existe um cookie no destino. Nada foi alterado.");
    this.name = "CookieConflictError";
    this.code = "COOKIE_DESTINATION_EXISTS";
  }
}

export class CookieTransactionError extends Error {
  constructor(cause, rollbackErrors = []) {
    super(rollbackErrors.length
      ? "Não foi possível concluir a edição e a reversão ficou incompleta. Atualize a lista antes de continuar."
      : "Não foi possível concluir a edição. O cookie original foi restaurado.");
    this.name = "CookieTransactionError";
    this.code = rollbackErrors.length ? "COOKIE_ROLLBACK_INCOMPLETE" : "COOKIE_TRANSACTION_REVERTED";
    this.cause = cause;
    this.rollbackErrors = rollbackErrors;
  }
}

export function normalizeDomain(domain = "") {
  return domain.trim().replace(/^\./, "").toLowerCase();
}

export function cookieId(cookie) {
  const partition = cookie.partitionKey?.topLevelSite ?? "";
  const ancestor = cookie.partitionKey?.hasCrossSiteAncestor ? "1" : "0";
  return JSON.stringify([
    cookie.storeId ?? "0",
    cookie.domain,
    cookie.path,
    cookie.name,
    partition,
    ancestor
  ]);
}

export function cookieUrl(cookie) {
  const scheme = cookie.secure ? "https" : "http";
  const host = normalizeDomain(cookie.domain);
  const path = cookie.path?.startsWith("/") ? cookie.path : "/";
  return `${scheme}://${host}${path}`;
}

export function protectionContext() {
  return chrome.extension?.inIncognitoContext ? "incognito" : "regular";
}

export function settingsStorageKey(context = protectionContext()) {
  if (context !== "regular" && context !== "incognito") {
    throw new TypeError("Contexto de configurações inválido.");
  }
  return `${SETTINGS_PREFIX}:${context}`;
}

function sessionKey(context = protectionContext()) {
  return `${PROTECTED_SESSION_PREFIX}:${context}`;
}

function metadataFingerprint(metadata) {
  return metadata
    .map((item) => `${item.id}\u0000${item.expiresAt ?? "session"}`)
    .sort()
    .join("\u0001");
}

function cacheProtectedRecords(context, records) {
  if (context !== protectionContext()) return;
  protectionCache.set(context, sanitizeProtectedRecords(records));
}

function installStorageChangeListener() {
  if (storageChangeListenerInstalled || !chrome.storage.onChanged?.addListener) return;
  storageChangeListenerInstalled = true;
  const context = protectionContext();
  const protectedSessionKey = sessionKey(context);
  chrome.storage.onChanged.addListener((changes, areaName) => {
    if (areaName === "session" && Object.prototype.hasOwnProperty.call(changes, protectedSessionKey)) {
      cacheProtectedRecords(context, changes[protectedSessionKey].newValue ?? []);
      return;
    }
    if (context !== "regular" || areaName !== "local" || !changes[PROTECTED_METADATA_KEY]) return;
    const cached = protectionCache.get(context);
    if (!cached) return;
    const metadata = sanitizeMetadata(changes[PROTECTED_METADATA_KEY].newValue);
    const cachedMetadata = cached.map(({ cookie }) => metadataFromCookie(cookie));
    if (metadataFingerprint(metadata) !== metadataFingerprint(cachedMetadata)) protectionCache.delete(context);
  });
}

async function withLock(name, operation) {
  const lockName = `ayla-cookie-manager:${name}`;
  if (globalThis.navigator?.locks?.request) {
    return globalThis.navigator.locks.request(lockName, operation);
  }

  const previous = queues.get(lockName) ?? Promise.resolve();
  const result = previous.catch(() => undefined).then(operation);
  queues.set(lockName, result.then(() => undefined, () => undefined));
  return result;
}

async function mapSettledBounded(items, operation, concurrency = COOKIE_MUTATION_CONCURRENCY) {
  const results = new Array(items.length);
  let nextIndex = 0;
  const workerCount = Math.min(items.length, Math.max(1, concurrency));
  const workers = Array.from({ length: workerCount }, async () => {
    while (nextIndex < items.length) {
      const index = nextIndex;
      nextIndex += 1;
      try {
        results[index] = { status: "fulfilled", value: await operation(items[index], index) };
      } catch (reason) {
        results[index] = { status: "rejected", reason };
      }
    }
  });
  await Promise.all(workers);
  return results;
}

function deduplicateCookies(cookies) {
  const unique = new Map();
  let duplicates = 0;
  for (const cookie of cookies) {
    const id = cookieId(cookie);
    if (unique.has(id)) {
      duplicates += 1;
      // Deleting first makes output order follow each identity's last source occurrence.
      unique.delete(id);
    }
    unique.set(id, cookie);
  }
  return { cookies: [...unique.values()], duplicates };
}

export async function restrictStorageAccess() {
  installStorageChangeListener();
  if (!storageAccessPromise) {
    storageAccessPromise = Promise.all([
      chrome.storage.local?.setAccessLevel?.({ accessLevel: "TRUSTED_CONTEXTS" }),
      chrome.storage.session?.setAccessLevel?.({ accessLevel: "TRUSTED_CONTEXTS" })
    ]).catch((error) => {
      storageAccessPromise = undefined;
      throw error;
    });
  }
  await storageAccessPromise;
}

function metadataFromCookie(cookie) {
  return {
    id: cookieId(cookie),
    expiresAt: cookie.session || !Number.isFinite(Number(cookie.expirationDate))
      ? null
      : Number(cookie.expirationDate)
  };
}

function sanitizeMetadata(value) {
  if (!value || value.version !== STORAGE_SCHEMA_VERSION || !Array.isArray(value.records)) return [];
  const records = new Map();
  for (const item of value.records) {
    if (!item || typeof item.id !== "string") continue;
    records.set(item.id, {
      id: item.id,
      expiresAt: item.expiresAt !== null && Number.isFinite(Number(item.expiresAt)) ? Number(item.expiresAt) : null
    });
  }
  return [...records.values()];
}

function legacyMetadata(records) {
  if (!Array.isArray(records)) return [];
  const safe = new Map();
  for (const item of records) {
    const cookie = item?.cookie;
    if (!cookie || typeof cookie !== "object" || typeof cookie.domain !== "string" || typeof cookie.name !== "string") continue;
    const metadata = metadataFromCookie(cookie);
    safe.set(metadata.id, metadata);
  }
  return [...safe.values()];
}

async function migrateProtectedStorage() {
  await restrictStorageAccess();
  if (!migrationPromise) {
    migrationPromise = withLock("migration", async () => {
      const stored = await chrome.storage.local.get([
        LEGACY_PROTECTED_KEY,
        PROTECTED_METADATA_KEY,
        PROTECTED_MIGRATION_KEY
      ]);
      const hasLegacyData = Object.prototype.hasOwnProperty.call(stored, LEGACY_PROTECTED_KEY);
      if (!hasLegacyData && stored[PROTECTED_MIGRATION_KEY] === STORAGE_SCHEMA_VERSION) return;

      const metadata = new Map(sanitizeMetadata(stored[PROTECTED_METADATA_KEY]).map((item) => [item.id, item]));
      for (const item of legacyMetadata(stored[LEGACY_PROTECTED_KEY])) metadata.set(item.id, item);

      // Purge the value-bearing legacy key before writing any migrated metadata.
      if (hasLegacyData) {
        try {
          await chrome.storage.local.remove(LEGACY_PROTECTED_KEY);
        } catch (error) {
          // Overwrite is a secure fallback if a browser-specific remove implementation fails.
          await chrome.storage.local.set({ [LEGACY_PROTECTED_KEY]: [] });
          throw error;
        }
      }

      await chrome.storage.local.set({
        [PROTECTED_METADATA_KEY]: { version: STORAGE_SCHEMA_VERSION, records: [...metadata.values()] },
        [PROTECTED_MIGRATION_KEY]: STORAGE_SCHEMA_VERSION
      });
    }).catch((error) => {
      migrationPromise = undefined;
      throw error;
    });
  }
  await migrationPromise;
}

function sanitizeSettings(value) {
  const candidate = value && typeof value === "object" && !Array.isArray(value) ? value : {};
  return {
    autoClearOnStartup: typeof candidate.autoClearOnStartup === "boolean" ? candidate.autoClearOnStartup : DEFAULT_SETTINGS.autoClearOnStartup,
    protectAgainstExternalDeletion: typeof candidate.protectAgainstExternalDeletion === "boolean" ? candidate.protectAgainstExternalDeletion : DEFAULT_SETTINGS.protectAgainstExternalDeletion,
    confirmDestructiveActions: typeof candidate.confirmDestructiveActions === "boolean" ? candidate.confirmDestructiveActions : DEFAULT_SETTINGS.confirmDestructiveActions
  };
}

async function cleanupLegacySettings() {
  try {
    const regularKey = settingsStorageKey("regular");
    const incognitoKey = settingsStorageKey("incognito");
    const stored = await chrome.storage.local.get([regularKey, incognitoKey]);
    if (Object.prototype.hasOwnProperty.call(stored, regularKey)
      && Object.prototype.hasOwnProperty.call(stored, incognitoKey)) {
      await chrome.storage.local.remove(LEGACY_SETTINGS_KEY);
    }
  } catch {
    // The legacy value is inert once both context keys exist. Cleanup is best effort
    // so a failed remove never turns a successfully persisted setting into ambiguity.
  }
}

async function readSettingsForContext(context, migrate) {
  const key = settingsStorageKey(context);
  const stored = await chrome.storage.local.get([key, LEGACY_SETTINGS_KEY]);
  if (Object.prototype.hasOwnProperty.call(stored, key)) return sanitizeSettings(stored[key]);

  const settings = sanitizeSettings(stored[LEGACY_SETTINGS_KEY]);
  if (migrate) {
    await chrome.storage.local.set({ [key]: settings });
    await cleanupLegacySettings();
  }
  return settings;
}

export async function getSettings() {
  await restrictStorageAccess();
  const context = protectionContext();
  return withLock(`settings:${context}`, () => readSettingsForContext(context, true));
}

function validateSettingsPatch(patch) {
  if (!patch || typeof patch !== "object" || Array.isArray(patch)) {
    throw new TypeError("O patch de configurações deve ser um objeto.");
  }
  const safe = {};
  for (const [key, value] of Object.entries(patch)) {
    if (!SETTING_KEYS.has(key)) continue;
    if (typeof value !== "boolean") {
      throw new TypeError(`A configuração ${key} deve ser booleana.`);
    }
    safe[key] = value;
  }
  return safe;
}

async function saveSettingsPatchInsideMutation(patch, context) {
  await restrictStorageAccess();
  const safe = validateSettingsPatch(patch);
  return withLock(`settings:${context}`, async () => {
    const current = await readSettingsForContext(context, false);
    const next = { ...current, ...safe };
    await chrome.storage.local.set({ [settingsStorageKey(context)]: next });
    await cleanupLegacySettings();
    return next;
  });
}

export async function saveSettingsPatch(patch) {
  const context = protectionContext();
  return withLock(`cookie-mutation:${context}`, () => saveSettingsPatchInsideMutation(patch, context));
}

export async function saveSettings(settings) {
  return saveSettingsPatch({
    autoClearOnStartup: Boolean(settings.autoClearOnStartup),
    protectAgainstExternalDeletion: Boolean(settings.protectAgainstExternalDeletion),
    confirmDestructiveActions: Boolean(settings.confirmDestructiveActions)
  });
}

function sanitizeProtectedRecords(records) {
  if (!Array.isArray(records)) return [];
  const safe = new Map();
  for (const item of records) {
    const cookie = item?.cookie;
    if (!cookie || typeof cookie !== "object" || typeof cookie.domain !== "string" || typeof cookie.name !== "string") continue;
    const snapshot = serializableCookie(cookie);
    if (!snapshot.session && Number(snapshot.expirationDate) <= Date.now() / 1000) continue;
    const id = cookieId(snapshot);
    safe.set(id, { id, cookie: snapshot });
  }
  return [...safe.values()];
}

async function readSessionRecords(context) {
  const key = sessionKey(context);
  if (!chrome.storage.session) return sanitizeProtectedRecords(volatileSession.get(key));
  const stored = await chrome.storage.session.get(key);
  return sanitizeProtectedRecords(stored[key]);
}

async function writeSessionRecords(context, records) {
  const key = sessionKey(context);
  const safe = sanitizeProtectedRecords(records);
  if (!chrome.storage.session) {
    volatileSession.set(key, safe);
    cacheProtectedRecords(context, safe);
    return;
  }
  await chrome.storage.session.set({ [key]: safe });
  cacheProtectedRecords(context, safe);
}

async function readRegularMetadata() {
  const stored = await chrome.storage.local.get(PROTECTED_METADATA_KEY);
  return sanitizeMetadata(stored[PROTECTED_METADATA_KEY]);
}

async function writeProtectionState(context, previous, next) {
  const safeNext = sanitizeProtectedRecords(next);
  await writeSessionRecords(context, safeNext);
  if (context === "incognito") return;

  try {
    await chrome.storage.local.set({
      [PROTECTED_METADATA_KEY]: {
        version: STORAGE_SCHEMA_VERSION,
        records: safeNext.map(({ cookie }) => metadataFromCookie(cookie))
      }
    });
  } catch (error) {
    try {
      await writeSessionRecords(context, previous);
    } catch {
      // The caller receives the original persistence error. No cookie values were written to local storage.
      protectionCache.delete(context);
    }
    throw error;
  }
}

async function reconcileProtectedState(context) {
  const previous = await readSessionRecords(context);
  if (context === "incognito") {
    cacheProtectedRecords(context, previous);
    return sanitizeProtectedRecords(previous);
  }

  const metadata = await readRegularMetadata();
  const metadataById = new Map(metadata.map((item) => [item.id, item]));
  for (const record of previous) metadataById.set(record.id, metadataFromCookie(record.cookie));

  let availableById;
  const next = [];
  for (const item of metadataById.values()) {
    const snapshot = previous.find((record) => record.id === item.id);
    if (snapshot) {
      next.push(snapshot);
      continue;
    }
    if (!availableById) {
      const available = await getAllCookies();
      availableById = new Map(available.map((cookie) => [cookieId(cookie), cookie]));
    }
    const liveCookie = availableById.get(item.id);
    if (liveCookie) next.push({ id: item.id, cookie: serializableCookie(liveCookie) });
  }

  const sanitized = sanitizeProtectedRecords(next);
  const previousSignature = JSON.stringify(previous);
  const metadataSignature = JSON.stringify(metadata);
  const nextMetadata = sanitized.map(({ cookie }) => metadataFromCookie(cookie));
  if (previousSignature !== JSON.stringify(sanitized) || metadataSignature !== JSON.stringify(nextMetadata)) {
    await writeProtectionState(context, previous, sanitized);
  }
  cacheProtectedRecords(context, sanitized);
  return sanitizeProtectedRecords(sanitized);
}

async function getCachedOrReconciledProtection(context) {
  if (!protectionCache.has(context)) return reconcileProtectedState(context);
  const cached = protectionCache.get(context);
  const sanitized = sanitizeProtectedRecords(cached);
  if (JSON.stringify(cached) !== JSON.stringify(sanitized)) {
    await writeProtectionState(context, cached, sanitized);
  }
  return sanitizeProtectedRecords(sanitized);
}

export async function initializeProtectedStorage() {
  await migrateProtectedStorage();
  return getProtectedCookies();
}

export async function purgeIncognitoProtectedStorage() {
  await restrictStorageAccess();
  const key = sessionKey("incognito");
  volatileSession.delete(key);
  if (protectionContext() === "incognito") protectionCache.set("incognito", []);
  if (chrome.storage.session) await chrome.storage.session.remove(key);
}

export async function getProtectedCookies() {
  await migrateProtectedStorage();
  const context = protectionContext();
  return withLock(`protection:${context}`, () => getCachedOrReconciledProtection(context));
}

async function setProtectedCookiesUnlocked(records) {
  await migrateProtectedStorage();
  const context = protectionContext();
  return withLock(`protection:${context}`, async () => {
    const previous = await getCachedOrReconciledProtection(context);
    const next = sanitizeProtectedRecords(records);
    await writeProtectionState(context, previous, next);
    return next;
  });
}

export async function setProtectedCookies(records) {
  const context = protectionContext();
  return withLock(`cookie-mutation:${context}`, () => setProtectedCookiesUnlocked(records));
}

async function mutateProtectedCookies(mutator) {
  await migrateProtectedStorage();
  const context = protectionContext();
  return withLock(`protection:${context}`, async () => {
    const previous = await getCachedOrReconciledProtection(context);
    const records = new Map(previous.map((item) => [item.id, item]));
    mutator(records);
    const next = sanitizeProtectedRecords([...records.values()]);
    await writeProtectionState(context, previous, next);
    return next;
  });
}

async function protectCookiesUnlocked(cookies) {
  const snapshots = cookies.map((cookie) => serializableCookie(cookie));
  return mutateProtectedCookies((records) => {
    for (const cookie of snapshots) {
      const id = cookieId(cookie);
      records.set(id, { id, cookie });
    }
  });
}

export async function protectCookies(cookies) {
  const context = protectionContext();
  return withLock(`cookie-mutation:${context}`, () => protectCookiesUnlocked(cookies));
}

async function unprotectCookiesUnlocked(cookies) {
  const ids = new Set(cookies.map(cookieId));
  return mutateProtectedCookies((records) => {
    for (const id of ids) records.delete(id);
  });
}

export async function unprotectCookies(cookies) {
  const context = protectionContext();
  return withLock(`cookie-mutation:${context}`, () => unprotectCookiesUnlocked(cookies));
}

export async function protectCookie(cookie) {
  return protectCookies([cookie]);
}

export async function unprotectCookie(cookie) {
  return unprotectCookies([cookie]);
}

async function replaceProtectedCookie(original, replacement) {
  const originalId = cookieId(original);
  const replacementSnapshot = serializableCookie(replacement);
  const replacementId = cookieId(replacementSnapshot);
  return mutateProtectedCookies((records) => {
    records.delete(originalId);
    records.set(replacementId, { id: replacementId, cookie: replacementSnapshot });
  });
}

export function serializableCookie(cookie) {
  return {
    domain: cookie.domain,
    expirationDate: cookie.expirationDate,
    hostOnly: Boolean(cookie.hostOnly),
    httpOnly: Boolean(cookie.httpOnly),
    name: cookie.name,
    partitionKey: cookie.partitionKey ? { ...cookie.partitionKey } : undefined,
    path: cookie.path || "/",
    sameSite: cookie.sameSite || "unspecified",
    secure: Boolean(cookie.secure),
    session: Boolean(cookie.session),
    storeId: cookie.storeId,
    value: cookie.value
  };
}

export function toSetDetails(cookie) {
  const details = {
    url: cookieUrl(cookie),
    name: cookie.name ?? "",
    value: cookie.value ?? "",
    path: cookie.path || "/",
    secure: Boolean(cookie.secure),
    httpOnly: Boolean(cookie.httpOnly),
    sameSite: cookie.sameSite || "unspecified"
  };

  if (!cookie.hostOnly && cookie.domain) details.domain = cookie.domain;
  if (!cookie.session && Number.isFinite(Number(cookie.expirationDate))) {
    details.expirationDate = Number(cookie.expirationDate);
  }
  if (cookie.storeId !== undefined) details.storeId = cookie.storeId;
  if (cookie.partitionKey?.topLevelSite) details.partitionKey = cookie.partitionKey;
  return details;
}

export async function getAllCookies(details = {}) {
  const query = { ...details };
  // Chromium otherwise returns only unpartitioned cookies. An explicitly empty
  // partition key requests both partitioned (CHIPS) and unpartitioned records.
  if (!Object.prototype.hasOwnProperty.call(query, "partitionKey")) query.partitionKey = {};
  return chrome.cookies.getAll(query);
}

async function currentCookieStoreId() {
  const currentCookies = await getAllCookies();
  const currentIds = [...new Set(currentCookies.map((cookie) => cookie.storeId).filter(Boolean))];
  if (currentIds.length === 1) return currentIds[0];

  const stores = await chrome.cookies.getAllCookieStores();
  if (stores.length === 1) return stores[0].id;
  try {
    const incognito = protectionContext() === "incognito";
    const tabs = await chrome.tabs.query({});
    const contextTabIds = new Set(tabs.filter((tab) => Boolean(tab.incognito) === incognito).map((tab) => tab.id));
    const matches = stores.filter((store) => store.tabIds.some((tabId) => contextTabIds.has(tabId)));
    if (matches.length === 1) return matches[0].id;
  } catch {
    // The caller can still rely on Chromium's default store if no tab can identify it.
  }
  return undefined;
}

export async function resolveImportCookies(cookies) {
  if (!cookies.some((cookie) => cookie.storeId === undefined)) return cookies.map((cookie) => ({ ...cookie }));
  const storeId = await currentCookieStoreId();
  return cookies.map((cookie) => cookie.storeId === undefined && storeId !== undefined
    ? { ...cookie, storeId }
    : { ...cookie });
}

async function setCookieUnlocked(cookie) {
  const result = await chrome.cookies.set(toSetDetails(cookie));
  if (!result) throw new Error("O Chromium recusou o cookie.");
  return result;
}

export async function setCookie(cookie) {
  const context = protectionContext();
  return withLock(`cookie-mutation:${context}`, () => setCookieUnlocked(cookie));
}

export async function removeCookie(cookie) {
  const details = {
    url: cookieUrl(cookie),
    name: cookie.name,
    storeId: cookie.storeId
  };
  if (cookie.partitionKey?.topLevelSite) details.partitionKey = cookie.partitionKey;
  return chrome.cookies.remove(details);
}

async function findExactCookie(cookie) {
  const details = {
    domain: normalizeDomain(cookie.domain),
    name: cookie.name,
    path: cookie.path || "/"
  };
  if (cookie.storeId !== undefined) details.storeId = cookie.storeId;
  if (cookie.partitionKey?.topLevelSite) details.partitionKey = cookie.partitionKey;
  const candidates = await getAllCookies(details);
  return candidates.find((candidate) => cookieId(candidate) === cookieId(cookie));
}

export async function restoreProtectedCookieIfEnabled(cookie) {
  const context = protectionContext();
  return withLock(`cookie-mutation:${context}`, async () => {
    const settings = await getSettings();
    if (!settings.protectAgainstExternalDeletion) return false;
    const protectedRecords = await getProtectedCookies();
    const record = protectedRecords.find((item) => item.id === cookieId(cookie));
    if (!record) return false;
    // A site may already have recreated or changed this identity while the event waited
    // for the mutation lock. Never overwrite that newer live value.
    if (await findExactCookie(record.cookie)) return false;
    await setCookieUnlocked(record.cookie);
    return true;
  });
}

function cookieMatches(actual, expected) {
  if (!actual || cookieId(actual) !== cookieId(expected)) return false;
  if (actual.value !== (expected.value ?? "")) return false;
  if (Boolean(actual.secure) !== Boolean(expected.secure)) return false;
  if (Boolean(actual.httpOnly) !== Boolean(expected.httpOnly)) return false;
  if ((actual.sameSite || "unspecified") !== (expected.sameSite || "unspecified")) return false;
  if (Boolean(actual.session) !== Boolean(expected.session)) return false;
  if (!expected.session) {
    const actualExpiry = Number(actual.expirationDate);
    const expectedExpiry = Number(expected.expirationDate);
    if (!Number.isFinite(actualExpiry) || !Number.isFinite(expectedExpiry) || Math.abs(actualExpiry - expectedExpiry) > 1) return false;
  }
  return true;
}

async function setAndValidateCookie(cookie) {
  let saved;
  try {
    saved = await setCookieUnlocked(cookie);
    if (!cookieMatches(saved, cookie)) throw new Error("O Chromium salvou um cookie diferente do solicitado.");
    const persisted = await findExactCookie(saved);
    if (!cookieMatches(persisted, cookie)) throw new Error("Não foi possível validar o cookie salvo.");
    return persisted;
  } catch (error) {
    if (saved) error.cookieWasSet = true;
    throw error;
  }
}

async function removeAndValidateCookie(cookie) {
  await removeCookie(cookie);
  if (await findExactCookie(cookie)) throw new Error("O Chromium não removeu o cookie original.");
}

async function removeReplacementDuringRollback(cookie) {
  const current = await findExactCookie(cookie);
  if (!current) return;
  if (!cookieMatches(current, cookie)) throw new Error("O cookie de destino mudou durante a reversão.");
  await removeAndValidateCookie(current);
}

export async function replaceCookie(original, replacement, keepProtected = false) {
  const context = protectionContext();
  // Keep the lock order canonical: cookie mutation first, protection state second.
  // No protection operation acquires the cookie-mutation lock in the opposite order.
  return withLock(`cookie-mutation:${context}`, () => replaceCookieTransaction(original, replacement, keepProtected));
}

async function replaceCookieTransaction(original, replacement, keepProtected) {
  const target = {
    ...replacement,
    storeId: replacement.storeId ?? original.storeId
  };
  const sameIdentity = cookieId(original) === cookieId(target);
  if (!sameIdentity && await findExactCookie(target)) throw new CookieConflictError();

  let protectionMoved = false;
  let targetMayExist = false;
  try {
    if (sameIdentity && keepProtected) {
      await replaceProtectedCookie(original, target);
      protectionMoved = true;
    }

    let saved;
    try {
      saved = await setAndValidateCookie(target);
      targetMayExist = true;
    } catch (error) {
      targetMayExist = Boolean(error.cookieWasSet);
      throw error;
    }

    if (!sameIdentity && keepProtected) {
      await replaceProtectedCookie(original, saved);
      protectionMoved = true;
    }
    if (!sameIdentity) await removeAndValidateCookie(original);
    return saved;
  } catch (cause) {
    const rollbackErrors = [];
    if (protectionMoved) {
      try {
        await replaceProtectedCookie(target, original);
      } catch (error) {
        rollbackErrors.push(error);
      }
    }
    if (!sameIdentity && targetMayExist) {
      try {
        await removeReplacementDuringRollback(target);
      } catch (error) {
        rollbackErrors.push(error);
      }
    }
    try {
      await setAndValidateCookie(original);
    } catch (error) {
      rollbackErrors.push(error);
    }
    throw new CookieTransactionError(cause, rollbackErrors);
  }
}

async function removeCookiesUnlocked(cookies, protectedIds = new Set()) {
  const removable = cookies.filter((cookie) => !protectedIds.has(cookieId(cookie)));
  const results = await mapSettledBounded(removable, removeCookie);
  return {
    removed: results.filter((result) => result.status === "fulfilled" && result.value).length,
    skipped: cookies.length - removable.length,
    failed: results.filter((result) => result.status === "rejected" || !result.value).length
  };
}

export async function removeCookies(cookies, protectedIds = new Set()) {
  const context = protectionContext();
  return withLock(`cookie-mutation:${context}`, () => removeCookiesUnlocked(cookies, protectedIds));
}

export async function removeUnprotectedCookies(cookies) {
  const context = protectionContext();
  return withLock(`cookie-mutation:${context}`, async () => {
    const protectedRecords = await getProtectedCookies();
    return removeCookiesUnlocked(cookies, new Set(protectedRecords.map((item) => item.id)));
  });
}

export async function deleteAllCookies() {
  const context = protectionContext();
  return withLock(`cookie-mutation:${context}`, async () => {
    const settings = await getSettings();
    if (settings.protectAgainstExternalDeletion) {
      await saveSettingsPatchInsideMutation({ protectAgainstExternalDeletion: false }, context);
    }
    await setProtectedCookiesUnlocked([]);

    const cookies = await getAllCookies();
    const results = await mapSettledBounded(cookies, removeCookie);
    const remainingCookies = await getAllCookies();
    const remainingIds = new Set(remainingCookies.map(cookieId));
    let removed = 0;
    for (let index = 0; index < cookies.length; index += 1) {
      const result = results[index];
      if (result?.status === "fulfilled" && result.value && !remainingIds.has(cookieId(cookies[index]))) {
        removed += 1;
      }
    }

    return {
      total: cookies.length,
      removed,
      failed: cookies.length - removed,
      remaining: remainingCookies.length,
      protectionDisabled: true
    };
  });
}

export async function getActiveTab() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  return tab;
}

export async function getCookiesForTab(tabId) {
  const tab = await chrome.tabs.get(tabId);
  if (!tab?.url || !/^https?:/i.test(tab.url)) return { tab, cookies: [] };
  return { tab, cookies: await getAllCookies({ url: tab.url }) };
}

export async function getCurrentSiteCookies() {
  const tab = await getActiveTab();
  if (!tab?.url || !/^https?:/i.test(tab.url)) return { tab, cookies: [] };
  return { tab, cookies: await getAllCookies({ url: tab.url }) };
}

export async function clearOriginStorage(url) {
  const origin = new URL(url).origin;
  await chrome.browsingData.removeLocalStorage({ origins: [origin] });
}

export function exportJson(cookies) {
  return JSON.stringify(cookies.map(serializableCookie), null, 2);
}

export function exportNetscape(cookies) {
  if (cookies.some((cookie) => cookie.partitionKey)) {
    throw new Error("Cookies particionados não podem ser exportados em Netscape; use JSON.");
  }
  const lines = [
    "# Netscape HTTP Cookie File",
    "# Exported by Ayla Cookie Manager",
    "# domain\tincludeSubdomains\tpath\tsecure\texpires\tname\tvalue"
  ];
  for (const cookie of cookies) {
    const domain = cookie.httpOnly ? `#HttpOnly_${cookie.domain}` : cookie.domain;
    lines.push([
      domain,
      cookie.hostOnly ? "FALSE" : "TRUE",
      cookie.path || "/",
      cookie.secure ? "TRUE" : "FALSE",
      cookie.session ? "0" : Math.floor(cookie.expirationDate || 0),
      cookie.name,
      cookie.value
    ].join("\t"));
  }
  return `${lines.join("\n")}\n`;
}

export function parseImportReport(text) {
  const source = String(text ?? "").replace(/^\uFEFF/, "");
  const trimmed = source.trim();
  if (!trimmed) return { cookies: [], sourceCount: 0, duplicates: 0, invalid: [] };

  const cookiesById = new Map();
  const invalid = [];
  let sourceCount = 0;
  let duplicates = 0;
  const accept = (candidate, line) => {
    sourceCount += 1;
    try {
      const cookie = normalizeImportedCookie(candidate);
      const id = cookieId(cookie);
      if (cookiesById.has(id)) {
        duplicates += 1;
        cookiesById.delete(id);
      }
      cookiesById.set(id, cookie);
    } catch (error) {
      invalid.push({ line, message: error?.message || "Cookie inválido." });
    }
  };

  if (trimmed.startsWith("[") || trimmed.startsWith("{")) {
    let parsed;
    try {
      parsed = JSON.parse(trimmed);
    } catch {
      return {
        cookies: [],
        sourceCount: 1,
        duplicates: 0,
        invalid: [{ line: 1, message: "JSON inválido." }]
      };
    }
    let items;
    if (Array.isArray(parsed)) {
      items = parsed;
    } else if (parsed && typeof parsed === "object" && Object.prototype.hasOwnProperty.call(parsed, "cookies")) {
      if (!Array.isArray(parsed.cookies)) {
        return {
          cookies: [],
          sourceCount: 1,
          duplicates: 0,
          invalid: [{ line: 1, message: "O campo cookies deve ser uma lista." }]
        };
      }
      items = parsed.cookies;
    } else {
      items = [parsed];
    }
    items.forEach((item, index) => accept(item, index + 1));
  } else {
    source.split(/\r?\n/).forEach((line, index) => {
      const content = line.replace(/^\uFEFF/, "");
      const marker = content.trimStart();
      if (!marker || (marker.startsWith("#") && !marker.startsWith("#HttpOnly_"))) return;
      sourceCount += 1;
      try {
        const cookie = parseNetscapeLine(marker);
        const id = cookieId(cookie);
        if (cookiesById.has(id)) {
          duplicates += 1;
          cookiesById.delete(id);
        }
        cookiesById.set(id, cookie);
      } catch (error) {
        invalid.push({ line: index + 1, message: error?.message || "Linha Netscape inválida." });
      }
    });
  }

  return { cookies: [...cookiesById.values()], sourceCount, duplicates, invalid };
}

export function parseImport(text) {
  const report = parseImportReport(text);
  if (report.invalid.length) {
    const first = report.invalid[0];
    throw new Error(`${report.invalid.length} cookie(s) inválido(s). Linha ${first.line}: ${first.message}`);
  }
  return report.cookies;
}

function normalizeImportedCookie(cookie) {
  if (!cookie || typeof cookie !== "object" || !cookie.domain || cookie.name === undefined) {
    throw new Error("Cookie JSON inválido: domain e name são obrigatórios.");
  }
  const rawDomain = String(cookie.domain).trim();
  const normalizedHost = normalizeDomain(rawDomain);
  if (!normalizedHost || /[\u0000-\u0020\u007f\\/:?#]/.test(normalizedHost)) {
    throw new Error("Domínio de cookie inválido.");
  }
  if (cookie.hostOnly !== undefined && typeof cookie.hostOnly !== "boolean") {
    throw new Error("hostOnly deve ser booleano.");
  }
  const hostOnly = cookie.hostOnly ?? !rawDomain.startsWith(".");
  const session = cookie.session ?? !cookie.expirationDate;
  if (typeof session !== "boolean") throw new Error("session deve ser booleano.");
  const expirationDate = session ? undefined : Number(cookie.expirationDate);
  if (!session && (!Number.isFinite(expirationDate) || expirationDate <= 0)) {
    throw new Error("Expiração persistente inválida.");
  }
  const name = String(cookie.name);
  const value = String(cookie.value ?? "");
  const path = String(cookie.path || "/");
  if (/[\u0000-\u001f\u007f]/.test(name) || /[\u0000-\u001f\u007f]/.test(value)) {
    throw new Error("Nome ou valor contém caracteres de controle.");
  }
  if (!path.startsWith("/") || /[\u0000-\u001f\u007f]/.test(path)) {
    throw new Error("Caminho de cookie inválido.");
  }
  const sameSite = cookie.sameSite || "unspecified";
  if (!["unspecified", "no_restriction", "lax", "strict"].includes(sameSite)) {
    throw new Error("SameSite inválido.");
  }
  for (const key of ["httpOnly", "secure"]) {
    if (cookie[key] !== undefined && typeof cookie[key] !== "boolean") {
      throw new Error(`${key} deve ser booleano.`);
    }
  }
  const secure = Boolean(cookie.secure);
  if (sameSite === "no_restriction" && !secure) {
    throw new Error("SameSite=None exige Secure.");
  }
  let partitionKey;
  if (cookie.partitionKey !== undefined) {
    if (!cookie.partitionKey || typeof cookie.partitionKey !== "object") {
      throw new Error("Chave de partição inválida.");
    }
    try {
      const site = new URL(cookie.partitionKey.topLevelSite);
      if (!/^https?:$/.test(site.protocol)) throw new Error();
      partitionKey = { topLevelSite: site.origin };
    } catch {
      throw new Error("Chave de partição inválida.");
    }
    if (cookie.partitionKey.hasCrossSiteAncestor !== undefined) {
      if (typeof cookie.partitionKey.hasCrossSiteAncestor !== "boolean") {
        throw new Error("Chave de partição inválida.");
      }
      partitionKey.hasCrossSiteAncestor = cookie.partitionKey.hasCrossSiteAncestor;
    }
    if (!secure) throw new Error("Cookie particionado exige Secure.");
  }
  return {
    domain: hostOnly ? normalizedHost : `.${normalizedHost}`,
    expirationDate,
    hostOnly,
    httpOnly: Boolean(cookie.httpOnly),
    name,
    partitionKey,
    path,
    sameSite,
    secure,
    session: Boolean(session),
    storeId: cookie.storeId === undefined ? undefined : String(cookie.storeId),
    value
  };
}

function parseNetscapeLine(line) {
  const fields = line.split("\t");
  if (fields.length < 7) throw new Error("Linha Netscape inválida.");
  if (!["TRUE", "FALSE"].includes(fields[1].toUpperCase())) {
    throw new Error("Flag de subdomínios inválida.");
  }
  if (!["TRUE", "FALSE"].includes(fields[3].toUpperCase())) {
    throw new Error("Flag Secure inválida.");
  }
  const httpOnly = fields[0].startsWith("#HttpOnly_");
  const domain = fields[0].replace(/^#HttpOnly_/, "");
  const expirationDate = Number(fields[4]);
  if (!Number.isFinite(expirationDate) || expirationDate < 0) {
    throw new Error("Expiração Netscape inválida.");
  }
  return normalizeImportedCookie({
    domain,
    hostOnly: fields[1].toUpperCase() !== "TRUE",
    path: fields[2],
    secure: fields[3].toUpperCase() === "TRUE",
    expirationDate: expirationDate > 0 ? expirationDate : undefined,
    session: expirationDate <= 0,
    name: fields[5],
    value: fields.slice(6).join("\t"),
    httpOnly,
    sameSite: "unspecified"
  });
}

async function rollbackImportWrite({ requested, saved, previous }) {
  const current = await findExactCookie(saved);
  if (!cookieMatches(current, saved)) {
    throw new Error("O cookie importado mudou antes da reversão.");
  }

  if (previous && cookieId(requested) === cookieId(saved)) {
    await setAndValidateCookie(previous);
    return;
  }

  await removeAndValidateCookie(current);
  if (previous) {
    const original = await findExactCookie(previous);
    if (!cookieMatches(original, previous)) {
      throw new Error("O cookie original mudou durante a reversão.");
    }
  }
}

async function captureImportPrevious(cookies) {
  const storeIds = [...new Set(cookies.map((cookie) => cookie.storeId))];
  const snapshots = await mapSettledBounded(storeIds, (storeId) => (
    storeId === undefined ? getAllCookies() : getAllCookies({ storeId })
  ));
  const failure = snapshots.find((result) => result.status === "rejected");
  if (failure) throw failure.reason;

  const previousById = new Map();
  for (const result of snapshots) {
    for (const cookie of result.value) previousById.set(cookieId(cookie), cookie);
  }
  return cookies.map((cookie) => previousById.get(cookieId(cookie)));
}

export async function importCookies(cookies) {
  const context = protectionContext();
  return withLock(`cookie-mutation:${context}`, async () => {
    const resolvedCookies = await resolveImportCookies(cookies);
    const deduplicated = deduplicateCookies(resolvedCookies);
    const previousProtection = await getProtectedCookies();
    const protectedById = new Map(previousProtection.map((item) => [item.id, item]));
    const previousLive = await captureImportPrevious(deduplicated.cookies);
    const results = await mapSettledBounded(deduplicated.cookies, setCookieUnlocked);
    let protectionChanged = false;

    for (let index = 0; index < results.length; index += 1) {
      const result = results[index];
      if (result.status !== "fulfilled") continue;
      const requestedId = cookieId(deduplicated.cookies[index]);
      const previous = protectedById.get(requestedId);
      if (!previous) continue;
      protectedById.delete(requestedId);
      const snapshot = serializableCookie(result.value);
      protectedById.set(cookieId(snapshot), { id: cookieId(snapshot), cookie: snapshot });
      protectionChanged = true;
    }

    if (protectionChanged) {
      try {
        await setProtectedCookiesUnlocked([...protectedById.values()]);
      } catch (cause) {
        const writes = results.flatMap((result, index) => result.status === "fulfilled" ? [{
          requested: deduplicated.cookies[index],
          saved: result.value,
          previous: previousLive[index]
        }] : []);
        const rollback = await mapSettledBounded(writes, rollbackImportWrite);
        const rollbackErrors = rollback
          .filter((result) => result.status === "rejected")
          .map((result) => result.reason);
        throw new CookieTransactionError(cause, rollbackErrors);
      }
    }

    return {
      imported: results.filter((result) => result.status === "fulfilled").length,
      failed: results.filter((result) => result.status === "rejected").length,
      duplicates: deduplicated.duplicates
    };
  });
}

export function downloadText(filename, contents, type = "text/plain") {
  const anchor = document.createElement("a");
  anchor.href = URL.createObjectURL(new Blob([contents], { type }));
  anchor.download = filename;
  anchor.click();
  setTimeout(() => URL.revokeObjectURL(anchor.href), 1000);
}
