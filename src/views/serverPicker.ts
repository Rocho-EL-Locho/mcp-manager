import { h } from "../dom";
import { icon } from "../icons";
import type { ClientInfo, Scope } from "../ipc";
import { openModal } from "../modal";
import type { ServerPreset } from "../presets";
import { PRESETS, presetEntry, presetTransport } from "../presets";
import { openAssistant } from "./assistant";
import { openRegistryBrowser } from "./registry";
import { openServerForm } from "./serverForm";

export interface PickerContext {
  projectPath?: string;
  defaultScope?: Scope;
  /// Ziel-Client (Feature 16): Vorlagen und leeres Formular speichern dann in
  /// dessen Konfigurationsdatei. Katalog und Link-Assistent bleiben bewusst
  /// scope-basiert (Claude Code) – von dort führt „Kopieren nach…" weiter.
  client?: ClientInfo;
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
    /// Gesetzt => Karte deaktiviert, der Text steht als Begründung darunter und
    /// im Tooltip. Besser hier als ein Formular, das erst beim Speichern an
    /// einer Validierung scheitert, die die Ursache nicht mehr benennt.
    blockedReason?: string;
  }): HTMLElement => {
    const blocked = opts.blockedReason !== undefined;
    const btn = h(
      "button",
      {
        class: "picker-card",
        type: "button",
        disabled: blocked,
        title: opts.blockedReason,
        onclick: blocked ? undefined : opts.onClick,
      },
      h(
        "div",
        { class: "picker-card-head" },
        h("span", { class: "picker-card-title", text: opts.title }),
        opts.badge ?? null,
      ),
      h("div", { class: "picker-card-desc", text: opts.desc }),
      blocked ? h("div", { class: "picker-card-blocked", text: opts.blockedReason }) : null,
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
      client: ctx.client,
      onSaved,
    });
  };

  const openEmpty = () => {
    modal.close();
    void openServerForm({
      mode: "add",
      projectPath: ctx.projectPath,
      defaultScope: ctx.defaultScope,
      client: ctx.client,
      onSaved,
    });
  };

  // Katalog und Link-Assistent bleiben bewusst scope-basiert (siehe
  // `PickerContext.client`): sie bekommen KEINE Client-Ziel-Auswahl. Das Feld
  // `client` wird deshalb ausdrücklich NICHT durchgereicht – strukturelle
  // Typisierung würde es sonst stillschweigend schlucken und aus der
  // Client-Ansicht angelegte Server landeten unbemerkt in Claude Code.
  const scopeCtx = { projectPath: ctx.projectPath, defaultScope: ctx.defaultScope };

  const openLink = () => {
    modal.close();
    openAssistant(onSaved, scopeCtx);
  };

  const openCatalog = () => {
    modal.close();
    openRegistryBrowser(onSaved, scopeCtx);
  };

  // In der Client-Ansicht muss an den beiden Karten stehen, wo der Server
  // landet – sonst ist die Weiterleitung nach Claude Code eine Falle.
  const claudeCodeHint = ctx.client
    ? ` Legt in Claude Code an – von dort weiter über „Kopieren nach…".`
    : "";

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
      desc:
        "Claude liest README/Doku einer URL und schlägt eine Konfiguration vor." + claudeCodeHint,
      badge: icon("sparkles"),
      onClick: openLink,
    }),
    card({
      title: "Aus Katalog wählen",
      desc: "Server in der offiziellen MCP-Registry suchen und übernehmen." + claudeCodeHint,
      badge: icon("globe"),
      onClick: openCatalog,
    }),
  );

  // Kann der Ziel-Client keine Remote-Server aus seiner Datei laden, sind die
  // Remote-Vorlagen dort eine Sackgasse: das Formular blendet url/headers aus
  // und erzwingt stdio, „Hinzufügen" scheiterte dann an „Command darf nicht leer
  // sein." – ohne Hinweis auf die Ursache. Begründung wortgleich zu
  // `clients::validate_entry`.
  const remoteBlocked =
    ctx.client && !ctx.client.caps.remote
      ? `${ctx.client.label} lädt Remote-Server nicht aus der Konfigurationsdatei – dort als ` +
        `Connector einrichten. Alternative: als stdio-Server über die Bridge „npx mcp-remote <url>“.`
      : undefined;

  const presetCards = PRESETS.map((p) =>
    card({
      title: p.label,
      desc: p.description,
      badge: h("span", { class: "badge badge-scope", text: presetTransport(p) }),
      onClick: () => openPreset(p),
      blockedReason: presetTransport(p) === "stdio" ? undefined : remoteBlocked,
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
