import { h } from "../dom";
import type { AppSettings, Theme } from "../ipc";
import { checkClaude, getSettings, setSettings } from "../ipc";
import { openModal } from "../modal";
import { field } from "./serverForm";
import { switchControl } from "../switch";
import { toast } from "../toast";
import { TIMEOUT_MIN, TIMEOUT_MAX, AUTO_REFRESH_MAX, RETENTION_MIN, RETENTION_MAX } from "../constants";

export interface SettingsOptions {
  /// Wird nach erfolgreichem Speichern aufgerufen (z. B. Claude-Badge neu prüfen).
  onSaved: (settings: AppSettings) => void;
}

const THEME_KEY = "mcpmgr-theme";

function isTheme(v: unknown): v is Theme {
  return v === "system" || v === "light" || v === "dark";
}

/// Wendet das Theme auf das Wurzelelement an (steuert die CSS-Variablen) und
/// merkt es lokal, damit der nächste Start es SOFORT (vor dem ersten Paint)
/// anwenden kann – kein Dark-Flash für Hell-Nutzer.
export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch {
    /* localStorage nicht verfügbar – nur der Flash-Schutz entfällt. */
  }
}

/// Wendet das zuletzt lokal gemerkte Theme an, ohne es zurückzuschreiben.
/// Beim Start vor dem Rendern aufrufen; die maßgebliche Fassung liefert danach
/// das Backend (get_settings -> applyTheme).
export function applyStoredTheme(): void {
  let theme: Theme = "system";
  try {
    const s = localStorage.getItem(THEME_KEY);
    if (isTheme(s)) theme = s;
  } catch {
    /* ignorieren, System-Default */
  }
  document.documentElement.dataset.theme = theme;
}

function group(title: string, ...children: Array<Node | null>): HTMLElement {
  return h(
    "section",
    { class: "settings-group" },
    h("h3", { class: "settings-group-title", text: title }),
    ...children,
  );
}

function numberInput(value: number, min: number, max?: number): HTMLInputElement {
  const inp = h("input", { class: "inp", type: "number", min: String(min) }) as HTMLInputElement;
  if (max !== undefined) inp.max = String(max);
  inp.value = String(value);
  return inp;
}

export async function openSettings(opts: SettingsOptions): Promise<void> {
  let settings: AppSettings;
  try {
    settings = await getSettings();
  } catch (e) {
    toast("Einstellungen konnten nicht geladen werden: " + String(e), "error");
    return;
  }

  // --- Claude CLI ---
  const pathInput = h("input", {
    class: "inp mono",
    placeholder: "automatisch (leer lassen)",
  }) as HTMLInputElement;
  pathInput.value = settings.claude_path ?? "";
  const autoBtn = h("button", { class: "btn btn-small", type: "button" }, "Automatisch") as HTMLButtonElement;
  autoBtn.addEventListener("click", () => {
    pathInput.value = "";
    pathInput.focus();
  });
  const pathRow = h("div", { class: "settings-path-row" }, pathInput, autoBtn);
  const resolved = h("div", { class: "field-hint", text: "Aufgelöster Pfad: wird geprüft…" });
  // Aktuell aufgelösten Pfad anzeigen (Diagnose neben dem Eingabefeld).
  void checkClaude()
    .then((info) => {
      resolved.textContent = info.ok
        ? `Aktuell aufgelöst: ${info.path}`
        : "Aktuell keine claude-CLI aufgelöst.";
    })
    .catch(() => {
      resolved.textContent = "Aufgelöster Pfad: unbekannt.";
    });

  const listTimeout = numberInput(settings.list_timeout_secs, TIMEOUT_MIN, TIMEOUT_MAX);
  const mutTimeout = numberInput(settings.mut_timeout_secs, TIMEOUT_MIN, TIMEOUT_MAX);

  const cliGroup = group(
    "Claude CLI",
    field("Pfad zur claude-CLI", pathRow, "Leer = automatische Auflösung (Env-Var überschreibt immer)."),
    resolved,
    field(`Timeout „Liste/Health-Check" (Sekunden)`, listTimeout, `${TIMEOUT_MIN}–${TIMEOUT_MAX} s.`),
    field(`Timeout „Änderungen" (Sekunden)`, mutTimeout, `${TIMEOUT_MIN}–${TIMEOUT_MAX} s.`),
  );

  // --- Verhalten ---
  const autoRefresh = numberInput(settings.auto_refresh_minutes, 0, AUTO_REFRESH_MAX);
  let notifOn = settings.notifications;
  const notif = switchControl({ on: settings.notifications, onChange: (v) => (notifOn = v) });
  const retention = numberInput(settings.snapshot_retention, RETENTION_MIN, RETENTION_MAX);

  const behaviorGroup = group(
    "Verhalten",
    field(
      "Auto-Refresh (Minuten, 0 = aus)",
      autoRefresh,
      "Aktualisiert den Status periodisch selbst (pausiert bei offenem Dialog/Hintergrund).",
    ),
    field("Benachrichtigungen", notif, "Desktop-Hinweis bei Statusverschlechterung (braucht Auto-Refresh)."),
    field(
      "Snapshot-Aufbewahrung",
      retention,
      `Anzahl automatisch aufbewahrter Snapshots (${RETENTION_MIN}–${RETENTION_MAX}). Manuelle bleiben immer erhalten.`,
    ),
  );

  // --- Darstellung ---
  const themeSelect = h(
    "select",
    { class: "inp" },
    h("option", { value: "system" }, "System"),
    h("option", { value: "light" }, "Hell"),
    h("option", { value: "dark" }, "Dunkel"),
  ) as HTMLSelectElement;
  themeSelect.value = settings.theme;
  // Bewusst keine Live-Vorschau beim Umschalten: das Modal kann per Escape/
  // Hintergrund-Klick geschlossen werden (ohne unseren Abbrechen-Pfad), was eine
  // nicht gespeicherte Vorschau hängen ließe. Das Theme greift beim Speichern.

  const langSelect = h(
    "select",
    { class: "inp" },
    h("option", { value: "system" }, "System"),
  ) as HTMLSelectElement;
  langSelect.disabled = true;

  const appearanceGroup = group(
    "Darstellung",
    field("Theme", themeSelect),
    field("Sprache", langSelect, "Verfügbar mit Internationalisierung (Feature 21)."),
  );

  const status = h("div", { class: "form-status" });
  const body = h("div", { class: "settings-form" }, cliGroup, behaviorGroup, appearanceGroup, status);

  const cancelBtn = h("button", { class: "btn" }, "Abbrechen") as HTMLButtonElement;
  const saveBtn = h("button", { class: "btn btn-primary" }, "Speichern") as HTMLButtonElement;
  const modal = openModal("Einstellungen", body, [cancelBtn, saveBtn]);

  cancelBtn.addEventListener("click", () => modal.close());

  // Clientseitig nur Basissanität (ganze Zahl > 0). Die exakte Bereichsprüfung
  // (TIMEOUT_MIN..TIMEOUT_MAX) erzwingt das Backend und meldet sie klar zurück –
  // so gibt es keine abweichende Annahme/Ablehnung zwischen Front- und Backend.
  const asPositiveInt = (inp: HTMLInputElement, label: string): number | string => {
    const n = Number(inp.value);
    if (!Number.isInteger(n) || n <= 0) {
      return `${label}: bitte eine ganze Zahl größer als 0 angeben.`;
    }
    return n;
  };

  saveBtn.addEventListener("click", async () => {
    const listV = asPositiveInt(listTimeout, "Listen-Timeout");
    if (typeof listV === "string") return fail(listV);
    const mutV = asPositiveInt(mutTimeout, "Änderungs-Timeout");
    if (typeof mutV === "string") return fail(mutV);
    const retentionV = asPositiveInt(retention, "Snapshot-Aufbewahrung");
    if (typeof retentionV === "string") return fail(retentionV);
    // Auf [0, AUTO_REFRESH_MAX] klemmen (u32-Überlauf vermeiden; Backend prüft
    // dieses Feld nicht).
    const refreshN = Math.min(AUTO_REFRESH_MAX, Math.max(0, Math.trunc(Number(autoRefresh.value) || 0)));

    const next: AppSettings = {
      ...settings,
      claude_path: pathInput.value.trim() || null,
      list_timeout_secs: listV,
      mut_timeout_secs: mutV,
      auto_refresh_minutes: refreshN,
      notifications: notifOn,
      snapshot_retention: retentionV,
      theme: themeSelect.value as Theme,
    };

    saveBtn.disabled = true;
    cancelBtn.disabled = true;
    status.className = "form-status";
    status.textContent = "wird gespeichert…";
    try {
      const saved = await setSettings(next);
      applyTheme(saved.theme);
      toast("Einstellungen gespeichert");
      modal.close();
      opts.onSaved(saved);
    } catch (e) {
      fail("Fehler: " + String(e));
      saveBtn.disabled = false;
      cancelBtn.disabled = false;
    }
  });

  function fail(msg: string): void {
    status.className = "form-status error";
    status.textContent = msg;
  }
}
