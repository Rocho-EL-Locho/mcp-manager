// Gemeinsames Scope-Vokabular des Frontends: Reihenfolge, Klartext-Beschriftung
// und der daraus gebaute Select. Bewusst ein eigenes kleines Modul (analog
// `transport.ts`) statt in `form.ts` – dort liegen die Bausteine OHNE
// Serverbezug.
//
// Warum zentral: die drei Beschriftungen standen zuvor in vier Ansichten
// (Formular, Detail, Duplizieren, Konfliktdialog) ausgeschrieben – teils sogar
// als nackte Scope-Namen. Eine Änderung an einer Stelle fiel nirgends auf.
import { h } from "./dom";
import type { Scope } from "./ipc";

/// Alle Scopes in der Reihenfolge, in der sie dem Nutzer angeboten werden.
export const ALL_SCOPES: readonly Scope[] = ["user", "local", "project"];

/// Klartext-Beschriftung je Scope – die einzige Quelle für Auswahl-, Dialog-
/// und Tabellentexte.
export const SCOPE_LABEL: Record<Scope, string> = {
  user: "user (global)",
  local: "local (projekt-privat)",
  project: "project (.mcp.json)",
};

/// Select über die Scopes mit Klartext-Beschriftung. `selected` wird vorbelegt
/// (nur sinnvoll, wenn der Wert auch angeboten wird), `choices` schränkt die
/// Auswahl ein – z. B. beim Verschieben auf „alle außer dem aktuellen Scope".
export function scopeSelect(selected?: Scope, choices: readonly Scope[] = ALL_SCOPES): HTMLSelectElement {
  const el = h(
    "select",
    { class: "inp" },
    ...choices.map((s) => h("option", { value: s }, SCOPE_LABEL[s])),
  ) as HTMLSelectElement;
  if (selected) el.value = selected;
  return el;
}
