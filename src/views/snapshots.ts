import { h } from "../dom";
import { icon } from "../icons";
import { openConfirm } from "../confirm";
import { field } from "../form";
import { toast } from "../toast";
import type { SnapshotInfo } from "../ipc";
import { homeRelative } from "../paths";

export interface SnapshotHandlers {
  /// Erstellt einen manuellen Snapshot (reiner IPC-Aufruf).
  create: (note: string | undefined) => Promise<void>;
  /// Stellt einen Snapshot wieder her; `onlyPaths` = undefined bedeutet „alle".
  restore: (id: string, onlyPaths: string[] | undefined) => Promise<void>;
  /// Löscht einen Snapshot.
  remove: (id: string) => Promise<void>;
  /// Nach jeder erfolgreichen Änderung aufrufen (löst refresh() aus).
  onChanged: () => void;
}

function formatTime(unixSecs: number): string {
  if (!unixSecs) return "—";
  return new Date(unixSecs * 1000).toLocaleString("de-DE", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatSize(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${i === 0 ? v : v.toFixed(1)} ${units[i]}`;
}

function totalSize(b: SnapshotInfo): number {
  return b.files.reduce((sum, f) => sum + (f.size || 0), 0);
}

/// Modal zum Erstellen eines manuellen Snapshots (mit optionaler Notiz).
function openCreateDialog(handlers: SnapshotHandlers): void {
  const input = h("input", {
    class: "inp",
    type: "text",
    placeholder: "Notiz (optional), z. B. vor dem Aufräumen",
  }) as HTMLInputElement;

  const extra = field(
    "Notiz",
    input,
    "Sichert ~/.claude.json, die settings-Dateien und alle Projekt-.mcp.json.",
  );

  openConfirm({
    title: "Snapshot erstellen",
    message: "Aktuellen Stand der Konfiguration sichern?",
    extra,
    confirmLabel: "Snapshot erstellen",
    onConfirm: async () => {
      const note = input.value.trim();
      await handlers.create(note || undefined);
    },
    onDone: () => {
      toast("Snapshot erstellt");
      handlers.onChanged();
    },
  });
  input.focus();
}

/// Wiederherstellen-Dialog mit Warnhinweis und Datei-Auswahl (Teil-Restore).
function openRestoreDialog(snapshot: SnapshotInfo, home: string, handlers: SnapshotHandlers): void {
  const restorable = snapshot.files;
  const boxes: Array<{ path: string; input: HTMLInputElement }> = [];

  const fileList = h(
    "div",
    { class: "restore-files" },
    ...restorable.map((f) => {
      const input = h("input", {
        type: "checkbox",
        class: "select-box",
      }) as HTMLInputElement;
      input.checked = true;
      boxes.push({ path: f.original_path, input });
      const label = h(
        "label",
        { class: "restore-file" },
        input,
        h("span", { class: "mono", text: homeRelative(f.original_path, home) }),
        f.existed
          ? h("span", { class: "muted", text: formatSize(f.size) })
          : h("span", { class: "muted", text: "(gelöscht → wird entfernt)" }),
      );
      return label;
    }),
  );

  const warning = h(
    "div",
    { class: "banner banner-warn" },
    icon("alert"),
    h("span", {
      text: "Claude Code sollte dabei nicht laufen – die Dateien werden direkt überschrieben.",
    }),
  );

  const extra = h(
    "div",
    {},
    warning,
    h("div", { class: "field-hint", text: "Vor dem Wiederherstellen wird automatisch ein Snapshot des aktuellen Standes angelegt." }),
    fileList,
  );

  openConfirm({
    title: "Snapshot wiederherstellen",
    message: `Snapshot vom ${formatTime(snapshot.created_at)} wiederherstellen?`,
    extra,
    confirmLabel: "Wiederherstellen",
    danger: true,
    onConfirm: async () => {
      const selected = boxes.filter((b) => b.input.checked).map((b) => b.path);
      if (selected.length === 0) {
        throw new Error("Keine Datei ausgewählt.");
      }
      // Alle ausgewählt -> undefined (kompletter Restore), sonst Teilmenge.
      const onlyPaths = selected.length === boxes.length ? undefined : selected;
      await handlers.restore(snapshot.id, onlyPaths);
    },
    onDone: () => {
      toast("Wiederhergestellt");
      handlers.onChanged();
    },
  });
}

function openDeleteDialog(snapshot: SnapshotInfo, handlers: SnapshotHandlers): void {
  openConfirm({
    title: "Snapshot löschen",
    message: `Snapshot vom ${formatTime(snapshot.created_at)} endgültig löschen?`,
    confirmLabel: "Löschen",
    danger: true,
    onConfirm: async () => {
      await handlers.remove(snapshot.id);
    },
    onDone: () => {
      toast("Snapshot gelöscht");
      handlers.onChanged();
    },
  });
}

export function renderSnapshots(
  snapshots: SnapshotInfo[],
  home: string,
  handlers: SnapshotHandlers,
): HTMLElement {
  const root = h("div", { class: "snapshots" });

  const createBtn = h(
    "button",
    { class: "btn btn-primary", onclick: () => openCreateDialog(handlers) },
    icon("plus"),
    "Snapshot erstellen",
  );
  root.append(h("div", { class: "snapshots-toolbar" }, createBtn));

  if (snapshots.length === 0) {
    root.append(h("div", { class: "muted empty", text: "Noch keine Snapshots vorhanden." }));
    return root;
  }

  const rows = snapshots.map((b) => {
    const kind = b.corrupt
      ? h("span", { class: "tag tag-warn", text: "beschädigt" })
      : b.auto
        ? h("span", { class: "tag", text: "automatisch" })
        : h("span", { class: "tag tag-manual", text: "manuell" });

    const restoreBtn = h(
      "button",
      {
        class: "btn btn-small",
        title: "Wiederherstellen",
        disabled: b.corrupt,
        onclick: () => openRestoreDialog(b, home, handlers),
      },
      "Wiederherstellen…",
    );
    const deleteBtn = h(
      "button",
      {
        class: "btn btn-small btn-danger",
        title: "Löschen",
        onclick: () => openDeleteDialog(b, handlers),
      },
      icon("x"),
    );

    return h(
      "tr",
      {},
      h("td", {}, h("span", { text: formatTime(b.created_at) })),
      h("td", {}, kind),
      h("td", {}, h("span", { class: "muted", text: b.note ?? "" })),
      h("td", { class: "num", text: b.corrupt ? "—" : String(b.files.filter((f) => f.existed).length) }),
      h("td", { class: "num", text: b.corrupt ? "—" : formatSize(totalSize(b)) }),
      h("td", { class: "snapshots-actions" }, restoreBtn, deleteBtn),
    );
  });

  const table = h(
    "table",
    { class: "data-table" },
    h(
      "thead",
      {},
      h(
        "tr",
        {},
        h("th", { text: "Zeitpunkt" }),
        h("th", { text: "Art" }),
        h("th", { text: "Notiz" }),
        h("th", { class: "num", text: "Dateien" }),
        h("th", { class: "num", text: "Größe" }),
        h("th", { text: "" }),
      ),
    ),
    h("tbody", {}, ...rows),
  );
  root.append(h("div", { class: "table-wrap" }, table));
  return root;
}
