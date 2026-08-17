import { h, clear, svgEl } from "../dom";
import { icon, setIcon } from "../icons";
import type { MergedServer, ServerEntry, Scope, Introspection, ServerStatus, RuntimePreflight, ProjectInfo, MetricPoint, ClientInfo, CopyEndpoint } from "../ipc";
import { revealServerEntry, setScope, listProjects, introspectServer, peekIntrospection, healthCheck, preflightServer, getMetrics } from "../ipc";
import {
  revealClientEntry,
  checkClientServer,
  introspectClientServer,
  peekClientIntrospection,
  preflightClientServer,
  copyServerTo,
} from "../ipc";
import { transportOfEntry } from "../transport";
import { openModal } from "../modal";
import { openConfirm } from "../confirm";
import { toast } from "../toast";
import { statusMeta, formatLatency } from "./serverList";
import { field } from "./serverForm";
import { openToolPlayground, openResourcePlayground, openPromptPlayground } from "./playground";
import { createLogView } from "./logView";

const ALL_SCOPES: Scope[] = ["user", "local", "project"];

const SCOPE_LABELS: Record<Scope, string> = {
  user: "Claude Code – user (global)",
  local: "Claude Code – local (projekt-privat)",
  project: "Claude Code – project (.mcp.json)",
};

/// Herkunft eines Servers als getaggte Union. Löst die Invariante „ohne
/// `client_id` ist `scope` gesetzt" **einmal** auf, statt sie an jeder
/// Aufrufstelle per `as Scope` zu behaupten.
/// `null` = extern verwaltet (claude.ai-Connector / Plugin): weder lokale
/// Definition noch Aktionen.
type ServerOrigin =
  | { kind: "client"; id: string }
  | { kind: "scope"; scope: Scope; projectPath?: string };

function originOf(server: MergedServer): ServerOrigin | null {
  if (server.client_id) return { kind: "client", id: server.client_id };
  if (server.scope) {
    return { kind: "scope", scope: server.scope, projectPath: server.project_path ?? undefined };
  }
  return null;
}

function row(label: string, value: Node | string): HTMLElement {
  return h(
    "div",
    { class: "kv-row" },
    h("div", { class: "kv-key", text: label }),
    h("div", { class: "kv-val" }, typeof value === "string" ? document.createTextNode(value) : value),
  );
}

function mapRows(map: Record<string, string> | undefined): HTMLElement {
  const wrap = h("div", { class: "kv-map" });
  const entries = Object.entries(map ?? {});
  if (entries.length === 0) {
    wrap.append(h("span", { class: "muted", text: "—" }));
    return wrap;
  }
  for (const [k, v] of entries) {
    wrap.append(
      h(
        "div",
        { class: "kv-sub" },
        h("span", { class: "mono kv-subkey", text: k }),
        h("span", { class: "mono kv-subval", text: v }),
      ),
    );
  }
  return wrap;
}

function effectiveType(entry: ServerEntry): string {
  if (entry.type) return entry.type;
  if (entry.url) return "http/sse";
  if (entry.command) return "stdio";
  return "?";
}

function definitionBody(entry: ServerEntry): HTMLElement {
  const wrap = h("div", { class: "kv-table" });
  wrap.append(row("Typ", effectiveType(entry)));
  if (entry.command) wrap.append(row("Command", h("span", { class: "mono", text: entry.command })));
  if (entry.args && entry.args.length) {
    wrap.append(row("Args", h("span", { class: "mono", text: entry.args.join(" ") })));
  }
  if (entry.url) wrap.append(row("URL", h("span", { class: "mono", text: entry.url })));
  wrap.append(row("Env", mapRows(entry.env)));
  wrap.append(row("Headers", mapRows(entry.headers)));
  return wrap;
}

interface CapItem {
  title: string;
  desc?: string;
  /// Optionaler Aktions-Button (Playground: Testen/Lesen/Abrufen).
  action?: HTMLElement | null;
}

/// Eine aufklappbare Gruppe (Tools/Ressourcen/Prompts) mit Namen + Beschreibung.
function capsGroup(label: string, items: CapItem[]): HTMLElement {
  const list = h("div", { class: "caps-list" });
  for (const it of items) {
    list.append(
      h(
        "div",
        { class: "caps-item" },
        h(
          "div",
          { class: "caps-item-head" },
          h("div", { class: "mono caps-item-name", text: it.title }),
          it.action ?? null,
        ),
        it.desc ? h("div", { class: "muted caps-item-desc", text: it.desc }) : null,
      ),
    );
  }
  return h(
    "details",
    { class: "caps-group" },
    h("summary", { text: `${label} (${items.length})` }),
    list,
  );
}

/// Kleiner Playground-Aktions-Button (nur wenn der Server aktiviert ist).
function playgroundBtn(label: string, onClick: () => void): HTMLElement {
  const btn = h("button", { class: "btn btn-small", type: "button" }, label);
  btn.addEventListener("click", onClick);
  return btn;
}

/// Rendert das Introspektions-Ergebnis: bei Erfolg Zähler/Server-Info/Listen,
/// bei Fehler ein Banner. Notizen und ein erfasster stderr-Log-Block (falls
/// vorhanden) werden in beiden Fällen angehängt.
function renderIntrospection(intro: Introspection, server: MergedServer): HTMLElement {
  const wrap = h("div", { class: "caps" });
  // Playground-Aktionen nur für aktivierte Claude-Code-Server: der Playground
  // löst die Definition über Scope + Projekt auf und kennt Datei-Clients nicht –
  // sonst erschienen tote „Testen…"-Knöpfe.
  const canRun = server.enabled && server.scope != null;

  if (intro.error) {
    wrap.append(h("p", { class: "form-status error", text: intro.error }));
    // Wenn initialize gelang, aber ein späterer Schritt scheiterte, ist die
    // gemessene Verbindungs-/Startzeit trotzdem aussagekräftig (Diagnose).
    if (intro.connectMs !== undefined) {
      wrap.append(
        h("p", {
          class: "muted",
          title: "Zeit bis zur initialize-Antwort – danach fehlgeschlagen",
          text: `Verbindungs-/Startzeit: ${formatLatency(intro.connectMs)}`,
        }),
      );
    }
  } else {
    wrap.append(
      h(
        "div",
        { class: "caps-summary" },
        h("span", { class: "badge badge-scope", text: `${intro.tools.length} Tools` }),
        h("span", { class: "badge badge-scope", text: `${intro.resources.length} Ressourcen` }),
        h("span", { class: "badge badge-scope", text: `${intro.prompts.length} Prompts` }),
        intro.connectMs !== undefined
          ? h(
              "span",
              { class: "badge badge-latency", title: "Verbindungs-/Startzeit (bis initialize)" },
              formatLatency(intro.connectMs),
            )
          : null,
      ),
    );

    if (intro.serverName) {
      const ver = intro.serverVersion ? ` v${intro.serverVersion}` : "";
      wrap.append(h("p", { class: "muted mono", text: `${intro.serverName}${ver}` }));
    }

    const groups = h("div", { class: "caps-groups" });
    if (intro.tools.length) {
      groups.append(
        capsGroup(
          "Tools",
          intro.tools.map((t) => ({
            title: t.name,
            desc: t.description,
            action: canRun ? playgroundBtn("Testen…", () => openToolPlayground(server, t)) : null,
          })),
        ),
      );
    }
    if (intro.resources.length) {
      groups.append(
        capsGroup(
          "Ressourcen",
          intro.resources.map((r) => ({
            title: r.name ?? r.uri,
            desc: r.description ?? r.uri,
            action: canRun ? playgroundBtn("Lesen…", () => openResourcePlayground(server, r)) : null,
          })),
        ),
      );
    }
    if (intro.prompts.length) {
      groups.append(
        capsGroup(
          "Prompts",
          intro.prompts.map((p) => ({
            title: p.name,
            desc: p.description,
            action: canRun ? playgroundBtn("Abrufen…", () => openPromptPlayground(server, p)) : null,
          })),
        ),
      );
    }
    if (groups.childElementCount) wrap.append(groups);
  }

  for (const note of intro.notes) {
    wrap.append(h("p", { class: "muted caps-note", text: note }));
  }

  // Erfasster stderr des Server-Subprozesses (redigiert). Aufklappbar, damit er
  // bei Erfolg nicht stört, bei Fehler aber den echten Grund liefert.
  if (intro.logs) {
    const logs = h(
      "details",
      { class: "caps-logs" },
      h("summary", { text: "Server-Log (stderr)" }),
      h("pre", { class: "mono caps-log", text: intro.logs }),
    ) as HTMLDetailsElement;
    // Bei einem Fehler direkt aufgeklappt zeigen.
    if (intro.error) logs.open = true;
    wrap.append(logs);
  }
  return wrap;
}

/// Rendert das Preflight-Ergebnis: gefunden (grün + Version/Pfad) oder nicht
/// gefunden (rot + umsetzbarer Hinweis).
function renderPreflight(pf: RuntimePreflight): HTMLElement {
  const wrap = h("div", { class: "caps" });
  wrap.append(
    h(
      "div",
      { class: "runtime-row" },
      pf.found
        ? h("span", { class: "badge badge-ok", text: "verfügbar" })
        : h("span", { class: "badge badge-error", text: "nicht auf PATH" }),
      h("span", { class: "badge badge-scope", text: pf.runtime }),
    ),
  );

  if (pf.found) {
    if (pf.version) wrap.append(h("p", { class: "muted mono", text: pf.version }));
    if (pf.path) wrap.append(h("p", { class: "muted mono", text: pf.path }));
  } else if (pf.hint) {
    wrap.append(h("p", { class: "form-status error", text: pf.hint }));
  }
  return wrap;
}

/// Abschnitt „Laufzeitumgebung": prüft beim Öffnen, ob der benötigte Befehl
/// (node/npx, python/uvx, docker, …) auf PATH verfügbar ist. Billig – startet
/// den Server nicht (Version nur für bekannte Laufzeiten via `--version`).
function runtimeSection(server: MergedServer): HTMLElement | null {
  // Nur für Server mit lokalem Befehl (stdio) und bekannter Herkunft sinnvoll
  // (Claude-Code-Scope oder Datei-Client).
  const origin = originOf(server);
  if (!server.entry?.command || !origin) return null;

  const content = h("div", { class: "caps-content" }, h("p", { class: "muted", text: "Wird geprüft…" }));

  const request =
    origin.kind === "client"
      ? preflightClientServer(origin.id, server.name)
      : preflightServer(server.name, origin.scope, origin.projectPath);

  void request
    .then((pf) => {
      // Modal inzwischen geschlossen? Dann nichts mehr rendern.
      if (!content.isConnected) return;
      clear(content);
      content.append(
        pf ? renderPreflight(pf) : h("p", { class: "muted", text: "Keine Laufzeit zu prüfen." }),
      );
    })
    .catch((e) => {
      if (!content.isConnected) return;
      clear(content);
      content.append(h("p", { class: "form-status error", text: "Preflight fehlgeschlagen: " + String(e) }));
    });

  return h(
    "div",
    { class: "detail-runtime" },
    h("div", { class: "detail-defhead" }, h("h3", { text: "Laufzeitumgebung" })),
    content,
  );
}

/// Abschnitt „Fähigkeiten": On-Demand-Introspektion mit Laden/Aktualisieren-Button.
/// Beim Öffnen wird ein bereits gecachtes Ergebnis (ohne Prozessstart) vorgeladen.
/// Mini-Sparkline der Status-/Latenz-Historie: farbige Punkte je Messung
/// (Verfügbarkeit), plus eine Latenzlinie über die Punkte mit `connectMs`.
function renderSparkline(points: MetricPoint[]): HTMLElement {
  if (points.length < 2) {
    return h("p", { class: "muted", text: "Noch keine Historie – nach ein paar Aktualisierungen." });
  }
  const W = 260;
  const H = 44;
  const pad = 5;
  const n = points.length;
  const xAt = (i: number) => pad + (i / (n - 1)) * (W - 2 * pad);

  const svg = svgEl("svg", {
    viewBox: `0 0 ${W} ${H}`,
    class: "sparkline",
    preserveAspectRatio: "none",
    role: "img",
  });

  // Latenzlinie (nur Punkte mit connectMs; braucht mind. 2).
  const lat = points
    .map((p, i) => ({ i, v: p.connectMs }))
    .filter((o): o is { i: number; v: number } => typeof o.v === "number");
  if (lat.length >= 2) {
    const vals = lat.map((o) => o.v);
    const max = Math.max(...vals);
    const min = Math.min(...vals);
    const range = max - min || 1;
    const yAt = (v: number) => H - pad - ((v - min) / range) * (H - 2 * pad - 6);
    const pts = lat.map((o) => `${xAt(o.i).toFixed(1)},${yAt(o.v).toFixed(1)}`).join(" ");
    svg.append(svgEl("polyline", { points: pts, class: "sparkline-line", fill: "none" }));
  }

  // Status-Punkte auf der Grundlinie.
  points.forEach((p, i) => {
    svg.append(
      svgEl("circle", {
        cx: xAt(i).toFixed(1),
        cy: H - pad,
        r: 2.2,
        class: `spark-dot spark-${p.statusKind}`,
      }),
    );
  });

  const latestLatency = lat.length ? lat[lat.length - 1].v : undefined;
  const caption =
    `letzte ${n} Messungen` + (latestLatency !== undefined ? ` · zuletzt ${formatLatency(latestLatency)}` : "");
  return h("div", { class: "sparkline-wrap" }, svg, h("div", { class: "muted sparkline-caption", text: caption }));
}

/// Abschnitt „Verlauf" im Detail-Modal: lädt die Historie und rendert die
/// Sparkline. Nur für Server mit Scope (externe haben keine Historie).
function metricsSection(server: MergedServer): HTMLElement | null {
  const scope = server.scope;
  if (!scope) return null;
  const box = h("div", { class: "detail-metrics" }, h("p", { class: "muted", text: "Verlauf wird geladen…" }));
  void getMetrics(server.name, scope, server.project_path ?? undefined)
    .then((pts) => {
      if (!box.isConnected) return;
      clear(box);
      box.append(renderSparkline(pts));
    })
    .catch(() => {
      if (!box.isConnected) return;
      clear(box);
      box.append(h("p", { class: "muted", text: "Keine Historie verfügbar." }));
    });
  return h("div", { class: "detail-section" }, h("h3", { text: "Verlauf" }), box);
}

/// „Logs"-Sektion (Feature 08): Live-Diagnose nur für stdio-Server. Gibt das
/// Element + eine `dispose`-Funktion zurück (Listener beim Modal-Schließen abmelden).
function logsSection(
  server: MergedServer,
  opts: DetailOptions,
): { element: HTMLElement; dispose: () => void } | null {
  const scope = server.scope;
  // Nur stdio-Server (lokaler Prozess) und mit bekanntem Scope.
  if (!scope || !server.entry?.command) return null;

  const view = createLogView(server, scope, opts.activeLogSession ?? null, {
    onStarted: (id) => opts.onLogSessionChange?.(server, id),
    onStopped: () => opts.onLogSessionChange?.(server, null),
  });
  const element = h("div", { class: "detail-section" }, h("h3", { text: "Logs (Diagnose)" }), view.element);
  return { element, dispose: view.dispose };
}

function capabilitiesSection(server: MergedServer, opts: DetailOptions): HTMLElement | null {
  // Nur für Server mit lokaler Definition (Claude-Code-Scope oder Datei-Client).
  const origin = originOf(server);
  if (!server.entry || !origin) return null;

  const content = h("div", { class: "caps-content" }, h("p", { class: "muted", text: "Noch nicht geladen." }));

  // Für fehlgeschlagene stdio-Server ist der Knopf primär ein Diagnose-Werkzeug
  // (erfasst den echten stderr), daher kontextabhängige Beschriftung.
  const isStdio = !!server.entry.command;
  const diagnose = server.status.kind === "failed" && isStdio;
  const btnIcon = icon("refresh");
  const btnLabel = h("span", { text: diagnose ? "Diagnose ausführen" : "Fähigkeiten laden" });
  const loadBtn = h("button", { class: "btn btn-small" }, btnIcon, btnLabel) as HTMLButtonElement;
  let loadedOnce = false;

  const showResult = (intro: Introspection) => {
    // Wurde das Modal inzwischen geschlossen (Promise löst verspätet auf),
    // nichts mehr rendern oder als Seiteneffekt die Liste neu zeichnen.
    if (!content.isConnected) return;
    clear(content);
    content.append(renderIntrospection(intro, server));
    loadedOnce = true;
    btnLabel.textContent = "Aktualisieren";
    // Liste nur bei erfolgreicher Introspektion über die Zähler informieren –
    // ein Fehlversuch (leere Listen) soll kein „0·0·0"-Badge erzeugen.
    if (!intro.error) opts.onIntrospected?.(server, intro);
  };

  const load = async (refresh: boolean) => {
    loadBtn.disabled = true;
    btnIcon.classList.add("spin");
    btnLabel.textContent = loadedOnce ? "Aktualisiere…" : "Lade…";
    clear(content);
    content.append(h("p", { class: "muted", text: "Server wird gestartet und abgefragt…" }));
    try {
      showResult(
        origin.kind === "client"
          ? await introspectClientServer(origin.id, server.name, refresh)
          : await introspectServer(server.name, origin.scope, origin.projectPath, refresh),
      );
    } catch (e) {
      clear(content);
      content.append(
        h("p", { class: "form-status error", text: "Introspektion fehlgeschlagen: " + String(e) }),
      );
      btnLabel.textContent = loadedOnce ? "Aktualisieren" : "Erneut versuchen";
    } finally {
      loadBtn.disabled = false;
      btnIcon.classList.remove("spin");
    }
  };

  loadBtn.addEventListener("click", () => void load(loadedOnce));

  // Bereits gecachtes Ergebnis sofort anzeigen (kein Prozessstart).
  const peek =
    origin.kind === "client"
      ? peekClientIntrospection(origin.id, server.name)
      : peekIntrospection(server.name, origin.scope, origin.projectPath);
  void peek
    .then((cached) => {
      if (cached && !loadedOnce) showResult(cached);
    })
    .catch(() => {
      /* Cache-Abruf ist best effort; Fehler ignorieren. */
    });

  return h(
    "div",
    { class: "detail-caps" },
    h("div", { class: "detail-defhead" }, h("h3", { text: "Fähigkeiten" }), loadBtn),
    content,
  );
}

export interface DetailOptions {
  onChanged?: () => void;
  /// Wird nach erfolgreicher Introspektion aufgerufen (z. B. um Listen-Zähler zu aktualisieren).
  onIntrospected?: (server: MergedServer, intro: Introspection) => void;
  /// Wird nach einem erneuten Health-Check aufgerufen, damit die Liste den neuen
  /// Status ohne teuren Full-Refresh übernehmen kann.
  onRechecked?: (server: MergedServer, status: ServerStatus) => void;
  /// Aktive Log-Session-Id für DIESEN Server (Feature 08), falls eine läuft.
  activeLogSession?: string | null;
  /// Meldet Start (Id) / Stop (null) einer Log-Session – für das Listen-Badge.
  onLogSessionChange?: (server: MergedServer, id: string | null) => void;
  /// Aktuell gewählter Projekt-Kontext (`undefined` = kein Projekt gewählt).
  /// Wird beim Scope-Wechsel als ZIEL-Projekt gebraucht.
  projectPath?: string;
  /// Erkannte Datei-Clients (Feature 16) – für „Kopieren nach…" und den
  /// Pfad-/Neustart-Hinweis bei Client-Servern.
  clients?: ClientInfo[];
}

/// Ein wählbares Ziel beim Kopieren – getaggt, damit der Select-Wert nur an
/// EINER Stelle interpretiert wird.
type CopyTarget = { kind: "client"; client: ClientInfo } | { kind: "scope"; scope: Scope };

interface TargetOption {
  value: string;
  label: string;
  target: CopyTarget;
  /// Das Ziel kann diese Definition nicht laden (Remote-Server in einen
  /// Client ohne Remote-Fähigkeit) – die Option wird deaktiviert statt den
  /// Fehler bis zum Speichern zu verstecken.
  blocked?: boolean;
}

/// Alle Ziel-Optionen: die drei Claude-Code-Scopes plus je ein erkannter
/// Datei-Client (die Quelle selbst ausgenommen).
function targetOptions(
  clients: ClientInfo[],
  sourceClientId: string | null,
  isRemote: boolean,
): TargetOption[] {
  const options: TargetOption[] = ALL_SCOPES.map((scope) => ({
    value: `scope:${scope}`,
    label: SCOPE_LABELS[scope],
    target: { kind: "scope", scope },
  }));
  for (const client of clients) {
    if (client.id === sourceClientId) continue;
    const blocked = isRemote && !client.caps.remote;
    options.push({
      value: `client:${client.id}`,
      label: blocked ? `${client.label} – nimmt keine Remote-Server` : client.label,
      target: { kind: "client", client },
      blocked,
    });
  }
  return options;
}

function buildTargetSelect(options: TargetOption[], selected: string): HTMLSelectElement {
  const select = h("select", { class: "inp" }) as HTMLSelectElement;
  for (const option of options) {
    const el = h("option", { value: option.value }, option.label) as HTMLOptionElement;
    if (option.blocked) el.disabled = true;
    select.append(el);
  }
  select.value = selected;
  return select;
}

/// Öffnet ein Formular-Modal zum Kopieren eines Servers. Das Original bleibt
/// bestehen; Ziel ist ein Claude-Code-Scope oder ein erkannter Datei-Client.
function openCopyModal(server: MergedServer, clients: ClientInfo[], onDone: () => void): void {
  const source = originOf(server);
  if (!source) return; // extern verwaltet: nichts zu kopieren
  const sourceClientId = source.kind === "client" ? source.id : null;
  const sourceScope = source.kind === "scope" ? source.scope : null;

  // Quelle als Endpunkt – unveränderlich, kommt aus dem angezeigten Server.
  const from: CopyEndpoint =
    source.kind === "client"
      ? { kind: "client", id: source.id }
      : { kind: "claude_code", scope: source.scope, project_path: server.project_path };

  // Remote-Definition (http/sse)? Dann sind Clients ohne Remote-Fähigkeit kein
  // gültiges Ziel.
  const isRemote = server.entry ? transportOfEntry(server.entry) !== "stdio" : false;

  const nameInput = h("input", { class: "inp" }) as HTMLInputElement;
  let nameTouched = false;
  nameInput.addEventListener("input", () => {
    nameTouched = true;
  });

  const options = targetOptions(clients, sourceClientId, isRemote);
  // Vorbelegung: bei Claude-Code-Servern der eigene Scope (klassisches
  // Duplizieren), bei Client-Servern der erste Eintrag (user).
  const targetSelect = buildTargetSelect(options, `scope:${sourceScope ?? "user"}`);

  // Projekt-Auswahl: Dropdown aus bekannten Projekten + Freitext-Pfad.
  const projSelect = h("select", { class: "inp" }, h("option", { value: "" }, "– Projekt wählen –")) as HTMLSelectElement;
  const projInput = h("input", { class: "inp mono", placeholder: "/pfad/zum/projekt" }) as HTMLInputElement;
  void listProjects()
    .then((projs: ProjectInfo[]) => {
      for (const p of projs) {
        const label = p.exists ? p.path : `${p.path} (fehlt)`;
        const opt = h("option", { value: p.path }, label) as HTMLOptionElement;
        if (!p.exists) opt.disabled = true; // fehlende Verzeichnisse nicht auswählbar
        projSelect.appendChild(opt);
      }
      // Standard: aktuelles Projekt des Servers vorbelegen, falls vorhanden.
      if (server.project_path) {
        projSelect.value = server.project_path;
        projInput.value = server.project_path;
      }
    })
    .catch(() => {
      projSelect.appendChild(
        h("option", { value: "" }, "(Projekte nicht ladbar – Pfad manuell eingeben)"),
      );
    });
  projSelect.addEventListener("change", () => {
    if (projSelect.value) projInput.value = projSelect.value;
  });

  const projField = field(
    "Projekt",
    h("div", { class: "scope-row" }, projSelect, projInput),
    "Ziel-Verzeichnis für local/project (Dropdown oder eigener Pfad).",
  );
  const targetHint = h("div", { class: "field-hint", text: "" });

  /// Gewähltes Ziel. Der Select wird ausschließlich aus `options` aufgebaut,
  /// daher greift der Fallback (erster Scope) nie.
  const currentTarget = (): CopyTarget =>
    (options.find((o) => o.value === targetSelect.value) ?? options[0]).target;

  /// Ziel als Endpunkt für das Backend.
  const endpointOf = (to: CopyTarget): CopyEndpoint =>
    to.kind === "client"
      ? { kind: "client", id: to.client.id }
      : {
          kind: "claude_code",
          scope: to.scope,
          project_path: to.scope === "user" ? null : projInput.value.trim() || null,
        };

  const syncTargetUi = () => {
    const to = currentTarget();
    projField.style.display = to.kind === "scope" && to.scope !== "user" ? "" : "none";
    targetHint.textContent =
      to.kind === "client"
        ? `${to.client.label} lädt die Konfiguration beim Neustart. Datei: ${to.client.config_path}`
        : to.scope === "project"
          ? "Der Server wird in .mcp.json des Zielprojekts angelegt und muss dort ggf. erst bestätigt werden."
          : "";
    // Zeigt das Ziel auf denselben Ort wie die Quelle, braucht die Kopie zwingend
    // einen anderen Namen. Solange der Nutzer den Namen nicht angefasst hat,
    // passend vorbelegen.
    const sameTarget = to.kind === "scope" && to.scope === sourceScope;
    if (!nameTouched) nameInput.value = sameTarget ? `${server.name}-kopie` : server.name;
  };
  targetSelect.addEventListener("change", syncTargetUi);
  syncTargetUi();

  const status = h("p", { class: "form-status" });
  const cancelBtn = h("button", { class: "btn" }, "Abbrechen") as HTMLButtonElement;
  const okBtn = h("button", { class: "btn btn-primary" }, "Kopieren") as HTMLButtonElement;

  const form = h(
    "div",
    { class: "server-form" },
    field("Name der Kopie", nameInput),
    field("Ziel", targetSelect),
    targetHint,
    projField,
    status,
  );

  const modal = openModal(`Kopieren nach…: ${server.name}`, form, [cancelBtn, okBtn]);
  cancelBtn.addEventListener("click", () => modal.close());
  nameInput.focus();
  nameInput.select();

  okBtn.addEventListener("click", async () => {
    const newName = nameInput.value.trim();
    const to = currentTarget();
    const endpoint = endpointOf(to);

    status.className = "form-status";
    if (!newName) {
      status.className = "form-status error";
      status.textContent = "Bitte einen Namen für die Kopie angeben.";
      return;
    }
    if (endpoint.kind === "claude_code" && endpoint.scope !== "user" && !endpoint.project_path) {
      status.className = "form-status error";
      status.textContent = "Bitte ein Zielprojekt wählen oder einen Pfad angeben.";
      return;
    }

    okBtn.disabled = true;
    cancelBtn.disabled = true;
    status.textContent = "wird angelegt…";
    try {
      await copyServerTo(server.name, from, endpoint, newName);
      toast(
        to.kind === "client"
          ? `„${newName}" in ${to.client.label} angelegt – Neustart nötig, damit es greift.`
          : `„${newName}" in ${to.scope} angelegt`,
      );
      modal.close();
      onDone();
    } catch (e) {
      okBtn.disabled = false;
      cancelBtn.disabled = false;
      status.className = "form-status error";
      status.textContent = "Fehler: " + String(e);
    }
  });
}

export function openDetail(server: MergedServer, opts: DetailOptions = {}): void {
  // Status-Bereich (Badges + sichtbare Fehlergrund-Zeile) neu-rendbar halten,
  // damit „Erneut prüfen" ihn nach einem Health-Check aktualisieren kann.
  const metaWrap = h("div");
  const recheckIcon = icon("refresh");
  const recheckBtn = h(
    "button",
    { class: "btn btn-small", title: "Status neu prüfen" },
    recheckIcon,
    h("span", { text: "Erneut prüfen" }),
  ) as HTMLButtonElement;

  const renderStatus = () => {
    clear(metaWrap);
    const st = statusMeta(server.status);
    metaWrap.append(
      h(
        "div",
        { class: "detail-meta" },
        h("span", { class: "badge badge-scope", text: server.origin }),
        h("span", { class: `badge ${st.cls}`, title: st.title }, st.label),
        server.enabled
          ? h("span", { class: "badge badge-ok", text: "aktiv" })
          : h("span", { class: "badge badge-muted", text: "deaktiviert" }),
        server.collision
          ? h("span", { class: "badge badge-warn", text: "Namens-Kollision" })
          : null,
        h("span", { class: "spacer" }),
        recheckBtn,
      ),
    );
    // Fehlergrund sichtbar machen (nicht nur als Tooltip am Badge).
    if (server.status.kind === "failed" && server.status.detail) {
      metaWrap.append(h("p", { class: "form-status error detail-status", text: server.status.detail }));
    }
  };

  recheckBtn.addEventListener("click", async () => {
    recheckBtn.disabled = true;
    recheckIcon.classList.add("spin");
    try {
      // Datei-Clients kennt die claude-CLI nicht – dort per echtem Handshake prüfen.
      const status = server.client_id
        ? await checkClientServer(server.client_id, server.name)
        : await healthCheck(server.name, server.project_path ?? undefined);
      server.status = status;
      renderStatus();
      opts.onRechecked?.(server, status);
      const m = statusMeta(status);
      toast(`Status: ${m.label}`, status.kind === "failed" ? "error" : "ok");
    } catch (e) {
      toast("Prüfen fehlgeschlagen: " + String(e), "error");
    } finally {
      recheckBtn.disabled = false;
      recheckIcon.classList.remove("spin");
    }
  });

  renderStatus();

  const defWrap = h("div");
  let revealed = false;
  let revealedEntry: ServerEntry | null = null;

  const revealIcon = icon("eye");
  const revealLabel = h("span", { text: "Secrets anzeigen" });
  const revealBtn = h("button", { class: "btn btn-small" }, revealIcon, revealLabel) as HTMLButtonElement;

  const renderDef = () => {
    clear(defWrap);
    if (!server.entry) {
      defWrap.append(h("p", { class: "muted" }, "Extern verwaltet – keine lokale Definition vorhanden."));
      if (server.summary) defWrap.append(h("div", { class: "mono", text: server.summary }));
      return;
    }
    const entry = revealed && revealedEntry ? revealedEntry : server.entry;
    defWrap.append(definitionBody(entry));
  };

  revealBtn.addEventListener("click", async () => {
    const origin = originOf(server);
    if (!origin) return;
    if (!revealed) {
      try {
        revealBtn.disabled = true;
        revealedEntry =
          origin.kind === "client"
            ? await revealClientEntry(origin.id, server.name)
            : await revealServerEntry(origin.scope, server.name, origin.projectPath);
        revealed = true;
        setIcon(revealIcon, "eye-off");
        revealLabel.textContent = "Secrets verbergen";
      } catch (e) {
        revealLabel.textContent = "Fehler beim Anzeigen";
        console.error(e);
      } finally {
        revealBtn.disabled = false;
      }
    } else {
      revealed = false;
      setIcon(revealIcon, "eye");
      revealLabel.textContent = "Secrets anzeigen";
    }
    renderDef();
  });

  renderDef();

  // Scope-Wechsel und Kopieren. Der Abschnitt erscheint für beide Welten;
  // „Scope ändern" gibt es nur in der Claude-Code-Welt.
  let scopeSection: HTMLElement | null = null;
  if (server.editable && (server.scope || server.client_id)) {
    const parts: Array<HTMLElement | null> = [];
    if (server.scope) {
      const currentScope = server.scope;
      const select = h(
        "select",
        { class: "inp" },
        ...ALL_SCOPES.filter((s) => s !== currentScope).map((s) => h("option", { value: s }, s)),
      ) as HTMLSelectElement;
      const moveBtn = h("button", { class: "btn btn-small" }, "Verschieben");
      moveBtn.addEventListener("click", () => {
        const target = select.value as Scope;
        // Zielprojekt MUSS mit: ohne den Parameter fällt das Backend aufs
        // Home-Verzeichnis zurück und legt dort an. Weil die Verifikation dann
        // ebenfalls Home liest, bleibt der Fehler unentdeckt – und danach wird die
        // Quelle korrekt gelöscht: der Server verschwindet aus dem Projekt und
        // liegt unbemerkt im Home-Projekt.
        const targetProject = opts.projectPath ?? server.project_path ?? undefined;
        // user-Scope liegt immer global in ~/.claude.json, unabhängig vom Projekt.
        const targetLabel =
          target === "user" ? "global (~/.claude.json)" : (targetProject ?? "Home-Verzeichnis");
        openConfirm({
          title: `Scope ändern: ${server.name}`,
          message: `„${server.name}" von ${currentScope} nach ${target} verschieben? Zuerst im Ziel anlegen, dann aus der Quelle entfernen.`,
          extra: h(
            "p",
            { class: "muted" },
            "Ziel: ",
            h("span", { class: "mono", text: targetLabel }),
          ),
          confirmLabel: "Verschieben",
          onConfirm: async () => {
            await setScope(
              server.name,
              currentScope,
              target,
              server.project_path ?? undefined,
              targetProject,
            );
          },
          onDone: () => {
            toast(`Scope → ${target}`);
            modal.close();
            opts.onChanged?.();
          },
        });
      });
      parts.push(
        h("h3", { text: "Scope ändern" }),
        h("div", { class: "scope-row" }, select, moveBtn),
      );
    }

    const copyBtn = h("button", { class: "btn btn-small" }, "Kopieren nach…");
    copyBtn.addEventListener("click", () => {
      openCopyModal(server, opts.clients ?? [], () => {
        modal.close();
        opts.onChanged?.();
      });
    });
    parts.push(
      h("h3", { text: "Kopieren nach…" }),
      h(
        "div",
        { class: "scope-row" },
        h("span", {
          class: "muted",
          text: "Kopie in einen anderen Scope, ein anderes Projekt oder einen anderen Client anlegen.",
        }),
        copyBtn,
      ),
    );
    scopeSection = h("div", { class: "detail-scope" }, ...parts);
  }

  const logs = logsSection(server, opts);
  // Datei-Client (Feature 16): Herkunftsdatei und Neustart-Hinweis zeigen.
  const clientInfo = server.client_id
    ? (opts.clients ?? []).find((c) => c.id === server.client_id)
    : undefined;
  const body = h(
    "div",
    { class: "detail" },
    metaWrap,
    server.project_path
      ? h("p", { class: "muted mono", text: `Projekt: ${server.project_path}` })
      : null,
    clientInfo ? h("p", { class: "muted mono", text: clientInfo.config_path }) : null,
    server.client_id
      ? h("p", {
          class: "muted",
          text: `${clientInfo?.label ?? "Der Client"} lädt die Konfiguration erst beim Neustart neu.`,
        })
      : null,
    h(
      "div",
      { class: "detail-defhead" },
      h("h3", { text: "Definition" }),
      server.editable && server.has_secrets ? revealBtn : null,
    ),
    defWrap,
    runtimeSection(server),
    capabilitiesSection(server, opts),
    logs?.element ?? null,
    metricsSection(server),
    scopeSection,
  );

  // Beim Schließen den Log-Event-Listener abmelden (Session läuft weiter).
  const modal = openModal(server.name, body, undefined, () => logs?.dispose());
}
