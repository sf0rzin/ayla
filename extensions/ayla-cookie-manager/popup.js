import {
  clearOriginStorage,
  cookieId,
  deleteAllCookies,
  getAllCookies,
  getCurrentSiteCookies,
  getProtectedCookies,
  getSettings,
  removeUnprotectedCookies
} from "./lib/cookies.js";
import { confirmWithDialog } from "./lib/dialogs.js";

const elements = {
  siteName: document.querySelector("#site-name"),
  siteUrl: document.querySelector("#site-url"),
  favicon: document.querySelector("#site-favicon"),
  count: document.querySelector("#cookie-count"),
  message: document.querySelector("#message"),
  deleteSite: document.querySelector("#delete-site"),
  clearStorage: document.querySelector("#clear-storage"),
  deleteAll: document.querySelector("#delete-all"),
  confirmDialog: document.querySelector("#confirm-dialog")
};

let current = { tab: null, cookies: [], allCookies: [], settings: null };

function setMessage(message, tone = "") {
  elements.message.textContent = message;
  elements.message.dataset.tone = tone;
}

async function refresh() {
  try {
    const [site, allCookies, settings] = await Promise.all([
      getCurrentSiteCookies(),
      getAllCookies(),
      getSettings()
    ]);
    current = { ...site, allCookies, settings };
    const url = current.tab?.url && /^https?:/i.test(current.tab.url) ? new URL(current.tab.url) : null;
    elements.siteName.textContent = url?.hostname ?? "Página não compatível";
    elements.siteUrl.textContent = url?.origin ?? "Abra um site HTTP ou HTTPS";
    elements.count.textContent = String(current.cookies.length);
    elements.favicon.textContent = (url?.hostname?.[0] ?? "A").toUpperCase();
    const disabled = !url;
    elements.deleteSite.disabled = disabled || current.cookies.length === 0;
    elements.clearStorage.disabled = disabled;
    elements.deleteAll.disabled = current.allCookies.length === 0;
  } catch (error) {
    setMessage(error.message, "danger");
  }
}

document.querySelector("#open-manager").addEventListener("click", async () => {
  await chrome.runtime.sendMessage({ type: "open-manager" });
  window.close();
});

elements.deleteSite.addEventListener("click", async () => {
  try {
    current.settings = await getSettings();
    const protectedRecords = await getProtectedCookies();
    const protectedIds = new Set(protectedRecords.map((item) => item.id));
    const removableCount = current.cookies.filter((cookie) => !protectedIds.has(cookieId(cookie))).length;
    if (!removableCount) {
      setMessage("Nenhum cookie não protegido para apagar.");
      return;
    }
    if (current.settings.confirmDestructiveActions) {
      const confirmed = await confirmWithDialog(elements.confirmDialog, {
        title: "Apagar cookies deste site?",
        description: `${removableCount} cookie(s) não protegido(s) serão apagados. Esta ação não pode ser desfeita.`,
        confirmLabel: `Apagar ${removableCount}`
      });
      if (!confirmed) return;
    }
    const result = await removeUnprotectedCookies(current.cookies);
    setMessage(`${result.removed} removido(s), ${result.skipped} protegido(s).`, result.failed ? "danger" : "success");
    await refresh();
  } catch (error) {
    setMessage(error.message, "danger");
  }
});

document.querySelector("#import-txt").addEventListener("click", async () => {
  try {
    const response = await chrome.runtime.sendMessage({ type: "open-manager", action: "import" });
    if (response?.ok === false) throw new Error(response.error);
    window.close();
  } catch (error) {
    setMessage(error.message, "danger");
  }
});

elements.clearStorage.addEventListener("click", async () => {
  try {
    current.settings = await getSettings();
    if (current.settings.confirmDestructiveActions) {
      const confirmed = await confirmWithDialog(elements.confirmDialog, {
        title: "Limpar o LocalStorage deste site?",
        description: "Todos os dados de LocalStorage da origem atual serão apagados. Esta ação pode desconectar ou redefinir o site e não pode ser desfeita.",
        confirmLabel: "Limpar LocalStorage"
      });
      if (!confirmed) return;
    }
    await clearOriginStorage(current.tab.url);
    setMessage("LocalStorage removido.", "success");
  } catch (error) {
    setMessage(error.message, "danger");
  }
});

elements.deleteAll.addEventListener("click", async () => {
  try {
    const allCookies = await getAllCookies();
    if (!allCookies.length) {
      setMessage("Não há cookies acessíveis neste perfil do Edge.");
      return;
    }
    const protectedRecords = await getProtectedCookies();
    const confirmed = await confirmWithDialog(elements.confirmDialog, {
      title: "Apagar todos os cookies do Edge?",
      description: `${allCookies.length} cookie(s) serão apagados, incluindo ${protectedRecords.length} protegido(s). A proteção será desativada e esta ação não pode ser desfeita.`,
      confirmLabel: `Apagar todos os ${allCookies.length}`
    });
    if (!confirmed) return;

    const result = await deleteAllCookies();
    setMessage(`${result.removed} removido(s); ${result.remaining} restante(s).`, result.failed || result.remaining ? "danger" : "success");
    await refresh();
  } catch (error) {
    setMessage(error.message, "danger");
  }
});

document.querySelector("#refresh").addEventListener("click", refresh);
void refresh();
