import { h } from "../dom";
import { icon } from "../icons";
import type { MergedServer, ServerStatus, Scope } from "../ipc";

export interface ListHandlers {
  onDetails: (server: MergedServer) => void;
  onRecheck: (server: MergedServer) => void;
  onEdit: (server: MergedServer) => void;
  onRemove: (server: MergedServer) => void;
  onLogin: (server: MergedServer) => void;
  onToggle: (server: MergedServer, enabled: boolean) => void;
}

/// Ein/Aus-Schalter (nur für user- und project-Scope sinnvoll).
function switchControl(on: boolean, onChange: (v: boolean) => void): HTMLElement {
  const input = h("input", { type: "checkbox", class: "switch-input" }) as HTMLInputElement;
  input.checked = on;
  input.addEventListener("change", () => onChange(input.checked));
  return h("label", { class: "switch", title: on ? "aktiv" : "deaktiviert" }, input, h("span", { class: "slider" }));
}

interface StatusMeta {
  label: string;
  cls: string;
  title?: string;
}

export function statusMeta(status: ServerStatus): StatusMeta {
  switch (status.kind) {
    case "connected":
      return { label: "verbunden", cls: "badge-ok" };
    case "failed":
      return {
        label: "Fehler",
        cls: "badge-error",
        title: status.detail ?? undefined,
      };
    case "needs_auth":
      return { label: "Login nötig", cls: "badge-warn" };
    case "pending_approval":
      return { label: "nicht freigegeben", cls: "badge-warn" };
    case "disabled":
      return { label: "deaktiviert", cls: "badge-muted" };
    case "unknown":
    default:
      return { label: "unbekannt", cls: "badge-muted" };
  }
}

interface Group {
  key: string;
  label: string;
  match: (s: MergedServer) => boolean;
}

const GROUPS: Group[] = [
  { key: "user", label: "Global (user)", match: (s) => s.scope === "user" },
  { key: "local", label: "Projekt-lokal (local)", match: (s) => s.scope === "local" },
  { key: "project", label: "Projekt (.mcp.json)", match: (s) => s.scope === "project" },
  { key: "external", label: "Extern (Connector / Plugin)", match: (s) => s.scope === null },
];

export function scopeLabel(scope: Scope | null): string {
  switch (scope) {
    case "user":
      return "user";
    case "local":
      return "local";
    case "project":
      return "project";
    default:
      return "extern";
  }
}

function serverCard(server: MergedServer, handlers: ListHandlers): HTMLElement {
  const st = statusMeta(server.status);

  const titleRow = h(
    "div",
    { class: "card-title" },
    h("span", { class: "server-name", text: server.name }),
    h("span", { class: "badge badge-scope", text: server.origin }),
    server.has_secrets ? icon("lock", "lock", "enthält Geheimnisse") : null,
    server.collision ? icon("alert", "warn-icon", "Name existiert in mehreren Scopes") : null,
  );

  const summary = h("div", { class: "card-summary mono", title: server.summary }, server.summary || "—");

  const recheckBtn = h(
    "button",
    {
      class: "btn btn-small",
      title: "Status neu prüfen",
      onclick: () => handlers.onRecheck(server),
    },
    icon("refresh"),
    "prüfen",
  );

  const detailBtn = h(
    "button",
    { class: "btn btn-small", onclick: () => handlers.onDetails(server) },
    "Details",
  );

  const loginBtn =
    server.status.kind === "needs_auth"
      ? h("button", { class: "btn btn-small", onclick: () => handlers.onLogin(server) }, "Anmelden")
      : null;

  const editBtn = server.editable
    ? h("button", { class: "btn btn-small", onclick: () => handlers.onEdit(server) }, "Bearbeiten")
    : null;

  const removeBtn = server.editable
    ? h("button", { class: "btn btn-small btn-danger", onclick: () => handlers.onRemove(server) }, "Entfernen")
    : null;

  const canToggle = server.scope === "user" || server.scope === "project";
  const toggle = canToggle
    ? switchControl(server.enabled, (enabled) => handlers.onToggle(server, enabled))
    : null;

  const actions = h(
    "div",
    { class: "card-actions" },
    toggle,
    h("span", { class: `badge ${st.cls}`, title: st.title }, st.label),
    h("span", { class: "spacer" }),
    loginBtn,
    recheckBtn,
    editBtn,
    detailBtn,
    removeBtn,
  );

  return h("div", { class: "card", "data-name": server.name }, titleRow, summary, actions);
}

export function renderServerList(
  servers: MergedServer[],
  handlers: ListHandlers,
  visibleGroups?: string[],
): HTMLElement {
  const root = h("div", { class: "server-list" });

  for (const group of GROUPS) {
    if (visibleGroups && !visibleGroups.includes(group.key)) continue;
    const members = servers.filter(group.match);
    if (members.length === 0) continue;
    root.append(
      h(
        "div",
        { class: "group-header" },
        h("span", { text: group.label }),
        h("span", { class: "count", text: String(members.length) }),
      ),
    );
    for (const s of members) root.append(serverCard(s, handlers));
  }

  if (root.childElementCount === 0) {
    root.append(h("p", { class: "muted" }, "Keine MCP-Server gefunden."));
  }
  return root;
}
