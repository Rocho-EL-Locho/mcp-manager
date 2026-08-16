// Auswahl-Zustandsmaschine der Serverliste: welche Karten sind ausgewählt, wie
// spiegeln die Gruppen-Checkboxen das (Tri-State), und wie überlebt die Auswahl
// einen Filterwechsel. Bewusst getrennt von der Darstellung in `serverList.ts` –
// die Liste platziert nur die hier gebauten Checkboxen und fragt die Auswahl ab.
import { h } from "../dom";
import type { MergedServer } from "../ipc";

/// Eindeutiger Auswahl-Schlüssel: Scope (bzw. origin bei externen) + Name + Projekt,
/// weil derselbe Name in mehreren Scopes/Projekten vorkommen kann (collision).
export function selectionKey(s: MergedServer): string {
  return `${s.scope ?? s.origin}::${s.name}::${s.project_path ?? ""}`;
}

/// Nur Server mit bekanntem Scope sind auswählbar; externe (Connector/Plugin) nicht.
export function isSelectable(s: MergedServer): boolean {
  return s.scope !== null;
}

export interface SelectionRegistry {
  /// Beginn eines Body-Neuaufbaus: Karten-/Gruppenregister leeren und merken,
  /// welche Server der aktuelle Filter zeigt.
  beginBody: (shown: MergedServer[]) => void;
  /// Checkbox einer Server-Karte (Zustand und Handler bereits verdrahtet). Der
  /// Eintrag ist damit registriert – die Checkbox synchronisiert von hier an,
  /// auch bevor die Karte selbst existiert.
  cardBox: (key: string) => HTMLInputElement;
  /// Die fertige Karte nachtragen – sie entsteht erst nach ihrer Checkbox. Nur
  /// die Karte: die Checkbox kennt das Register bereits aus `cardBox`.
  attachCard: (key: string, card: HTMLElement) => void;
  /// Tri-State-Checkbox „alle in dieser Gruppe auswählen".
  groupBox: (keys: string[]) => HTMLInputElement;
  /// Zahl der ausgewählten Server.
  size: () => number;
  /// Ausgewählte Server – auch die, die der aktuelle Filter ausblendet.
  selectedServers: () => MergedServer[];
  /// Zahl der ausgewählten Server, die der aktuelle Filter ausblendet.
  hiddenCount: () => number;
  /// Auswahl vollständig aufheben (inklusive Karten- und Gruppen-Sync).
  clearSelection: () => void;
}

/// Baut das Auswahl-Register über `selection`. Das Set wird direkt mutiert und
/// gehört dem Aufrufer (main.ts), damit die Auswahl `refresh()` überlebt.
/// `onChange` läuft nach jeder Änderung – die Liste rendert damit ihre Bulk-Leiste
/// neu.
export function createSelectionRegistry(
  servers: MergedServer[],
  selection: Set<string>,
  onChange: () => void,
): SelectionRegistry {
  // Einmalige Key→Server-Map + gecachte Keys der aktuell sichtbaren (gefilterten)
  // Server: hält Auswahl-Toggles bei O(Auswahl) statt jedes Mal O(alle Server).
  const serverByKey = new Map(servers.map((s) => [selectionKey(s), s]));
  let shownKeys = new Set<string>();

  // Register der Karten und Gruppen-Boxen des aktuell gerenderten Bodys – erlaubt
  // gezielte Auswahl-Updates ohne kompletten Neuaufbau (nur ein Filterwechsel
  // baut den Body neu).
  // `card` ist zwischen `cardBox` und `attachCard` kurz `null` – die Checkbox
  // entsteht vor der Karte, die sie enthält.
  const cards = new Map<string, { card: HTMLElement | null; box: HTMLInputElement }>();
  let groups: Array<{ box: HTMLInputElement; keys: string[] }> = [];

  const syncGroup = (g: { box: HTMLInputElement; keys: string[] }): void => {
    const all = g.keys.every((k) => selection.has(k));
    const some = g.keys.some((k) => selection.has(k));
    g.box.checked = all;
    g.box.indeterminate = !all && some;
  };

  const setCardSelected = (key: string, selected: boolean): void => {
    const entry = cards.get(key);
    if (!entry) return;
    entry.box.checked = selected;
    entry.card?.classList.toggle("selected", selected);
  };

  return {
    beginBody(shown) {
      cards.clear();
      groups = [];
      shownKeys = new Set(shown.map(selectionKey));
    },

    cardBox(key) {
      const box = h("input", {
        type: "checkbox",
        class: "select-box",
        title: "Für Bulk-Aktion auswählen",
      }) as HTMLInputElement;
      box.checked = selection.has(key);
      box.addEventListener("change", () => {
        const checked = box.checked;
        if (checked) selection.add(key);
        else selection.delete(key);
        cards.get(key)?.card?.classList.toggle("selected", checked);
        for (const g of groups) if (g.keys.includes(key)) syncGroup(g);
        onChange();
      });
      // Sofort registrieren: so kann der Aufrufer die Karte später allein
      // nachtragen und muss die Checkbox nicht zurückreichen.
      cards.set(key, { card: null, box });
      return box;
    },

    attachCard(key, card) {
      const entry = cards.get(key);
      if (entry) entry.card = card;
    },

    groupBox(keys) {
      const box = h("input", {
        type: "checkbox",
        class: "select-box",
        title: "Alle in dieser Gruppe auswählen",
      }) as HTMLInputElement;
      const g = { box, keys };
      groups.push(g);
      syncGroup(g);
      box.addEventListener("change", () => {
        const checked = box.checked;
        for (const key of keys) {
          if (checked) selection.add(key);
          else selection.delete(key);
          setCardSelected(key, checked);
        }
        box.indeterminate = false;
        onChange();
      });
      return box;
    },

    size: () => selection.size,

    selectedServers: () =>
      [...selection]
        .map((k) => serverByKey.get(k))
        .filter((s): s is MergedServer => s !== undefined),

    hiddenCount: () =>
      [...selection].filter((k) => serverByKey.has(k) && !shownKeys.has(k)).length,

    clearSelection() {
      for (const key of selection) setCardSelected(key, false);
      selection.clear();
      for (const g of groups) syncGroup(g);
      onChange();
    },
  };
}
