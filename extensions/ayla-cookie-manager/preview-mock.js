// Browser-only preview data. This file is inert inside a real Chromium extension.
if (!globalThis.chrome?.cookies) {
  const listeners = [];
  const now = Math.floor(Date.now() / 1000);
  const sampleCookies = [
    { domain: ".openai.com", hostOnly: false, name: "session", value: "synthetic-preview", path: "/", secure: true, httpOnly: true, sameSite: "lax", session: false, expirationDate: now + 86400 * 14, storeId: "0" },
    { domain: "chatgpt.com", hostOnly: true, name: "theme", value: "dark", path: "/", secure: true, httpOnly: false, sameSite: "lax", session: false, expirationDate: now + 86400 * 365, storeId: "0" },
    { domain: ".twitch.tv", hostOnly: false, name: "language", value: "pt-BR", path: "/", secure: true, httpOnly: false, sameSite: "unspecified", session: true, storeId: "0" },
    { domain: ".max.com", hostOnly: false, name: "device", value: "synthetic-device", path: "/", secure: true, httpOnly: true, sameSite: "no_restriction", session: false, expirationDate: now + 86400 * 30, storeId: "0", partitionKey: { topLevelSite: "https://max.com" } },
    { domain: "ayla.rindexx.cc", hostOnly: true, name: "locale", value: "pt-BR", path: "/", secure: true, httpOnly: false, sameSite: "strict", session: true, storeId: "0" }
  ];
  const local = {};
  const keyFor = (cookie) => [cookie.storeId, cookie.domain, cookie.path, cookie.name, cookie.partitionKey?.topLevelSite ?? ""].join("|");
  const chromeApi = globalThis.chrome ?? {};
  chromeApi.cookies = {
    getAll: async (filter = {}) => sampleCookies.filter((cookie) => !filter.url || new URL(filter.url).hostname.endsWith(cookie.domain.replace(/^\./, ""))),
    set: async (details) => {
      const cookie = { ...details, domain: details.domain ?? new URL(details.url).hostname, hostOnly: !details.domain, session: !details.expirationDate, storeId: details.storeId ?? "0" };
      const index = sampleCookies.findIndex((item) => keyFor(item) === keyFor(cookie));
      if (index >= 0) sampleCookies[index] = cookie; else sampleCookies.push(cookie);
      listeners.forEach((listener) => listener({ removed: false, cookie }));
      return cookie;
    },
    remove: async (details) => {
      const host = new URL(details.url).hostname;
      const index = sampleCookies.findIndex((cookie) => cookie.name === details.name && host.endsWith(cookie.domain.replace(/^\./, "")));
      if (index < 0) return undefined;
      const [cookie] = sampleCookies.splice(index, 1);
      listeners.forEach((listener) => listener({ removed: true, cookie, cause: "explicit" }));
      return { name: cookie.name, url: details.url, storeId: cookie.storeId };
    },
    onChanged: { addListener: (listener) => listeners.push(listener) }
  };
  chromeApi.storage = { local: {
    get: async (key) => typeof key === "string" ? { [key]: local[key] } : { ...local },
    set: async (values) => Object.assign(local, values)
  } };
  chromeApi.tabs = {
    query: async () => [{ id: 1, url: "https://ayla.rindexx.cc/", title: "Ayla" }],
    create: async () => ({}), update: async () => ({})
  };
  chromeApi.runtime = { sendMessage: async () => ({}), getURL: (path) => path };
  chromeApi.browsingData = { removeLocalStorage: async () => undefined };
  globalThis.chrome = chromeApi;
}
