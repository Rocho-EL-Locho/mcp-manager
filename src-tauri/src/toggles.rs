//! Atomares Editieren der enable/disable-Arrays für .mcp.json-Server.
//!
//! Geschrieben wird in ~/.claude/settings.local.json (nutzer-privat, nicht
//! eingecheckt). Unbekannte Keys bleiben erhalten; der Write ist atomar
//! (Temp-Datei + rename).
//!
//! Hier wohnt außerdem die **gemeinsame** Schreib-Routine für alle kleinen,
//! selbst verwalteten Dateien (`stash.rs`, `settings.rs`, `metrics.rs`,
//! `snapshot.rs`): nutzer-privat (0600), ohne Symlinks zu folgen, und atomar
//! über eine Temp-Datei mit unvorhersagbarem Namen.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::config_read::read_json_value;
use crate::models::AppError;

/// Wie oft ein neuer Temp-Name probiert wird, bevor aufgegeben wird.
const TEMP_ATTEMPTS: u32 = 32;

/// Öffnet `path` zum Schreiben als nutzer-private Datei.
///
/// Unix: Modus 0600 direkt beim Anlegen (kein kurzes world-/group-readable
/// Fenster – die Inhalte können Klartext-Secrets enthalten) und `O_NOFOLLOW`,
/// damit ein untergeschobener Symlink den Schreibvorgang nicht auf ein fremdes
/// Ziel umlenkt. `exclusive` verlangt zusätzlich, dass die Datei neu entsteht
/// (`create_new`) – für Temp-Dateien.
fn open_private(path: &Path, exclusive: bool) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true);
    if exclusive {
        opts.create_new(true);
    } else {
        opts.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path)
}

/// Schreibt `bytes` in die nutzer-private Datei `path` (siehe [`open_private`]).
/// Eine bereits vorhandene reguläre Datei wird überschrieben, ein Symlink an
/// dieser Stelle führt zum Fehler statt zum Schreiben ins Symlink-Ziel.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    use std::io::Write;
    let mut f = open_private(path, false).map_err(|e| AppError::Io(e.to_string()))?;
    f.write_all(bytes).map_err(|e| AppError::Io(e.to_string()))?;
    Ok(())
}

/// Unvorhersagbares Namensfragment für Temp-Dateien. Quelle ist der vom
/// Betriebssystem gesäte Seed von `RandomState` – kein zusätzliches Crate nötig.
fn temp_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_usize(std::process::id() as usize);
    format!("{:016x}", h.finish())
}

/// Legt in `dir` eine **neue** nutzer-private Temp-Datei mit unvorhersagbarem
/// Namen an, schreibt `bytes` hinein und liefert ihren Pfad.
///
/// `create_new` + `O_NOFOLLOW`: ist der Name schon belegt (auch von einem
/// vorbereiteten Symlink), schlägt das Anlegen fehl und es wird ein neuer Name
/// probiert – ein Angreifer kann den Schreibvorgang also weder umlenken noch
/// die Rechte der Zieldatei über die umask beeinflussen.
pub(crate) fn write_private_temp(dir: &Path, bytes: &[u8]) -> Result<PathBuf, AppError> {
    use std::io::Write;
    let mut last: Option<std::io::Error> = None;
    for _ in 0..TEMP_ATTEMPTS {
        let candidate = dir.join(format!(".mcpmgr-{}.tmp", temp_token()));
        match open_private(&candidate, true) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(bytes) {
                    let _ = std::fs::remove_file(&candidate);
                    return Err(AppError::Io(e.to_string()));
                }
                return Ok(candidate);
            }
            // Name belegt: nächsten Versuch starten.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(AppError::Io(e.to_string())),
        }
    }
    Err(AppError::Io(format!(
        "keine freie Temp-Datei in {}{}",
        dir.display(),
        last.map(|e| format!(": {e}")).unwrap_or_default()
    )))
}

/// Schreibt `bytes` atomar nach `path`: private Temp-Datei im Zielverzeichnis
/// (gleiches Dateisystem) + `rename`. Leser sehen nie einen halben Stand, und
/// das Ergebnis ist unabhängig von der umask auf 0600 beschränkt.
pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Io("kein übergeordnetes Verzeichnis".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| AppError::Io(e.to_string()))?;
    let tmp = write_private_temp(parent, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(AppError::Io(e.to_string()));
    }
    Ok(())
}

/// Schreibt `value` atomar als hübsch formatiertes JSON nach `path`.
pub fn atomic_write_json(path: &Path, value: &Value) -> Result<(), AppError> {
    let mut text = serde_json::to_string_pretty(value).map_err(|e| AppError::Parse(e.to_string()))?;
    text.push('\n');
    atomic_write_bytes(path, text.as_bytes())
}

fn string_vec(obj: &Map<String, Value>, key: &str) -> Vec<String> {
    obj.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Aktueller **expliziter** Toggle-Zustand von `name`:
/// `Some(false)` = steht in `disabledMcpjsonServers`, `Some(true)` = steht in
/// `enabledMcpjsonServers`, `None` = kein Eintrag (es gilt der Default).
///
/// Reihenfolge wie in `DisabledInfo::is_enabled`: disabled gewinnt.
/// Wird beim Umbenennen gebraucht, um den Zustand mitzunehmen – die Arrays sind
/// NAMENSbasiert, sonst erschiene ein zuvor aktivierter Server nach dem
/// Umbenennen schlagartig als deaktiviert.
pub fn mcpjson_state(settings_path: &Path, name: &str) -> Option<bool> {
    let root = read_json_value(settings_path)?;
    let obj = root.as_object()?;
    if string_vec(obj, "disabledMcpjsonServers").iter().any(|s| s == name) {
        return Some(false);
    }
    if string_vec(obj, "enabledMcpjsonServers").iter().any(|s| s == name) {
        return Some(true);
    }
    None
}

/// Streicht `name` aus BEIDEN Arrays.
///
/// Für Entfernen, Umbenennen und Scope-Wechsel: die Arrays sind namensbasiert,
/// ein verwaister Eintrag würde einen später gleichnamig angelegten Server
/// ungefragt vor-aktivieren bzw. vor-deaktivieren.
/// No-op (ohne Schreibvorgang), wenn die Datei fehlt, kein Objekt ist oder der
/// Name gar nicht vorkommt.
pub fn forget_mcpjson(settings_path: &Path, name: &str) -> Result<(), AppError> {
    let Some(mut root) = read_json_value(settings_path) else {
        return Ok(());
    };
    let Some(obj) = root.as_object_mut() else {
        return Ok(());
    };
    let mut en = string_vec(obj, "enabledMcpjsonServers");
    let mut dis = string_vec(obj, "disabledMcpjsonServers");
    let before = en.len() + dis.len();
    en.retain(|s| s != name);
    dis.retain(|s| s != name);
    if en.len() + dis.len() == before {
        return Ok(());
    }
    obj.insert("enabledMcpjsonServers".into(), json!(en));
    obj.insert("disabledMcpjsonServers".into(), json!(dis));
    atomic_write_json(settings_path, &root)
}

/// Aktiviert/deaktiviert einen .mcp.json-Server über die enable/disable-Arrays
/// der angegebenen settings-Datei (`settings_path`).
pub fn toggle_mcpjson(settings_path: &Path, name: &str, enabled: bool) -> Result<(), AppError> {
    let path = settings_path;
    let mut root = read_json_value(path).unwrap_or_else(|| json!({}));
    if !root.is_object() {
        root = json!({});
    }
    let obj = root.as_object_mut().unwrap();

    let mut en = string_vec(obj, "enabledMcpjsonServers");
    let mut dis = string_vec(obj, "disabledMcpjsonServers");

    if enabled {
        dis.retain(|s| s != name);
        if !en.iter().any(|s| s == name) {
            en.push(name.to_string());
        }
    } else {
        en.retain(|s| s != name);
        if !dis.iter().any(|s| s == name) {
            dis.push(name.to_string());
        }
    }

    obj.insert("enabledMcpjsonServers".into(), json!(en));
    obj.insert("disabledMcpjsonServers".into(), json!(dis));
    atomic_write_json(path, &root)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Eindeutiges Temp-Verzeichnis pro Test (arbeitet nur unter /tmp).
    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mcpmgr-toggles-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Namen aller Temp-Dateien, die in `dir` liegen geblieben sind.
    fn leftover_temps(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".mcpmgr-") && n.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn atomic_write_json_schreibt_und_raeumt_auf() {
        let dir = tmp("atomic");
        let target = dir.join("settings.json");
        atomic_write_json(&target, &json!({"a": 1})).unwrap();
        assert_eq!(
            read_json_value(&target).unwrap(),
            json!({"a": 1}),
            "Inhalt geschrieben"
        );
        // Zweiter Durchlauf überschreibt (rename über die vorhandene Datei).
        atomic_write_json(&target, &json!({"a": 2})).unwrap();
        assert_eq!(read_json_value(&target).unwrap(), json!({"a": 2}));
        assert!(
            leftover_temps(&dir).is_empty(),
            "keine Temp-Datei zurückgelassen"
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_json_ergebnis_ist_0600() {
        let dir = tmp("mode");
        let target = dir.join("geheim.json");
        atomic_write_json(&target, &json!({"token": "s3cr3t"})).unwrap();
        assert_eq!(
            mode(&target),
            0o600,
            "Ergebnis unabhängig von der umask nutzer-privat"
        );
    }

    #[test]
    fn write_private_temp_erzeugt_je_aufruf_einen_neuen_namen() {
        let dir = tmp("temp-namen");
        let a = write_private_temp(&dir, b"A").unwrap();
        let b = write_private_temp(&dir, b"B").unwrap();
        assert_ne!(a, b, "Temp-Namen sind nicht vorhersagbar/wiederverwendet");
        assert_eq!(a.parent().unwrap(), dir, "Temp-Datei liegt im Zielordner");
        assert_eq!(std::fs::read(&a).unwrap(), b"A");
        assert_eq!(std::fs::read(&b).unwrap(), b"B");
        #[cfg(unix)]
        assert_eq!(mode(&a), 0o600, "Temp-Datei direkt 0600");
    }

    #[cfg(unix)]
    #[test]
    fn write_private_folgt_keinem_symlink() {
        let dir = tmp("nofollow");
        let opfer = dir.join("opfer.json");
        std::fs::write(&opfer, "unberührt").unwrap();
        let link = dir.join("link.json");
        std::os::unix::fs::symlink(&opfer, &link).unwrap();

        assert!(
            write_private(&link, b"angriff").is_err(),
            "O_NOFOLLOW verhindert das Schreiben durch den Symlink"
        );
        assert_eq!(
            std::fs::read_to_string(&opfer).unwrap(),
            "unberührt",
            "Symlink-Ziel bleibt unangetastet"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_private_temp_uebernimmt_keinen_vorhandenen_symlink() {
        // Der Name ist zufällig; wir simulieren die Kollision, indem wir jeden
        // erzeugten Namen sofort wieder als Symlink belegen und prüfen, dass ein
        // belegter Name nie beschrieben wird.
        let dir = tmp("temp-kollision");
        let opfer = dir.join("opfer.json");
        std::fs::write(&opfer, "unberührt").unwrap();

        let erster = write_private_temp(&dir, b"X").unwrap();
        std::fs::remove_file(&erster).unwrap();
        std::os::unix::fs::symlink(&opfer, &erster).unwrap();

        // Erneutes Schreiben nimmt einen anderen Namen und lässt den Symlink liegen.
        let zweiter = write_private_temp(&dir, b"Y").unwrap();
        assert_ne!(zweiter, erster);
        assert_eq!(
            std::fs::read_to_string(&opfer).unwrap(),
            "unberührt",
            "Symlink-Ziel bleibt unangetastet"
        );
    }

    #[test]
    fn toggle_mcpjson_haelt_fremde_keys() {
        let dir = tmp("toggle");
        let settings = dir.join("settings.local.json");
        atomic_write_json(&settings, &json!({"fremd": "bleibt"})).unwrap();

        toggle_mcpjson(&settings, "github", false).unwrap();
        let v = read_json_value(&settings).unwrap();
        assert_eq!(v["fremd"], json!("bleibt"), "unbekannter Key bleibt erhalten");
        assert_eq!(v["disabledMcpjsonServers"], json!(["github"]));
        assert_eq!(v["enabledMcpjsonServers"], json!([]));

        toggle_mcpjson(&settings, "github", true).unwrap();
        let v = read_json_value(&settings).unwrap();
        assert_eq!(v["disabledMcpjsonServers"], json!([]));
        assert_eq!(v["enabledMcpjsonServers"], json!(["github"]));
    }

    #[test]
    fn mcpjson_state_liest_beide_arrays() {
        let dir = tmp("state");
        let settings = dir.join("settings.local.json");

        // Ohne Datei gibt es keinen expliziten Zustand.
        assert_eq!(mcpjson_state(&settings, "github"), None);

        toggle_mcpjson(&settings, "github", true).unwrap();
        toggle_mcpjson(&settings, "linear", false).unwrap();
        assert_eq!(mcpjson_state(&settings, "github"), Some(true));
        assert_eq!(mcpjson_state(&settings, "linear"), Some(false));
        // Unbekannter Name: kein Eintrag, nicht etwa `Some(true)`.
        assert_eq!(mcpjson_state(&settings, "unbekannt"), None);
    }

    #[test]
    fn mcpjson_state_disabled_gewinnt() {
        // Steht ein Name (durch Fremdbearbeitung) in BEIDEN Arrays, muss die
        // Auskunft mit `DisabledInfo::is_enabled` übereinstimmen: disabled gewinnt.
        let dir = tmp("state-konflikt");
        let settings = dir.join("settings.local.json");
        atomic_write_json(
            &settings,
            &json!({
                "enabledMcpjsonServers": ["github"],
                "disabledMcpjsonServers": ["github"],
            }),
        )
        .unwrap();
        assert_eq!(mcpjson_state(&settings, "github"), Some(false));
    }

    #[test]
    fn forget_mcpjson_streicht_aus_beiden_arrays() {
        let dir = tmp("forget");
        let settings = dir.join("settings.local.json");
        atomic_write_json(
            &settings,
            &json!({
                "enabledMcpjsonServers": ["github", "linear"],
                "disabledMcpjsonServers": ["github", "notion"],
                "fremderKey": {"bleibt": true},
            }),
        )
        .unwrap();

        forget_mcpjson(&settings, "github").unwrap();

        let v = read_json_value(&settings).unwrap();
        assert_eq!(v["enabledMcpjsonServers"], json!(["linear"]));
        assert_eq!(v["disabledMcpjsonServers"], json!(["notion"]));
        // Fremde Keys bleiben unangetastet.
        assert_eq!(v["fremderKey"]["bleibt"], json!(true));
        assert_eq!(mcpjson_state(&settings, "github"), None);
    }

    #[test]
    fn forget_mcpjson_ohne_treffer_schreibt_nicht() {
        let dir = tmp("forget-noop");
        let settings = dir.join("settings.local.json");

        // Fehlende Datei: no-op, es darf keine angelegt werden.
        forget_mcpjson(&settings, "github").unwrap();
        assert!(!settings.exists());

        toggle_mcpjson(&settings, "linear", true).unwrap();
        let before = std::fs::read_to_string(&settings).unwrap();
        forget_mcpjson(&settings, "github").unwrap();
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), before);
        assert!(leftover_temps(&dir).is_empty());
    }
}
