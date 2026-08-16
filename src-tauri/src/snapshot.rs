//! Snapshots der MCP-relevanten Claude-Konfiguration: sichern und
//! wiederherstellen.
//!
//! Gesichert werden die globalen Dateien (`~/.claude.json`,
//! `~/.claude/settings.json`, `~/.claude/settings.local.json`) sowie pro
//! bekanntem Projekt dessen `.mcp.json` und `.claude/settings.local.json`.
//! Da `~/.claude.json` Klartext-Secrets enthalten kann, liegt jedes
//! Snapshot-Verzeichnis unter $XDG_CONFIG_HOME/mcp-manager/snapshots/ mit
//! Modus 0700, die kopierten Dateien mit 0600.
//!
//! Zwei Auslöser: **manuell** (Nutzer sichert vor dem Aufräumen) und
//! **automatisch** als erster Schritt jeder destruktiven Aktion. Restore legt
//! selbst vorher einen Auto-Snapshot des Ist-Zustands an und ist damit
//! umkehrbar. Retention begrenzt nur die automatischen Snapshots.
//!
//! Die Kern-Logik arbeitet gegen einen injizierbaren Wurzelpfad
//! (`*_in`-Funktionen), damit Unit-Tests ohne Env-Mutation gegen ein Temp-Dir
//! laufen können.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config_read::{
    claude_json_path, project_settings_local_path, read_json_value, settings_local_path,
    settings_path,
};
use crate::models::AppError;

/// Manifest eines Snapshots (`manifest.json` im Snapshot-Verzeichnis).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    /// "<unix_ts>-<nanos>" – zugleich der Verzeichnisname.
    pub id: String,
    /// Erstellungszeit (Unix-Sekunden).
    pub created_at: u64,
    /// Notiz: manuell = Nutzertext, automatisch = z. B. "auto: remove_server github".
    pub note: Option<String>,
    /// Automatisch (vor destruktiver Aktion) vs. manuell.
    pub auto: bool,
    /// Gesicherte Dateien.
    pub files: Vec<SnapshotFile>,
    /// Manifest fehlte/war unlesbar (nur beim Auflisten gesetzt) – dann ist nur
    /// noch Löschen sinnvoll.
    #[serde(default)]
    pub corrupt: bool,
}

/// Eine gesicherte Datei innerhalb eines Snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFile {
    /// Absoluter Originalpfad (Restore-Ziel).
    pub original_path: String,
    /// Dateiname der Kopie innerhalb des Snapshot-Verzeichnisses.
    pub stored: String,
    /// Existierte die Datei beim Erstellen? `false` => Restore entfernt das Ziel.
    pub existed: bool,
    /// Größe in Bytes (0, wenn nicht existiert).
    pub size: u64,
    /// Fehlt beim Restore das Zielverzeichnis: `true` => neu anlegen (globale
    /// Config wie ~/.claude/settings.json), `false` => überspringen (Datei eines
    /// inzwischen gelöschten Projekts nicht wieder auferstehen lassen).
    #[serde(default)]
    pub create_parent: bool,
}

/// Wurzelverzeichnis aller Snapshots (nutzer-privat, neben Stash/Settings).
fn snapshots_root() -> PathBuf {
    crate::util::config_dir().join("snapshots")
}

/// Prüft, dass eine Snapshot-Id **genau eine gewöhnliche Pfadkomponente** ist
/// und dem erzeugten Format `{ts}-{nanos:09}` entspricht (nur Ziffern und `-`).
///
/// Die Id kommt vom Webview und wird zu `root.join(id)`; ohne diese Prüfung
/// würde `"../.."` `remove_dir_all`/Restore aus `snapshots/` herausführen.
/// Verschärfend wäre sonst, dass die Id früher aus dem *Inhalt* des Manifests
/// stammte – siehe [`list_in`], das sie jetzt aus dem Verzeichnisnamen setzt.
fn valid_snapshot_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 64 {
        return false;
    }
    if !id.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return false;
    }
    let mut comps = Path::new(id).components();
    matches!(comps.next(), Some(std::path::Component::Normal(_))) && comps.next().is_none()
}

/// Verwirft ungültige Ids mit klarer Meldung (gemeinsamer Eingang für
/// Restore und Löschen).
fn check_snapshot_id(id: &str) -> Result<(), AppError> {
    if valid_snapshot_id(id) {
        Ok(())
    } else {
        Err(AppError::Io(format!("Ungültige Snapshot-Id: {id}")))
    }
}

/// Setzt Unix-Rechte best-effort (no-op auf Nicht-Unix).
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}
#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

/// Schreibt Bytes in eine **neu** angelegte, private Datei (Unix: Modus 0600 –
/// kein world-readable-Fenster, die Inhalte können Secrets enthalten).
///
/// Dünne Hülle um [`crate::util::create_private_new`] (`create_new` +
/// `O_NOFOLLOW`) statt einer eigenen Kopie des Härtungs-Blocks. `create_new`
/// schadet hier nicht: das Snapshot-Verzeichnis wird direkt davor frisch
/// angelegt und die Ablagenamen sind index-eindeutig – ein bereits vorhandener
/// Zielname wäre ein Fremdeingriff und wird abgewiesen statt verfolgt.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    crate::util::create_private_new(path, bytes).map_err(|e| AppError::Io(e.to_string()))
}

/// Schreibt eine eindeutig benannte Temp-Datei neben das Restore-Ziel und gibt
/// deren Pfad zurück. Dünne Hülle um [`crate::util::write_private_temp`] mit
/// dem Restore-spezifischen Namens-Tag.
fn write_restore_temp(parent: &Path, fname: &str, bytes: &[u8]) -> Result<PathBuf, AppError> {
    crate::util::write_private_temp(parent, fname, "mcpmgr-restore", bytes)
}

/// Legt ein Verzeichnis an und schränkt es (Unix) auf 0700 ein.
fn create_private_dir(path: &Path) -> Result<(), AppError> {
    std::fs::create_dir_all(path).map_err(|e| AppError::Io(e.to_string()))?;
    set_mode(path, 0o700);
    Ok(())
}

/// Alle Quellpfade, die ein Snapshot sichert, je mit `create_parent`-Flag:
/// globale Dateien (Flag `true` – Zielverzeichnis beim Restore neu anlegen) +
/// pro bekanntem Projekt dessen `.mcp.json` und `.claude/settings.local.json`
/// (Flag `false` – gelöschte Projekte nicht wieder auferstehen lassen).
/// Doppelte Pfade (z. B. Home-Projekt == globale settings.local.json) werden
/// entfernt; das globale Flag `true` gewinnt dabei.
fn collect_source_paths() -> Vec<(PathBuf, bool)> {
    // stash.json gehört dazu: dort liegen die deaktivierten user-scope Server.
    let mut v: Vec<(PathBuf, bool)> = vec![
        (claude_json_path(), true),
        (settings_path(), true),
        (settings_local_path(), true),
        (crate::stash::stash_path(), true),
    ];
    if let Some(root) = read_json_value(&claude_json_path()) {
        if let Some(projects) = root.get("projects").and_then(|p| p.as_object()) {
            for path in projects.keys() {
                let p = PathBuf::from(path);
                v.push((project_settings_local_path(&p), false));
                v.push((p.join(".mcp.json"), false));
            }
        }
    }
    // Nach Pfad sortieren; bei Duplikaten den Eintrag mit create_parent=true
    // (globale Datei) bevorzugen.
    v.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    v.dedup_by(|a, b| a.0 == b.0);
    v
}

/// Erstellt einen Snapshot der aktuellen Konfiguration.
pub fn create(
    note: Option<String>,
    auto: bool,
    retention: u32,
) -> Result<SnapshotManifest, AppError> {
    create_in(
        &snapshots_root(),
        &collect_source_paths(),
        note,
        auto,
        retention,
    )
}

/// Kern von [`create`], gegen einen expliziten Wurzelpfad und eine explizite
/// Quellliste (Pfad + `create_parent`-Flag) – für Tests ohne Env-Mutation.
fn create_in(
    root: &Path,
    sources: &[(PathBuf, bool)],
    note: Option<String>,
    auto: bool,
    retention: u32,
) -> Result<SnapshotManifest, AppError> {
    let ts = crate::util::unix_now();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let id = format!("{ts}-{nanos:09}");

    let snap_dir = root.join(&id);
    create_private_dir(root)?;
    create_private_dir(&snap_dir)?;

    let mut files = Vec::new();
    for (i, (src, create_parent)) in sources.iter().enumerate() {
        let existed = src.is_file();
        // Eindeutiger Ablagename (Index-Präfix verhindert Kollisionen gleicher
        // Basenamen aus verschiedenen Projekten, z. B. mehrere `.mcp.json`).
        let base = src.file_name().and_then(|f| f.to_str()).unwrap_or("datei");
        let stored = format!("{i:03}-{base}");
        let mut size = 0u64;
        if existed {
            let bytes = std::fs::read(src).map_err(|e| AppError::Io(e.to_string()))?;
            size = bytes.len() as u64;
            write_private(&snap_dir.join(&stored), &bytes)?;
        }
        files.push(SnapshotFile {
            original_path: src.to_string_lossy().to_string(),
            stored,
            existed,
            size,
            create_parent: *create_parent,
        });
    }

    let manifest = SnapshotManifest {
        id,
        created_at: ts,
        note,
        auto,
        files,
        corrupt: false,
    };
    let text =
        serde_json::to_string_pretty(&manifest).map_err(|e| AppError::Parse(e.to_string()))?;
    write_private(&snap_dir.join("manifest.json"), text.as_bytes())?;

    enforce_retention(root, retention);
    Ok(manifest)
}

/// Listet alle Snapshots, neueste zuerst.
pub fn list() -> Result<Vec<SnapshotManifest>, AppError> {
    list_in(&snapshots_root())
}

fn list_in(root: &Path) -> Result<Vec<SnapshotManifest>, AppError> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        // Noch kein Snapshot angelegt: leere Liste, kein Fehler.
        Err(_) => return Ok(out),
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        // Nur Verzeichnisse listen, deren Name eine gültige Snapshot-Id ist.
        // Sonst böte die Snapshot-Ansicht ein „(beschädigtes)" Verzeichnis an,
        // dessen Löschen `delete_in` -> `check_snapshot_id` dauerhaft mit
        // „Ungültige Snapshot-Id" abweist – der einzige vorgesehene Umgang wäre
        // blockiert und `enforce_retention` käme nie an den Ordner heran.
        if !valid_snapshot_id(&id) {
            continue;
        }
        let manifest_path = entry.path().join("manifest.json");
        match std::fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|t| serde_json::from_str::<SnapshotManifest>(&t).ok())
        {
            // Id IMMER aus dem Verzeichnisnamen – niemals aus dem Manifest-Inhalt:
            // sonst bestimmt eine (kopierte oder manipulierte) manifest.json, worauf
            // sich „Löschen"/„Wiederherstellen" später beziehen.
            Some(mut m) => {
                m.id = id.clone();
                out.push(m);
            }
            // Fehlendes/kaputtes Manifest: als beschädigt listen (nur löschbar).
            None => out.push(SnapshotManifest {
                id: id.clone(),
                created_at: 0,
                note: Some("(beschädigt)".into()),
                auto: false,
                files: Vec::new(),
                corrupt: true,
            }),
        }
    }
    out.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(out)
}

/// Stellt einen Snapshot wieder her. Legt vorher selbst einen Auto-Snapshot des
/// Ist-Zustands an ("auto: vor Restore"), damit der Restore umkehrbar ist.
/// `only_paths` (Originalpfade) beschränkt auf einen Teil der Dateien.
///
/// **Dokumentierte Ausnahme von der Leitplanke „Claude-Code-Konfiguration nur
/// über die `claude`-CLI".** Ein Restore schreibt `~/.claude.json` und die
/// settings-/`.mcp.json`-Dateien direkt (Temp-Datei + `rename`), weil die CLI
/// kein Äquivalent zum Zurückspielen eines Gesamtzustands anbietet. Abgesichert
/// wird das nicht technisch, sondern durch das Verfahren: die UI warnt, dass
/// Claude Code dabei nicht laufen soll, und legt vorher den Auto-Snapshot an
/// (analog zur ebenfalls dokumentierten Ausnahme in `delete_project`).
pub fn restore(id: &str, only_paths: Option<Vec<String>>, retention: u32) -> Result<(), AppError> {
    restore_in(&snapshots_root(), id, only_paths, retention)
}

/// Kern des Restores (testbar mit eigenem `root`). Schreibt die Zieldateien
/// direkt – siehe die dokumentierte Ausnahme im Doc-Comment von `restore`.
fn restore_in(
    root: &Path,
    id: &str,
    only_paths: Option<Vec<String>>,
    retention: u32,
) -> Result<(), AppError> {
    check_snapshot_id(id)?;
    let snap_dir = root.join(id);
    let manifest = read_manifest(&snap_dir)?;

    // Zwei-Phasen-Restore: erst ALLE Ziele vorbereiten (Temp-Dateien schreiben
    // bzw. zu löschende Ziele sammeln), dann committen (rename/remove).
    //
    // WICHTIG – Reihenfolge: Der Ziel-Snapshot wird in dieser Phase VOLLSTÄNDIG
    // ausgelesen, BEVOR der "vor Restore"-Snapshot angelegt wird. Andernfalls
    // könnte dessen Retention (enforce_retention) genau den gerade
    // wiederherzustellenden (alten Auto-)Snapshot evicten, bevor wir seine
    // Dateien gelesen haben – der Restore würde fehlschlagen und das Ziel wäre
    // verloren.
    let mut to_rename: Vec<(PathBuf, PathBuf)> = Vec::new(); // (tmp, target)
    let mut to_remove: Vec<PathBuf> = Vec::new();
    // Beim create_parent-Restore neu angelegte Verzeichnisse, um sie bei einem
    // Abbruch der Vorbereitungsphase wieder zu entfernen (Ausgangszustand).
    let mut created_dirs: Vec<PathBuf> = Vec::new();

    let cleanup = |temps: &[(PathBuf, PathBuf)], dirs: &[PathBuf]| {
        for (tmp, _) in temps {
            let _ = std::fs::remove_file(tmp);
        }
        // Zuletzt Angelegte zuerst entfernen; remove_dir löscht nur leere Dirs,
        // reißt also nichts Vorhandenes mit.
        for d in dirs.iter().rev() {
            let _ = std::fs::remove_dir(d);
        }
    };

    for file in &manifest.files {
        if let Some(only) = &only_paths {
            if !only.iter().any(|p| p == &file.original_path) {
                continue;
            }
        }
        let target = PathBuf::from(&file.original_path);
        let Some(parent) = target.parent() else {
            continue;
        };

        if file.existed {
            // Fehlendes Zielverzeichnis: für globale Config neu anlegen, für
            // Projektdateien (gelöschtes Projekt) überspringen.
            if !parent.is_dir() {
                if file.create_parent {
                    if let Err(e) = create_private_dir(parent) {
                        cleanup(&to_rename, &created_dirs);
                        return Err(e);
                    }
                    created_dirs.push(parent.to_path_buf());
                } else {
                    continue;
                }
            }
            let bytes = match std::fs::read(snap_dir.join(&file.stored)) {
                Ok(b) => b,
                Err(e) => {
                    cleanup(&to_rename, &created_dirs);
                    return Err(AppError::Io(e.to_string()));
                }
            };
            let fname = target
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("datei");
            let tmp = match write_restore_temp(parent, fname, &bytes) {
                Ok(p) => p,
                Err(e) => {
                    cleanup(&to_rename, &created_dirs);
                    return Err(e);
                }
            };
            to_rename.push((tmp, target));
        } else if target.exists() && parent.is_dir() {
            // Existierte beim Snapshot nicht -> beim Restore entfernen.
            to_remove.push(target);
        }
    }

    // Jetzt – nachdem der Ziel-Snapshot komplett gelesen ist – den Ist-Zustand
    // sichern (macht den Restore umkehrbar). Schlägt das fehl, wird nichts
    // committet und die Vorbereitungen werden zurückgerollt.
    let current_sources: Vec<(PathBuf, bool)> = manifest
        .files
        .iter()
        .map(|f| (PathBuf::from(&f.original_path), f.create_parent))
        .collect();
    if let Err(e) = create_in(
        root,
        &current_sources,
        Some("auto: vor Restore".into()),
        true,
        retention,
    ) {
        cleanup(&to_rename, &created_dirs);
        return Err(e);
    }

    // Commit-Phase: nur noch atomare Renames und Löschungen (praktisch nicht
    // fehlschlagend, da Temp-Dateien bereits auf demselben Dateisystem liegen).
    for (tmp, target) in &to_rename {
        std::fs::rename(tmp, target).map_err(|e| AppError::Io(e.to_string()))?;
    }
    for target in &to_remove {
        std::fs::remove_file(target).map_err(|e| AppError::Io(e.to_string()))?;
    }
    Ok(())
}

/// Löscht einen Snapshot samt Verzeichnis.
pub fn delete(id: &str) -> Result<(), AppError> {
    delete_in(&snapshots_root(), id)
}

fn delete_in(root: &Path, id: &str) -> Result<(), AppError> {
    check_snapshot_id(id)?;
    let dir = root.join(id);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| AppError::Io(e.to_string()))?;
    }
    Ok(())
}

fn read_manifest(snap_dir: &Path) -> Result<SnapshotManifest, AppError> {
    let text = std::fs::read_to_string(snap_dir.join("manifest.json"))
        .map_err(|e| AppError::Io(format!("Snapshot nicht lesbar: {e}")))?;
    serde_json::from_str(&text).map_err(|e| AppError::Parse(e.to_string()))
}

/// Begrenzt die **automatischen** Snapshots auf die jüngsten `retention`.
/// Manuelle Snapshots bleiben unangetastet. Best-effort (Fehler beim Löschen
/// brechen die auslösende Aktion nicht ab).
fn enforce_retention(root: &Path, retention: u32) {
    // Mindestens 1 behalten: sonst würde ein retention==0 (hand-editierte
    // settings.json, an validate() vorbei) den soeben für die destruktive Aktion
    // angelegten Auto-Snapshot sofort wieder löschen – die Aktion liefe dann
    // ungesichert. Der neueste Auto-Snapshot muss seine eigene Retention-Runde
    // stets überleben.
    let keep = retention.max(1) as usize;
    let Ok(all) = list_in(root) else { return };
    let autos: Vec<&SnapshotManifest> = all.iter().filter(|m| m.auto).collect();
    // `list_in` ist bereits nach created_at absteigend sortiert -> die ersten
    // `keep` behalten, den Rest löschen.
    for m in autos.into_iter().skip(keep) {
        let _ = delete_in(root, &m.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Legt eine Datei mit Inhalt an (inkl. Elternverzeichnisse).
    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Eindeutiges Temp-Verzeichnis pro Test (kein Date/rand nötig).
    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mcpmgr-snap-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_list_restore_roundtrip() {
        let base = tmp("roundtrip");
        let root = base.join("snapshots");
        let a = base.join("a.json");
        let b = base.join("proj/.mcp.json");
        write(&a, r#"{"v":1}"#);
        write(&b, r#"{"mcpServers":{}}"#);
        let sources = vec![(a.clone(), true), (b.clone(), false)];

        let m = create_in(&root, &sources, Some("manuell".into()), false, 20).unwrap();
        assert_eq!(m.files.len(), 2);
        assert!(m.files.iter().all(|f| f.existed));

        // Datei nach dem Snapshot verändern.
        std::fs::write(&a, r#"{"v":999}"#).unwrap();

        let listed = list_in(&root).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, m.id);

        restore_in(&root, &m.id, None, 20).unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), r#"{"v":1}"#);
        // Restore hat selbst einen Auto-Snapshot ("vor Restore") angelegt.
        let after = list_in(&root).unwrap();
        assert_eq!(after.len(), 2);
        assert!(after
            .iter()
            .any(|s| s.auto && s.note.as_deref() == Some("auto: vor Restore")));
    }

    #[test]
    fn partial_restore_only_touches_selected_paths() {
        let base = tmp("partial");
        let root = base.join("snapshots");
        let a = base.join("a.json");
        let b = base.join("b.json");
        write(&a, "A0");
        write(&b, "B0");
        let m = create_in(
            &root,
            &[(a.clone(), true), (b.clone(), true)],
            None,
            false,
            20,
        )
        .unwrap();

        std::fs::write(&a, "A1").unwrap();
        std::fs::write(&b, "B1").unwrap();

        restore_in(
            &root,
            &m.id,
            Some(vec![a.to_string_lossy().to_string()]),
            20,
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "A0"); // wiederhergestellt
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "B1"); // unberührt
    }

    #[test]
    fn restore_removes_files_that_did_not_exist() {
        let base = tmp("existed-false");
        let root = base.join("snapshots");
        let missing = base.join("later.json");
        // Snapshot, während die Datei noch nicht existiert.
        let m = create_in(&root, &[(missing.clone(), true)], None, false, 20).unwrap();
        assert!(!m.files[0].existed);

        // Datei taucht später auf; Restore muss sie wieder entfernen.
        write(&missing, "neu");
        restore_in(&root, &m.id, None, 20).unwrap();
        assert!(!missing.exists());
    }

    #[test]
    fn retention_limits_only_auto_snapshots() {
        let base = tmp("retention");
        let root = base.join("snapshots");
        let a = base.join("a.json");
        write(&a, "x");
        let sources = vec![(a.clone(), true)];

        // Ein manueller Snapshot bleibt immer erhalten.
        create_in(&root, &sources, Some("manuell".into()), false, 3).unwrap();
        // Mehr Auto-Snapshots als die Retention (3) erlaubt.
        for i in 0..5 {
            create_in(&root, &sources, Some(format!("auto {i}")), true, 3).unwrap();
        }

        let listed = list_in(&root).unwrap();
        let autos = listed.iter().filter(|m| m.auto).count();
        let manuals = listed.iter().filter(|m| !m.auto).count();
        assert_eq!(autos, 3, "Auto-Snapshots auf Retention begrenzt");
        assert_eq!(manuals, 1, "manuelle Snapshots bleiben erhalten");
    }

    #[test]
    fn retention_zero_keeps_the_just_created_snapshot() {
        // Regression: retention==0 (an validate() vorbei hand-editiert) darf den
        // gerade angelegten Auto-Snapshot NICHT sofort löschen.
        let base = tmp("retention-zero");
        let root = base.join("snapshots");
        let a = base.join("a.json");
        write(&a, "x");
        let m = create_in(&root, &[(a.clone(), true)], Some("auto".into()), true, 0).unwrap();
        let listed = list_in(&root).unwrap();
        assert!(
            listed.iter().any(|s| s.id == m.id),
            "der soeben erzeugte Auto-Snapshot muss erhalten bleiben"
        );
    }

    #[test]
    fn restore_recreates_missing_global_parent_but_skips_project_dir() {
        let base = tmp("create-parent");
        let root = base.join("snapshots");
        // „Global" (create_parent=true) in einem Unterordner, „Projekt"
        // (create_parent=false) in einem anderen.
        let global = base.join("cfgdir/settings.json");
        let project = base.join("projdir/.mcp.json");
        write(&global, "G0");
        write(&project, "P0");
        let m = create_in(
            &root,
            &[(global.clone(), true), (project.clone(), false)],
            None,
            false,
            20,
        )
        .unwrap();

        // Beide Zielverzeichnisse nach dem Snapshot entfernen.
        std::fs::remove_dir_all(base.join("cfgdir")).unwrap();
        std::fs::remove_dir_all(base.join("projdir")).unwrap();

        restore_in(&root, &m.id, None, 20).unwrap();
        assert_eq!(
            std::fs::read_to_string(&global).unwrap(),
            "G0",
            "globales Verzeichnis wird neu angelegt und die Datei wiederhergestellt"
        );
        assert!(
            !project.exists(),
            "gelöschtes Projektverzeichnis wird NICHT wieder auferstehen"
        );
    }

    #[test]
    fn restore_is_atomic_on_missing_stored_file() {
        // Fehlt eine Snapshot-Kopie, darf KEIN Ziel halb überschrieben werden.
        let base = tmp("atomic");
        let root = base.join("snapshots");
        let a = base.join("a.json");
        let b = base.join("b.json");
        write(&a, "A0");
        write(&b, "B0");
        let m = create_in(
            &root,
            &[(a.clone(), true), (b.clone(), true)],
            None,
            false,
            20,
        )
        .unwrap();

        // Aktuellen Stand verändern und eine Snapshot-Kopie sabotieren.
        std::fs::write(&a, "A1").unwrap();
        std::fs::write(&b, "B1").unwrap();
        let stored_b = &m
            .files
            .iter()
            .find(|f| f.original_path == b.to_string_lossy())
            .unwrap()
            .stored;
        std::fs::remove_file(root.join(&m.id).join(stored_b)).unwrap();

        let err = restore_in(&root, &m.id, None, 20);
        assert!(err.is_err(), "Restore muss abbrechen");
        // Weder a noch b dürfen aus dem Snapshot überschrieben worden sein.
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            "A1",
            "a bleibt unverändert (kein Teil-Restore)"
        );
        assert_eq!(
            std::fs::read_to_string(&b).unwrap(),
            "B1",
            "b bleibt unverändert"
        );
        // Keine Temp-Dateien zurückgelassen.
        assert!(
            !base.join(".a.json.mcpmgr-restore.tmp").exists(),
            "Temp-Datei aufgeräumt"
        );
    }

    #[test]
    fn restore_survives_retention_eviction_of_target() {
        // Regression: Wird ein alter Auto-Snapshot am Retention-Limit
        // wiederhergestellt, darf der 'vor Restore'-Snapshot den Ziel-Snapshot
        // nicht evicten, bevor er ausgelesen wurde.
        let base = tmp("restore-evict");
        let root = base.join("snapshots");
        let a = base.join("a.json");
        write(&a, "V0");
        // retention=1: der Ziel-Snapshot ist der einzige Auto-Snapshot.
        let target =
            create_in(&root, &[(a.clone(), true)], Some("auto A".into()), true, 1).unwrap();

        std::fs::write(&a, "V1").unwrap();

        // Der 'vor Restore'-Snapshot brächte die Zahl auf 2 -> Retention(1) würde
        // den ältesten (= Ziel) löschen. Muss trotzdem gelingen.
        restore_in(&root, &target.id, None, 1).unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "V0");
    }

    #[test]
    fn restore_abort_removes_freshly_created_dir() {
        // Bricht der Restore ab, nachdem für eine globale Datei ein fehlendes
        // Zielverzeichnis neu angelegt wurde, muss dieses wieder verschwinden.
        let base = tmp("abort-dir");
        let root = base.join("snapshots");
        let g = base.join("cfgdir/settings.json"); // global, create_parent=true
        let o = base.join("other.json");
        write(&g, "G0");
        write(&o, "O0");
        let m = create_in(
            &root,
            &[(g.clone(), true), (o.clone(), true)],
            None,
            false,
            20,
        )
        .unwrap();

        // cfgdir entfernen (Verzeichnis fehlt -> Restore legt es neu an) und die
        // zweite Snapshot-Kopie sabotieren -> Abbruch NACH der Dir-Anlage.
        std::fs::remove_dir_all(base.join("cfgdir")).unwrap();
        let stored_o = &m
            .files
            .iter()
            .find(|f| f.original_path == o.to_string_lossy())
            .unwrap()
            .stored;
        std::fs::remove_file(root.join(&m.id).join(stored_o)).unwrap();

        assert!(
            restore_in(&root, &m.id, None, 20).is_err(),
            "Restore muss abbrechen"
        );
        assert!(
            !base.join("cfgdir").exists(),
            "neu angelegtes Verzeichnis bei Abbruch wieder entfernt"
        );
    }

    #[test]
    fn corrupt_snapshot_is_listed_not_fatal() {
        let base = tmp("corrupt");
        let root = base.join("snapshots");
        // Verzeichnis ohne (bzw. mit kaputtem) Manifest.
        let bad = root.join("1234-000000000");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("manifest.json"), "{ kaputt").unwrap();

        let listed = list_in(&root).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].corrupt);
    }

    #[test]
    fn snapshot_ids_are_validated() {
        assert!(valid_snapshot_id("1700000000-000000001"));
        assert!(valid_snapshot_id("1234-000000000"));
        // Pfad-Traversal und alles, was mehr als eine Komponente ist.
        assert!(!valid_snapshot_id(".."));
        assert!(!valid_snapshot_id("../../.."));
        assert!(!valid_snapshot_id("/etc"));
        assert!(!valid_snapshot_id("a/b"));
        assert!(!valid_snapshot_id("."));
        assert!(!valid_snapshot_id(""));
        // Zeichensatz: nur Ziffern und '-'.
        assert!(!valid_snapshot_id("1234-abc"));
        assert!(!valid_snapshot_id("1234 000"));
        assert!(!valid_snapshot_id(&"1".repeat(65)));
    }

    /// Eine Id mit `..` darf niemals außerhalb von `snapshots/` wirken.
    #[test]
    fn delete_and_restore_reject_traversing_ids() {
        let base = tmp("traversal");
        let root = base.join("snapshots");
        let opfer = base.join("opfer");
        std::fs::create_dir_all(&opfer).unwrap();
        write(&opfer.join("wichtig.txt"), "bitte nicht löschen");

        assert!(delete_in(&root, "../opfer").is_err());
        assert!(delete_in(&root, "..").is_err());
        assert!(restore_in(&root, "../opfer", None, 20).is_err());
        assert!(
            opfer.join("wichtig.txt").is_file(),
            "Verzeichnis außerhalb von snapshots/ muss unangetastet bleiben"
        );
    }

    /// Die Id beim Auflisten kommt aus dem VERZEICHNISNAMEN, nicht aus dem
    /// Manifest-Inhalt – sonst bestimmt eine kopierte/manipulierte
    /// manifest.json, worauf sich Löschen/Wiederherstellen bezieht.
    #[test]
    fn list_takes_id_from_directory_not_manifest() {
        let base = tmp("id-source");
        let root = base.join("snapshots");
        let dir = root.join("1700000000-000000042");
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = SnapshotManifest {
            id: "../../..".into(),
            created_at: 1_700_000_000,
            note: None,
            auto: false,
            files: Vec::new(),
            corrupt: false,
        };
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();

        let listed = list_in(&root).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "1700000000-000000042");
    }

    /// Die Restore-Temp-Datei wird exklusiv neu angelegt: ein vorab platzierter
    /// Symlink darf weder verfolgt noch überschrieben werden.
    #[cfg(unix)]
    #[test]
    fn restore_temp_does_not_follow_symlinks() {
        let base = tmp("tmp-symlink");
        let ziel = base.join("fremd.txt");
        write(&ziel, "unberuehrt");

        // Vorhersehbaren alten Namen als Symlink platzieren – wäre der Name noch
        // fest, würde der Restore hier hineinschreiben.
        let alt = base.join(".mcp.json.mcpmgr-restore.tmp");
        std::os::unix::fs::symlink(&ziel, &alt).unwrap();

        let tmp_path = write_restore_temp(&base, ".mcp.json", b"geheim").unwrap();
        assert_ne!(tmp_path, alt);
        assert_eq!(std::fs::read_to_string(&ziel).unwrap(), "unberuehrt");
        assert_eq!(mode(&tmp_path), 0o600);

        // Und direkt auf einen existierenden Symlink schreiben schlägt fehl.
        let err = crate::util::create_private_new(&alt, b"x").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&ziel).unwrap(), "unberuehrt");
    }

    /// Regression P3-12: Ein beschädigter Snapshot (Manifest fehlt/kaputt) muss
    /// gelistet UND löschbar sein; ein Verzeichnis mit fremdem Namen darf gar
    /// nicht erst angeboten werden – sein Löschen scheiterte sonst dauerhaft an
    /// `check_snapshot_id`.
    #[test]
    fn beschaedigte_snapshots_bleiben_loeschbar() {
        let base = tmp("corrupt-delete");
        let root = base.join("snapshots");
        let kaputt = root.join("1700000000-000000001");
        std::fs::create_dir_all(&kaputt).unwrap();
        write(&kaputt.join("home.claude.json"), "{}"); // Manifest fehlt
        let fremd = root.join("nicht-meine_ablage");
        std::fs::create_dir_all(&fremd).unwrap();

        let listed = list_in(&root).unwrap();
        assert_eq!(listed.len(), 1, "nur der Snapshot mit gültiger Id: {listed:?}");
        assert!(listed[0].corrupt);
        assert_eq!(listed[0].id, "1700000000-000000001");

        delete_in(&root, &listed[0].id).expect("beschädigter Snapshot muss löschbar sein");
        assert!(!kaputt.exists());
        // Das fremde Verzeichnis bleibt unangetastet (nie gelistet, nie gelöscht).
        assert!(fremd.exists());
    }

    /// Opt-in-Smoketest gegen die echte Umgebung dieser Maschine: erstellt einen
    /// manuellen Snapshot (liest nur die Claude-Config, ändert sie nicht),
    /// prüft, dass er gelistet wird, und räumt ihn wieder auf. Nur mit
    /// `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_env_create_list_delete() {
        let m = create(Some("mcpmgr-selftest".into()), false, 20).expect("create");
        eprintln!("Snapshot {} mit {} Dateien angelegt", m.id, m.files.len());
        assert!(
            !m.files.is_empty(),
            "es sollten Quellpfade gesammelt werden"
        );

        let listed = list().expect("list");
        assert!(listed.iter().any(|s| s.id == m.id), "Snapshot ist gelistet");

        delete(&m.id).expect("delete");
        let after = list().expect("list2");
        assert!(
            !after.iter().any(|s| s.id == m.id),
            "Snapshot wieder entfernt"
        );
        eprintln!("real_env Roundtrip OK");
    }

    #[cfg(unix)]
    #[test]
    fn permissions_are_restrictive() {
        let base = tmp("perms");
        let root = base.join("snapshots");
        let a = base.join("secret.json");
        write(&a, r#"{"token":"geheim"}"#);
        let m = create_in(&root, &[(a.clone(), true)], None, false, 20).unwrap();

        assert_eq!(mode(&root.join(&m.id)), 0o700, "Snapshot-Dir 0700");
        assert_eq!(
            mode(&root.join(&m.id).join(&m.files[0].stored)),
            0o600,
            "Kopie 0600"
        );
    }
}
