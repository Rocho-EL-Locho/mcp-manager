import { h } from "../dom";
import { openModal } from "../modal";
import { openConfirm } from "../confirm";
import { toast } from "../toast";
import type { ConflictInfo, Scope } from "../ipc";

export interface ConflictHandlers {
  /// Entfernt eine der kollidierenden Definitionen (bestehendes removeServer).
  remove: (name: string, scope: Scope, projectPath: string | undefined) => Promise<void>;
  /// Benennt eine Definition um (neues rename_server-Command).
  rename: (
    name: string,
    scope: Scope,
    projectPath: string | undefined,
    newName: string,
  ) => Promise<void>;
  /// Nach erfolgreicher Auflösung aufrufen (löst refresh() aus).
  onChanged: () => void;
}

const SCOPE_LABEL: Record<Scope, string> = {
  user: "user (global)",
  local: "local (projekt-privat)",
  project: "project (.mcp.json)",
};

/// Kleiner Umbenennen-Dialog für eine einzelne Definition.
function openRenameDialog(
  conflict: ConflictInfo,
  scope: Scope,
  projectPath: string | undefined,
  handlers: ConflictHandlers,
  parentClose: () => void,
): void {
  const input = h("input", {
    class: "inp",
    type: "text",
    value: conflict.name,
  }) as HTMLInputElement;
  const status = h("div", { class: "form-status" });
  const cancelBtn = h("button", { class: "btn" }, "Abbrechen") as HTMLButtonElement;
  const okBtn = h("button", { class: "btn btn-primary" }, "Umbenennen") as HTMLButtonElement;

  const body = h(
    "div",
    {},
    h(
      "div",
      { class: "field" },
      h("label", { class: "field-label", text: `Neuer Name (${SCOPE_LABEL[scope]})` }),
      input,
    ),
    status,
  );
  const modal = openModal(`„${conflict.name}" umbenennen`, body, [cancelBtn, okBtn]);
  cancelBtn.addEventListener("click", () => modal.close());
  okBtn.addEventListener("click", async () => {
    const newName = input.value.trim();
    if (!newName || newName === conflict.name) {
      status.className = "form-status error";
      status.textContent = "Bitte einen abweichenden, nicht-leeren Namen angeben.";
      return;
    }
    okBtn.disabled = true;
    cancelBtn.disabled = true;
    status.className = "form-status";
    status.textContent = "wird umbenannt…";
    try {
      await handlers.rename(conflict.name, scope, projectPath, newName);
      modal.close();
      parentClose();
      toast("Server umbenannt");
      handlers.onChanged();
    } catch (e) {
      status.className = "form-status error";
      status.textContent = "Fehler: " + String(e);
      okBtn.disabled = false;
      cancelBtn.disabled = false;
    }
  });
  input.focus();
  input.select();
}

/// Konflikt-Dialog: zeigt alle Definitionen desselben Namens, markiert die
/// effektive und bietet Entfernen/Umbenennen an.
export function openConflictDialog(conflict: ConflictInfo, handlers: ConflictHandlers): void {
  const statusBadge = conflict.identical
    ? h("span", { class: "badge badge-ok", text: "identisch" })
    : h("span", { class: "badge badge-warn", text: "abweichend" });

  const intro = h(
    "div",
    { class: "conflict-intro" },
    h("span", {
      text: conflict.identical
        ? "Alle Definitionen sind inhaltsgleich (Duplikat, kein echter Konflikt)."
        : "Die Definitionen unterscheiden sich – Claude Code nutzt nur die effektive.",
    }),
    statusBadge,
  );

  const rows = conflict.definitions.map((d) => {
    const effective = d.scope === conflict.effective_scope;
    const projectPath = d.project_path ?? undefined;

    const removeBtn = h(
      "button",
      { class: "btn btn-small btn-danger", title: "Diese Definition entfernen" },
      "Entfernen",
    ) as HTMLButtonElement;
    removeBtn.addEventListener("click", () => {
      openConfirm({
        title: `Definition entfernen (${SCOPE_LABEL[d.scope]})`,
        message: `Die ${SCOPE_LABEL[d.scope]}-Definition von „${conflict.name}" entfernen?`,
        confirmLabel: "Entfernen",
        danger: true,
        onConfirm: async () => {
          await handlers.remove(conflict.name, d.scope, projectPath);
        },
        onDone: () => {
          modal.close();
          toast("Definition entfernt");
          handlers.onChanged();
        },
      });
    });

    const renameBtn = h(
      "button",
      { class: "btn btn-small", title: "Diese Definition umbenennen" },
      "Umbenennen…",
    );
    renameBtn.addEventListener("click", () =>
      openRenameDialog(conflict, d.scope, projectPath, handlers, () => modal.close()),
    );

    return h(
      "tr",
      { class: effective ? "conflict-effective" : "" },
      h(
        "td",
        {},
        h("span", { class: "mono", text: SCOPE_LABEL[d.scope] }),
        effective ? h("span", { class: "badge badge-ok", text: "wird verwendet" }) : null,
      ),
      h("td", {}, h("span", { class: "muted", text: d.project_path ?? "—" })),
      h("td", {}, h("span", { class: "mono", text: d.summary || "—" })),
      h("td", { class: "conflict-actions" }, renameBtn, removeBtn),
    );
  });

  const table = h(
    "table",
    { class: "backups-table" },
    h(
      "thead",
      {},
      h(
        "tr",
        {},
        h("th", { text: "Scope" }),
        h("th", { text: "Projekt" }),
        h("th", { text: "Definition" }),
        h("th", { text: "" }),
      ),
    ),
    h("tbody", {}, ...rows),
  );

  const body = h("div", { class: "conflict-dialog" }, intro, h("div", { class: "table-wrap" }, table));
  const closeBtn = h("button", { class: "btn" }, "Schließen") as HTMLButtonElement;
  const modal = openModal(`Namenskonflikt: „${conflict.name}"`, body, [closeBtn]);
  closeBtn.addEventListener("click", () => modal.close());
}

/// Übersicht aller Namenskonflikte; ein Klick öffnet den Detail-Dialog.
export function openConflictsOverview(conflicts: ConflictInfo[], handlers: ConflictHandlers): void {
  const list = h(
    "div",
    { class: "conflict-list" },
    ...conflicts.map((c) => {
      const row = h(
        "button",
        { class: "conflict-list-item" },
        h("span", { class: "server-name", text: c.name }),
        c.identical
          ? h("span", { class: "badge badge-ok", text: "identisch" })
          : h("span", { class: "badge badge-warn", text: "abweichend" }),
        h("span", { class: "muted", text: `${c.definitions.length} Definitionen` }),
      );
      row.addEventListener("click", () => {
        modal.close();
        openConflictDialog(c, handlers);
      });
      return row;
    }),
  );
  const closeBtn = h("button", { class: "btn" }, "Schließen") as HTMLButtonElement;
  const modal = openModal("Namenskonflikte", list, [closeBtn]);
  closeBtn.addEventListener("click", () => modal.close());
}
