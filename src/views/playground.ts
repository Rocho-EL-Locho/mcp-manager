import { h, clear } from "../dom";
import { openModal } from "../modal";
import { openConfirm } from "../confirm";
import { field, kvEditor } from "./serverForm";
import { callTool, readResource, getPrompt } from "../ipc";
import type { MergedServer, McpTool, McpResource, McpPrompt, PlaygroundResult } from "../ipc";

// Session-Gedächtnis: zuletzt eingegebene Argumente pro Tool (rohes JSON),
// nicht persistent – geht beim App-Neustart verloren.
const lastToolArgs = new Map<string, string>();
const toolKey = (s: MergedServer, tool: string) => `${s.scope}:${s.name}:${s.project_path ?? ""}:${tool}`;

function asObj(v: unknown): Record<string, unknown> | undefined {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : undefined;
}

interface Collected {
  ok: boolean;
  value?: unknown;
  error?: string;
}

interface ArgsEditor {
  element: HTMLElement;
  collect: () => Collected;
}

interface PropControl {
  control: HTMLElement;
  /// Liest den Wert; `provided:false` = Feld leer (weglassen).
  read: () => { provided: boolean; value?: unknown; error?: string };
}

/// Ein Eingabefeld für eine einzelne Schema-Property.
function propControl(key: string, schema: unknown, required: boolean, initial: unknown): PropControl {
  const ps = asObj(schema) ?? {};
  const type = typeof ps.type === "string" ? ps.type : undefined;
  const desc = typeof ps.description === "string" ? ps.description : undefined;
  const label = required ? `${key} *` : key;
  const enumVals = Array.isArray(ps.enum) ? ps.enum : undefined;

  // enum -> Select
  if (enumVals) {
    const sel = h("select", { class: "inp" }) as HTMLSelectElement;
    if (!required) sel.append(h("option", { value: "" }, "—"));
    for (const v of enumVals) sel.append(h("option", { value: String(v) }, String(v)));
    if (initial !== undefined) sel.value = String(initial);
    return {
      control: field(label, sel, desc),
      read: () => (sel.value === "" ? { provided: false } : { provided: true, value: sel.value }),
    };
  }

  if (type === "boolean") {
    const box = h("input", { type: "checkbox", class: "select-box" }) as HTMLInputElement;
    if (initial === true) box.checked = true;
    return {
      control: field(label, box, desc),
      read: () => ({ provided: true, value: box.checked }),
    };
  }

  if (type === "number" || type === "integer") {
    const inp = h("input", { class: "inp", type: "number" }) as HTMLInputElement;
    if (typeof initial === "number") inp.value = String(initial);
    return {
      control: field(label, inp, desc),
      read: () => {
        const t = inp.value.trim();
        if (t === "") return { provided: false };
        const n = Number(t);
        if (!Number.isFinite(n)) return { provided: true, error: `${key}: keine gültige Zahl` };
        return { provided: true, value: n };
      },
    };
  }

  if (type === "string") {
    const inp = h("input", { class: "inp", type: "text" }) as HTMLInputElement;
    const fmt = typeof ps.format === "string" ? ps.format : undefined;
    if (fmt) inp.placeholder = fmt;
    if (typeof initial === "string") inp.value = initial;
    return {
      control: field(label, inp, desc),
      read: () => {
        const v = inp.value;
        return v === "" ? { provided: false } : { provided: true, value: v };
      },
    };
  }

  // object/array/anyOf/unbekannt -> JSON-Textarea für dieses Feld.
  const area = h("textarea", { class: "inp mono", rows: "3" }) as HTMLTextAreaElement;
  if (initial !== undefined) area.value = JSON.stringify(initial, null, 2);
  return {
    control: field(label, area, (desc ? desc + " — " : "") + "JSON"),
    read: () => {
      const t = area.value.trim();
      if (t === "") return { provided: false };
      try {
        return { provided: true, value: JSON.parse(t) };
      } catch (e) {
        return { provided: true, error: `${key}: ungültiges JSON (${String(e)})` };
      }
    },
  };
}

/// Baut den Argument-Editor für ein Tool aus dem `inputSchema`. Formular für
/// einfache Typen; „Als JSON bearbeiten" schaltet auf eine Textarea um, die dann
/// die Quelle der Wahrheit ist. Ohne nutzbares Schema startet direkt der JSON-Modus.
function buildArgsEditor(schema: unknown, initialRaw?: string): ArgsEditor {
  const obj = asObj(schema);
  const props = obj && obj.type === "object" ? asObj(obj.properties) : undefined;
  const requiredArr = Array.isArray(obj?.required) ? (obj!.required as unknown[]) : [];
  const required = new Set(requiredArr.filter((x): x is string => typeof x === "string"));

  // Gemerkte letzte Eingaben (falls vorhanden) parsen, um sowohl Formularfelder
  // als auch die JSON-Textarea vorzubefüllen.
  let initialObj: Record<string, unknown> | undefined;
  if (initialRaw) {
    try {
      initialObj = asObj(JSON.parse(initialRaw));
    } catch {
      /* ungültiges Gedächtnis ignorieren */
    }
  }

  const controls: { key: string; read: PropControl["read"] }[] = [];
  const formWrap = h("div", { class: "pg-form" });
  const hasForm = !!props && Object.keys(props).length > 0;
  if (props) {
    for (const [key, ps] of Object.entries(props)) {
      const c = propControl(key, ps, required.has(key), initialObj?.[key]);
      controls.push({ key, read: c.read });
      formWrap.append(c.control);
    }
  }

  const rawArea = h("textarea", { class: "inp mono pg-json", rows: "8" }) as HTMLTextAreaElement;
  const rawWrap = field("Argumente (JSON)", rawArea, "JSON-Objekt; im JSON-Modus maßgeblich.");
  rawArea.value = initialRaw ?? "{}";

  // Formular aus den Feldern einsammeln (ohne Fehler bei Weglassungen).
  const collectForm = (): Collected => {
    const out: Record<string, unknown> = {};
    for (const c of controls) {
      const r = c.read();
      if (r.error) return { ok: false, error: r.error };
      if (r.provided) out[c.key] = r.value;
    }
    for (const req of required) {
      if (!(req in out)) return { ok: false, error: `Pflichtfeld „${req}" fehlt` };
    }
    return { ok: true, value: out };
  };
  const collectRaw = (): Collected => {
    const t = rawArea.value.trim();
    if (t === "") return { ok: true, value: {} };
    try {
      return { ok: true, value: JSON.parse(t) };
    } catch (e) {
      return { ok: false, error: "Ungültiges JSON: " + String(e) };
    }
  };

  let rawMode = !hasForm;
  const element = h("div", { class: "pg-args" });
  const toggle = h(
    "button",
    { class: "btn btn-small", type: "button" },
    hasForm ? "Als JSON bearbeiten" : "Als Formular bearbeiten",
  ) as HTMLButtonElement;
  // Ohne Formular gibt es nichts umzuschalten.
  if (!hasForm) toggle.style.display = "none";

  const render = () => {
    clear(element);
    if (rawMode) {
      element.append(rawWrap);
    } else {
      element.append(formWrap);
    }
    if (hasForm) element.append(toggle);
  };
  toggle.addEventListener("click", () => {
    if (!rawMode) {
      // Formular -> JSON: aktuelle Formularwerte übernehmen (falls gültig).
      const c = collectForm();
      if (c.ok) rawArea.value = JSON.stringify(c.value, null, 2);
      rawMode = true;
      toggle.textContent = "Als Formular bearbeiten";
    } else {
      rawMode = false;
      toggle.textContent = "Als JSON bearbeiten";
    }
    render();
  });
  render();

  return {
    element,
    collect: () => (rawMode ? collectRaw() : collectForm()),
  };
}

// ---------------------------------------------------------------------------
// Ergebnisanzeige
// ---------------------------------------------------------------------------

/// Extrahiert Textblöcke aus einem MCP-`content`/`contents`/`messages`-Array.
function extractTexts(result: unknown): string[] {
  const out: string[] = [];
  const push = (arr: unknown) => {
    if (!Array.isArray(arr)) return;
    for (const item of arr) {
      const o = asObj(item);
      if (!o) continue;
      if (typeof o.text === "string") out.push(o.text);
      const inner = asObj(o.content);
      if (inner && typeof inner.text === "string") out.push(inner.text); // prompts/get messages
    }
  };
  const r = asObj(result);
  if (r) {
    push(r.content);
    push(r.contents);
    push(r.messages);
  }
  return out;
}

function renderResult(box: HTMLElement, res: PlaygroundResult): void {
  clear(box);
  const meta = h("div", { class: "pg-result-meta" });
  if (!res.ok) {
    meta.append(h("span", { class: "badge badge-error", text: "Fehler" }));
  } else if (res.isError) {
    meta.append(h("span", { class: "badge badge-warn", text: "Tool meldete einen Fehler" }));
  } else {
    meta.append(h("span", { class: "badge badge-ok", text: "OK" }));
  }
  if (res.durationMs !== undefined) {
    meta.append(h("span", { class: "muted", text: `${res.durationMs} ms` }));
  }
  box.append(meta);

  if (!res.ok && res.error) {
    box.append(h("div", { class: "banner banner-error", text: res.error }));
  }

  // Freundliche Textblöcke (falls vorhanden).
  const texts = extractTexts(res.result);
  for (const t of texts) {
    box.append(h("pre", { class: "mono pg-text", text: t }));
  }

  // Roh-JSON immer aufklappbar anbieten (Quelle der Wahrheit).
  if (res.result !== undefined && res.result !== null) {
    const raw = h(
      "details",
      { class: "pg-raw" },
      h("summary", { text: "Rohantwort (JSON)" }),
      h("pre", { class: "mono", text: JSON.stringify(res.result, null, 2) }),
    ) as HTMLDetailsElement;
    if (texts.length === 0) raw.open = true;
    box.append(raw);
  }

  for (const n of res.notes) {
    box.append(h("p", { class: "muted caps-note", text: n }));
  }
  if (res.logs) {
    box.append(
      h(
        "details",
        { class: "caps-logs" },
        h("summary", { text: "Server-Log (stderr)" }),
        h("pre", { class: "mono caps-log", text: res.logs }),
      ),
    );
  }
}

/// Führt `action` nach einem Sicherheits-Confirm aus (Sekundenzähler via
/// Inline-Status), rendert danach das Ergebnis in `box`.
function runWithConfirm(
  message: string,
  box: HTMLElement,
  action: () => Promise<PlaygroundResult>,
): void {
  let res: PlaygroundResult | null = null;
  openConfirm({
    title: "Wirklich ausführen?",
    message,
    confirmLabel: "Ausführen",
    danger: true,
    onConfirm: async (setStatus) => {
      const start = Date.now();
      const timer = window.setInterval(
        () => setStatus(`läuft… ${Math.round((Date.now() - start) / 1000)} s`),
        1000,
      );
      try {
        res = await action();
      } finally {
        window.clearInterval(timer);
      }
    },
    onDone: () => {
      if (res) renderResult(box, res);
    },
  });
}

const CALL_WARNING =
  "Der Aufruf wird mit echten Zugangsdaten gegen den echten Server ausgeführt und kann echte Nebenwirkungen haben.";

// ---------------------------------------------------------------------------
// Öffentliche Einstiegspunkte
// ---------------------------------------------------------------------------

export function openToolPlayground(server: MergedServer, tool: McpTool): void {
  const scope = server.scope;
  if (!scope) return;
  const key = toolKey(server, tool.name);
  const editor = buildArgsEditor(tool.inputSchema, lastToolArgs.get(key));
  const status = h("div", { class: "form-status" });
  const resultBox = h("div", { class: "pg-result" });

  const runBtn = h("button", { class: "btn btn-primary", type: "button" }, "Testen…") as HTMLButtonElement;
  runBtn.addEventListener("click", () => {
    const args = editor.collect();
    if (!args.ok) {
      status.className = "form-status error";
      status.textContent = args.error ?? "Ungültige Eingabe";
      return;
    }
    status.className = "form-status";
    status.textContent = "";
    lastToolArgs.set(key, JSON.stringify(args.value, null, 2));
    runWithConfirm(CALL_WARNING, resultBox, () =>
      callTool(server.name, scope, server.project_path ?? undefined, tool.name, args.value),
    );
  });

  const body = h(
    "div",
    { class: "pg" },
    tool.description ? h("p", { class: "muted", text: tool.description }) : null,
    editor.element,
    h("div", { class: "pg-actions" }, runBtn),
    status,
    resultBox,
  );
  openModal(`Tool testen: ${tool.name}`, body);
}

export function openResourcePlayground(server: MergedServer, resource: McpResource): void {
  const scope = server.scope;
  if (!scope) return;
  const uriInput = h("input", { class: "inp mono", type: "text" }) as HTMLInputElement;
  uriInput.value = resource.uri;
  const isTemplate = resource.uri.includes("{");
  const resultBox = h("div", { class: "pg-result" });
  const status = h("div", { class: "form-status" });

  const runBtn = h("button", { class: "btn btn-primary", type: "button" }, "Lesen…") as HTMLButtonElement;
  runBtn.addEventListener("click", () => {
    const uri = uriInput.value.trim();
    if (!uri || uri.includes("{")) {
      status.className = "form-status error";
      status.textContent = "Bitte eine konkrete URI angeben (Platzhalter {…} ersetzen).";
      return;
    }
    status.className = "form-status";
    status.textContent = "";
    runWithConfirm(CALL_WARNING, resultBox, () =>
      readResource(server.name, scope, server.project_path ?? undefined, uri),
    );
  });

  const body = h(
    "div",
    { class: "pg" },
    resource.description ? h("p", { class: "muted", text: resource.description }) : null,
    field("URI", uriInput, isTemplate ? "Template – Platzhalter {…} durch echte Werte ersetzen." : undefined),
    h("div", { class: "pg-actions" }, runBtn),
    status,
    resultBox,
  );
  openModal(`Resource lesen: ${resource.name ?? resource.uri}`, body);
}

export function openPromptPlayground(server: MergedServer, prompt: McpPrompt): void {
  const scope = server.scope;
  if (!scope) return;
  const kv = kvEditor();
  const resultBox = h("div", { class: "pg-result" });

  const runBtn = h("button", { class: "btn btn-primary", type: "button" }, "Abrufen…") as HTMLButtonElement;
  runBtn.addEventListener("click", () => {
    runWithConfirm(CALL_WARNING, resultBox, () =>
      getPrompt(server.name, scope, server.project_path ?? undefined, prompt.name, kv.getValues()),
    );
  });

  const body = h(
    "div",
    { class: "pg" },
    prompt.description ? h("p", { class: "muted", text: prompt.description }) : null,
    field("Argumente", kv.el, "Optionale Schlüssel/Wert-Paare."),
    h("div", { class: "pg-actions" }, runBtn),
    resultBox,
  );
  openModal(`Prompt abrufen: ${prompt.name}`, body);
}
