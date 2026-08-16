//! Atomares Editieren der enable/disable-Arrays für .mcp.json-Server.
//!
//! Geschrieben wird in ~/.claude/settings.local.json (nutzer-privat, nicht
//! eingecheckt). Unbekannte Keys bleiben erhalten; der Write ist atomar
//! (Temp-Datei + rename).

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::config_read::read_json_value;
use crate::models::AppError;

/// Schreibt `value` atomar nach `path` (Temp-Datei + `rename`).
///
/// Die Temp-Datei wird über [`crate::util::write_private_temp`] angelegt:
/// unvorhersagbarer Name, `create_new` + `O_NOFOLLOW` und Modus 0600. Der feste
/// Name `.{fname}.mcpmgr.tmp` plus `std::fs::write` war angreifbar (ein vorab
/// platzierter Symlink wurde verfolgt) und vererbte über `rename` den
/// umask-Modus (i. d. R. 0644) auf das Ziel – bei `~/.claude.json` mit
/// Klartext-Secrets die eigentliche Lücke.
pub fn atomic_write_json(path: &Path, value: &Value) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Io("kein übergeordnetes Verzeichnis".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| AppError::Io(e.to_string()))?;
    let fname = path.file_name().and_then(|f| f.to_str()).unwrap_or("tmp");

    let mut text = serde_json::to_string_pretty(value).map_err(|e| AppError::Parse(e.to_string()))?;
    text.push('\n');
    let tmp = crate::util::write_private_temp(parent, fname, "mcpmgr", text.as_bytes())?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp); // keine Leiche im Zielverzeichnis
        return Err(AppError::Io(e.to_string()));
    }
    Ok(())
}

fn string_vec(obj: &Map<String, Value>, key: &str) -> Vec<String> {
    obj.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Setzt `name` in genau eines der beiden Arrays (und entfernt ihn aus dem
/// anderen).
fn set_state(en: &mut Vec<String>, dis: &mut Vec<String>, name: &str, enabled: bool) {
    let (add, remove) = if enabled { (en, dis) } else { (dis, en) };
    remove.retain(|s| s != name);
    if !add.iter().any(|s| s == name) {
        add.push(name.to_string());
    }
}

/// Liest die enable/disable-Arrays der settings-Datei, lässt `edit` sie ändern
/// und schreibt nur, wenn `edit` das meldet.
///
/// Das „nur bei Änderung schreiben" verhindert, dass ein Aufräum-Aufruf
/// (`forget_mcpjson`) eine gar nicht existierende `settings.local.json` neu
/// anlegt, bloß weil ein Server entfernt wurde, der dort nie stand.
fn edit_arrays(
    settings_path: &Path,
    edit: impl FnOnce(&mut Vec<String>, &mut Vec<String>) -> bool,
) -> Result<(), AppError> {
    let mut root = read_json_value(settings_path).unwrap_or_else(|| json!({}));
    if !root.is_object() {
        root = json!({});
    }
    let obj = root.as_object_mut().unwrap();

    let mut en = string_vec(obj, "enabledMcpjsonServers");
    let mut dis = string_vec(obj, "disabledMcpjsonServers");
    if !edit(&mut en, &mut dis) {
        return Ok(());
    }

    obj.insert("enabledMcpjsonServers".into(), json!(en));
    obj.insert("disabledMcpjsonServers".into(), json!(dis));
    atomic_write_json(settings_path, &root)
}

/// Aktiviert/deaktiviert einen .mcp.json-Server über die enable/disable-Arrays
/// der angegebenen settings-Datei (`settings_path`).
pub fn toggle_mcpjson(settings_path: &Path, name: &str, enabled: bool) -> Result<(), AppError> {
    edit_arrays(settings_path, |en, dis| {
        set_state(en, dis, name, enabled);
        true // Toggle schreibt immer – der Zustand soll explizit in der Datei stehen.
    })
}

/// Streicht `name` aus BEIDEN Arrays.
///
/// Nötig, wenn ein `.mcp.json`-Server verschwindet (entfernt, umbenannt oder in
/// einen anderen Scope verschoben): die Arrays sind rein namensbasiert, ein
/// verwaister Eintrag würde einen später gleichnamig angelegten Server ungefragt
/// vor-aktivieren oder vor-deaktivieren.
pub fn forget_mcpjson(settings_path: &Path, name: &str) -> Result<(), AppError> {
    edit_arrays(settings_path, |en, dis| {
        let before = en.len() + dis.len();
        en.retain(|s| s != name);
        dis.retain(|s| s != name);
        en.len() + dis.len() != before
    })
}

/// Überträgt den enable/disable-Zustand eines `.mcp.json`-Servers beim
/// Umbenennen auf den neuen Namen und entfernt den alten aus beiden Arrays.
///
/// `enabled` ist der zuvor ermittelte effektive Zustand (`DisabledInfo::is_enabled`).
/// Ohne diese Migration stünde der neue Name in keinem Array – und
/// `is_enabled` liefert dann `false`: ein bloßes Umbenennen hätte den Server
/// stillschweigend deaktiviert.
pub fn rename_mcpjson(
    settings_path: &Path,
    old: &str,
    new: &str,
    enabled: bool,
) -> Result<(), AppError> {
    edit_arrays(settings_path, |en, dis| {
        en.retain(|s| s != old);
        dis.retain(|s| s != old);
        set_state(en, dis, new, enabled);
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Eindeutiges Temp-Verzeichnis pro Test.
    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mcpmgr-toggles-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn arrays(path: &Path) -> (Vec<String>, Vec<String>) {
        let root = read_json_value(path).unwrap_or_else(|| json!({}));
        let obj = root.as_object().unwrap().clone();
        (
            string_vec(&obj, "enabledMcpjsonServers"),
            string_vec(&obj, "disabledMcpjsonServers"),
        )
    }

    /// Regression P3-4: Umbenennen muss den enable-Zustand mitnehmen, sonst
    /// steht der neue Name in keinem Array und gilt damit als deaktiviert.
    #[test]
    fn rename_migriert_den_zustand() {
        let dir = tmp("rename");
        let p = dir.join("settings.local.json");
        toggle_mcpjson(&p, "alt", true).unwrap();
        assert_eq!(arrays(&p).0, vec!["alt".to_string()]);

        rename_mcpjson(&p, "alt", "neu", true).unwrap();
        let (en, dis) = arrays(&p);
        assert_eq!(en, vec!["neu".to_string()], "neuer Name muss aktiviert sein");
        assert!(!dis.contains(&"alt".to_string()));
        assert!(!en.contains(&"alt".to_string()), "alter Name als Leiche zurückgeblieben");

        // Ein deaktivierter Server bleibt deaktiviert.
        toggle_mcpjson(&p, "aus", false).unwrap();
        rename_mcpjson(&p, "aus", "aus2", false).unwrap();
        let (en2, dis2) = arrays(&p);
        assert!(dis2.contains(&"aus2".to_string()));
        assert!(!dis2.contains(&"aus".to_string()));
        assert!(!en2.contains(&"aus".to_string()));
    }

    /// Regression P3-4: Entfernen/Verschieben darf keinen verwaisten Eintrag
    /// hinterlassen, der einen später gleichnamigen Server vorbelegt.
    #[test]
    fn forget_streicht_aus_beiden_arrays() {
        let dir = tmp("forget");
        let p = dir.join("settings.local.json");
        toggle_mcpjson(&p, "a", true).unwrap();
        toggle_mcpjson(&p, "b", false).unwrap();

        forget_mcpjson(&p, "a").unwrap();
        forget_mcpjson(&p, "b").unwrap();
        let (en, dis) = arrays(&p);
        assert!(en.is_empty(), "enabled nicht geleert: {en:?}");
        assert!(dis.is_empty(), "disabled nicht geleert: {dis:?}");
    }

    /// `forget_mcpjson` darf keine settings-Datei aus dem Nichts erzeugen.
    #[test]
    fn forget_legt_keine_datei_an() {
        let dir = tmp("forget-noop");
        let p = dir.join(".claude/settings.local.json");
        forget_mcpjson(&p, "gibtsnicht").unwrap();
        assert!(!p.exists(), "settings.local.json wurde grundlos angelegt");
    }

    /// Regression P3-10: die Temp-Datei bekommt einen unvorhersagbaren Namen,
    /// wird exklusiv angelegt und vererbt über `rename` den Modus 0600.
    #[cfg(unix)]
    #[test]
    fn atomic_write_ist_privat_und_folgt_keinem_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp("atomic");
        let ziel = dir.join("fremd.txt");
        std::fs::write(&ziel, "unberuehrt").unwrap();
        // Alter, fest vorhersagbarer Temp-Name als Symlink auf eine fremde Datei.
        let alt = dir.join(".settings.local.json.mcpmgr.tmp");
        std::os::unix::fs::symlink(&ziel, &alt).unwrap();

        let p = dir.join("settings.local.json");
        toggle_mcpjson(&p, "x", true).unwrap();

        assert_eq!(std::fs::read_to_string(&ziel).unwrap(), "unberuehrt");
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "Zieldatei muss privat sein");
    }
}
