import { h, clear } from "../dom";
import { icon } from "../icons";
import { switchControl } from "../switch";
import { transportOfEntry } from "../transport";
import type { MergedServer, ServerStatus } from "../ipc";
import { createSelectionRegistry, isSelectable, selectionKey } from "./serverListSelection";

export interface ListHandlers {
  onDetails: (server: MergedServer) => void;
  onRecheck: (server: MergedServer) => void;
  onEdit: (server: MergedServer) => void;
  onRemove: (server: MergedServer) => void;
  onLogin: (server: MergedServer) => void;
  onToggle: (server: MergedServer, enabled: boolean) => void;
  /// Öffnet den Konflikt-Dialog für einen Server mit Namenskollision.
  onConflict: (server: MergedServer) => void;
}

export type StatusFilter = "all" | "connected" | "failed" | "needs_auth" | "disabled";
export type TransportFilter = "all" | "stdio" | "http" | "sse";

export interface FilterState {
  query: string;
  status: StatusFilter;
  transport: TransportFilter;
}

export function defaultFilter(): FilterState {
  return { query: "", status: "all", transport: "all" };
}

export type BulkAction = "enable" | "disable" | "remove";

export interface BulkContext {
  /// Aktueller Filter (wird direkt mutiert; überlebt so `refresh()`).
  filter: FilterState;
  /// Ausgewählte Server (Schlüssel via `selectionKey`); wird direkt mutiert.
  selection: Set<string>;
  /// Führt eine Bulk-Aktion aus (Bestätigung + sequentielle Ausführung in main.ts).
  onBulk: (action: BulkAction, servers: MergedServer[]) => void;
}

/// Transport aus der Definition ableiten – gemeinsame Quelle `transport.ts`
/// (spiegelt `commands.rs::transport_of`). Null, wenn unbekannt (externe Server
/// ohne Definition).
function serverTransport(s: MergedServer): "stdio" | "http" | "sse" | null {
  return s.entry ? transportOfEntry(s.entry) : null;
}

function matchesStatus(s: MergedServer, f: StatusFilter): boolean {
  switch (f) {
    case "all":
      return true;
    case "disabled":
      // enabled ist die maßgebliche Quelle für „deaktiviert" (Toggle-Zustand).
      return !s.enabled;
    default:
      return s.status.kind === f;
  }
}

function matchesTransport(s: MergedServer, f: TransportFilter): boolean {
  if (f === "all") return true;
  return serverTransport(s) === f;
}

/// Suche über Name + Summary, kombiniert mit Status- und Transport-Filter.
function filterServers(servers: MergedServer[], filter: FilterState): MergedServer[] {
  const q = filter.query.trim().toLowerCase();
  return servers.filter((s) => {
    if (q) {
      const hay = `${s.name}\n${s.summary ?? ""}`.toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return matchesStatus(s, filter.status) && matchesTransport(s, filter.transport);
  });
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

interface SelectContext {
  /// Vorgefertigte Checkbox (Zustand + Handler vom Auswahl-Register verdrahtet),
  /// hier nur platziert – so kann die Auswahl gezielt (ohne Full-Rebuild) toggeln.
  /// Ihr `checked` IST der Auswahlzustand; ein zweites Feld dafür liefe auseinander.
  checkbox: HTMLInputElement;
}

function serverCard(
  server: MergedServer,
  handlers: ListHandlers,
  sel: SelectContext | null,
  logActive = false,
): HTMLElement {
  const st = statusMeta(server.status);

  const checkbox = sel ? sel.checkbox : null;

  const titleRow = h(
    "div",
    { class: "card-title" },
    checkbox,
    h("span", { class: "server-name", text: server.name }),
    h("span", { class: "badge badge-scope", text: server.origin }),
    capsBadge(server),
    latencyBadge(server),
    logActive ? h("span", { class: "badge badge-live", title: "Diagnose-Session läuft" }, "● Diagnose") : null,
    server.has_secrets ? icon("lock", "lock", "enthält Geheimnisse") : null,
    server.collision
      ? h(
          "button",
          {
            class: "icon-btn warn-icon",
            title: "Name existiert in mehreren Scopes – Konflikt anzeigen",
            onclick: (e: Event) => {
              e.stopPropagation();
              handlers.onConflict(server);
            },
          },
          icon("alert"),
        )
      : null,
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

  const cls = sel?.checkbox.checked ? "card selected" : "card";
  return h("div", { class: cls, "data-name": server.name }, titleRow, summary, actions);
}

/// Filterleiste (Suche, Status, Transport). Mutiert `filter` direkt – der State
/// gehört main.ts und überlebt so `refresh()` – und meldet jede Änderung.
function filterBar(filter: FilterState, onChange: () => void): HTMLElement {
  const search = h("input", {
    type: "search",
    class: "inp filter-search",
    placeholder: "Server suchen (Name, Beschreibung)…",
  }) as HTMLInputElement;
  search.value = filter.query;
  search.addEventListener("input", () => {
    filter.query = search.value;
    onChange();
  });

  const statusSel = h(
    "select",
    { class: "inp filter-select", title: "Nach Status filtern" },
    h("option", { value: "all" }, "Status: alle"),
    h("option", { value: "connected" }, "verbunden"),
    h("option", { value: "failed" }, "Fehler"),
    h("option", { value: "needs_auth" }, "Login nötig"),
    h("option", { value: "disabled" }, "deaktiviert"),
  ) as HTMLSelectElement;
  statusSel.value = filter.status;
  statusSel.addEventListener("change", () => {
    filter.status = statusSel.value as StatusFilter;
    onChange();
  });

  const transportSel = h(
    "select",
    { class: "inp filter-select", title: "Nach Transport filtern" },
    h("option", { value: "all" }, "Transport: alle"),
    h("option", { value: "stdio" }, "stdio"),
    h("option", { value: "http" }, "http"),
    h("option", { value: "sse" }, "sse"),
  ) as HTMLSelectElement;
  transportSel.value = filter.transport;
  transportSel.addEventListener("change", () => {
    filter.transport = transportSel.value as TransportFilter;
    onChange();
  });

  return h("div", { class: "filter-bar" }, search, statusSel, transportSel);
}

export interface ServerListOptions {
  handlers: ListHandlers;
  /// Sichtbare Gruppen (Scope-Abschnitte) nach `GROUPS.key`; undefined = alle.
  visibleGroups?: string[];
  /// Auswahl + Bulk-Aktionen. Ohne diesen Kontext gibt es weder Filterleiste
  /// noch Auswahl-Checkboxen (z. B. in reduzierten Ansichten).
  bulk?: BulkContext;
  /// Auswahl-Schlüssel des Servers mit laufender Diagnose-Session (Badge „Diagnose").
  activeLogKey?: string | null;
}

export function renderServerList(
  servers: MergedServer[],
  opts: ServerListOptions,
): HTMLElement {
  const { handlers, visibleGroups, bulk, activeLogKey } = opts;
  const root = h("div", { class: "server-list" });
  const bodyEl = h("div", { class: "server-list-body" });
  const bulkBar = h("div", { class: "bulk-bar" });

  // Server, die in der aktuellen Ansicht sichtbaren Gruppen angehören.
  const inView = (s: MergedServer): boolean => {
    for (const g of GROUPS) {
      if (visibleGroups && !visibleGroups.includes(g.key)) continue;
      if (g.match(s)) return true;
    }
    return false;
  };

  // Auswahl gibt es nur mit Bulk-Kontext; die Zustandsmaschine dazu liegt in
  // `serverListSelection.ts` (Karten-/Gruppenregister, Tri-State, Filter-Sicht).
  const sel = bulk
    ? createSelectionRegistry(servers, bulk.selection, () => rerenderBulkBar())
    : null;

  const rerenderBulkBar = (): void => {
    clear(bulkBar);
    if (!bulk || !sel || sel.size() === 0) {
      bulkBar.classList.remove("visible");
      return;
    }
    bulkBar.classList.add("visible");
    const selected = sel.selectedServers();
    // Ausgewählte, die der aktuelle Filter ausblendet – ehrlich ausweisen.
    const hidden = sel.hiddenCount();
    const countText = hidden > 0 ? `${selected.length} ausgewählt (${hidden} ausgeblendet)` : `${selected.length} ausgewählt`;
    bulkBar.append(
      h("span", { class: "bulk-count", text: countText }),
      h("span", { class: "spacer" }),
      h(
        "button",
        { class: "btn btn-small", onclick: () => bulk.onBulk("enable", selected) },
        "Aktivieren",
      ),
      h(
        "button",
        { class: "btn btn-small", onclick: () => bulk.onBulk("disable", selected) },
        "Deaktivieren",
      ),
      h(
        "button",
        { class: "btn btn-small btn-danger", onclick: () => bulk.onBulk("remove", selected) },
        "Entfernen",
      ),
      h(
        "button",
        { class: "btn btn-small", onclick: () => sel.clearSelection() },
        "Auswahl aufheben",
      ),
    );
  };

  const rerenderBody = (): void => {
    clear(bodyEl);
    const shown = bulk ? filterServers(servers, bulk.filter) : servers;
    sel?.beginBody(shown);

    for (const group of GROUPS) {
      if (visibleGroups && !visibleGroups.includes(group.key)) continue;
      const members = shown.filter(group.match);
      if (members.length === 0) continue;

      const header = h(
        "div",
        { class: "group-header" },
        h("span", { text: group.label }),
        h("span", { class: "count", text: String(members.length) }),
      );

      // „Alle auswählen" nur, wenn die Gruppe auswählbare Server enthält.
      if (sel) {
        const selectable = members.filter(isSelectable);
        if (selectable.length > 0) {
          header.prepend(sel.groupBox(selectable.map(selectionKey)));
        }
      }

      bodyEl.append(header);
      for (const s of members) {
        const key = selectionKey(s);
        const logActive = activeLogKey != null && key === activeLogKey;
        if (sel && isSelectable(s)) {
          const card = serverCard(s, handlers, { checkbox: sel.cardBox(key) }, logActive);
          sel.attachCard(key, card);
          bodyEl.append(card);
        } else {
          bodyEl.append(serverCard(s, handlers, null, logActive));
        }
      }
    }

    if (bodyEl.childElementCount === 0) {
      const anyInView = servers.some(inView);
      const msg = anyInView ? "Keine Server passen zum Filter." : "Keine MCP-Server gefunden.";
      bodyEl.append(h("p", { class: "muted" }, msg));
    }
  };

  // Filterleiste (nur bei vorhandenen Servern; bleibt beim Tippen bestehen -> Fokus).
  if (bulk && servers.some(inView)) {
    root.append(filterBar(bulk.filter, () => {
      rerenderBody();
      rerenderBulkBar(); // „ausgeblendet"-Zähler aktualisieren
    }));
  }

  rerenderBody();
  rerenderBulkBar();
  root.append(bodyEl, bulkBar);
  return root;
}
