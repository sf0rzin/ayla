import {
  cookieId,
  deleteAllCookies,
  downloadText,
  exportJson,
  exportNetscape,
  getAllCookies,
  getCookiesForTab,
  getProtectedCookies,
  getSettings,
  importCookies,
  normalizeDomain,
  parseImportReport,
  protectCookies,
  removeUnprotectedCookies,
  resolveImportCookies,
  replaceCookie,
  saveSettingsPatch,
  setCookie,
  settingsStorageKey,
  unprotectCookies
} from "./lib/cookies.js";
import { closeModal, confirmWithDialog, openModal } from "./lib/dialogs.js";

const MAX_IMPORT_FILE_BYTES = 32 * 1024 * 1024;
let importAnalysisGeneration = 0;

const state = {
  cookies: [],
  protectedRecords: [],
  settings: null,
  domain: null,
  onlyProtected: false,
  settingsOpen: false,
  domainSearch: "",
  cookieSearch: "",
  selected: new Set(),
  editing: null,
  transferMode: "export",
  importReport: null,
  importFilename: ""
};

const $ = (selector) => document.querySelector(selector);
const elements = {
  domainList: $("#domain-list"), domainTotal: $("#domain-total"), protectedTotal: $("#protected-total"),
  domainSearch: $("#domain-search"), cookieSearch: $("#cookie-search"), rows: $("#cookie-rows"),
  empty: $("#empty-state"), selectAll: $("#select-all"), selection: $("#selection-label"),
  title: $("#view-title"), eyebrow: $("#view-eyebrow"), cookiesView: $("#cookies-view"), settingsView: $("#settings-view"),
  cookieDialog: $("#cookie-dialog"), cookieForm: $("#cookie-form"), cookieError: $("#cookie-form-error"),
  transferDialog: $("#transfer-dialog"), transferForm: $("#transfer-form"), transferError: $("#transfer-error"),
  confirmDialog: $("#confirm-dialog"), toast: $("#toast")
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
  const focusedDomain = document.activeElement?.dataset?.domain;
  const query = state.domainSearch.trim().toLowerCase();
  const groups = domainGroups().filter(([domain]) => domain.includes(query));
  elements.domainTotal.textContent = String(domainGroups().length);
  elements.protectedTotal.textContent = String(state.protectedRecords.length);
  const allActive = !state.domain && !state.onlyProtected;
  elements.domainList.innerHTML = `<button type="button" data-domain="" class="${allActive ? "active" : ""}" ${allActive ? 'aria-current="page"' : ""}><span class="domain-icon" aria-hidden="true">A</span><span>Todos os cookies</span><small>${state.cookies.length}</small></button>` + groups.map(([domain, count]) =>
    `<button type="button" data-domain="${escapeHtml(domain)}" class="${state.domain === domain ? "active" : ""}" ${state.domain === domain ? 'aria-current="page"' : ""}><span class="domain-icon" aria-hidden="true">${escapeHtml(domain[0] || "?")}</span><span title="${escapeHtml(domain)}">${escapeHtml(domain)}</span><small>${count}</small></button>`
  ).join("");
  if (focusedDomain !== undefined) {
    queueMicrotask(() => [...elements.domainList.querySelectorAll("[data-domain]")]
      .find((button) => button.dataset.domain === focusedDomain)?.focus());
  }
}

function runAction(action) {
  void action().catch((error) => toast(error.message, "danger"));
}

function expirationLabel(cookie) {
  if (cookie.session || !cookie.expirationDate) return "Sessão";
  const date = new Date(cookie.expirationDate * 1000);
  return Number.isNaN(date.valueOf()) ? "Inválida" : date.toLocaleString("pt-BR", { dateStyle: "short", timeStyle: "short" });
}

function renderRows() {
  const focusedCookieId = document.activeElement?.dataset?.selectCookie ?? document.activeElement?.dataset?.editCookie;
  const focusedControl = document.activeElement?.dataset?.selectCookie !== undefined ? "select" : "edit";
  const cookies = visibleCookies();
  const protectedSet = protectedIds();
  elements.rows.innerHTML = cookies.map((cookie) => {
    const id = cookieId(cookie);
    const selected = state.selected.has(id);
    const flags = [cookie.secure && "Secure", cookie.httpOnly && "HttpOnly", cookie.partitionKey && "CHIPS"].filter(Boolean);
    const accessibleName = `Selecionar ${cookie.name || "cookie sem nome"} de ${cookie.domain}`;
    return `<tr data-cookie-id="${escapeHtml(id)}" class="${selected ? "selected" : ""}">
      <td><input type="checkbox" data-select-cookie="${escapeHtml(id)}" aria-label="${escapeHtml(accessibleName)}" ${selected ? "checked" : ""}></td>
      <td class="cookie-name" title="${escapeHtml(cookie.name)}">${protectedSet.has(id) ? '<span class="tag protected-tag">◆</span>' : ""}${escapeHtml(cookie.name || "(vazio)")}</td>
      <td title="${escapeHtml(cookie.domain)}">${escapeHtml(cookie.domain)}</td><td>${escapeHtml(cookie.path)}</td>
      <td title="${escapeHtml(expirationLabel(cookie))}">${escapeHtml(expirationLabel(cookie))}</td>
      <td>${flags.map((flag) => `<span class="tag">${flag}</span>`).join("") || '<span class="tag">—</span>'}</td>
      <td><button type="button" class="row-menu" data-edit-cookie="${escapeHtml(id)}" aria-label="Editar ${escapeHtml(cookie.name || "cookie sem nome")} de ${escapeHtml(cookie.domain)}">•••</button></td></tr>`;
  }).join("");
  elements.empty.classList.toggle("hidden", cookies.length > 0);
  const selectedVisible = cookies.filter((cookie) => state.selected.has(cookieId(cookie))).length;
  const selectedTotal = state.cookies.filter((cookie) => state.selected.has(cookieId(cookie))).length;
  const selectedProtected = state.cookies.filter((cookie) => state.selected.has(cookieId(cookie)) && protectedSet.has(cookieId(cookie))).length;
  elements.selectAll.checked = cookies.length > 0 && selectedVisible === cookies.length;
  elements.selectAll.indeterminate = selectedVisible > 0 && selectedVisible < cookies.length;
  elements.selection.textContent = selectedTotal
    ? `${selectedTotal} selecionado(s)${selectedTotal !== selectedVisible ? `; ${selectedVisible} visível(is)` : ""}`
    : "Nenhum selecionado";
  $("#protect-selected").disabled = selectedTotal === 0 || selectedProtected === selectedTotal;
  $("#unprotect-selected").disabled = selectedProtected === 0;
  $("#delete-selected").disabled = selectedTotal === 0;
  if (focusedCookieId !== undefined) {
    const attribute = focusedControl === "select" ? "selectCookie" : "editCookie";
    queueMicrotask(() => [...elements.rows.querySelectorAll(`[data-${focusedControl}-cookie]`)]
      .find((control) => control.dataset[attribute] === focusedCookieId)?.focus());
  }
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
  renderDomains();
  if (state.settingsOpen) {
    elements.eyebrow.textContent = "PREFERÊNCIAS";
    elements.title.textContent = "Configurações";
  } else {
    renderHeading();
  }
  renderSummary(); renderRows();
  $("#show-protected").classList.toggle("active", state.onlyProtected && !state.settingsOpen);
  $("#show-protected").setAttribute("aria-pressed", String(state.onlyProtected && !state.settingsOpen));
  $("#show-settings").setAttribute("aria-pressed", String(state.settingsOpen));
}

async function refresh({ preserveSelection = false } = {}) {
  try {
    [state.cookies, state.protectedRecords, state.settings] = await Promise.all([
      getAllCookies(), getProtectedCookies(), getSettings()
    ]);
    if (!preserveSelection) state.selected.clear();
    state.selected = new Set([...state.selected].filter((id) => state.cookies.some((cookie) => cookieId(cookie) === id)));
    const visibleIds = new Set(visibleCookies().map(cookieId));
    state.selected = new Set([...state.selected].filter((id) => visibleIds.has(id)));
    renderSettings();
    $("#delete-all").disabled = state.cookies.length === 0;
    render();
    if (elements.transferDialog.open && state.transferMode === "export") renderExportOptions();
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
  openModal(elements.cookieDialog, { initialFocus: elements.cookieForm.elements.name });
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
  state.protectedRecords = await getProtectedCookies();
  const protectedSet = protectedIds();
  const protectedCount = selected.filter((cookie) => protectedSet.has(cookieId(cookie))).length;
  const removableCount = selected.length - protectedCount;
  if (!removableCount) {
    toast("Todos os cookies selecionados estão protegidos.");
    return;
  }
  state.settings = await getSettings();
  renderSettings();
  if (state.settings.confirmDestructiveActions) {
    const confirmed = await confirmWithDialog(elements.confirmDialog, {
      title: "Apagar cookies selecionados?",
      description: `${removableCount} cookie(s) serão apagados desta seleção. ${protectedCount} protegido(s) serão preservados. Esta ação não pode ser desfeita.`,
      confirmLabel: `Apagar ${removableCount}`
    });
    if (!confirmed) return;
  }
  const result = await removeUnprotectedCookies(selected);
  toast(`${result.removed} removido(s); ${result.skipped} protegido(s).`, result.failed ? "danger" : "success");
  await refresh();
}

function renderSettings() {
  if (!state.settings) return;
  $("#setting-protection").checked = state.settings.protectAgainstExternalDeletion;
  $("#setting-autoclear").checked = state.settings.autoClearOnStartup;
  $("#setting-confirm").checked = state.settings.confirmDestructiveActions;
}

async function deleteEveryCookie() {
  const [cookies, protectedRecords] = await Promise.all([
    getAllCookies(),
    getProtectedCookies()
  ]);
  if (!cookies.length) {
    toast("Não há cookies acessíveis neste perfil do Edge.");
    return;
  }
  const confirmed = await confirmWithDialog(elements.confirmDialog, {
    title: "Apagar todos os cookies do Edge?",
    description: `${cookies.length} cookie(s) acessível(is) serão apagados, incluindo ${protectedRecords.length} protegido(s). A proteção externa será desativada e seus snapshots serão removidos para impedir restauração. Esta ação não pode ser desfeita.`,
    confirmLabel: `Apagar todos os ${cookies.length}`
  });
  if (!confirmed) return;

  const result = await deleteAllCookies();
  state.selected.clear();
  const tone = result.failed || result.remaining ? "danger" : "success";
  toast(`${result.removed} removido(s); ${result.failed} falha(s); ${result.remaining} restante(s).`, tone);
  await refresh();
}

async function protectSelection() {
  const protectedSet = protectedIds();
  const selected = state.cookies.filter((cookie) => state.selected.has(cookieId(cookie)) && !protectedSet.has(cookieId(cookie)));
  if (!selected.length) return;
  await protectCookies(selected);
  toast(`${selected.length} cookie(s) protegido(s).`, "success");
  await refresh({ preserveSelection: true });
}

async function unprotectSelection() {
  const protectedSet = protectedIds();
  const selected = state.cookies.filter((cookie) => state.selected.has(cookieId(cookie)) && protectedSet.has(cookieId(cookie)));
  if (!selected.length) return;
  await unprotectCookies(selected);
  toast(`${selected.length} cookie(s) desprotegido(s).`, "success");
  await refresh({ preserveSelection: true });
}

function openTransfer(mode) {
  state.transferMode = mode; elements.transferError.textContent = "";
  const exporting = mode === "export";
  $("#export-panel").classList.toggle("hidden", !exporting); $("#import-panel").classList.toggle("hidden", exporting);
  $("#transfer-eyebrow").textContent = exporting ? "BACKUP" : "RESTAURAÇÃO";
  $("#transfer-title").textContent = exporting ? "Exportar cookies" : "Importar cookies";
  $("#transfer-submit").textContent = exporting ? "Exportar" : "Importar todos";
  $("#transfer-submit").disabled = !exporting;
  $("#export-description").textContent = `${visibleCookies().length} cookie(s) da visualização atual serão exportados.`;
  if (exporting) renderExportOptions(); else resetImportEditor();
  openModal(elements.transferDialog, { initialFocus: exporting ? "#export-format" : "#import-file" });
}

function renderExportOptions() {
  const cookies = visibleCookies();
  const chipsCount = cookies.filter((cookie) => cookie.partitionKey).length;
  const format = $("#export-format");
  const netscape = format.querySelector('option[value="netscape"]');
  netscape.disabled = chipsCount > 0;
  if (netscape.disabled && format.value === "netscape") format.value = "json";
  $("#export-warning").textContent = chipsCount
    ? `${chipsCount} cookie(s) CHIPS exigem JSON; o formato Netscape foi desativado para evitar perda de partição.`
    : "JSON preserva store e metadados do Chromium; Netscape é indicado apenas para cookies não particionados.";
}

function resetImportEditor() {
  importAnalysisGeneration += 1;
  state.importReport = null;
  state.importFilename = "";
  $("#import-file").value = "";
  $("#import-text").value = "";
  $("#import-file-name").textContent = "Nenhum arquivo selecionado";
  $("#import-summary").textContent = "Escolha um TXT para analisar todos os registros.";
  $("#import-summary").dataset.tone = "";
  $("#transfer-submit").disabled = true;
}

async function analyzeImportText() {
  const generation = ++importAnalysisGeneration;
  const text = $("#import-text").value;
  const bytes = new Blob([text]).size;
  if (!text.trim()) {
    state.importReport = null;
    $("#import-summary").textContent = "O arquivo não contém cookies.";
    $("#import-summary").dataset.tone = "warning";
    $("#transfer-submit").disabled = true;
    return null;
  }
  if (bytes > MAX_IMPORT_FILE_BYTES) {
    throw new Error("O arquivo excede 32 MiB. Divida-o em partes menores antes de importar.");
  }

  $("#import-summary").textContent = "Analisando cookies e verificando substituições…";
  $("#import-summary").dataset.tone = "";
  $("#transfer-submit").disabled = true;
  const report = parseImportReport(text);
  const [resolvedCookies, currentCookies] = await Promise.all([
    resolveImportCookies(report.cookies),
    getAllCookies()
  ]);
  if (generation !== importAnalysisGeneration) return null;
  const deduplicated = new Map();
  let resolvedDuplicates = report.duplicates;
  for (const cookie of resolvedCookies) {
    const id = cookieId(cookie);
    if (deduplicated.has(id)) {
      resolvedDuplicates += 1;
      deduplicated.delete(id);
    }
    deduplicated.set(id, cookie);
  }
  const cookies = [...deduplicated.values()];
  const currentIds = new Set(currentCookies.map(cookieId));
  const overwrites = cookies.filter((cookie) => currentIds.has(cookieId(cookie))).length;
  state.importReport = { ...report, cookies, duplicates: resolvedDuplicates, overwrites };
  const details = [
    `${cookies.length} válido(s)`,
    overwrites ? `${overwrites} substituição(ões)` : "nenhuma substituição",
    resolvedDuplicates ? `${resolvedDuplicates} duplicata(s)` : "sem duplicatas",
    report.invalid.length ? `${report.invalid.length} linha(s) inválida(s)` : "sem linhas inválidas"
  ];
  $("#import-summary").textContent = details.join(" · ");
  $("#import-summary").dataset.tone = report.invalid.length ? "warning" : "success";
  $("#transfer-submit").disabled = cookies.length === 0;
  return state.importReport;
}

elements.domainList.addEventListener("click", (event) => {
  const button = event.target.closest("button[data-domain]"); if (!button) return;
  state.domain = button.dataset.domain || null; state.onlyProtected = false; state.settingsOpen = false; state.selected.clear(); elements.settingsView.classList.add("hidden"); elements.cookiesView.classList.remove("hidden"); render();
});
$("#show-protected").addEventListener("click", () => { state.domain = null; state.onlyProtected = true; state.settingsOpen = false; state.selected.clear(); elements.settingsView.classList.add("hidden"); elements.cookiesView.classList.remove("hidden"); render(); });
$("#show-settings").addEventListener("click", () => {
  state.onlyProtected = false;
  state.settingsOpen = true;
  elements.cookiesView.classList.add("hidden");
  elements.settingsView.classList.remove("hidden");
  render();
});
elements.domainSearch.addEventListener("input", () => { state.domainSearch = elements.domainSearch.value; renderDomains(); });
elements.cookieSearch.addEventListener("input", () => {
  state.cookieSearch = elements.cookieSearch.value;
  // A hidden selection must never survive a filter change and be deleted invisibly.
  state.selected.clear();
  renderSummary();
  renderRows();
});
document.addEventListener("keydown", (event) => { if (event.key === "/" && !/INPUT|TEXTAREA|SELECT/.test(document.activeElement.tagName)) { event.preventDefault(); elements.domainSearch.focus(); } });
elements.selectAll.addEventListener("change", () => { for (const cookie of visibleCookies()) elements.selectAll.checked ? state.selected.add(cookieId(cookie)) : state.selected.delete(cookieId(cookie)); renderRows(); });
elements.rows.addEventListener("click", (event) => {
  const edit = event.target.closest("[data-edit-cookie]"); if (edit) { openCookieDialog(findCookie(edit.dataset.editCookie)); return; }
  const checkbox = event.target.closest("[data-select-cookie]");
  const row = event.target.closest("tr[data-cookie-id]"); if (!row) return;
  if (!checkbox) return;
  checkbox.checked ? state.selected.add(row.dataset.cookieId) : state.selected.delete(row.dataset.cookieId);
  renderRows();
});
$("#new-cookie").addEventListener("click", () => openCookieDialog());
$("#refresh").addEventListener("click", () => refresh());
$("#delete-selected").addEventListener("click", () => runAction(deleteSelection));
$("#delete-all").addEventListener("click", () => runAction(deleteEveryCookie));
$("#protect-selected").addEventListener("click", () => runAction(protectSelection));
$("#unprotect-selected").addEventListener("click", () => runAction(unprotectSelection));
elements.cookieForm.elements.session.addEventListener("change", () => { elements.cookieForm.elements.expiration.disabled = elements.cookieForm.elements.session.checked; });
elements.cookieForm.addEventListener("submit", async (event) => { event.preventDefault(); try { await saveCookieForm(); closeModal(elements.cookieDialog, "saved"); toast("Cookie salvo.", "success"); await refresh(); } catch (error) { elements.cookieError.textContent = error.message; } });
$("#export-open").addEventListener("click", () => openTransfer("export")); $("#import-open").addEventListener("click", () => openTransfer("import"));
$("#export-format").addEventListener("change", renderExportOptions);
$("#import-file").addEventListener("change", async (event) => {
  const [file] = event.target.files;
  if (!file) return;
  elements.transferError.textContent = "";
  try {
    if (file.size > MAX_IMPORT_FILE_BYTES) throw new Error("O arquivo excede 32 MiB. Divida-o em partes menores antes de importar.");
    state.importFilename = file.name;
    $("#import-file-name").textContent = file.name;
    $("#import-text").value = await file.text();
    await analyzeImportText();
  } catch (error) {
    state.importReport = null;
    $("#transfer-submit").disabled = true;
    elements.transferError.textContent = error.message;
  }
});
$("#import-text").addEventListener("input", () => {
  clearTimeout(analyzeImportText.timer);
  analyzeImportText.timer = setTimeout(async () => {
    elements.transferError.textContent = "";
    try { await analyzeImportText(); } catch (error) {
      state.importReport = null;
      $("#transfer-submit").disabled = true;
      elements.transferError.textContent = error.message;
    }
  }, 180);
});
elements.transferForm.addEventListener("submit", async (event) => {
  event.preventDefault(); elements.transferError.textContent = "";
  try {
    if (state.transferMode === "export") {
      const cookies = visibleCookies(), format = $("#export-format").value;
      downloadText(format === "json" ? "ayla-cookies.json" : "cookies.txt", format === "json" ? exportJson(cookies) : exportNetscape(cookies), format === "json" ? "application/json" : "text/plain");
      toast(`${cookies.length} cookie(s) exportado(s).`, "success");
    } else {
      const report = await analyzeImportText();
      if (!report?.cookies.length) throw new Error("Nenhum cookie válido foi encontrado.");
      if (report.overwrites || report.invalid.length) {
        const confirmed = await confirmWithDialog(elements.confirmDialog, {
          title: "Importar todos os cookies válidos?",
          description: `${report.cookies.length} cookie(s) serão gravados; ${report.overwrites} existente(s) serão substituídos; ${report.invalid.length} linha(s) inválida(s) serão ignoradas.`,
          confirmLabel: `Importar ${report.cookies.length}`
        });
        if (!confirmed) return;
      }
      let result;
      try {
        result = await importCookies(report.cookies);
      } catch (error) {
        await refresh();
        throw error;
      }
      const tone = result.failed || report.invalid.length ? "danger" : "success";
      toast(`${result.imported} importado(s); ${result.failed} falha(s); ${report.invalid.length} linha(s) ignorada(s).`, tone);
      await refresh();
      resetImportEditor();
    }
    closeModal(elements.transferDialog, "completed");
  } catch (error) { elements.transferError.textContent = error.message; }
});
for (const [selector, key] of [["#setting-protection", "protectAgainstExternalDeletion"], ["#setting-autoclear", "autoClearOnStartup"], ["#setting-confirm", "confirmDestructiveActions"]]) {
  $(selector).addEventListener("change", (event) => runAction(async () => {
    state.settings = await saveSettingsPatch({ [key]: event.target.checked });
    renderSettings();
    toast("Configuração salva.", "success");
  }));
}
document.querySelectorAll("[data-close-dialog]").forEach((button) => {
  button.addEventListener("click", () => closeModal(document.querySelector(`#${button.dataset.closeDialog}`)));
});
chrome.cookies.onChanged.addListener(() => { clearTimeout(refresh.timer); refresh.timer = setTimeout(() => refresh({ preserveSelection: true }), 160); });
chrome.storage.onChanged.addListener((changes, areaName) => {
  if (areaName !== "local" || !changes[settingsStorageKey()]) return;
  clearTimeout(renderSettings.timer);
  renderSettings.timer = setTimeout(() => {
    runAction(async () => {
      state.settings = await getSettings();
      renderSettings();
    });
  }, 40);
});

async function handleManagerAction(action) {
  if (action?.type === "import") {
    if (elements.transferDialog.open && state.transferMode === "import") {
      (state.importReport ? $("#transfer-submit") : $("#import-file")).focus();
      return;
    }
    openTransfer("import");
    return;
  }
  if (action?.type !== "delete-site" || !Number.isInteger(action.tabId)) return;

  const tabId = action.tabId;
  try {
    const { tab, cookies } = await getCookiesForTab(tabId);
    state.protectedRecords = await getProtectedCookies();
    const protectedSet = protectedIds();
    const removableCount = cookies.filter((cookie) => !protectedSet.has(cookieId(cookie))).length;
    if (!removableCount) {
      toast("Nenhum cookie não protegido para apagar.");
      return;
    }
    const site = new URL(tab.url).hostname;
    state.settings = await getSettings();
    renderSettings();
    if (state.settings.confirmDestructiveActions) {
      const confirmed = await confirmWithDialog(elements.confirmDialog, {
        title: `Apagar cookies de ${site}?`,
        description: `${removableCount} cookie(s) não protegido(s) deste site serão apagados. Esta ação não pode ser desfeita.`,
        confirmLabel: `Apagar ${removableCount}`
      });
      if (!confirmed) return;
    }
    const result = await removeUnprotectedCookies(cookies);
    toast(`${result.removed} removido(s); ${result.skipped} protegido(s).`, result.failed ? "danger" : "success");
    await refresh();
  } catch (error) {
    toast(error.message, "danger");
  }
}

async function handlePendingAction() {
  const currentUrl = new URL(location.href);
  const type = currentUrl.searchParams.get("action");
  const tabId = Number(currentUrl.searchParams.get("tabId"));
  if (!type) return;
  history.replaceState(null, "", chrome.runtime.getURL("manager.html"));
  await handleManagerAction({ type, tabId });
}

chrome.runtime.onMessage.addListener((message) => {
  if (message?.type !== "manager-action") return undefined;
  runAction(() => handleManagerAction(message.action));
  return undefined;
});

void (async () => {
  await refresh();
  await handlePendingAction();
})();
