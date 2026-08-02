const PROTECTED_KEY = "protectedCookies";
const SETTINGS_KEY = "settings";

export const DEFAULT_SETTINGS = Object.freeze({
  autoClearOnStartup: false,
  protectAgainstExternalDeletion: true,
  confirmDestructiveActions: true
});

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

export async function getSettings() {
  const stored = await chrome.storage.local.get(SETTINGS_KEY);
  return { ...DEFAULT_SETTINGS, ...(stored[SETTINGS_KEY] ?? {}) };
}

export async function saveSettings(settings) {
  const next = { ...DEFAULT_SETTINGS, ...settings };
  await chrome.storage.local.set({ [SETTINGS_KEY]: next });
  return next;
}

export async function getProtectedCookies() {
  const stored = await chrome.storage.local.get(PROTECTED_KEY);
  return Array.isArray(stored[PROTECTED_KEY]) ? stored[PROTECTED_KEY] : [];
}

export async function setProtectedCookies(records) {
  await chrome.storage.local.set({ [PROTECTED_KEY]: records });
}

export async function protectCookie(cookie) {
  const records = await getProtectedCookies();
  const id = cookieId(cookie);
  const next = records.filter((item) => item.id !== id);
  next.push({ id, cookie: serializableCookie(cookie) });
  await setProtectedCookies(next);
}

export async function unprotectCookie(cookie) {
  const id = cookieId(cookie);
  const records = await getProtectedCookies();
  await setProtectedCookies(records.filter((item) => item.id !== id));
}

export function serializableCookie(cookie) {
  return {
    domain: cookie.domain,
    expirationDate: cookie.expirationDate,
    hostOnly: Boolean(cookie.hostOnly),
    httpOnly: Boolean(cookie.httpOnly),
    name: cookie.name,
    partitionKey: cookie.partitionKey,
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

export async function setCookie(cookie) {
  const result = await chrome.cookies.set(toSetDetails(cookie));
  if (!result) throw new Error("O Chromium recusou o cookie.");
  return result;
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

export async function replaceCookie(original, replacement, keepProtected = false) {
  if (keepProtected) {
    await unprotectCookie(original);
    await protectCookie(replacement);
  }
  if (cookieId(original) !== cookieId(replacement)) await removeCookie(original);
  const saved = await setCookie(replacement);
  if (keepProtected) await protectCookie(saved);
  return saved;
}

export async function removeCookies(cookies, protectedIds = new Set()) {
  const removable = cookies.filter((cookie) => !protectedIds.has(cookieId(cookie)));
  const results = await Promise.allSettled(removable.map(removeCookie));
  return {
    removed: results.filter((result) => result.status === "fulfilled" && result.value).length,
    skipped: cookies.length - removable.length,
    failed: results.filter((result) => result.status === "rejected").length
  };
}

export async function getActiveTab() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  return tab;
}

export async function getCurrentSiteCookies() {
  const tab = await getActiveTab();
  if (!tab?.url || !/^https?:/i.test(tab.url)) return { tab, cookies: [] };
  return { tab, cookies: await chrome.cookies.getAll({ url: tab.url }) };
}

export async function clearOriginStorage(url) {
  const origin = new URL(url).origin;
  await chrome.browsingData.removeLocalStorage({ origins: [origin] });
}

export function exportJson(cookies) {
  return JSON.stringify(cookies.map(serializableCookie), null, 2);
}

export function exportNetscape(cookies) {
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

export function parseImport(text) {
  const trimmed = text.trim();
  if (!trimmed) return [];
  if (trimmed.startsWith("[") || trimmed.startsWith("{")) {
    const parsed = JSON.parse(trimmed);
    const items = Array.isArray(parsed) ? parsed : (parsed.cookies ?? [parsed]);
    return items.map(normalizeImportedCookie);
  }
  return trimmed.split(/\r?\n/)
    .filter((line) => line && (!line.startsWith("#") || line.startsWith("#HttpOnly_")))
    .map(parseNetscapeLine);
}

function normalizeImportedCookie(cookie) {
  if (!cookie || typeof cookie !== "object" || !cookie.domain || cookie.name === undefined) {
    throw new Error("Cookie JSON inválido: domain e name são obrigatórios.");
  }
  const session = cookie.session ?? !cookie.expirationDate;
  return {
    domain: String(cookie.domain),
    expirationDate: session ? undefined : Number(cookie.expirationDate),
    hostOnly: cookie.hostOnly ?? !String(cookie.domain).startsWith("."),
    httpOnly: Boolean(cookie.httpOnly),
    name: String(cookie.name),
    partitionKey: cookie.partitionKey,
    path: String(cookie.path || "/"),
    sameSite: cookie.sameSite || "unspecified",
    secure: Boolean(cookie.secure),
    session: Boolean(session),
    storeId: cookie.storeId,
    value: String(cookie.value ?? "")
  };
}

function parseNetscapeLine(line) {
  const fields = line.split("\t");
  if (fields.length < 7) throw new Error("Linha Netscape inválida.");
  const httpOnly = fields[0].startsWith("#HttpOnly_");
  const domain = fields[0].replace(/^#HttpOnly_/, "");
  const expirationDate = Number(fields[4]);
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

export async function importCookies(cookies) {
  const results = await Promise.allSettled(cookies.map(setCookie));
  return {
    imported: results.filter((result) => result.status === "fulfilled").length,
    failed: results.filter((result) => result.status === "rejected").length
  };
}

export function downloadText(filename, contents, type = "text/plain") {
  const anchor = document.createElement("a");
  anchor.href = URL.createObjectURL(new Blob([contents], { type }));
  anchor.download = filename;
  anchor.click();
  setTimeout(() => URL.revokeObjectURL(anchor.href), 1000);
}
