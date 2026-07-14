//! Persistente App-Konfiguration.
//!
//! Ablage: $XDG_CONFIG_HOME/mcp-manager/settings.json (normale Rechte, neben
//! dem Stash). **Hier liegen bewusst keine Secrets** – jetzt nicht und in
//! Zukunft nicht; die Datei darf also world-readable sein.
//!
//! Robustheit: `#[serde(default)]` sorgt dafür, dass jede fehlende Option auf
//! ihren Default fällt – alte Dateien (und Zukunftsversionen mit zusätzlichen
//! Feldern) bleiben für immer lesbar. Unbekannte Felder gehen beim Zurück-
//! schreiben verloren; das ist akzeptiert (eigene Datei, keine fremden Schreiber).
//!
//! Kein Datei-Watching: gelesen wird beim Start, geschrieben beim Speichern –
//! bei einem Handedit während laufender App gewinnt der letzte Schreiber.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::models::AppError;

/// Erscheinungsbild. `System` folgt dem OS-Farbschema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

const DEFAULT_LIST_TIMEOUT: u64 = 45;
const DEFAULT_MUT_TIMEOUT: u64 = 30;
const DEFAULT_SNAPSHOT_RETENTION: u32 = 20;

/// Erlaubter Bereich für die konfigurierbaren Timeouts (Sekunden).
pub const TIMEOUT_MIN: u64 = 5;
pub const TIMEOUT_MAX: u64 = 600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Pfad zur claude-CLI. `None` = automatische Auflösung (Env > which > bekannte Pfade).
    pub claude_path: Option<String>,
    /// Timeout für `claude mcp list` (Health-Check aller Server).
    pub list_timeout_secs: u64,
    /// Timeout für mutierende claude-Aufrufe (add/remove/…).
    pub mut_timeout_secs: u64,
    /// Auto-Refresh-Intervall in Minuten (0 = aus). Konsument: Feature 09.
    pub auto_refresh_minutes: u32,
    /// Desktop-Benachrichtigungen. Konsument: Feature 09.
    pub notifications: bool,
    /// Zahl aufbewahrter Snapshots. Konsument: Feature 05.
    pub snapshot_retention: u32,
    /// UI-Sprache (None = System). Konsument: Feature 21.
    pub language: Option<String>,
    pub theme: Theme,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            claude_path: None,
            list_timeout_secs: DEFAULT_LIST_TIMEOUT,
            mut_timeout_secs: DEFAULT_MUT_TIMEOUT,
            auto_refresh_minutes: 0,
            notifications: false,
            snapshot_retention: DEFAULT_SNAPSHOT_RETENTION,
            language: None,
            theme: Theme::System,
        }
    }
}

impl AppSettings {
    pub fn list_timeout(&self) -> Duration {
        Duration::from_secs(self.list_timeout_secs)
    }

    pub fn mut_timeout(&self) -> Duration {
        Duration::from_secs(self.mut_timeout_secs)
    }

    /// Konfigurierter claude-Pfad, sofern nicht leer/whitespace (dann `None` =
    /// automatische Auflösung).
    pub fn claude_path(&self) -> Option<&str> {
        self.claude_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
    }
}

fn settings_path() -> std::path::PathBuf {
    crate::stash::config_dir().join("settings.json")
}

/// Lädt die Einstellungen. Eine fehlende Datei (normaler Erstlauf) oder eine
/// korrupte Datei liefern die Defaults – nie einen Startfehler.
pub fn load() -> AppSettings {
    match std::fs::read_to_string(settings_path()) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!(
                "mcp-manager: settings.json nicht lesbar ({e}) – Standardwerte werden verwendet."
            );
            AppSettings::default()
        }),
        // Datei fehlt (o. ä.): stiller Erstlauf mit Defaults.
        Err(_) => AppSettings::default(),
    }
}

/// Schreibt die Einstellungen atomar (Temp-Datei + Rename). Keine Secrets ->
/// normale Rechte genügen.
pub fn save(settings: &AppSettings) -> Result<(), AppError> {
    let value = serde_json::to_value(settings).map_err(|e| AppError::Parse(e.to_string()))?;
    crate::toggles::atomic_write_json(&settings_path(), &value)
}

/// Prüft Grenzen und – wenn gesetzt – dass `claude_path` auf eine existierende
/// Datei zeigt. Bewusst dieselbe Bedingung (`is_file`) wie `resolve_claude`:
/// Ein bloßer Befehlsname (per PATH auffindbar, aber keine Datei) würde zur
/// Laufzeit ignoriert – also hier klar ablehnen statt still schlucken.
pub fn validate(settings: &AppSettings) -> Result<(), AppError> {
    for (label, secs) in [
        ("List-Timeout", settings.list_timeout_secs),
        ("Änderungs-Timeout", settings.mut_timeout_secs),
    ] {
        if !(TIMEOUT_MIN..=TIMEOUT_MAX).contains(&secs) {
            return Err(AppError::Io(format!(
                "{label} muss zwischen {TIMEOUT_MIN} und {TIMEOUT_MAX} Sekunden liegen (war {secs})."
            )));
        }
    }
    if let Some(path) = settings.claude_path() {
        if !std::path::Path::new(path).is_file() {
            return Err(AppError::Io(format!(
                "claude-Pfad ist keine Datei: {path}. Bitte den vollständigen Pfad zur claude-CLI \
                 angeben (kein bloßer Befehlsname)."
            )));
        }
    }
    Ok(())
}

/// Normalisiert Nutzer-Eingaben vor dem Speichern: leerer claude-Pfad -> `None`
/// (automatische Auflösung). Idempotent.
pub fn normalize(mut settings: AppSettings) -> AppSettings {
    if settings.claude_path().is_none() {
        settings.claude_path = None;
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let s = AppSettings::default();
        assert_eq!(s.list_timeout_secs, DEFAULT_LIST_TIMEOUT);
        assert_eq!(s.mut_timeout_secs, DEFAULT_MUT_TIMEOUT);
        assert_eq!(s.theme, Theme::System);
        assert!(s.claude_path().is_none());
    }

    #[test]
    fn partial_json_falls_back_to_defaults() {
        // Nur ein Feld gesetzt -> Rest kommt aus Default (serde(default)).
        let s: AppSettings = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert_eq!(s.theme, Theme::Dark);
        assert_eq!(s.list_timeout_secs, DEFAULT_LIST_TIMEOUT);
        assert_eq!(s.mut_timeout_secs, DEFAULT_MUT_TIMEOUT);
    }

    #[test]
    fn corrupt_json_deserializes_to_error_not_panic() {
        assert!(serde_json::from_str::<AppSettings>("{ not json").is_err());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let s: AppSettings =
            serde_json::from_str(r#"{"list_timeout_secs":10,"zukunftsfeld":42}"#).unwrap();
        assert_eq!(s.list_timeout_secs, 10);
    }

    #[test]
    fn validate_rejects_out_of_range_timeouts() {
        let mut s = AppSettings::default();
        s.list_timeout_secs = TIMEOUT_MAX + 1;
        assert!(validate(&s).is_err());
        s.list_timeout_secs = TIMEOUT_MIN - 1;
        assert!(validate(&s).is_err());
        s.list_timeout_secs = TIMEOUT_MIN;
        s.mut_timeout_secs = 0;
        assert!(validate(&s).is_err());
    }

    #[test]
    fn validate_accepts_defaults() {
        assert!(validate(&AppSettings::default()).is_ok());
    }

    #[test]
    fn validate_checks_claude_path() {
        let mut s = AppSettings::default();
        s.claude_path = Some("/bin/sh".into()); // existiert + ausführbar
        assert!(validate(&s).is_ok());
        s.claude_path = Some("/nicht/vorhanden/mcpmgr-xyz".into());
        assert!(validate(&s).is_err());
    }

    #[test]
    fn normalize_blanks_claude_path() {
        let mut s = AppSettings::default();
        s.claude_path = Some("   ".into());
        assert_eq!(normalize(s).claude_path, None);
    }

    /// Opt-in (schreibt in ein temporäres XDG_CONFIG_HOME, mutiert Prozess-Env –
    /// daher `#[ignore]`, um Races mit anderen Tests zu vermeiden). Beweist den
    /// save -> load Round-Trip über das echte Dateisystem. Mit `-- --ignored`.
    #[test]
    #[ignore]
    fn save_load_roundtrip() {
        let tmp = std::env::temp_dir().join("mcpmgr-settings-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("XDG_CONFIG_HOME", &tmp);

        let mut s = AppSettings::default();
        s.theme = Theme::Light;
        s.list_timeout_secs = 60;
        s.claude_path = Some("/usr/bin/claude".into());
        save(&s).expect("save");

        let loaded = load();
        assert_eq!(loaded, s);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn claude_path_helper_trims_and_filters() {
        let mut s = AppSettings::default();
        s.claude_path = Some("  /usr/bin/claude  ".into());
        assert_eq!(s.claude_path(), Some("/usr/bin/claude"));
        s.claude_path = Some("".into());
        assert_eq!(s.claude_path(), None);
    }
}
