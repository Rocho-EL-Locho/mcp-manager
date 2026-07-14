import { h } from "./dom";
import { openModal } from "./modal";

export interface ConfirmOptions {
  title: string;
  message: string;
  extra?: Node;
  confirmLabel?: string;
  danger?: boolean;
  /// Die eigentliche Aktion. `setStatus` aktualisiert die Inline-Statuszeile live
  /// (z. B. Fortschritt „3/7 …" bei Bulk-Aktionen).
  onConfirm: (setStatus: (msg: string) => void) => Promise<void>;
  onDone?: () => void;
}

/// Bestätigungsdialog mit Inline-Status. Die eigentliche Aktion läuft in onConfirm;
/// erst bei Erfolg wird geschlossen und onDone aufgerufen.
export function openConfirm(opts: ConfirmOptions): void {
  const status = h("div", { class: "form-status" });
  const cancelBtn = h("button", { class: "btn" }, "Abbrechen") as HTMLButtonElement;
  const okBtn = h(
    "button",
    { class: `btn ${opts.danger ? "btn-danger" : "btn-primary"}` },
    opts.confirmLabel ?? "OK",
  ) as HTMLButtonElement;

  const body = h("div", {}, h("p", { text: opts.message }), opts.extra ?? null, status);
  const modal = openModal(opts.title, body, [cancelBtn, okBtn]);

  cancelBtn.addEventListener("click", () => modal.close());
  okBtn.addEventListener("click", async () => {
    okBtn.disabled = true;
    cancelBtn.disabled = true;
    status.className = "form-status";
    status.textContent = "wird ausgeführt…";
    const setStatus = (msg: string) => {
      status.textContent = msg;
    };
    try {
      await opts.onConfirm(setStatus);
      modal.close();
      opts.onDone?.();
    } catch (e) {
      status.className = "form-status error";
      status.textContent = "Fehler: " + String(e);
      okBtn.disabled = false;
      cancelBtn.disabled = false;
    }
  });
}
