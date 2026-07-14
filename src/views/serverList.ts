import { h } from "../dom";
import { icon } from "../icons";
import { switchControl } from "../switch";
import type { MergedServer, ServerStatus, Scope } from "../ipc";

export interface ListHandlers {
  onDetails: (server: MergedServer) => void;
  onRecheck: (server: MergedServer) => void;
  onEdit: (server: MergedServer) => void;
  onRemove: (server: MergedServer) => void;
  onLogin: (server: MergedServer) => void;
  onToggle: (server: MergedServer, enabled: boolean) => void;
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

/// Formatiert eine Verbindungs-/Startzeit: ganzzahlige Millisekunden, ab 1000 ms
/// kompakt in Sekunden mit deutschem Dezimalkomma ("1,2 s").
export function formatLatency(ms: number): string {
  return ms >= 1000 ? `${(ms / 1000).toFixed(1).replace(".", ",")} s` : `${ms} ms`;
}

/// Kleines Latenz-Pill (Verbindungs-/Startzeit), nur wenn schon introspiziert.
function latencyBadge(server: MergedServer): HTMLElement | null {
  if (server.connect_ms === undefined) return null;
  return h(
    "span",
    { class: "badge badge-latency", title: "Verbindungs-/Startzeit (bis initialize)" },
    formatLatency(server.connect_ms),
  );
}

/// Kompaktes Zähler-Badge (Tools·Ressourcen·Prompts), nur wenn schon introspiziert.
function capsBadge(server: MergedServer): HTMLElement | null {
  if (
    server.tool_count === undefined &&
    server.resource_count === undefined &&
    server.prompt_count === undefined
  ) {
    return null;
  }
  const t = server.tool_count ?? 0;
  const r = server.resource_count ?? 0;
  const p = server.prompt_count ?? 0;
  return h(
    "span",
    { class: "badge badge-caps", title: `${t} Tools · ${r} Ressourcen · ${p} Prompts` },
    `${t}·${r}·${p}`,
  );
}

function serverCard(server: MergedServer, handlers: ListHandlers): HTMLElement {
  const st = statusMeta(server.status);

  const titleRow = h(
    "div",
    { class: "card-title" },
    h("span", { class: "server-name", text: server.name }),
    h("span", { class: "badge badge-scope", text: server.origin }),
    capsBadge(server),
    latencyBadge(server),
    server.has_secrets ? icon("lock", "lock", "enthält Geheimnisse") : null,
    server.collision ? icon("alert", "warn-icon", "Name existiert in mehreren Scopes") : null,
    server.runtime_missing
      ? icon("terminal", "warn-icon", "Laufzeit nicht auf PATH – Details öffnen")
      : null,
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
    ? switchControl({ on: server.enabled, onChange: (enabled) => handlers.onToggle(server, enabled) })
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
