import { h, clear, svgEl } from "../dom";
import { icon, setIcon } from "../icons";
import type { MergedServer, ServerEntry, Scope, Introspection, ServerStatus, RuntimePreflight, ProjectInfo, MetricPoint } from "../ipc";
import { revealServerEntry, setScope, cloneServer, listProjects, introspectServer, peekIntrospection, healthCheck, preflightServer, getMetrics } from "../ipc";
import { openModal } from "../modal";
import { openConfirm } from "../confirm";
import { toast } from "../toast";
import { statusMeta, formatLatency } from "./serverList";
import { field } from "./serverForm";
import { openToolPlayground, openResourcePlayground, openPromptPlayground } from "./playground";
import { createLogView } from "./logView";

const ALL_SCOPES: Scope[] = ["user", "local", "project"];

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
  // Playground-Aktionen nur für aktivierte Server (kein heimlicher Start eines
  // deaktivierten Servers).
  const canRun = server.enabled;

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
  // Nur für Server mit lokalem Befehl (stdio) und bekanntem Scope sinnvoll.
  if (!server.entry?.command || !server.scope) return null;
  const scope = server.scope;

  const content = h("div", { class: "caps-content" }, h("p", { class: "muted", text: "Wird geprüft…" }));

  void preflightServer(server.name, scope, server.project_path ?? undefined)
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
  // Nur für Server mit lokaler Definition (Scope bekannt) sinnvoll.
  if (!server.entry || !server.scope) return null;
  const scope = server.scope;

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
      showResult(await introspectServer(server.name, scope, server.project_path ?? undefined, refresh));
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
  void peekIntrospection(server.name, scope, server.project_path ?? undefined)
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
}

/// Öffnet ein Formular-Modal zum Duplizieren eines Servers (Original bleibt bestehen).
function openDuplicateModal(server: MergedServer, onDone: () => void): void {
  const currentScope = server.scope as Scope;

  const nameInput = h("input", { class: "inp" }) as HTMLInputElement;
  nameInput.value = `${server.name}-kopie`;

  const scopeSelect = h(
    "select",
    { class: "inp" },
    h("option", { value: "user" }, "user (global)"),
    h("option", { value: "local" }, "local (projekt-privat)"),
    h("option", { value: "project" }, "project (.mcp.json)"),
  ) as HTMLSelectElement;
  scopeSelect.value = currentScope;

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
  const scopeHint = h("div", { class: "field-hint", text: "" });

  const syncScopeUi = () => {
    const isProjectScoped = scopeSelect.value === "local" || scopeSelect.value === "project";
    projField.style.display = isProjectScoped ? "" : "none";
    scopeHint.textContent =
      scopeSelect.value === "project"
        ? "Der Server wird in .mcp.json des Zielprojekts angelegt und muss dort ggf. erst bestätigt werden."
        : "";
  };
  scopeSelect.addEventListener("change", syncScopeUi);
  syncScopeUi();

  const status = h("p", { class: "form-status" });
  const cancelBtn = h("button", { class: "btn" }, "Abbrechen") as HTMLButtonElement;
  const okBtn = h("button", { class: "btn btn-primary" }, "Duplizieren") as HTMLButtonElement;

  const form = h(
    "div",
    { class: "server-form" },
    field("Neuer Name", nameInput),
    field("Ziel-Scope", scopeSelect, undefined),
    scopeHint,
    projField,
    status,
  );

  const modal = openModal(`Duplizieren: ${server.name}`, form, [cancelBtn, okBtn]);
  cancelBtn.addEventListener("click", () => modal.close());
  nameInput.focus();
  nameInput.select();

  okBtn.addEventListener("click", async () => {
    const newName = nameInput.value.trim();
    const toScope = scopeSelect.value as Scope;
    const isProjectScoped = toScope === "local" || toScope === "project";
    const toProject = isProjectScoped ? projInput.value.trim() : undefined;

    status.className = "form-status";
    if (!newName) {
      status.className = "form-status error";
      status.textContent = "Bitte einen neuen Namen angeben.";
      return;
    }
    if (isProjectScoped && !toProject) {
      status.className = "form-status error";
      status.textContent = "Bitte ein Zielprojekt wählen oder einen Pfad angeben.";
      return;
    }

    okBtn.disabled = true;
    cancelBtn.disabled = true;
    status.textContent = "wird angelegt…";
    try {
      await cloneServer(
        server.name,
        currentScope,
        newName,
        toScope,
        server.project_path ?? undefined,
        toProject,
      );
      toast(`„${newName}" in ${toScope} angelegt`);
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
      const status = await healthCheck(server.name, server.project_path ?? undefined);
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
    if (!server.scope) return;
    if (!revealed) {
      try {
        revealBtn.disabled = true;
        revealedEntry = await revealServerEntry(server.scope, server.name, server.project_path ?? undefined);
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

  // Scope-Wechsel (nur für editierbare Server mit bekanntem Scope).
  let scopeSection: HTMLElement | null = null;
  if (server.editable && server.scope) {
    const currentScope = server.scope;
    const select = h(
      "select",
      { class: "inp" },
      ...ALL_SCOPES.filter((s) => s !== currentScope).map((s) => h("option", { value: s }, s)),
    ) as HTMLSelectElement;
    const moveBtn = h("button", { class: "btn btn-small" }, "Verschieben");
    moveBtn.addEventListener("click", () => {
      const target = select.value as Scope;
      openConfirm({
        title: `Scope ändern: ${server.name}`,
        message: `„${server.name}" von ${currentScope} nach ${target} verschieben? Zuerst im Ziel anlegen, dann aus der Quelle entfernen.`,
        confirmLabel: "Verschieben",
        onConfirm: async () => {
          await setScope(server.name, currentScope, target, server.project_path ?? undefined, undefined);
        },
        onDone: () => {
          toast(`Scope → ${target}`);
          modal.close();
          opts.onChanged?.();
        },
      });
    });
    const dupBtn = h("button", { class: "btn btn-small" }, "Duplizieren");
    dupBtn.addEventListener("click", () => {
      openDuplicateModal(server, () => {
        modal.close();
        opts.onChanged?.();
      });
    });
    scopeSection = h(
      "div",
      { class: "detail-scope" },
      h("h3", { text: "Scope ändern" }),
      h("div", { class: "scope-row" }, select, moveBtn),
      h("h3", { text: "Duplizieren" }),
      h(
        "div",
        { class: "scope-row" },
        h("span", { class: "muted", text: "Kopie in einen anderen Scope / ein anderes Projekt anlegen." }),
        dupBtn,
      ),
    );
  }

  const logs = logsSection(server, opts);
  const body = h(
    "div",
    { class: "detail" },
    metaWrap,
    server.project_path
      ? h("p", { class: "muted mono", text: `Projekt: ${server.project_path}` })
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
