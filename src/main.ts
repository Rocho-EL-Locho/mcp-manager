import "./styles.css";
import { h, clear } from "./dom";
import {
  checkClaude,
  listServers,
  listProjects,
  deleteProject,
  healthCheck,
  removeServer,
  loginServer,
  toggleMcpjsonServer,
  toggleUserServer,
} from "./ipc";
import type { MergedServer, ProjectInfo, Scope } from "./ipc";
import { renderServerList, defaultFilter, selectionKey } from "./views/serverList";
import type { FilterState, BulkAction } from "./views/serverList";
import { renderSidebar } from "./views/sidebar";
import type { View } from "./views/sidebar";
import { openDetail } from "./views/serverDetail";
import { openServerForm } from "./views/serverForm";
import { openServerPicker } from "./views/serverPicker";
import { openSettings, applyTheme, applyStoredTheme } from "./views/settings";
import { getSettings } from "./ipc";
import { openConfirm } from "./confirm";
import { cleanupHints } from "./cleanup";
import { toast } from "./toast";
import { icon, setIcon } from "./icons";

interface State {
  servers: MergedServer[];
  projects: ProjectInfo[];
  home: string;
  view: View;
  loading: boolean;
  reveal: boolean;
  error: string | null;
  lastRefresh: Date | null;
  sidebarVisible: boolean;
  filter: FilterState;
  selection: Set<string>;
}

const state: State = {
  servers: [],
  projects: [],
  home: "",
  view: { kind: "global" },
  loading: false,
  reveal: false,
  error: null,
  lastRefresh: null,
  sidebarVisible: true,
  filter: defaultFilter(),
  selection: new Set(),
};

let contentEl: HTMLElement;
let sidebarEl: HTMLElement;
let refreshBtn: HTMLButtonElement;
let refreshIcon: HTMLElement;
let refreshLabel: HTMLElement;
let revealBtn: HTMLButtonElement;
let revealIcon: HTMLElement;
let revealLabel: HTMLElement;
let sidebarToggleBtn: HTMLButtonElement;
let stampEl: HTMLElement;
let claudeBadge: HTMLElement;

function timeStamp(d: Date): string {
  return d.toLocaleTimeString("de-DE", { hour: "2-digit", minute: "2-digit" });
}

function currentProjectPath(): string | undefined {
  return state.view.kind === "project" ? state.view.path : undefined;
}

function visibleGroups(): string[] {
  return state.view.kind === "project" ? ["local", "project"] : ["user", "external"];
}

async function loadProjects(): Promise<void> {
  try {
    state.projects = await listProjects();
    const home = state.projects.find((p) => p.is_home);
    if (home) state.home = home.path;
  } catch (e) {
    state.error = String(e);
  }
  renderSidebarEl();
}

// Auswahl-Schlüssel verwerfen, die nach einem Refresh keinem Server mehr entsprechen
// (Server entfernt/umbenannt) – verhindert verwaiste Selektion.
function pruneSelection(): void {
  if (state.selection.size === 0) return;
  const alive = new Set(state.servers.map(selectionKey));
  for (const key of state.selection) {
    if (!alive.has(key)) state.selection.delete(key);
  }
}

// Sequenz-Zähler: verhindert, dass ein überholter refresh() mit veralteten Daten
// die Ansicht überschreibt. Nur der jüngste Aufruf darf schreiben/rendern.
let refreshSeq = 0;

async function refresh(): Promise<void> {
  const seq = ++refreshSeq;
  state.error = null;
  await loadProjects();
  if (seq !== refreshSeq) return;
  const project = currentProjectPath();

  // Phase 1: schnelle Liste (Status aus Cache, kein Health-Check) -> sofort da.
  try {
    const servers = await listServers(state.reveal, project, false);
    if (seq !== refreshSeq) return;
    state.servers = servers;
    pruneSelection();
    renderContent();
  } catch (e) {
    if (seq !== refreshSeq) return;
    state.error = String(e);
    renderContent();
  }

  // Phase 2: frischer Health-Status im Hintergrund.
  state.loading = true;
  renderControls();
  try {
    const servers = await listServers(state.reveal, project, true);
    if (seq !== refreshSeq) return;
    state.servers = servers;
    pruneSelection();
    state.lastRefresh = new Date();
  } catch (e) {
    if (seq !== refreshSeq) return;
    state.error = String(e);
  } finally {
    if (seq === refreshSeq) {
      state.loading = false;
      renderControls();
      renderContent();
    }
  }
}

async function recheck(server: MergedServer): Promise<void> {
  try {
    const status = await healthCheck(server.name, server.project_path ?? undefined);
    const target = state.servers.find((s) => s.name === server.name && s.scope === server.scope);
    if (target) target.status = status;
  } catch (e) {
    state.error = String(e);
  }
  renderContent();
}

async function toggleReveal(): Promise<void> {
  state.reveal = !state.reveal;
  await refresh();
}

function applySidebarVisibility(): void {
  sidebarEl.style.display = state.sidebarVisible ? "" : "none";
  sidebarToggleBtn.classList.toggle("active", !state.sidebarVisible);
  sidebarToggleBtn.title = state.sidebarVisible ? "Seitenleiste ausblenden" : "Seitenleiste einblenden";
}

function toggleSidebar(): void {
  state.sidebarVisible = !state.sidebarVisible;
  applySidebarVisibility();
}

function onSelectView(view: View): void {
  state.view = view;
  // Filter und Auswahl gelten pro Ansicht – beim Wechsel zurücksetzen.
  state.filter = defaultFilter();
  state.selection.clear();
  renderSidebarEl();
  void refresh();
}

function onEdit(server: MergedServer): void {
  void openServerForm({ mode: "edit", server, onSaved: () => void refresh() });
}

function addContext(): { projectPath?: string; defaultScope: Scope } {
  return state.view.kind === "project"
    ? { projectPath: state.view.path, defaultScope: "local" }
    : { defaultScope: "user" };
}

function onAdd(): void {
  openServerPicker(() => void refresh(), addContext());
}

/// Claude-Badge (Version/Pfad) neu prüfen – z. B. nach Pfadwechsel in den Einstellungen.
function refreshClaudeBadge(): void {
  claudeBadge.className = "badge";
  claudeBadge.textContent = "claude: …";
  claudeBadge.removeAttribute("title");
  void checkClaude()
    .then((info) => {
      if (info.ok) {
        claudeBadge.textContent = `claude ${info.version}`;
        claudeBadge.classList.add("badge-ok");
        claudeBadge.title = info.path;
      } else {
        claudeBadge.textContent = "claude nicht gefunden";
        claudeBadge.classList.add("badge-error");
      }
    })
    .catch(() => {
      claudeBadge.textContent = "claude-Fehler";
      claudeBadge.classList.add("badge-error");
    });
}

function onSettings(): void {
  void openSettings({
    onSaved: () => {
      // Pfad-/Timeout-Änderungen können Auflösung und Status betreffen.
      refreshClaudeBadge();
      void refresh();
    },
  });
}

function onRemove(server: MergedServer): void {
  const hints = cleanupHints(server);
  let extra: HTMLElement | undefined;
  if (hints.length) {
    extra = h(
      "div",
      { class: "cleanup" },
      h("div", { class: "cleanup-title", text: "Nicht automatisch bereinigt:" }),
      ...hints.map((hnt) =>
        h(
          "div",
          { class: "cleanup-item" },
          h("div", { class: "muted", text: hnt.note }),
          hnt.command ? h("code", { class: "mono", text: hnt.command }) : null,
        ),
      ),
    );
  }
  openConfirm({
    title: `Server entfernen: ${server.name}`,
    message: `„${server.name}" (${server.origin}) wirklich entfernen? Die Definition wird über claude gelöscht.`,
    extra,
    confirmLabel: "Entfernen",
    danger: true,
    onConfirm: async () => {
      if (!server.scope) throw new Error("Externer Server kann hier nicht entfernt werden.");
      await removeServer(server.name, server.scope, server.project_path ?? undefined);
    },
    onDone: () => {
      toast("Server entfernt");
      void refresh();
    },
  });
}

function onLogin(server: MergedServer): void {
  toast("Anmeldung gestartet – ggf. öffnet sich der Browser…");
  void loginServer(server.name)
    .then(() => {
      toast("Anmeldung abgeschlossen");
      void refresh();
    })
    .catch((e) => {
      state.error = String(e);
      renderContent();
    });
}

/// Toggle-Routing (ohne Toast/Refresh) – von onToggle und den Bulk-Aktionen genutzt.
/// Projekt-Scope über settings.local.json, User-Scope über Stash-and-restore.
async function applyToggle(server: MergedServer, enabled: boolean): Promise<void> {
  if (server.scope === "project") {
    await toggleMcpjsonServer(server.name, enabled, server.project_path ?? undefined);
  } else if (server.scope === "user") {
    await toggleUserServer(server.name, enabled);
  } else {
    throw new Error(`Kein Aktivieren/Deaktivieren für Scope „${server.origin}"`);
  }
}

function canToggle(server: MergedServer): boolean {
  return server.scope === "user" || server.scope === "project";
}

async function onToggle(server: MergedServer, enabled: boolean): Promise<void> {
  if (!canToggle(server)) return;
  try {
    await applyToggle(server, enabled);
    toast(enabled ? `${server.name} aktiviert` : `${server.name} deaktiviert`);
  } catch (e) {
    state.error = String(e);
  }
  await refresh();
}

interface BulkMeta {
  title: string;
  confirmLabel: string;
  danger: boolean;
  gerund: string; // „aktiviert" / „deaktiviert" / „entfernt"
}

const BULK_META: Record<BulkAction, BulkMeta> = {
  enable: { title: "Server aktivieren", confirmLabel: "Aktivieren", danger: false, gerund: "aktiviert" },
  disable: { title: "Server deaktivieren", confirmLabel: "Deaktivieren", danger: false, gerund: "deaktiviert" },
  remove: { title: "Server entfernen", confirmLabel: "Entfernen", danger: true, gerund: "entfernt" },
};

/// Bulk-Aktion: Bestätigung mit Aufzählung, dann sequentielle Ausführung
/// (Fehler pro Server gesammelt), genau ein refresh() und ein Abschluss-Toast.
function onBulk(action: BulkAction, servers: MergedServer[]): void {
  const meta = BULK_META[action];
  const targetEnabled = action === "enable";

  const actionable = (s: MergedServer): boolean =>
    action === "remove"
      ? s.editable && s.scope !== null
      : canToggle(s) && s.enabled !== targetEnabled;

  const planned = servers.filter(actionable);
  const skipped = servers.filter((s) => !actionable(s)).map((s) => s.name);

  if (planned.length === 0) {
    toast(`Nichts zu tun – ${skipped.length} übersprungen`);
    return;
  }

  const names = planned.map((s) => s.name);
  const extra = h(
    "div",
    { class: "bulk-list" },
    h("div", { class: "muted", text: `${planned.length} betroffen:` }),
    ...names.map((n) => h("div", { class: "mono", text: n })),
    skipped.length
      ? h("div", { class: "field-hint", text: `${skipped.length} übersprungen: ${skipped.join(", ")}` })
      : null,
  );

  let done = 0;
  const failures: string[] = [];

  openConfirm({
    title: meta.title,
    message:
      action === "remove"
        ? `${planned.length} Server werden über claude gelöscht. Das kann nicht rückgängig gemacht werden.`
        : `${planned.length} Server werden ${meta.gerund}.`,
    extra,
    confirmLabel: meta.confirmLabel,
    danger: meta.danger,
    onConfirm: async (setStatus) => {
      done = 0;
      failures.length = 0;
      for (let i = 0; i < planned.length; i++) {
        const s = planned[i];
        setStatus(`${i + 1}/${planned.length} … ${s.name}`);
        try {
          if (action === "remove") {
            if (!s.scope) throw new Error("Externer Server kann nicht entfernt werden.");
            await removeServer(s.name, s.scope, s.project_path ?? undefined);
          } else {
            await applyToggle(s, targetEnabled);
          }
          done++;
        } catch (e) {
          failures.push(`${s.name} (${String(e)})`);
        }
      }
    },
    onDone: () => {
      const parts = [`${done} ${meta.gerund}`];
      if (failures.length) parts.push(`${failures.length} fehlgeschlagen: ${failures.join("; ")}`);
      if (skipped.length) parts.push(`${skipped.length} übersprungen`);
      toast(parts.join(", "), failures.length ? "error" : "ok");
      state.selection.clear();
      void refresh();
    },
  });
}

function onDeleteProject(project: ProjectInfo): void {
  openConfirm({
    title: `Projekt entfernen`,
    message: `Projekteintrag „${project.path}" aus ~/.claude.json entfernen? Das löscht dessen local-scope Server und Verlauf. Direkter, gesicherter Edit der Datei.`,
    confirmLabel: "Projekt entfernen",
    danger: true,
    onConfirm: async () => {
      await deleteProject(project.path);
    },
    onDone: () => {
      toast("Projekt entfernt");
      if (state.view.kind === "project" && state.view.path === project.path) {
        state.view = { kind: "global" };
      }
      void refresh();
    },
  });
}

function renderSidebarEl(): void {
  clear(sidebarEl);
  sidebarEl.append(
    renderSidebar(state.projects, state.view, state.home, {
      onSelect: onSelectView,
      onDeleteProject,
    }),
  );
}

function renderControls(): void {
  refreshBtn.disabled = state.loading;
  refreshLabel.textContent = state.loading ? "Prüft…" : "Aktualisieren";
  refreshIcon.classList.toggle("spin", state.loading);
  setIcon(revealIcon, state.reveal ? "eye-off" : "eye");
  revealLabel.textContent = state.reveal ? "Secrets verbergen" : "Secrets anzeigen";
  revealBtn.classList.toggle("active", state.reveal);
  stampEl.textContent = state.lastRefresh ? `Stand ${timeStamp(state.lastRefresh)}` : "";
}

function renderContent(): void {
  // Fokus + Caret des Suchfelds über den Full-Rebuild retten (z. B. wenn ein
  // Hintergrund-Refresh oder recheck/introspect mitten im Tippen rendert).
  const active = document.activeElement;
  const searchFocused = active instanceof HTMLInputElement && active.classList.contains("filter-search");
  const caret = searchFocused ? active.selectionStart : null;

  clear(contentEl);

  const heading =
    state.view.kind === "project"
      ? h("div", { class: "view-head" }, h("span", { class: "mono", text: state.view.path }))
      : h("div", { class: "view-head" }, h("span", { text: "Globale & externe Server" }));
  contentEl.append(heading);

  if (state.error) {
    contentEl.append(h("div", { class: "banner banner-error", text: state.error }));
  }

  contentEl.append(
    renderServerList(
      state.servers,
      {
        onDetails: (s) =>
          openDetail(s, {
            onChanged: () => void refresh(),
            onRechecked: (srv, status) => {
              // Neuen Health-Status ohne teuren Full-Refresh in die Liste übernehmen.
              const target = state.servers.find(
                (x) => x.name === srv.name && x.scope === srv.scope,
              );
              if (target) {
                target.status = status;
                renderContent();
              }
            },
            onIntrospected: (srv, intro) => {
              // Zähler des betroffenen Servers aktualisieren und Liste neu rendern
              // (kein teurer Full-Refresh mit Health-Check).
              const target = state.servers.find(
                (x) => x.name === srv.name && x.scope === srv.scope,
              );
              if (target) {
                target.tool_count = intro.tools.length;
                target.resource_count = intro.resources.length;
                target.prompt_count = intro.prompts.length;
                target.connect_ms = intro.connectMs;
                renderContent();
              }
            },
          }),
        onRecheck: recheck,
        onEdit,
        onRemove,
        onLogin,
        onToggle: (s, enabled) => void onToggle(s, enabled),
      },
      visibleGroups(),
      {
        filter: state.filter,
        selection: state.selection,
        onBulk,
      },
    ),
  );

  if (searchFocused) {
    const el = contentEl.querySelector<HTMLInputElement>(".filter-search");
    if (el) {
      el.focus();
      if (caret != null) el.setSelectionRange(caret, caret);
    }
  }
}

async function main(): Promise<void> {
  // Zuletzt gemerktes Theme sofort anwenden (vor dem Rendern) – verhindert den
  // Dark-Flash für Hell-Nutzer; das Backend liefert unten die maßgebliche Fassung.
  applyStoredTheme();

  const app = document.querySelector<HTMLDivElement>("#app");
  if (!app) return;

  claudeBadge = h("span", { class: "badge", text: "claude: …" });
  sidebarToggleBtn = h("button", { class: "btn btn-icon", title: "Seitenleiste ein-/ausblenden", onclick: toggleSidebar }, icon("menu")) as HTMLButtonElement;
  const addBtn = h("button", { class: "btn btn-primary", onclick: onAdd }, icon("plus"), "Server");
  const settingsBtn = h("button", { class: "btn btn-icon", title: "Einstellungen", onclick: onSettings }, icon("settings")) as HTMLButtonElement;

  refreshIcon = icon("refresh");
  refreshLabel = h("span", { text: "Aktualisieren" });
  refreshBtn = h("button", { class: "btn", onclick: () => void refresh() }, refreshIcon, refreshLabel) as HTMLButtonElement;

  revealIcon = icon("eye");
  revealLabel = h("span", { text: "Secrets anzeigen" });
  revealBtn = h("button", { class: "btn", onclick: () => void toggleReveal() }, revealIcon, revealLabel) as HTMLButtonElement;

  stampEl = h("span", { class: "muted stamp" });

  const topbar = h(
    "header",
    { class: "topbar" },
    sidebarToggleBtn,
    h("h1", { text: "MCP-Manager" }),
    stampEl,
    addBtn,
    revealBtn,
    refreshBtn,
    settingsBtn,
    claudeBadge,
  );

  sidebarEl = h("aside", { class: "sidebar-wrap" });
  contentEl = h("main", { id: "content" });
  const layout = h("div", { class: "layout" }, sidebarEl, contentEl);

  clear(app);
  app.append(topbar, layout);
  applySidebarVisibility();

  // Gespeichertes Theme früh anwenden (best effort; Fehler => System-Default).
  void getSettings()
    .then((s) => applyTheme(s.theme))
    .catch(() => applyTheme("system"));

  refreshClaudeBadge();

  await refresh();
}

void main();
