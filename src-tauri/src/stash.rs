//! Ablage für deaktivierte user-scope Server.
//!
//! Claude Code kennt kein natives "disabled" für user-scope. Zum Deaktivieren
//! sichern wir die vollständige Definition hier und entfernen sie via CLI;
//! zum Reaktivieren spielen wir sie zurück. Die Datei liegt nutzer-privat unter
//! $XDG_CONFIG_HOME/mcp-manager/stash.json (Modus 0600, enthält Klartext-Secrets).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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

pub fn stash_path() -> PathBuf {
    crate::util::config_dir().join("stash.json")
}

pub fn load() -> Stash {
    std::fs::read_to_string(stash_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(stash: &Stash) -> Result<(), AppError> {
    save_to(&stash_path(), stash)
}

/// Kern von [`save`] gegen einen expliziten Zielpfad – für Tests ohne
/// Env-Mutation (Muster wie `snapshot::create_in`).
///
/// Atomar, Muster `toggles::atomic_write_json`: die Temp-Datei kommt aus
/// [`crate::util::write_private_temp`] – unvorhersagbarer Name, `create_new` +
/// `O_NOFOLLOW`, Modus 0600 vor dem ersten Byte. Der frühere feste Name
/// `.stash.json.tmp` mit `create(true).truncate(true)` war vorhersagbar und
/// folgte einem vorab platzierten Symlink; ausgerechnet hier stehen
/// Klartext-Secrets.
fn save_to(path: &Path, stash: &Stash) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Io("kein Config-Verzeichnis".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| AppError::Io(e.to_string()))?;

    if path.exists() {
        let bak = parent.join("stash.json.bak");
        if std::fs::copy(path, &bak).is_ok() {
            // Backup enthält Klartext-Secrets -> ebenfalls auf 0600 einschränken.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&bak, std::fs::Permissions::from_mode(0o600));
            }
        }
    }

    let text = serde_json::to_string_pretty(stash).map_err(|e| AppError::Parse(e.to_string()))?;
    let fname = path.file_name().and_then(|f| f.to_str()).unwrap_or("stash.json");
    let tmp = crate::util::write_private_temp(parent, fname, "mcpmgr", text.as_bytes())?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp); // keine Leiche im Config-Verzeichnis
        return Err(AppError::Io(e.to_string()));
    }
    Ok(())
}

/// Definition ablegen (vor dem Entfernen aufrufen).
pub fn upsert(name: &str, entry: ServerEntry) -> Result<(), AppError> {
    let mut s = load();
    s.user.insert(
        name.to_string(),
        StashItem {
            entry,
            disabled_at: crate::util::unix_now(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mcpmgr-stash-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn eintrag(cmd: &str) -> ServerEntry {
        ServerEntry {
            command: Some(cmd.into()),
            ..Default::default()
        }
    }

    /// Schreibhärtung von [`save_to`]: die fertige `stash.json` ist 0600 (sie
    /// enthält Klartext-Secrets) und es bleibt keine Temp-Datei zurück,
    /// insbesondere keine unter dem früheren festen Namen `.stash.json.tmp`.
    #[test]
    fn save_schreibt_privat_und_ohne_festen_temp_namen() {
        let dir = tmpdir("hardening");
        let path = dir.join("stash.json");

        let mut s = Stash::default();
        s.user.insert(
            "a".into(),
            StashItem {
                entry: eintrag("echo"),
                disabled_at: 1,
            },
        );
        save_to(&path, &s).expect("save_to");

        assert!(path.is_file(), "stash.json wurde angelegt");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "Klartext-Secrets nur für den Nutzer lesbar");
        }
        assert!(
            !dir.join(".stash.json.tmp").exists(),
            "der feste, vorhersagbare Temp-Name ist abgelöst"
        );
        let leichen = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|d| d.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leichen, 0, "nach dem rename bleibt keine Temp-Datei liegen");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Zweiter Schreibvorgang: `rename` über die vorhandene Datei behält 0600,
    /// und das `.bak` mit denselben Klartext-Secrets wird ebenfalls eingeschränkt.
    #[test]
    fn zweiter_save_haelt_rechte_und_sichert_das_backup() {
        let dir = tmpdir("rewrite");
        let path = dir.join("stash.json");

        let mut s = Stash::default();
        s.user.insert(
            "a".into(),
            StashItem {
                entry: eintrag("alt"),
                disabled_at: 1,
            },
        );
        save_to(&path, &s).expect("erster save");
        s.user.insert(
            "a".into(),
            StashItem {
                entry: eintrag("neu"),
                disabled_at: 2,
            },
        );
        save_to(&path, &s).expect("zweiter save");

        let text = std::fs::read_to_string(&path).unwrap();
        let gelesen: Stash = serde_json::from_str(&text).unwrap();
        assert_eq!(gelesen.user["a"].entry.command.as_deref(), Some("neu"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(m(&path), 0o600, "Zieldatei bleibt privat");
            assert_eq!(m(&dir.join("stash.json.bak")), 0o600, "Backup ebenfalls privat");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
