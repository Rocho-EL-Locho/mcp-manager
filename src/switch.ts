import { h } from "./dom";

export interface SwitchOptions {
  /// Anfangszustand (an/aus).
  on: boolean;
  /// Deaktiviert (nicht klickbar, ausgegraut). Standard: false.
  disabled?: boolean;
  /// Tooltip. Standard: „aktiv"/„deaktiviert" je nach `on`.
  title?: string;
  /// Änderungs-Callback (entfällt bei `disabled`).
  onChange?: (v: boolean) => void;
}

/// Ein/Aus-Schalter im App-Stil (`.switch`/`.slider`). Einzige Quelle des
/// Switch-Markups – von serverList (interaktiv) und settings (deaktiviert) genutzt.
export function switchControl(opts: SwitchOptions): HTMLElement {
  const input = h("input", { type: "checkbox", class: "switch-input" }) as HTMLInputElement;
  input.checked = opts.on;
  input.disabled = opts.disabled ?? false;
  if (opts.onChange) {
    const cb = opts.onChange;
    input.addEventListener("change", () => cb(input.checked));
  }
  const title = opts.title ?? (opts.on ? "aktiv" : "deaktiviert");
  return h("label", { class: "switch", title }, input, h("span", { class: "slider" }));
}
