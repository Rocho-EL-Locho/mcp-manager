//! Ablage für deaktivierte user-scope Server.
//!
//! Claude Code kennt kein natives "disabled" für user-scope. Zum Deaktivieren
//! sichern wir die vollständige Definition hier und entfernen sie via CLI;
//! zum Reaktivieren spielen wir sie zurück. Die Datei liegt nutzer-privat unter
//! $XDG_CONFIG_HOME/mcp-manager/stash.json (Modus 0600, enthält Klartext-Secrets).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::claude_cli::home_dir;
use crate::models::{AppError, ServerEntry};

#[derive(Default, Serialize, Deserialize)]
pub struct Stash {
    #[serde(default)]
    pub user: BTreeMap<String, StashItem>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StashItem {
    pub entry: ServerEntry,
    #[serde(default)]
    pub disabled_at: u64,
}

/// Nutzer-privates Config-Verzeichnis dieser App ($XDG_CONFIG_HOME/mcp-manager
/// bzw. ~/.config/mcp-manager). Gemeinsame Ablage für Stash und `settings.rs`.
pub(crate) fn config_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(x).join("mcp-manager")
    } else {
        home_dir().unwrap_or_default().join(".config/mcp-manager")
    }
}

pub fn stash_path() -> PathBuf {
    config_dir().join("stash.json")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn load() -> Stash {
    std::fs::read_to_string(stash_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(stash: &Stash) -> Result<(), AppError> {
    let path = stash_path();
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Io("kein Config-Verzeichnis".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| AppError::Io(e.to_string()))?;

    if path.exists() {
        let bak = parent.join("stash.json.bak");
        if std::fs::copy(&path, &bak).is_ok() {
            // Backup enthält Klartext-Secrets -> ebenfalls auf 0600 einschränken.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&bak, std::fs::Permissions::from_mode(0o600));
            }
        }
    }

    let tmp = parent.join(".stash.json.tmp");
    let text = serde_json::to_string_pretty(stash).map_err(|e| AppError::Parse(e.to_string()))?;

    // Temp-Datei unter Unix direkt mit Modus 0600 anlegen, BEVOR Klartext-Secrets
    // hineingeschrieben werden – kein kurzes world-/group-readable-Fenster.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| AppError::Io(e.to_string()))?;
        f.write_all(text.as_bytes())
            .map_err(|e| AppError::Io(e.to_string()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&tmp, text).map_err(|e| AppError::Io(e.to_string()))?;
    }

    std::fs::rename(&tmp, &path).map_err(|e| AppError::Io(e.to_string()))?;
    Ok(())
}

/// Definition ablegen (vor dem Entfernen aufrufen).
pub fn upsert(name: &str, entry: ServerEntry) -> Result<(), AppError> {
    let mut s = load();
    s.user.insert(
        name.to_string(),
        StashItem {
            entry,
            disabled_at: now_secs(),
        },
    );
    save(&s)
}

/// Definition lesen, ohne sie zu entfernen.
pub fn peek(name: &str) -> Option<StashItem> {
    load().user.get(name).cloned()
}

/// Eintrag entfernen (erst nach erfolgreichem Reaktivieren aufrufen).
pub fn remove(name: &str) -> Result<(), AppError> {
    let mut s = load();
    if s.user.remove(name).is_some() {
        save(&s)?;
    }
    Ok(())
}

pub fn all() -> Stash {
    load()
}
