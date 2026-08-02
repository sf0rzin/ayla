import {
  cookieId,
  getCurrentSiteCookies,
  getProtectedCookies,
  getSettings,
  removeCookies,
  setCookie
} from "./lib/cookies.js";

async function openManager() {
  const url = chrome.runtime.getURL("manager.html");
  const tabs = await chrome.tabs.query({ url });
  if (tabs[0]?.id) {
    await chrome.tabs.update(tabs[0].id, { active: true });
    if (tabs[0].windowId) await chrome.windows.update(tabs[0].windowId, { focused: true });
    return;
  }
  await chrome.tabs.create({ url });
}

chrome.runtime.onInstalled.addListener(async () => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({ id: "open-manager", title: "Abrir Ayla Cookie Manager", contexts: ["action", "page"] });
    chrome.contextMenus.create({ id: "delete-site", title: "Apagar cookies deste site (exceto protegidos)", contexts: ["action", "page"] });
  });
});

chrome.commands.onCommand.addListener((command) => {
  if (command === "open-manager") void openManager();
});

chrome.contextMenus.onClicked.addListener(async (info) => {
  if (info.menuItemId === "open-manager") await openManager();
  if (info.menuItemId === "delete-site") {
    const [{ cookies }, protectedRecords] = await Promise.all([
      getCurrentSiteCookies(),
      getProtectedCookies()
    ]);
    await removeCookies(cookies, new Set(protectedRecords.map((item) => item.id)));
  }
});

chrome.runtime.onMessage.addListener((message) => {
  if (message?.type === "open-manager") return openManager();
  return undefined;
});

chrome.runtime.onStartup.addListener(async () => {
  const settings = await getSettings();
  if (!settings.autoClearOnStartup) return;
  const [cookies, protectedRecords] = await Promise.all([
    chrome.cookies.getAll({}),
    getProtectedCookies()
  ]);
  await removeCookies(cookies, new Set(protectedRecords.map((item) => item.id)));
});

chrome.cookies.onChanged.addListener(async ({ removed, cookie }) => {
  if (!removed) return;
  const settings = await getSettings();
  if (!settings.protectAgainstExternalDeletion) return;
  const protectedRecords = await getProtectedCookies();
  const record = protectedRecords.find((item) => item.id === cookieId(cookie));
  if (!record) return;
  try {
    await setCookie(record.cookie);
  } catch (error) {
    console.warn("Ayla não conseguiu restaurar um cookie protegido.", error);
  }
});
