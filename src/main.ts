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
import { renderServerList } from "./views/serverList";
import { renderSidebar } from "./views/sidebar";
import type { View } from "./views/sidebar";
import { openDetail } from "./views/serverDetail";
import { openServerForm } from "./views/serverForm";
import { openAssistant } from "./views/assistant";
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
  const ctx = addContext();
  void openServerForm({
    mode: "add",
    projectPath: ctx.projectPath,
    defaultScope: ctx.defaultScope,
    onSaved: () => void refresh(),
  });
}

function onAssistant(): void {
  openAssistant(() => void refresh(), addContext());
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

async function onToggle(server: MergedServer, enabled: boolean): Promise<void> {
  try {
    if (server.scope === "project") {
      await toggleMcpjsonServer(server.name, enabled, server.project_path ?? undefined);
    } else if (server.scope === "user") {
      await toggleUserServer(server.name, enabled);
    } else {
      return;
    }
    toast(enabled ? `${server.name} aktiviert` : `${server.name} deaktiviert`);
  } catch (e) {
    state.error = String(e);
  }
  await refresh();
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
    ),
  );
}

async function main(): Promise<void> {
  const app = document.querySelector<HTMLDivElement>("#app");
  if (!app) return;

  const claudeBadge = h("span", { class: "badge", text: "claude: …" });
  sidebarToggleBtn = h("button", { class: "btn btn-icon", title: "Seitenleiste ein-/ausblenden", onclick: toggleSidebar }, icon("menu")) as HTMLButtonElement;
  const assistantBtn = h("button", { class: "btn", onclick: onAssistant }, icon("sparkles"), "per Link");
  const addBtn = h("button", { class: "btn btn-primary", onclick: onAdd }, icon("plus"), "Server");

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
    assistantBtn,
    addBtn,
    revealBtn,
    refreshBtn,
    claudeBadge,
  );

  sidebarEl = h("aside", { class: "sidebar-wrap" });
  contentEl = h("main", { id: "content" });
  const layout = h("div", { class: "layout" }, sidebarEl, contentEl);

  clear(app);
  app.append(topbar, layout);
  applySidebarVisibility();

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

  await refresh();
}

void main();
