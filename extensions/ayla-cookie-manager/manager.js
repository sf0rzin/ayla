import {
  cookieId,
  downloadText,
  exportJson,
  exportNetscape,
  getProtectedCookies,
  getSettings,
  importCookies,
  normalizeDomain,
  parseImport,
  protectCookie,
  removeCookie,
  removeCookies,
  replaceCookie,
  saveSettings,
  setCookie,
  unprotectCookie
} from "./lib/cookies.js";

const state = {
  cookies: [],
  protectedRecords: [],
  settings: null,
  domain: null,
  onlyProtected: false,
  domainSearch: "",
  cookieSearch: "",
  selected: new Set(),
  editing: null,
  transferMode: "export"
};

const $ = (selector) => document.querySelector(selector);
const elements = {
  domainList: $("#domain-list"), domainTotal: $("#domain-total"), protectedTotal: $("#protected-total"),
  domainSearch: $("#domain-search"), cookieSearch: $("#cookie-search"), rows: $("#cookie-rows"),
  empty: $("#empty-state"), selectAll: $("#select-all"), selection: $("#selection-label"),
  title: $("#view-title"), eyebrow: $("#view-eyebrow"), cookiesView: $("#cookies-view"), settingsView: $("#settings-view"),
  cookieDialog: $("#cookie-dialog"), cookieForm: $("#cookie-form"), cookieError: $("#cookie-form-error"),
  transferDialog: $("#transfer-dialog"), transferForm: $("#transfer-form"), transferError: $("#transfer-error"), toast: $("#toast")
};

const protectedIds = () => new Set(state.protectedRecords.map((item) => item.id));
const visibleCookies = () => {
  const protectedSet = protectedIds();
  const query = state.cookieSearch.trim().toLowerCase();
  return state.cookies.filter((cookie) => {
    if (state.domain && normalizeDomain(cookie.domain) !== state.domain && !normalizeDomain(cookie.domain).endsWith(`.${state.domain}`)) return false;
    if (state.onlyProtected && !protectedSet.has(cookieId(cookie))) return false;
    if (query && !`${cookie.name}\n${cookie.value}\n${cookie.domain}`.toLowerCase().includes(query)) return false;
    return true;
  });
};

function escapeHtml(value) {
  return String(value).replace(/[&<>'"]/g, (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" }[char]));
}

function toast(message, tone = "") {
  elements.toast.textContent = message;
  elements.toast.className = `toast visible ${tone}`;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => { elements.toast.className = "toast"; }, 2600);
}

function domainGroups() {
  const groups = new Map();
  for (const cookie of state.cookies) {
    const domain = normalizeDomain(cookie.domain);
    groups.set(domain, (groups.get(domain) ?? 0) + 1);
  }
  return [...groups.entries()].sort(([a], [b]) => a.localeCompare(b));
}

function renderDomains() {
  const query = state.domainSearch.trim().toLowerCase();
  const groups = domainGroups().filter(([domain]) => domain.includes(query));
  elements.domainTotal.textContent = String(domainGroups().length);
  elements.protectedTotal.textContent = String(state.protectedRecords.length);
  const allActive = !state.domain && !state.onlyProtected;
  elements.domainList.innerHTML = `<button data-domain="" class="${allActive ? "active" : ""}"><span class="domain-icon">A</span><span>Todos os cookies</span><small>${state.cookies.length}</small></button>` + groups.map(([domain, count]) =>
    `<button data-domain="${escapeHtml(domain)}" class="${state.domain === domain ? "active" : ""}"><span class="domain-icon">${escapeHtml(domain[0] || "?")}</span><span title="${escapeHtml(domain)}">${escapeHtml(domain)}</span><small>${count}</small></button>`
  ).join("");
}

function expirationLabel(cookie) {
  if (cookie.session || !cookie.expirationDate) return "Sessão";
  const date = new Date(cookie.expirationDate * 1000);
  return Number.isNaN(date.valueOf()) ? "Inválida" : date.toLocaleString("pt-BR", { dateStyle: "short", timeStyle: "short" });
}

function renderRows() {
  const cookies = visibleCookies();
  const protectedSet = protectedIds();
  elements.rows.innerHTML = cookies.map((cookie) => {
    const id = cookieId(cookie);
    const selected = state.selected.has(id);
    const flags = [cookie.secure && "Secure", cookie.httpOnly && "HttpOnly", cookie.partitionKey && "CHIPS"].filter(Boolean);
    return `<tr data-cookie-id="${escapeHtml(id)}" class="${selected ? "selected" : ""}">
      <td><input type="checkbox" data-select-cookie="${escapeHtml(id)}" ${selected ? "checked" : ""}></td>
      <td class="cookie-name" title="${escapeHtml(cookie.name)}">${protectedSet.has(id) ? '<span class="tag protected-tag">◆</span>' : ""}${escapeHtml(cookie.name || "(vazio)")}</td>
      <td title="${escapeHtml(cookie.domain)}">${escapeHtml(cookie.domain)}</td><td>${escapeHtml(cookie.path)}</td>
      <td title="${escapeHtml(expirationLabel(cookie))}">${escapeHtml(expirationLabel(cookie))}</td>
      <td>${flags.map((flag) => `<span class="tag">${flag}</span>`).join("") || '<span class="tag">—</span>'}</td>
      <td><button class="row-menu" data-edit-cookie="${escapeHtml(id)}" title="Editar">•••</button></td></tr>`;
  }).join("");
  elements.empty.classList.toggle("hidden", cookies.length > 0);
  const selectedVisible = cookies.filter((cookie) => state.selected.has(cookieId(cookie))).length;
  elements.selectAll.checked = cookies.length > 0 && selectedVisible === cookies.length;
  elements.selectAll.indeterminate = selectedVisible > 0 && selectedVisible < cookies.length;
  elements.selection.textContent = selectedVisible ? `${selectedVisible} selecionado(s)` : "Nenhum selecionado";
}

function renderSummary() {
  const cookies = visibleCookies();
  const protectedSet = protectedIds();
  $("#summary-total").textContent = String(cookies.length);
  $("#summary-domains").textContent = String(new Set(cookies.map((cookie) => normalizeDomain(cookie.domain))).size);
  $("#summary-protected").textContent = String(cookies.filter((cookie) => protectedSet.has(cookieId(cookie))).length);
  $("#summary-session").textContent = String(cookies.filter((cookie) => cookie.session).length);
}

function renderHeading() {
  if (state.onlyProtected) {
    elements.eyebrow.textContent = "COOKIES PROTEGIDOS"; elements.title.textContent = "Protegidos";
  } else if (state.domain) {
    elements.eyebrow.textContent = "DOMÍNIO E SUBDOMÍNIOS"; elements.title.textContent = state.domain;
  } else {
    elements.eyebrow.textContent = "TODOS OS COOKIES"; elements.title.textContent = "Visão geral";
  }
}

function render() {
  renderDomains(); renderHeading(); renderSummary(); renderRows();
  $("#show-protected").classList.toggle("active", state.onlyProtected);
}

async function refresh({ preserveSelection = false } = {}) {
  try {
    [state.cookies, state.protectedRecords, state.settings] = await Promise.all([
      chrome.cookies.getAll({}), getProtectedCookies(), getSettings()
    ]);
    if (!preserveSelection) state.selected.clear();
    state.selected = new Set([...state.selected].filter((id) => state.cookies.some((cookie) => cookieId(cookie) === id)));
    $("#setting-protection").checked = state.settings.protectAgainstExternalDeletion;
    $("#setting-autoclear").checked = state.settings.autoClearOnStartup;
    $("#setting-confirm").checked = state.settings.confirmDestructiveActions;
    render();
  } catch (error) { toast(error.message, "danger"); }
}

function findCookie(id) { return state.cookies.find((cookie) => cookieId(cookie) === id); }

function openCookieDialog(cookie = null) {
  state.editing = cookie;
  elements.cookieForm.reset();
  elements.cookieError.textContent = "";
  $("#cookie-dialog-title").textContent = cookie ? "Editar cookie" : "Novo cookie";
  const f = elements.cookieForm.elements;
  f.name.value = cookie?.name ?? ""; f.value.value = cookie?.value ?? "";
  f.domain.value = cookie?.domain ?? state.domain ?? ""; f.path.value = cookie?.path ?? "/";
  f.sameSite.value = cookie?.sameSite ?? "unspecified"; f.hostOnly.checked = cookie?.hostOnly ?? true;
  f.secure.checked = cookie?.secure ?? true; f.httpOnly.checked = cookie?.httpOnly ?? false;
  f.session.checked = cookie?.session ?? true; f.partitionSite.value = cookie?.partitionKey?.topLevelSite ?? "";
  f.expiration.disabled = f.session.checked;
  if (cookie?.expirationDate) {
    const date = new Date(cookie.expirationDate * 1000);
    f.expiration.value = new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
  }
  elements.cookieDialog.showModal();
}

async function saveCookieForm() {
  const f = elements.cookieForm.elements;
  const partitionSite = f.partitionSite.value.trim();
  const replacement = {
    domain: f.hostOnly.checked ? normalizeDomain(f.domain.value) : (f.domain.value.startsWith(".") ? f.domain.value : `.${normalizeDomain(f.domain.value)}`),
    hostOnly: f.hostOnly.checked, name: f.name.value, value: f.value.value, path: f.path.value || "/",
    sameSite: f.sameSite.value, secure: f.secure.checked, httpOnly: f.httpOnly.checked,
    session: f.session.checked, expirationDate: f.session.checked ? undefined : new Date(f.expiration.value).getTime() / 1000,
    storeId: state.editing?.storeId, partitionKey: partitionSite ? { topLevelSite: new URL(partitionSite).origin } : undefined
  };
  if (!replacement.session && !Number.isFinite(replacement.expirationDate)) throw new Error("Informe uma data de expiração válida.");
  if (replacement.sameSite === "no_restriction" && !replacement.secure) throw new Error("SameSite=None exige a flag Secure.");
  const wasProtected = state.editing && protectedIds().has(cookieId(state.editing));
  if (state.editing) await replaceCookie(state.editing, replacement, wasProtected); else await setCookie(replacement);
}

async function deleteSelection() {
  const selected = state.cookies.filter((cookie) => state.selected.has(cookieId(cookie)));
  if (!selected.length) return;
  if (state.settings.confirmDestructiveActions && !confirm(`Apagar ${selected.length} cookie(s)? Cookies protegidos serão preservados.`)) return;
  const result = await removeCookies(selected, protectedIds());
  toast(`${result.removed} removido(s); ${result.skipped} protegido(s).`, result.failed ? "danger" : "success");
  await refresh();
}

async function protectSelection() {
  const selected = state.cookies.filter((cookie) => state.selected.has(cookieId(cookie)));
  await Promise.all(selected.map((cookie) => protectedIds().has(cookieId(cookie)) ? unprotectCookie(cookie) : protectCookie(cookie)));
  toast("Proteção atualizada.", "success");
  await refresh({ preserveSelection: true });
}

function openTransfer(mode) {
  state.transferMode = mode; elements.transferError.textContent = "";
  const exporting = mode === "export";
  $("#export-panel").classList.toggle("hidden", !exporting); $("#import-panel").classList.toggle("hidden", exporting);
  $("#transfer-eyebrow").textContent = exporting ? "BACKUP" : "RESTAURAÇÃO";
  $("#transfer-title").textContent = exporting ? "Exportar cookies" : "Importar cookies";
  $("#transfer-submit").textContent = exporting ? "Exportar" : "Importar";
  $("#export-description").textContent = `${visibleCookies().length} cookie(s) da visualização atual serão exportados.`;
  elements.transferDialog.showModal();
}

elements.domainList.addEventListener("click", (event) => {
  const button = event.target.closest("button[data-domain]"); if (!button) return;
  state.domain = button.dataset.domain || null; state.onlyProtected = false; state.selected.clear(); render();
});
$("#show-protected").addEventListener("click", () => { state.domain = null; state.onlyProtected = true; state.selected.clear(); elements.settingsView.classList.add("hidden"); elements.cookiesView.classList.remove("hidden"); render(); });
$("#show-settings").addEventListener("click", () => { elements.cookiesView.classList.add("hidden"); elements.settingsView.classList.remove("hidden"); elements.eyebrow.textContent = "PREFERÊNCIAS"; elements.title.textContent = "Configurações"; });
elements.domainSearch.addEventListener("input", () => { state.domainSearch = elements.domainSearch.value; renderDomains(); });
elements.cookieSearch.addEventListener("input", () => { state.cookieSearch = elements.cookieSearch.value; renderSummary(); renderRows(); });
document.addEventListener("keydown", (event) => { if (event.key === "/" && !/INPUT|TEXTAREA|SELECT/.test(document.activeElement.tagName)) { event.preventDefault(); elements.domainSearch.focus(); } });
elements.selectAll.addEventListener("change", () => { for (const cookie of visibleCookies()) elements.selectAll.checked ? state.selected.add(cookieId(cookie)) : state.selected.delete(cookieId(cookie)); renderRows(); });
elements.rows.addEventListener("click", (event) => {
  const edit = event.target.closest("[data-edit-cookie]"); if (edit) { openCookieDialog(findCookie(edit.dataset.editCookie)); return; }
  const checkbox = event.target.closest("[data-select-cookie]");
  const row = event.target.closest("tr[data-cookie-id]"); if (!row) return;
  if (checkbox) checkbox.checked ? state.selected.add(row.dataset.cookieId) : state.selected.delete(row.dataset.cookieId);
  else openCookieDialog(findCookie(row.dataset.cookieId)); renderRows();
});
$("#new-cookie").addEventListener("click", () => openCookieDialog());
$("#refresh").addEventListener("click", () => refresh());
$("#delete-selected").addEventListener("click", deleteSelection);
$("#protect-selected").addEventListener("click", protectSelection);
elements.cookieForm.elements.session.addEventListener("change", () => { elements.cookieForm.elements.expiration.disabled = elements.cookieForm.elements.session.checked; });
elements.cookieForm.addEventListener("submit", async (event) => { event.preventDefault(); try { await saveCookieForm(); elements.cookieDialog.close(); toast("Cookie salvo.", "success"); await refresh(); } catch (error) { elements.cookieError.textContent = error.message; } });
$("#export-open").addEventListener("click", () => openTransfer("export")); $("#import-open").addEventListener("click", () => openTransfer("import"));
$("#import-file").addEventListener("change", async (event) => { const [file] = event.target.files; if (file) $("#import-text").value = await file.text(); });
elements.transferForm.addEventListener("submit", async (event) => {
  event.preventDefault(); elements.transferError.textContent = "";
  try {
    if (state.transferMode === "export") {
      const cookies = visibleCookies(), format = $("#export-format").value;
      downloadText(format === "json" ? "ayla-cookies.json" : "cookies.txt", format === "json" ? exportJson(cookies) : exportNetscape(cookies), format === "json" ? "application/json" : "text/plain");
      toast(`${cookies.length} cookie(s) exportado(s).`, "success");
    } else {
      const cookies = parseImport($("#import-text").value); const result = await importCookies(cookies);
      toast(`${result.imported} importado(s), ${result.failed} falha(s).`, result.failed ? "danger" : "success"); await refresh();
    }
    elements.transferDialog.close();
  } catch (error) { elements.transferError.textContent = error.message; }
});
for (const [selector, key] of [["#setting-protection", "protectAgainstExternalDeletion"], ["#setting-autoclear", "autoClearOnStartup"], ["#setting-confirm", "confirmDestructiveActions"]]) {
  $(selector).addEventListener("change", async (event) => { state.settings[key] = event.target.checked; state.settings = await saveSettings(state.settings); toast("Configuração salva.", "success"); });
}
document.querySelectorAll("[data-close-dialog]").forEach((button) => {
  button.addEventListener("click", () => document.querySelector(`#${button.dataset.closeDialog}`).close());
});
chrome.cookies.onChanged.addListener(() => { clearTimeout(refresh.timer); refresh.timer = setTimeout(() => refresh({ preserveSelection: true }), 160); });
void refresh();
