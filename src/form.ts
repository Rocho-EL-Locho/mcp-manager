// Generische Formular-Bausteine ohne Serverbezug: Label/Hint-Wrapper und der
// Schlüssel/Wert-Editor. Neben dom.ts / modal.ts / confirm.ts die gemeinsame
// UI-Grundlage – genutzt von Server-Formular, Einstellungen, Playground,
// Detail-Ansicht und Assistent.
import { h } from "./dom";
import { icon } from "./icons";

export interface KvEditor {
  el: HTMLElement;
  getValues: () => Record<string, string>;
}

/// Editor für Schlüssel/Wert-Paare (env, Header, Prompt-Argumente). `secretKeys`
/// markiert Schlüssel, die als Geheimnis geführt werden (sichtbarer Hinweis).
export function kvEditor(initial?: Record<string, string>, secretKeys?: string[]): KvEditor {
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

/// Ein beschriftetes Formularfeld mit optionalem Hinweistext unter dem Control.
export function field(label: string, control: HTMLElement, hint?: string): HTMLElement {
  return h(
    "div",
    { class: "field" },
    h("label", { class: "field-label", text: label }),
    control,
    hint ? h("div", { class: "field-hint", text: hint }) : null,
  );
}
