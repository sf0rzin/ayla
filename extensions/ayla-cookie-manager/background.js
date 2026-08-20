import {
  getAllCookies,
  getSettings,
  initializeProtectedStorage,
  purgeIncognitoProtectedStorage,
  removeUnprotectedCookies,
  restoreProtectedCookieIfEnabled
} from "./lib/cookies.js";

function normalizeManagerAction(action) {
  if (action?.type === "import") return { type: "import" };
  if (action?.type === "delete-site" && Number.isInteger(action.tabId)) {
    return { type: "delete-site", tabId: action.tabId };
  }
  return null;
}

async function openManager(requestedAction = null) {
  const action = normalizeManagerAction(requestedAction);
  const baseUrl = chrome.runtime.getURL("manager.html");
  const targetUrl = new URL(baseUrl);
  if (action?.type) targetUrl.searchParams.set("action", action.type);
  if (Number.isInteger(action?.tabId)) targetUrl.searchParams.set("tabId", String(action.tabId));
  const tabs = await chrome.tabs.query({});
  const existing = tabs.find((tab) => tab.url?.startsWith(baseUrl));
  if (existing?.id) {
    await chrome.tabs.update(existing.id, { active: true });
    if (existing.windowId) await chrome.windows.update(existing.windowId, { focused: true });
    if (action) {
      if (existing.status === "complete") {
        await chrome.runtime.sendMessage({ type: "manager-action", action });
      } else {
        // A page that is still loading cannot have an unsaved draft yet. Keep the
        // query-string fallback so its first render receives the requested action.
        await chrome.tabs.update(existing.id, { url: targetUrl.href });
      }
    }
    return;
  }
  await chrome.tabs.create({ url: targetUrl.href });
}

chrome.runtime.onInstalled.addListener(() => {
  void initializeProtectedStorage().catch((error) => {
    console.warn("Ayla não conseguiu migrar a proteção de cookies.", error);
  });
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({ id: "open-manager", title: "Abrir Ayla Cookies for Edge", contexts: ["action", "page"] });
    chrome.contextMenus.create({ id: "delete-site", title: "Apagar cookies deste site (exceto protegidos)", contexts: ["action", "page"] });
  });
});

chrome.commands.onCommand.addListener((command) => {
  if (command === "open-manager") void openManager();
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId === "open-manager") await openManager();
  if (info.menuItemId === "delete-site") {
    await openManager({ type: "delete-site", tabId: tab?.id });
  }
});

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message?.type !== "open-manager") return undefined;
  void openManager(message.action ? { type: message.action } : null).then(
    () => sendResponse({ ok: true }),
    (error) => sendResponse({ ok: false, error: error?.message ?? "Não foi possível abrir o gerenciador." })
  );
  // Promise responses only work in recent Chromium releases. Keep the channel
  // alive explicitly so the declared minimum Edge/Chrome 130 stays supported.
  return true;
});

chrome.runtime.onStartup.addListener(async () => {
  await initializeProtectedStorage();
  const settings = await getSettings();
  if (!settings.autoClearOnStartup) return;
  const cookies = await getAllCookies();
  await removeUnprotectedCookies(cookies);
});

chrome.windows.onRemoved.addListener(async () => {
  if (!chrome.extension?.inIncognitoContext) return;
  const windows = await chrome.windows.getAll();
  if (!windows.some((window) => window.incognito)) await purgeIncognitoProtectedStorage();
});

chrome.cookies.onChanged.addListener(({ removed, cookie, cause }) => {
  if (!removed) return;
  // A set operation emits a transient overwrite removal before the replacement event.
  if (cause === "overwrite") return;
  void restoreProtectedCookieIfEnabled(cookie).catch((error) => {
    console.warn("Ayla não conseguiu restaurar um cookie protegido.", error);
  });
});

void initializeProtectedStorage().catch((error) => {
  console.warn("Ayla não conseguiu inicializar a proteção de cookies.", error);
});
