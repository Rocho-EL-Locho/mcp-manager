import { h } from "../dom";
import { icon } from "../icons";
import type { ProjectInfo } from "../ipc";

export type View =
  | { kind: "global" }
  | { kind: "project"; path: string }
  | { kind: "backups" };

export interface SidebarHandlers {
  onSelect: (view: View) => void;
  onDeleteProject: (project: ProjectInfo) => void;
}

/// Kürzt lange Pfade für die Anzeige (Home -> ~, sonst letzte Segmente).
function shortPath(path: string, home: string): string {
  if (path === home) return "~  (Home)";
  let p = path;
  if (home && p.startsWith(home + "/")) p = "~/" + p.slice(home.length + 1);
  const parts = p.split("/");
  if (parts.length > 3) return parts.slice(0, 1).concat("…", parts.slice(-2)).join("/");
  return p;
}

function isActive(view: View, item: View): boolean {
  if (view.kind !== item.kind) return false;
  if (view.kind === "project" && item.kind === "project") return view.path === item.path;
  // global / backups: Gleichheit der Art genügt.
  return true;
}

export function renderSidebar(
  projects: ProjectInfo[],
  view: View,
  home: string,
  handlers: SidebarHandlers,
): HTMLElement {
  const root = h("nav", { class: "sidebar" });

  // Global-Eintrag
  const globalItem = h(
    "button",
    {
      class: `side-item ${isActive(view, { kind: "global" }) ? "side-active" : ""}`,
      onclick: () => handlers.onSelect({ kind: "global" }),
    },
    icon("globe"),
    h("span", { class: "side-label", text: "Global (user)" }),
  );
  root.append(globalItem);

  // Backups-Eintrag (eigene Content-Ansicht, kein Projekt).
  root.append(
    h(
      "button",
      {
        class: `side-item ${isActive(view, { kind: "backups" }) ? "side-active" : ""}`,
        onclick: () => handlers.onSelect({ kind: "backups" }),
      },
      icon("archive"),
      h("span", { class: "side-label", text: "Backups" }),
    ),
  );

  root.append(h("div", { class: "side-header", text: `Projekte (${projects.length})` }));

  for (const p of projects) {
    const itemView: View = { kind: "project", path: p.path };
    const label = h("span", { class: "side-label", text: shortPath(p.path, home) });
    label.title = p.path + (p.exists ? "" : "  (Verzeichnis fehlt)");

    const meta = h(
      "span",
      { class: "side-meta" },
      p.server_count > 0 ? h("span", { class: "side-count", text: String(p.server_count) }) : null,
      !p.exists ? icon("alert", "side-missing", "Verzeichnis existiert nicht mehr") : null,
    );

    const del = h(
      "button",
      {
        class: "side-del",
        title: "Projekt entfernen",
        onclick: (e: Event) => {
          e.stopPropagation();
          handlers.onDeleteProject(p);
        },
      },
      icon("x"),
    );

    const item = h(
      "div",
      {
        class: `side-item side-project ${isActive(view, itemView) ? "side-active" : ""} ${p.is_home ? "side-home" : ""}`,
        onclick: () => handlers.onSelect(itemView),
      },
      label,
      meta,
      del,
    );
    root.append(item);
  }

  if (projects.length === 0) {
    root.append(h("div", { class: "muted side-empty", text: "Keine Projekte" }));
  }
  return root;
}
