import {
  clearOriginStorage,
  getCurrentSiteCookies,
  getProtectedCookies,
  removeCookies
} from "./lib/cookies.js";

const elements = {
  siteName: document.querySelector("#site-name"),
  siteUrl: document.querySelector("#site-url"),
  favicon: document.querySelector("#site-favicon"),
  count: document.querySelector("#cookie-count"),
  message: document.querySelector("#message"),
  deleteSite: document.querySelector("#delete-site"),
  clearStorage: document.querySelector("#clear-storage")
};

let current = { tab: null, cookies: [] };

function setMessage(message, tone = "") {
  elements.message.textContent = message;
  elements.message.dataset.tone = tone;
}

async function refresh() {
  try {
    current = await getCurrentSiteCookies();
    const url = current.tab?.url && /^https?:/i.test(current.tab.url) ? new URL(current.tab.url) : null;
    elements.siteName.textContent = url?.hostname ?? "Página não compatível";
    elements.siteUrl.textContent = url?.origin ?? "Abra um site HTTP ou HTTPS";
    elements.count.textContent = String(current.cookies.length);
    elements.favicon.textContent = (url?.hostname?.[0] ?? "A").toUpperCase();
    const disabled = !url;
    elements.deleteSite.disabled = disabled || current.cookies.length === 0;
    elements.clearStorage.disabled = disabled;
  } catch (error) {
    setMessage(error.message, "danger");
  }
}

document.querySelector("#open-manager").addEventListener("click", async () => {
  await chrome.runtime.sendMessage({ type: "open-manager" });
  window.close();
});

elements.deleteSite.addEventListener("click", async () => {
  const protectedRecords = await getProtectedCookies();
  const result = await removeCookies(current.cookies, new Set(protectedRecords.map((item) => item.id)));
  setMessage(`${result.removed} removido(s), ${result.skipped} protegido(s).`, result.failed ? "danger" : "success");
  await refresh();
});

elements.clearStorage.addEventListener("click", async () => {
  try {
    await clearOriginStorage(current.tab.url);
    setMessage("LocalStorage removido.", "success");
  } catch (error) {
    setMessage(error.message, "danger");
  }
});

document.querySelector("#refresh").addEventListener("click", refresh);
void refresh();
