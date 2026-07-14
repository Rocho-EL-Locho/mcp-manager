import { h } from "../dom";
import { icon } from "../icons";
import type { Scope } from "../ipc";
import { openModal } from "../modal";
import type { ServerPreset } from "../presets";
import { PRESETS, presetEntry, presetTransport } from "../presets";
import { openAssistant } from "./assistant";
import { openServerForm } from "./serverForm";

export interface PickerContext {
  projectPath?: string;
  defaultScope?: Scope;
}

/// Auswahl-Schritt vor dem Server-Formular: bündelt Vorlagen, leeres Formular
/// und den Link-Assistenten an einem Ort.
export function openServerPicker(onSaved: () => void, ctx: PickerContext = {}): void {
  const modal = openModal("Server hinzufügen – Vorlage wählen", h("div"));

  const card = (opts: {
    title: string;
    desc: string;
    badge?: HTMLElement;
    onClick: () => void;
  }): HTMLElement => {
    const btn = h(
      "button",
      { class: "picker-card", type: "button", onclick: opts.onClick },
      h(
        "div",
        { class: "picker-card-head" },
        h("span", { class: "picker-card-title", text: opts.title }),
        opts.badge ?? null,
      ),
      h("div", { class: "picker-card-desc", text: opts.desc }),
    );
    return btn;
  };

  const openPreset = (preset: ServerPreset) => {
    modal.close();
    void openServerForm({
      mode: "add",
      preset,
      prefill: { name: preset.id, entry: presetEntry(preset) },
      projectPath: ctx.projectPath,
      defaultScope: ctx.defaultScope,
      onSaved,
    });
  };

  const openEmpty = () => {
    modal.close();
    void openServerForm({
      mode: "add",
      projectPath: ctx.projectPath,
      defaultScope: ctx.defaultScope,
      onSaved,
    });
  };

  const openLink = () => {
    modal.close();
    openAssistant(onSaved, ctx);
  };

  // Erste Reihe: die zwei „freien" Einstiege prominent.
  const special = h(
    "div",
    { class: "picker-grid" },
    card({
      title: "Leeres Formular",
      desc: "Alle Felder selbst ausfüllen – für Server, die keine Vorlage haben.",
      badge: icon("plus"),
      onClick: openEmpty,
    }),
    card({
      title: "Per Link einrichten",
      desc: "Claude liest README/Doku einer URL und schlägt eine Konfiguration vor.",
      badge: icon("sparkles"),
      onClick: openLink,
    }),
  );

  const presetCards = PRESETS.map((p) =>
    card({
      title: p.label,
      desc: p.description,
      badge: h("span", { class: "badge badge-scope", text: presetTransport(p) }),
      onClick: () => openPreset(p),
    }),
  );
  const presetGrid = h("div", { class: "picker-grid" }, ...presetCards);

  const body = h(
    "div",
    { class: "server-picker" },
    special,
    h("div", { class: "picker-section-label", text: "Vorlagen" }),
    presetGrid,
  );

  modal.setBody(body);
}
