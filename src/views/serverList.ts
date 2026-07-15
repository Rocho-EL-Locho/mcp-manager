import { h, clear } from "../dom";
import { icon } from "../icons";
import { switchControl } from "../switch";
import { transportOfEntry } from "../transport";
import type { MergedServer, ServerStatus, Scope } from "../ipc";

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

/// Eindeutiger Auswahl-Schlüssel: Scope (bzw. origin bei externen) + Name + Projekt,
/// weil derselbe Name in mehreren Scopes/Projekten vorkommen kann (collision).
export function selectionKey(s: MergedServer): string {
  return `${s.scope ?? s.origin}::${s.name}::${s.project_path ?? ""}`;
}

/// Nur Server mit bekanntem Scope sind auswählbar; externe (Connector/Plugin) nicht.
export function isSelectable(s: MergedServer): boolean {
  return s.scope !== null;
}

/// Transport aus der Definition ableiten (analog serverForm). Null, wenn unbekannt
/// (externe Server ohne Definition).
export function serverTransport(s: MergedServer): "stdio" | "http" | "sse" | null {
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

/// Reine Filterfunktion (testbar): Suche über Name + Summary, kombiniert mit
/// Status- und Transport-Filter.
export function filterServers(servers: MergedServer[], filter: FilterState): MergedServer[] {
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

interface SelectContext {
  selected: boolean;
  /// Vorgefertigte Checkbox (Zustand + Handler in renderServerList verdrahtet),
  /// hier nur platziert – so kann die Auswahl gezielt (ohne Full-Rebuild) toggeln.
  checkbox: HTMLInputElement;
}

function serverCard(
  server: MergedServer,
  handlers: ListHandlers,
  sel: SelectContext | null,
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

  const cls = sel?.selected ? "card selected" : "card";
  return h("div", { class: cls, "data-name": server.name }, titleRow, summary, actions);
}

export function renderServerList(
  servers: MergedServer[],
  handlers: ListHandlers,
  visibleGroups?: string[],
  bulk?: BulkContext,
): HTMLElement {
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

  // Register der auswählbaren Karten und Gruppen-Boxen der aktuell gerenderten
  // Liste – erlaubt gezielte Auswahl-Updates ohne kompletten Neuaufbau (nur Filter-
  // wechsel baut den Body neu).
  const selCards = new Map<string, { card: HTMLElement; box: HTMLInputElement }>();
  let groupBoxes: Array<{ box: HTMLInputElement; keys: string[] }> = [];

  // Einmalige Key→Server-Map + gecachte Keys der aktuell sichtbaren (gefilterten)
  // Server: hält Auswahl-Toggles bei O(Auswahl) statt jedes Mal O(alle Server).
  const serverByKey = new Map(servers.map((s) => [selectionKey(s), s]));
  let shownKeys = new Set<string>();

  const syncGroupBox = (g: { box: HTMLInputElement; keys: string[] }): void => {
    if (!bulk) return;
    const all = g.keys.every((k) => bulk.selection.has(k));
    const some = g.keys.some((k) => bulk.selection.has(k));
    g.box.checked = all;
    g.box.indeterminate = !all && some;
  };

  const setCardSelected = (key: string, selected: boolean): void => {
    const entry = selCards.get(key);
    if (!entry) return;
    entry.box.checked = selected;
    entry.card.classList.toggle("selected", selected);
  };

  const rerenderBulkBar = (): void => {
    clear(bulkBar);
    if (!bulk || bulk.selection.size === 0) {
      bulkBar.classList.remove("visible");
      return;
    }
    bulkBar.classList.add("visible");
    const selectedKeys = [...bulk.selection];
    const selected = selectedKeys
      .map((k) => serverByKey.get(k))
      .filter((s): s is MergedServer => s !== undefined);
    // Ausgewählte, die der aktuelle Filter ausblendet – ehrlich ausweisen (shownKeys
    // wird von rerenderBody gepflegt, ändert sich nur bei Filterwechsel).
    const hidden = selectedKeys.filter((k) => serverByKey.has(k) && !shownKeys.has(k)).length;
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
        {
          class: "btn btn-small",
          onclick: () => {
            if (!bulk) return;
            for (const key of bulk.selection) setCardSelected(key, false);
            bulk.selection.clear();
            for (const g of groupBoxes) syncGroupBox(g);
            rerenderBulkBar();
          },
        },
        "Auswahl aufheben",
      ),
    );
  };

  const rerenderBody = (): void => {
    clear(bodyEl);
    selCards.clear();
    groupBoxes = [];
    const shown = bulk ? filterServers(servers, bulk.filter) : servers;
    shownKeys = new Set(shown.map(selectionKey));

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
      if (bulk) {
        const selectable = members.filter(isSelectable);
        if (selectable.length > 0) {
          const keys = selectable.map(selectionKey);
          const groupBox = h("input", {
            type: "checkbox",
            class: "select-box",
            title: "Alle in dieser Gruppe auswählen",
          }) as HTMLInputElement;
          const g = { box: groupBox, keys };
          groupBoxes.push(g);
          syncGroupBox(g);
          groupBox.addEventListener("change", () => {
            const checked = groupBox.checked;
            for (const key of keys) {
              if (checked) bulk.selection.add(key);
              else bulk.selection.delete(key);
              setCardSelected(key, checked);
            }
            groupBox.indeterminate = false;
            rerenderBulkBar();
          });
          header.prepend(groupBox);
        }
      }

      bodyEl.append(header);
      for (const s of members) {
        let sel: SelectContext | null = null;
        if (bulk && isSelectable(s)) {
          const key = selectionKey(s);
          const box = h("input", {
            type: "checkbox",
            class: "select-box",
            title: "Für Bulk-Aktion auswählen",
          }) as HTMLInputElement;
          box.checked = bulk.selection.has(key);
          box.addEventListener("change", () => {
            const checked = box.checked;
            if (checked) bulk.selection.add(key);
            else bulk.selection.delete(key);
            const entry = selCards.get(key);
            if (entry) entry.card.classList.toggle("selected", checked);
            for (const g of groupBoxes) if (g.keys.includes(key)) syncGroupBox(g);
            rerenderBulkBar();
          });
          sel = { selected: box.checked, checkbox: box };
          const card = serverCard(s, handlers, sel);
          selCards.set(key, { card, box });
          bodyEl.append(card);
        } else {
          bodyEl.append(serverCard(s, handlers, null));
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
    const filter = bulk.filter;

    const search = h("input", {
      type: "search",
      class: "inp filter-search",
      placeholder: "Server suchen (Name, Beschreibung)…",
    }) as HTMLInputElement;
    search.value = filter.query;
    search.addEventListener("input", () => {
      filter.query = search.value;
      rerenderBody();
      rerenderBulkBar(); // „ausgeblendet"-Zähler aktualisieren
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
      rerenderBody();
      rerenderBulkBar();
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
      rerenderBody();
      rerenderBulkBar();
    });

    root.append(h("div", { class: "filter-bar" }, search, statusSel, transportSel));
  }

  rerenderBody();
  rerenderBulkBar();
  root.append(bodyEl, bulkBar);
  return root;
}
