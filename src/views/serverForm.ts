import { h } from "../dom";
import { icon } from "../icons";
import type { MergedServer, Scope, ServerEntry } from "../ipc";
import { addServer, updateServer, revealServerEntry } from "../ipc";
import { openModal } from "../modal";
import type { ServerPreset } from "../presets";
import { toast } from "../toast";

export interface ServerFormOptions {
  mode: "add" | "edit";
  server?: MergedServer;
  prefill?: { name?: string; entry?: ServerEntry };
  /// Gewähltes Preset (Add-Modus): liefert Prefill, docsUrl und Secret-Führung.
  preset?: ServerPreset;
  /// Zielprojekt für local/project-Scope (Add-Modus).
  projectPath?: string;
  /// Vorbelegter Scope im Add-Modus.
  defaultScope?: Scope;
  onSaved: () => void;
}

interface KvEditor {
  el: HTMLElement;
  getValues: () => Record<string, string>;
}

function kvEditor(initial?: Record<string, string>, secretKeys?: string[]): KvEditor {
  const rows = h("div", { class: "kv-editor" });
  const secrets = new Set(secretKeys ?? []);

  const addRow = (k = "", v = "") => {
    const kIn = h("input", { class: "inp mono", placeholder: "KEY" }) as HTMLInputElement;
    const vIn = h("input", { class: "inp mono", placeholder: "Wert" }) as HTMLInputElement;
    kIn.value = k;
    vIn.value = v;
    const rm = h("button", { class: "btn btn-icon", type: "button", title: "Zeile entfernen" }, icon("x"));
    const inputs = h("div", { class: "kv-editrow" }, kIn, vIn, rm);
    // Vom Preset als Secret markierte Keys sichtbar als "erforderlich" führen.
    const hint = secrets.has(k)
      ? h("div", { class: "kv-secret-hint", text: "erforderlich – wird maskiert gespeichert" })
      : null;
    const row = hint ? h("div", { class: "kv-row" }, inputs, hint) : inputs;
    rm.addEventListener("click", () => row.remove());
    rows.append(row);
  };

  for (const [k, v] of Object.entries(initial ?? {})) addRow(k, v);

  const addBtn = h(
    "button",
    { class: "btn btn-small", type: "button", onclick: () => addRow() },
    "+ Zeile",
  );

  const el = h("div", {}, rows, addBtn);
  const getValues = (): Record<string, string> => {
    const out: Record<string, string> = {};
    rows.querySelectorAll(".kv-editrow").forEach((r) => {
      const inputs = r.querySelectorAll("input");
      const key = (inputs[0] as HTMLInputElement).value.trim();
      if (key) out[key] = (inputs[1] as HTMLInputElement).value;
    });
    return out;
  };
  return { el, getValues };
}

export function field(label: string, control: HTMLElement, hint?: string): HTMLElement {
  return h(
    "div",
    { class: "field" },
    h("label", { class: "field-label", text: label }),
    control,
    hint ? h("div", { class: "field-hint", text: hint }) : null,
  );
}

export async function openServerForm(opts: ServerFormOptions): Promise<void> {
  const isEdit = opts.mode === "edit";
  let initEntry: ServerEntry = {};
  let name = "";
  let scope: Scope = "user";

  if (isEdit && opts.server) {
    name = opts.server.name;
    scope = opts.server.scope ?? "user";
    if (opts.server.scope) {
      // Klartext-Konfiguration laden. opts.server.entry ist ggf. maskiert (••••••••)
      // und darf NICHT als Fallback ins Formular, sonst überschreibt ein Speichern
      // echte Secrets mit den Platzhaltern.
      let revealed: ServerEntry | null = null;
      try {
        revealed = await revealServerEntry(
          opts.server.scope,
          name,
          opts.server.project_path ?? undefined,
        );
      } catch {
        revealed = null;
      }
      if (revealed) {
        initEntry = revealed;
      } else if (opts.server.has_secrets) {
        // Reveal fehlgeschlagen und der Server hält Secrets: nicht mit maskierten
        // Werten öffnen, um Secret-Zerstörung beim Speichern zu vermeiden.
        const info = h("div", { class: "form-status error" },
          "Konnte Klartext-Konfiguration nicht laden – Bearbeiten abgebrochen, um Secrets nicht zu überschreiben.");
        const okBtn = h("button", { class: "btn btn-primary" }, "OK") as HTMLButtonElement;
        const errModal = openModal(`Server bearbeiten: ${name}`, info, [okBtn]);
        okBtn.addEventListener("click", () => errModal.close());
        return;
      } else {
        // Keine Secrets: der (unmaskierte) entry ist gefahrlos verwendbar.
        initEntry = opts.server.entry ?? {};
      }
    }
  } else {
    if (opts.preset) {
      name = opts.preset.id;
      initEntry = opts.prefill?.entry ?? opts.preset.entry;
    }
    if (opts.prefill) {
      name = opts.prefill.name ?? name;
      initEntry = opts.prefill.entry ?? initEntry;
    }
    if (opts.defaultScope) scope = opts.defaultScope;
  }

  const secretKeys = opts.preset?.secretKeys ?? [];

  // Ursprüngliches type merken, um bei stdio keinen "type"-Key neu hinzuzufügen.
  const hadType = initEntry.type != null;
  // type normalisieren: nur stdio/http/sse sind gültige Optionen, sonst aus url ableiten.
  const initTransport = ["stdio", "http", "sse"].includes(initEntry.type ?? "")
    ? (initEntry.type as string)
    : (initEntry.url ? "http" : "stdio");

  // Felder
  const nameInput = h("input", { class: "inp" }) as HTMLInputElement;
  nameInput.value = name;
  if (isEdit) nameInput.disabled = true;

  const scopeSelect = h(
    "select",
    { class: "inp" },
    h("option", { value: "user" }, "user (global)"),
    h("option", { value: "local" }, "local (projekt-privat)"),
    h("option", { value: "project" }, "project (.mcp.json)"),
  ) as HTMLSelectElement;
  scopeSelect.value = scope;
  if (isEdit) scopeSelect.disabled = true;

  const transportSelect = h(
    "select",
    { class: "inp" },
    h("option", { value: "stdio" }, "stdio (lokaler Prozess)"),
    h("option", { value: "http" }, "http"),
    h("option", { value: "sse" }, "sse"),
  ) as HTMLSelectElement;
  transportSelect.value = initTransport;

  // stdio-Felder
  const commandInput = h("input", { class: "inp mono", placeholder: "z. B. npx / docker / uvx" }) as HTMLInputElement;
  commandInput.value = initEntry.command ?? "";
  const argsArea = h("textarea", { class: "inp mono", rows: "4", placeholder: "ein Argument pro Zeile" }) as HTMLTextAreaElement;
  argsArea.value = (initEntry.args ?? []).join("\n");
  const envEd = kvEditor(initEntry.env, secretKeys);
  const stdioSection = h(
    "div",
    {},
    field("Command", commandInput),
    field("Args", argsArea, "Ein Argument pro Zeile (bewahrt Leerzeichen/Quoting)."),
    field("Env", envEd.el, "Werte werden im Klartext an claude übergeben."),
  );

  // http/sse-Felder
  const urlInput = h("input", { class: "inp mono", placeholder: "https://…" }) as HTMLInputElement;
  urlInput.value = initEntry.url ?? "";
  const headersEd = kvEditor(initEntry.headers, secretKeys);
  const remoteSection = h("div", {}, field("URL", urlInput), field("Headers", headersEd.el));

  const applyTransport = () => {
    const t = transportSelect.value;
    stdioSection.style.display = t === "stdio" ? "" : "none";
    remoteSection.style.display = t === "stdio" ? "none" : "";
  };
  transportSelect.addEventListener("change", applyTransport);
  applyTransport();

  const status = h("div", { class: "form-status" });

  // Preset-Kopf: Beschreibung + prominenter Link auf die offizielle Doku
  // (Presets veralten; die Doku ist die maßgebliche Quelle).
  const presetHead = opts.preset
    ? h(
        "div",
        { class: "preset-head" },
        h("div", { class: "preset-head-desc", text: opts.preset.description }),
        h(
          "a",
          { class: "preset-head-docs", href: opts.preset.docsUrl, target: "_blank", rel: "noreferrer noopener" },
          icon("globe"),
          "Offizielle Doku",
        ),
      )
    : null;

  const body = h(
    "div",
    { class: "server-form" },
    presetHead,
    field("Name", nameInput, isEdit ? "Name unveränderlich (zum Umbenennen: entfernen + neu anlegen)." : undefined),
    field(
      "Scope",
      scopeSelect,
      isEdit
        ? "Scope-Wechsel folgt separat."
        : opts.projectPath
          ? `local/project zielen auf: ${opts.projectPath}`
          : "local/project zielen auf das Standard-Projekt (Home).",
    ),
    field("Transport", transportSelect),
    stdioSection,
    remoteSection,
    status,
  );

  const cancelBtn = h("button", { class: "btn" }, "Abbrechen") as HTMLButtonElement;
  const saveBtn = h("button", { class: "btn btn-primary" }, isEdit ? "Speichern" : "Hinzufügen") as HTMLButtonElement;
  const modal = openModal(isEdit ? `Server bearbeiten: ${name}` : "Server hinzufügen", body, [cancelBtn, saveBtn]);
  cancelBtn.addEventListener("click", () => modal.close());

  const buildEntry = (): ServerEntry => {
    const t = transportSelect.value;
    const entry: ServerEntry = {};
    if (t === "stdio") {
      // Für stdio nur dann type setzen, wenn der Ursprungseintrag bereits einen hatte,
      // um keinen überflüssigen "type":"stdio"-Key hinzuzufügen.
      if (hadType) entry.type = t;
      entry.command = commandInput.value.trim();
      const args = argsArea.value.split("\n").map((s) => s.trim()).filter(Boolean);
      if (args.length) entry.args = args;
      const env = envEd.getValues();
      if (Object.keys(env).length) entry.env = env;
    } else {
      // Für http/sse type immer setzen (unterscheidet die Transports).
      entry.type = t;
      entry.url = urlInput.value.trim();
      const headers = headersEd.getValues();
      if (Object.keys(headers).length) entry.headers = headers;
    }
    return entry;
  };

  const validate = (entry: ServerEntry): string | null => {
    if (!isEdit && !nameInput.value.trim()) return "Name darf nicht leer sein.";
    // Transport aus dem Select, nicht aus entry.type (kann bei stdio bewusst fehlen).
    const t = transportSelect.value;
    if (t === "stdio" && !entry.command) return "Command darf nicht leer sein.";
    if (t !== "stdio" && !entry.url) return "URL darf nicht leer sein.";
    // Unersetzte Preset-Platzhalter (<PFAD>, <CONNECTION_STRING>, …) blockieren
    // das Speichern – sonst würde ein unbrauchbarer Server angelegt.
    const placeholders = [...(entry.args ?? []), entry.url ?? ""]
      .flatMap((s) => s.match(/<[^>]+>/g) ?? []);
    if (placeholders.length) {
      const uniq = [...new Set(placeholders)];
      return `Platzhalter noch ersetzen: ${uniq.join(", ")}`;
    }
    return null;
  };

  /// Vom Preset als erforderlich markierte Secret-Keys, die leer geblieben sind.
  const emptySecretKeys = (entry: ServerEntry): string[] => {
    if (!secretKeys.length) return [];
    const values = { ...(entry.env ?? {}), ...(entry.headers ?? {}) };
    return secretKeys.filter((k) => !(values[k] ?? "").trim());
  };

  saveBtn.addEventListener("click", async () => {
    const entry = buildEntry();
    const err = validate(entry);
    if (err) {
      status.className = "form-status error";
      status.textContent = err;
      return;
    }
    const finalName = isEdit ? name : nameInput.value.trim();
    const finalScope = (isEdit ? scope : (scopeSelect.value as Scope));
    saveBtn.disabled = true;
    cancelBtn.disabled = true;
    status.className = "form-status";
    status.textContent = "wird gespeichert…";
    try {
      if (isEdit) await updateServer(finalName, finalScope, entry, opts.server?.project_path ?? undefined);
      else await addServer(finalName, finalScope, entry, opts.projectPath);
      toast(isEdit ? "Server gespeichert" : "Server hinzugefügt");
      // Nicht blockierend: leere Secret-Keys als Hinweis nachreichen.
      const empty = emptySecretKeys(entry);
      if (empty.length) toast(`Hinweis: leer gelassen – ${empty.join(", ")}`);
      modal.close();
      opts.onSaved();
    } catch (e) {
      status.className = "form-status error";
      status.textContent = "Fehler: " + String(e);
      saveBtn.disabled = false;
      cancelBtn.disabled = false;
    }
  });
}
