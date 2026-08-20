function clone(value) {
  return value === undefined ? undefined : structuredClone(value);
}

function partitionPart(cookie) {
  return `${cookie.partitionKey?.topLevelSite ?? ""}|${cookie.partitionKey?.hasCrossSiteAncestor ? "1" : "0"}`;
}

export function mockCookieId(cookie) {
  return [cookie.storeId ?? "0", cookie.domain, cookie.path || "/", cookie.name, partitionPart(cookie)].join("|");
}

export function installNavigatorLocksMock() {
  const previousDescriptor = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  const tails = new Map();
  const locks = {
    request(name, callback) {
      const previous = tails.get(name) ?? Promise.resolve();
      const ready = previous.catch(() => undefined);
      let release;
      const hold = new Promise((resolve) => { release = resolve; });
      const tail = ready.then(() => hold);
      tails.set(name, tail);
      return ready.then(callback).finally(() => {
        release();
        if (tails.get(name) === tail) tails.delete(name);
      });
    }
  };
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    enumerable: true,
    value: { locks },
    writable: true
  });
  return () => {
    if (previousDescriptor) Object.defineProperty(globalThis, "navigator", previousDescriptor);
    else delete globalThis.navigator;
  };
}

function cookieMatchesUrl(cookie, url) {
  const parsed = new URL(url);
  const domain = cookie.domain.replace(/^\./, "");
  const domainMatches = cookie.hostOnly ? parsed.hostname === domain : (parsed.hostname === domain || parsed.hostname.endsWith(`.${domain}`));
  return domainMatches && parsed.pathname.startsWith(cookie.path || "/");
}

function storageArea(data, name, control, emitChange) {
  return {
    async get(keys) {
      control.storageGetCount[name] += 1;
      if (keys === null || keys === undefined) return clone(data);
      if (typeof keys === "string") return Object.hasOwn(data, keys) ? { [keys]: clone(data[keys]) } : {};
      if (Array.isArray(keys)) {
        return Object.fromEntries(keys.filter((key) => Object.hasOwn(data, key)).map((key) => [key, clone(data[key])]));
      }
      return Object.fromEntries(Object.entries(keys).map(([key, fallback]) => [key, clone(Object.hasOwn(data, key) ? data[key] : fallback)]));
    },
    async set(values) {
      control.storageSetCount[name] += 1;
      if (name === "local" && control.failLocalSetOnce) {
        control.failLocalSetOnce = false;
        throw new Error("synthetic local quota failure");
      }
      const changes = {};
      for (const [key, value] of Object.entries(values)) {
        changes[key] = { oldValue: clone(data[key]), newValue: clone(value) };
      }
      Object.assign(data, clone(values));
      emitChange(changes, name);
    },
    async remove(keys) {
      control.storageRemoveCount[name] += 1;
      const changes = {};
      for (const key of Array.isArray(keys) ? keys : [keys]) {
        if (Object.hasOwn(data, key)) changes[key] = { oldValue: clone(data[key]) };
        delete data[key];
      }
      emitChange(changes, name);
    },
    async setAccessLevel({ accessLevel }) {
      control.accessLevels[name] = accessLevel;
    }
  };
}

export function createChromeMock({
  incognito = false,
  cookies = [],
  local = {},
  session = {},
  sharedLocalData
} = {}) {
  const jar = cookies.map(clone);
  const jarById = new Map(jar.map((cookie) => [mockCookieId(cookie), cookie]));
  const localData = sharedLocalData ?? clone(local);
  const sessionData = clone(session);
  const storageChangeListeners = [];
  const emitStorageChange = (changes, areaName) => {
    if (!Object.keys(changes).length) return;
    for (const listener of storageChangeListeners) listener(clone(changes), areaName);
  };
  const control = {
    activeCookieSets: 0,
    accessLevels: {},
    blockRemoveIds: new Set(),
    cookieSetDelayMs: 0,
    cookieGetAllCount: 0,
    cookieGetAllQueries: [],
    failLocalSetOnce: false,
    failNextCookieSet: 0,
    operations: [],
    peakCookieSets: 0,
    storageSetCount: { local: 0, session: 0 },
    storageGetCount: { local: 0, session: 0 },
    storageRemoveCount: { local: 0, session: 0 }
  };

  const api = {
    extension: { inIncognitoContext: incognito },
    storage: {
      local: storageArea(localData, "local", control, emitStorageChange),
      session: storageArea(sessionData, "session", control, emitStorageChange),
      onChanged: { addListener(listener) { storageChangeListeners.push(listener); } }
    },
    cookies: {
      async getAll(details = {}) {
        control.cookieGetAllCount += 1;
        control.cookieGetAllQueries.push(clone(details));
        return jar.filter((cookie) => {
          if (details.domain) {
            const requested = details.domain.replace(/^\./, "");
            const actual = cookie.domain.replace(/^\./, "");
            if (actual !== requested && !actual.endsWith(`.${requested}`)) return false;
          }
          if (details.name !== undefined && cookie.name !== details.name) return false;
          if (details.path !== undefined && cookie.path !== details.path) return false;
          if (details.storeId !== undefined && cookie.storeId !== details.storeId) return false;
          if (details.secure !== undefined && cookie.secure !== details.secure) return false;
          if (details.session !== undefined && cookie.session !== details.session) return false;
          if (details.url && !cookieMatchesUrl(cookie, details.url)) return false;
          const hasPartitionFilter = Object.prototype.hasOwnProperty.call(details, "partitionKey");
          if (!hasPartitionFilter && cookie.partitionKey) return false;
          if (hasPartitionFilter && details.partitionKey && Object.keys(details.partitionKey).length) {
            if (details.partitionKey.topLevelSite !== undefined
              && cookie.partitionKey?.topLevelSite !== details.partitionKey.topLevelSite) return false;
            if (details.partitionKey.hasCrossSiteAncestor !== undefined
              && Boolean(cookie.partitionKey?.hasCrossSiteAncestor) !== details.partitionKey.hasCrossSiteAncestor) return false;
          }
          return true;
        }).map(clone);
      },
      async set(details) {
        const parsed = new URL(details.url);
        const cookie = {
          domain: details.domain ?? parsed.hostname,
          hostOnly: details.domain === undefined,
          httpOnly: Boolean(details.httpOnly),
          name: details.name ?? "",
          path: details.path || "/",
          sameSite: details.sameSite || "unspecified",
          secure: Boolean(details.secure),
          session: details.expirationDate === undefined,
          storeId: details.storeId ?? (incognito ? "1" : "0"),
          value: details.value ?? ""
        };
        if (details.expirationDate !== undefined) cookie.expirationDate = details.expirationDate;
        if (details.partitionKey !== undefined) cookie.partitionKey = clone(details.partitionKey);
        control.activeCookieSets += 1;
        control.peakCookieSets = Math.max(control.peakCookieSets, control.activeCookieSets);
        try {
          if (control.cookieSetDelayMs > 0) {
            await new Promise((resolve) => setTimeout(resolve, control.cookieSetDelayMs));
          }
          control.operations.push(`set:${mockCookieId(cookie)}`);
          if (control.failNextCookieSet > 0) {
            control.failNextCookieSet -= 1;
            throw new Error("synthetic cookie set failure");
          }
          const id = mockCookieId(cookie);
          const existing = jarById.get(id);
          if (existing) {
            for (const key of Object.keys(existing)) delete existing[key];
            Object.assign(existing, cookie);
          } else {
            jar.push(cookie);
            jarById.set(id, cookie);
          }
          return clone(cookie);
        } finally {
          control.activeCookieSets -= 1;
        }
      },
      async remove(details) {
        const candidates = jar
          .map((cookie, index) => ({ cookie, index }))
          .filter(({ cookie }) => cookie.name === details.name
            && (details.storeId === undefined || cookie.storeId === details.storeId)
            && (!details.partitionKey || partitionPart(cookie) === partitionPart({ partitionKey: details.partitionKey }))
            && cookieMatchesUrl(cookie, details.url))
          .sort((left, right) => right.cookie.path.length - left.cookie.path.length);
        const match = candidates[0];
        if (!match) return undefined;
        control.operations.push(`remove:${mockCookieId(match.cookie)}`);
        if (control.blockRemoveIds.has(mockCookieId(match.cookie))) return undefined;
        const [removed] = jar.splice(match.index, 1);
        jarById.delete(mockCookieId(removed));
        return { name: removed.name, storeId: removed.storeId, url: details.url };
      },
      async getAllCookieStores() {
        return [{ id: incognito ? "1" : "0", tabIds: [1] }];
      },
      onChanged: { addListener() {} }
    },
    tabs: {
      async query() { return [{ id: 1, url: "https://example.test/", incognito }]; },
      async get() { return { id: 1, url: "https://example.test/", incognito }; }
    },
    browsingData: { async removeLocalStorage() {} }
  };

  return { api, control, jar, localData, sessionData };
}
