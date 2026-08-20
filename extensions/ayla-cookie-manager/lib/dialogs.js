const restoreTargets = new WeakMap();
const installedDialogs = new WeakSet();

function focusableElements(dialog) {
  return [...dialog.querySelectorAll([
    "button:not([disabled])",
    "input:not([disabled])",
    "select:not([disabled])",
    "textarea:not([disabled])",
    "[href]",
    "[tabindex]:not([tabindex='-1'])"
  ].join(","))].filter((element) => !element.closest(".hidden"));
}

function installDialogBehavior(dialog) {
  if (installedDialogs.has(dialog)) return;
  installedDialogs.add(dialog);

  dialog.addEventListener("keydown", (event) => {
    if (event.key !== "Tab") return;
    const focusable = focusableElements(dialog);
    if (!focusable.length) {
      event.preventDefault();
      dialog.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable.at(-1);
    if (event.shiftKey && (document.activeElement === first || !dialog.contains(document.activeElement))) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  });

  dialog.addEventListener("cancel", () => {
    dialog.returnValue = "cancel";
  });

  dialog.addEventListener("close", () => {
    const target = restoreTargets.get(dialog);
    restoreTargets.delete(dialog);
    if (target?.isConnected) queueMicrotask(() => target.focus());
  });
}

export function openModal(dialog, { initialFocus } = {}) {
  installDialogBehavior(dialog);
  if (dialog.open) return;
  restoreTargets.set(dialog, document.activeElement);
  dialog.showModal();
  queueMicrotask(() => {
    const requested = typeof initialFocus === "string" ? dialog.querySelector(initialFocus) : initialFocus;
    (requested ?? focusableElements(dialog)[0] ?? dialog).focus();
  });
}

export function closeModal(dialog, returnValue = "cancel") {
  if (dialog.open) dialog.close(returnValue);
}

export function confirmWithDialog(dialog, { title, description, confirmLabel = "Confirmar" }) {
  dialog.querySelector("[data-confirm-title]").textContent = title;
  dialog.querySelector("[data-confirm-description]").textContent = description;
  dialog.querySelector("[data-confirm-submit]").textContent = confirmLabel;
  dialog.returnValue = "cancel";

  return new Promise((resolve) => {
    dialog.addEventListener("close", () => resolve(dialog.returnValue === "confirm"), { once: true });
    openModal(dialog, { initialFocus: "[data-confirm-cancel]" });
  });
}
