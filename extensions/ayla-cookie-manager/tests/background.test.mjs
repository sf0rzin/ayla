import assert from "node:assert/strict";
import test from "node:test";

let moduleNumber = 0;

function eventSlot() {
  const listeners = [];
  return {
    listeners,
    api: {
      addListener(listener) {
        listeners.push(listener);
      }
    }
  };
}

function storageArea() {
  const values = {};
  return {
    async get(keys) {
      if (keys === undefined || keys === null) return structuredClone(values);
      if (typeof keys === "string") return Object.hasOwn(values, keys) ? { [keys]: structuredClone(values[keys]) } : {};
      if (Array.isArray(keys)) {
        return Object.fromEntries(keys.filter((key) => Object.hasOwn(values, key)).map((key) => [key, structuredClone(values[key])]));
      }
      return Object.fromEntries(Object.entries(keys).map(([key, fallback]) => [key, Object.hasOwn(values, key) ? structuredClone(values[key]) : fallback]));
    },
    async set(next) {
      Object.assign(values, structuredClone(next));
    },
    async remove(keys) {
      for (const key of Array.isArray(keys) ? keys : [keys]) delete values[key];
    },
    async setAccessLevel() {}
  };
}

async function setupBackground({ managerStatus = "complete" } = {}) {
  const runtimeOnMessage = eventSlot();
  const runtimeOnInstalled = eventSlot();
  const commandOnCommand = eventSlot();
  const contextMenuOnClicked = eventSlot();
  const runtimeOnStartup = eventSlot();
  const windowsOnRemoved = eventSlot();
  const cookiesOnChanged = eventSlot();
  const storageOnChanged = eventSlot();
  const managerUrl = "chrome-extension://ayla/manager.html";
  const tab = { id: 41, windowId: 7, status: managerStatus, url: managerUrl };
  const calls = {
    createdTabs: [],
    sentMessages: [],
    tabUpdates: [],
    windowUpdates: []
  };

  globalThis.chrome = {
    extension: { inIncognitoContext: false },
    runtime: {
      getURL(path) { return `chrome-extension://ayla/${path}`; },
      onInstalled: runtimeOnInstalled.api,
      onMessage: runtimeOnMessage.api,
      onStartup: runtimeOnStartup.api,
      async sendMessage(message) {
        calls.sentMessages.push(structuredClone(message));
      }
    },
    commands: { onCommand: commandOnCommand.api },
    contextMenus: {
      create() {},
      onClicked: contextMenuOnClicked.api,
      removeAll(callback) { callback(); }
    },
    tabs: {
      async create(details) {
        calls.createdTabs.push(structuredClone(details));
      },
      async query() {
        return [structuredClone(tab)];
      },
      async update(tabId, details) {
        calls.tabUpdates.push({ tabId, details: structuredClone(details) });
      }
    },
    windows: {
      async getAll() { return []; },
      onRemoved: windowsOnRemoved.api,
      async update(windowId, details) {
        calls.windowUpdates.push({ windowId, details: structuredClone(details) });
      }
    },
    cookies: {
      async getAll() { return []; },
      onChanged: cookiesOnChanged.api
    },
    storage: {
      local: storageArea(),
      onChanged: storageOnChanged.api,
      session: storageArea()
    }
  };

  await import(`../background.js?background-test=${moduleNumber += 1}`);
  assert.equal(runtimeOnMessage.listeners.length, 1);
  return { calls, managerUrl, onMessage: runtimeOnMessage.listeners[0] };
}

function invokeMessage(listener, message) {
  let resolveResponse;
  const response = new Promise((resolve) => { resolveResponse = resolve; });
  const returned = listener(message, {}, resolveResponse);
  return { response, returned };
}

test("deep-link de importação reutiliza manager completo sem recarregar a aba", { concurrency: false }, async () => {
  const { calls, onMessage } = await setupBackground();

  const invocation = invokeMessage(onMessage, { type: "open-manager", action: "import" });

  assert.equal(invocation.returned, true, "o canal assíncrono deve permanecer aberto no Chromium 130");
  assert.deepEqual(await invocation.response, { ok: true });
  assert.deepEqual(calls.createdTabs, []);
  assert.deepEqual(calls.tabUpdates, [{ tabId: 41, details: { active: true } }]);
  assert.equal(calls.tabUpdates.some(({ details }) => Object.hasOwn(details, "url")), false);
  assert.deepEqual(calls.windowUpdates, [{ windowId: 7, details: { focused: true } }]);
  assert.deepEqual(calls.sentMessages, [{ type: "manager-action", action: { type: "import" } }]);
});

test("listener usa sendResponse e return true para concluir erros assíncronos", { concurrency: false }, async () => {
  const { calls, onMessage } = await setupBackground();
  globalThis.chrome.tabs.update = async () => {
    throw new Error("synthetic tab failure");
  };

  const invocation = invokeMessage(onMessage, { type: "open-manager" });

  assert.equal(invocation.returned, true);
  assert.deepEqual(await invocation.response, { ok: false, error: "synthetic tab failure" });
  assert.deepEqual(calls.sentMessages, []);
});

test("mensagens e ações fora da allowlist não executam deep-links", { concurrency: false }, async () => {
  const { calls, onMessage } = await setupBackground();

  let unexpectedResponse = false;
  const ignored = onMessage({ type: "manager-action", action: "import" }, {}, () => { unexpectedResponse = true; });
  assert.equal(ignored, undefined);
  await Promise.resolve();
  assert.equal(unexpectedResponse, false);
  assert.deepEqual(calls.tabUpdates, []);
  assert.deepEqual(calls.sentMessages, []);

  const unknownAction = invokeMessage(onMessage, { type: "open-manager", action: "delete-all" });
  assert.equal(unknownAction.returned, true);
  assert.deepEqual(await unknownAction.response, { ok: true });
  assert.deepEqual(calls.tabUpdates, [{ tabId: 41, details: { active: true } }]);
  assert.equal(calls.tabUpdates.some(({ details }) => Object.hasOwn(details, "url")), false);
  assert.deepEqual(calls.sentMessages, []);
});
