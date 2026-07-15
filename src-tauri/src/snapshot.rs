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
    crate::stash::config_dir().join("snapshots")
}

/// Setzt Unix-Rechte best-effort (no-op auf Nicht-Unix).
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}
#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

/// Schreibt Bytes in eine Datei, die unter Unix direkt mit Modus 0600 angelegt
/// wird (kein kurzes world-readable-Fenster – die Inhalte können Secrets
/// enthalten). Muster wie `stash::save`.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| AppError::Io(e.to_string()))?;
        f.write_all(bytes)
            .map_err(|e| AppError::Io(e.to_string()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).map_err(|e| AppError::Io(e.to_string()))?;
    }
    Ok(())
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
    let ts = crate::introspect::unix_now();
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
        let manifest_path = entry.path().join("manifest.json");
        match std::fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|t| serde_json::from_str::<SnapshotManifest>(&t).ok())
        {
            Some(m) => out.push(m),
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
pub fn restore(id: &str, only_paths: Option<Vec<String>>, retention: u32) -> Result<(), AppError> {
    restore_in(&snapshots_root(), id, only_paths, retention)
}

fn restore_in(
    root: &Path,
    id: &str,
    only_paths: Option<Vec<String>>,
    retention: u32,
) -> Result<(), AppError> {
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
            let tmp = parent.join(format!(".{fname}.mcpmgr-restore.tmp"));
            if let Err(e) = write_private(&tmp, &bytes) {
                cleanup(&to_rename, &created_dirs);
                return Err(e);
            }
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
