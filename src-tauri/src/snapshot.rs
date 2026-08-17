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
use crate::toggles::{write_private, write_private_temp};

/// Manifest eines Snapshots (`manifest.json` im Snapshot-Verzeichnis).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    /// "<unix_ts>-<nanos>" – zugleich der Verzeichnisname. Beim Auflisten wird
    /// sie aus dem Verzeichnisnamen gesetzt (nicht aus dem Manifest-Inhalt) und
    /// vor jeder Pfad-Bildung validiert (siehe `valid_id`).
    pub id: String,
    /// Erstellungszeit (Unix-Sekunden).
    pub created_at: u64,
    /// Notiz: manuell = Nutzertext, automatisch = z. B. "auto: remove_server github".
    pub note: Option<String>,
    /// Automatisch (vor destruktiver Aktion) vs. manuell.
    pub auto: bool,
    /// Gesicherte Dateien.
    pub files: Vec<SnapshotFile>,
    /// Wurde beim Erstellen für **jede** Datei erhoben, ob ihr Pfad selbst ein
    /// Symlink war? Manifeste aus der Zeit vor [`SnapshotFile::link_target`]
    /// kennen das Feld nicht (`serde(default)` ⇒ `false`); dort bedeutet
    /// `link_target: None` „nicht aufgezeichnet“ und gerade **nicht** „war
    /// nachweislich kein Symlink“. Nur mit dieser Unterscheidung kann
    /// [`restore_write_target`] einen untergeschobenen Link erkennen, ohne jeden
    /// älteren Snapshot unbrauchbar zu machen.
    #[serde(default)]
    pub link_targets_recorded: bool,
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
    /// Kanonisches Ziel, falls `original_path` beim Sichern **selbst** ein
    /// Symlink war (Dotfiles-Setup). Nur dann darf der Restore demselben Link
    /// wieder folgen — siehe [`restore_write_target`]. `None` heißt allerdings
    /// nur dann nachweislich „war kein Symlink“, wenn das Manifest
    /// [`SnapshotManifest::link_targets_recorded`] gesetzt hat; sonst ist der
    /// Zustand von damals schlicht unbekannt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
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

/// Obergrenze für die Länge einer Snapshot-Id (das erzeugte Format
/// `<unix_ts>-<nanos:09>` ist rund 20 Zeichen lang).
const MAX_ID_LEN: usize = 64;

/// Prüft, ob `id` eine unbedenkliche Snapshot-Id ist.
///
/// Die Id landet in `root.join(id)` und steuert damit `remove_dir_all` bzw. den
/// Restore – ein `..` darin würde aus dem Löschen eines Snapshots das Löschen
/// eines beliebigen Verzeichnisses machen. Erlaubt ist deshalb ausschließlich
/// das selbst erzeugte Format: nicht leer, höchstens [`MAX_ID_LEN`] Zeichen,
/// nur ASCII-Ziffern und Bindestrich – und in der Pfad-Zerlegung genau eine
/// normale Komponente. Damit sind `/`, `..`, `.` und absolute Pfade
/// ausgeschlossen.
fn valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return false;
    }
    if !id.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return false;
    }
    let mut comps = Path::new(id).components();
    let einzelne_komponente = matches!(
        comps.next(),
        Some(std::path::Component::Normal(c)) if c.to_str() == Some(id)
    );
    einzelne_komponente && comps.next().is_none()
}

/// Prüft die Id und liefert das zugehörige Snapshot-Verzeichnis. Einziger Weg,
/// aus einer von außen kommenden Id einen Pfad zu machen.
fn snapshot_dir(root: &Path, id: &str) -> Result<PathBuf, AppError> {
    if !valid_id(id) {
        return Err(AppError::Io(format!("ungültige Snapshot-Id: {id}")));
    }
    Ok(root.join(id))
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
/// (Flag `false` – gelöschte Projekte nicht wieder auferstehen lassen) +
/// die Konfiguration jedes **erkannten** Datei-Clients (ebenfalls Flag `false`,
/// siehe `client_source_paths`).
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
    // Konfiguration erkannter Datei-Clients (Feature 16).
    v.extend(client_source_paths());
    // Nach Pfad sortieren; bei Duplikaten den Eintrag mit create_parent=true
    // (globale Datei) bevorzugen.
    v.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    v.dedup_by(|a, b| a.0 == b.0);
    v
}

/// Config-Dateien aller **erkannten** Datei-Clients (Feature 16), je mit
/// `create_parent = false`: auf einem Rechner ohne den jeweiligen Client darf ein
/// Restore dessen Verzeichnis nicht wieder auferstehen lassen.
fn client_source_paths() -> Vec<(PathBuf, bool)> {
    crate::clients::adapters()
        .into_iter()
        .filter_map(|a| a.detect().map(|p| (p, false)))
        .collect()
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
        // Ist der Quellpfad selbst ein Symlink, gesichert wird der Inhalt
        // DAHINTER (`std::fs::read` folgt dem Link). Genau dieses Ziel wird
        // festgehalten, damit der Restore weiß, wohin er zurückschreiben darf.
        let link_target = if existed {
            canonical_link_target(src)
        } else {
            None
        };
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
            link_target,
        });
    }

    let manifest = SnapshotManifest {
        id,
        created_at: ts,
        note,
        auto,
        files,
        // Ab hier wird `link_target` für jede Datei erhoben – dieses Manifest
        // darf also als Beweis gelesen werden (siehe `restore_write_target`).
        link_targets_recorded: true,
        corrupt: false,
    };
    let text =
        serde_json::to_string_pretty(&manifest).map_err(|e| AppError::Parse(e.to_string()))?;
    write_private(&snap_dir.join("manifest.json"), text.as_bytes())?;

    enforce_retention(root, retention);
    Ok(manifest)
}

/// Kanonisches Ziel von `src`, **falls** `src` selbst ein Symlink ist; sonst
/// `None`. Ein toter oder nicht auflösbarer Link liefert ebenfalls `None` — dann
/// gilt er als „nicht über einen Link gesichert“ und der Restore folgt ihm nicht.
fn canonical_link_target(src: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(src).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    std::fs::canonicalize(src)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

/// Der einzige Ausweg aus einem Symlink-bedingten Abbruch – gehört an jede
/// dieser Meldungen, sonst steht der Nutzer vor einer Sackgasse.
const ABWAEHLEN: &str = "Diese Datei in der Auswahl abwählen, um die übrigen wiederherzustellen.";

/// Wohin ein Restore die gesicherten Bytes schreiben darf. `Ok(None)` bedeutet
/// „diese eine Datei überspringen“ (siehe unten), nicht „Restore abbrechen“.
///
/// Grundregel: einem Symlink wird **nur** gefolgt, wenn schon der Snapshot über
/// genau diesen Link gesichert hat. Andernfalls schriebe der Restore rohe Bytes
/// in eine Datei, die er nie gelesen hat (z. B. ein untergeschobener Link auf
/// `~/.ssh/authorized_keys`); die stumme Alternative — den Link durch eine
/// reguläre Datei zu ersetzen — hängte ein Dotfiles-Repo ab. Beides ist es wert,
/// den Restore stattdessen mit einer klaren Meldung abzubrechen.
///
/// Ob „kein `link_target` aufgezeichnet“ tatsächlich „war kein Symlink“ heißt,
/// weiß allein das Manifest ([`SnapshotManifest::link_targets_recorded`]):
/// * **Neues Manifest** (Feld erhoben): `None` ist ein Beweis. Liegt heute
///   trotzdem ein Link am Pfad, ist er nachträglich entstanden — genau der
///   Angriffsfall oben, also harter Abbruch des gesamten Restores.
/// * **Altmanifest** (Feld nie geschrieben): `None` ist keine Aussage. Ein
///   Abbruch machte hier jeden vor diesem Feld angelegten Snapshot unbrauchbar,
///   sobald der Nutzer sein Dotfiles-Setup einrichtet — und zwar komplett, weil
///   der Fehler aus der Vorbereitungsphase kommt. Deshalb wird nur **diese eine
///   Datei** ausgelassen und gemeldet; alle übrigen werden normal
///   wiederhergestellt. Blind dem Link zu folgen scheidet aus: dann schriebe
///   der Restore doch wieder in eine nie gelesene Datei.
fn restore_write_target(
    manifest: &SnapshotManifest,
    file: &SnapshotFile,
) -> Result<Option<PathBuf>, AppError> {
    let original = PathBuf::from(&file.original_path);
    let is_link = std::fs::symlink_metadata(&original)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if !is_link {
        // Am sichtbaren Ort liegt kein Link (oder gar nichts) ⇒ dorthin
        // zurückschreiben. Auch wenn beim Sichern ein Link im Spiel war: was
        // der Nutzer heute sieht, ist eine reguläre Datei.
        return Ok(Some(original));
    }
    let Some(recorded) = &file.link_target else {
        if !manifest.link_targets_recorded {
            return Ok(None);
        }
        return Err(AppError::Io(format!(
            "{} ist heute ein Symlink, war es beim Sichern aber nicht – \
             Restore abgebrochen, um weder den Link noch sein Ziel zu überschreiben. \
             {ABWAEHLEN}",
            file.original_path
        )));
    };
    let current = std::fs::canonicalize(&original).map_err(|e| {
        AppError::Io(format!(
            "{} ist ein Symlink, dessen Ziel nicht auflösbar ist ({e}) – \
             Restore abgebrochen. {ABWAEHLEN}",
            file.original_path
        ))
    })?;
    if current.as_path() != Path::new(recorded) {
        return Err(AppError::Io(format!(
            "{} zeigt inzwischen auf ein anderes Ziel als beim Sichern – \
             Restore abgebrochen. {ABWAEHLEN}",
            file.original_path
        )));
    }
    Ok(Some(current))
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
        // `file_type()` stammt aus dem Verzeichniseintrag und folgt keinen
        // Symlinks – ein untergeschobener Symlink taucht gar nicht erst als
        // Snapshot auf (und wäre über `delete` auch nicht entfernbar).
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        // Konsistenz mit `delete_in`/`restore_in`: was dort abgelehnt würde,
        // wird hier gar nicht erst angeboten – sonst stünde ein nicht
        // löschbarer Eintrag in der Liste.
        if !valid_id(&id) {
            continue;
        }
        let parsed = std::fs::read_to_string(entry.path().join("manifest.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<SnapshotManifest>(&t).ok());
        match parsed {
            Some(mut m) => {
                // Maßgeblich ist der Verzeichnisname, nicht der Manifest-Inhalt:
                // eine Id aus dem Inhalt könnte auf ein fremdes Verzeichnis
                // zeigen (kopierter Snapshot, manipulierte manifest.json).
                m.id = id;
                m.corrupt = false;
                out.push(m);
            }
            // Fehlendes/kaputtes Manifest: als beschädigt listen (nur löschbar).
            None => out.push(SnapshotManifest {
                id,
                created_at: 0,
                note: Some("(beschädigt)".into()),
                auto: false,
                files: Vec::new(),
                // Ohne Dateien belanglos; `false` ist der konservative Wert.
                link_targets_recorded: false,
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
/// Liefert die Originalpfade, die übersprungen werden mussten (siehe
/// [`restore_write_target`]) – eine leere Liste heißt „vollständig
/// wiederhergestellt“. Der Aufrufer muss sie dem Nutzer zeigen, sonst wirkt ein
/// Teil-Restore wie ein vollständiger.
pub fn restore(
    id: &str,
    only_paths: Option<Vec<String>>,
    retention: u32,
) -> Result<Vec<String>, AppError> {
    restore_in(&snapshots_root(), id, only_paths, retention)
}

fn restore_in(
    root: &Path,
    id: &str,
    only_paths: Option<Vec<String>>,
    retention: u32,
) -> Result<Vec<String>, AppError> {
    let snap_dir = snapshot_dir(root, id)?;
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
    // Übersprungene Originalpfade (Altmanifest + heute Symlink) für die Meldung
    // an den Nutzer.
    let mut skipped: Vec<String> = Vec::new();

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
        // Ziel bestimmen. Schreiben und Löschen haben hier BEWUSST verschiedene
        // Semantik (siehe `restore_write_target`):
        //  * schreiben – einem Symlink wird nur gefolgt, wenn schon der Snapshot
        //    über genau diesen Link gesichert hat; sonst Abbruch (bzw. bei
        //    einem Altmanifest ohne Linkaufzeichnung: nur diese Datei
        //    überspringen).
        //  * löschen – immer der UNaufgelöste Pfad (siehe unten).
        // Wichtig: Temp-Datei und Rename müssen im Elternverzeichnis des
        // tatsächlichen Ziels liegen, sonst scheitert der Rename über eine
        // Dateisystemgrenze hinweg.
        if !file.existed {
            // Existierte beim Snapshot nicht ⇒ am **sichtbaren** Ort entfernen.
            // Der unaufgelöste Pfad ist hier richtig: „war nicht da" bezieht sich
            // auf das, was der Nutzer sieht. Ein Symlink verschwindet damit als
            // Link; sein Ziel (etwa die versionierte Datei im Dotfiles-Repo)
            // bleibt unangetastet. `symlink_metadata` statt `exists()`, damit
            // auch ein toter Link erkannt und mitentfernt wird.
            let original = PathBuf::from(&file.original_path);
            if std::fs::symlink_metadata(&original).is_ok() {
                to_remove.push(original);
            }
            continue;
        }
        let target = match restore_write_target(&manifest, file) {
            Ok(Some(t)) => t,
            Ok(None) => {
                // Altmanifest ohne Linkaufzeichnung, heute liegt dort ein
                // Symlink: nur diese Datei auslassen (siehe
                // `restore_write_target`) und am Ende melden.
                skipped.push(file.original_path.clone());
                continue;
            }
            Err(e) => {
                cleanup(&to_rename, &created_dirs);
                return Err(e);
            }
        };
        let Some(parent) = target.parent() else {
            continue;
        };

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
        // Temp-Datei mit unvorhersagbarem Namen, exklusiv angelegt und ohne
        // Symlinks zu folgen: das Zielverzeichnis (z. B. ein Projektordner)
        // ist nicht zwingend nutzer-privat.
        let tmp = match write_private_temp(parent, &bytes) {
            Ok(p) => p,
            Err(e) => {
                cleanup(&to_rename, &created_dirs);
                return Err(e);
            }
        };
        to_rename.push((tmp, target));
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
    Ok(skipped)
}

/// Löscht einen Snapshot samt Verzeichnis.
pub fn delete(id: &str) -> Result<(), AppError> {
    delete_in(&snapshots_root(), id)
}

fn delete_in(root: &Path, id: &str) -> Result<(), AppError> {
    let dir = snapshot_dir(root, id)?;
    // `symlink_metadata` folgt keinem Symlink: nur ein echtes Verzeichnis wird
    // entfernt – dasselbe Kriterium, nach dem `list_in` überhaupt auflistet.
    if std::fs::symlink_metadata(&dir)
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
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

    /// Namen aller in `dir` zurückgebliebenen Temp-Dateien.
    fn leftover_temps(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".mcpmgr-") && n.ends_with(".tmp"))
            .collect()
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

    /// Ist das Ziel ein Symlink (Config ins Dotfiles-Repo verlinkt), muss der
    /// Restore dem Link folgen statt ihn durch eine reguläre Datei zu ersetzen –
    /// gleiche Semantik wie beim Schreiben der Client-Konfiguration.
    #[cfg(unix)]
    #[test]
    fn restore_folgt_symlink_statt_ihn_zu_ersetzen() {
        use std::os::unix::fs::symlink;
        let base = tmp("restore-symlink");
        let root = base.join("snapshots");
        // Echtes Ziel liegt in einem anderen Verzeichnis („Dotfiles-Repo").
        let repo = base.join("dotfiles");
        std::fs::create_dir_all(&repo).unwrap();
        let real = repo.join("claude_desktop_config.json");
        write(&real, "ALT");

        let cfg_dir = base.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let link = cfg_dir.join("claude_desktop_config.json");
        symlink(&real, &link).unwrap();

        let m = create_in(&root, &[(link.clone(), false)], None, false, 20).unwrap();
        write(&real, "NEU");
        restore_in(&root, &m.id, None, 20).unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "der Symlink muss ein Symlink bleiben"
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "ALT");
    }

    /// Gegenprobe zum Test darüber: existierte am Pfad beim Sichern NICHTS
    /// (`existed == false`), entfernt der Restore den **Link**, nicht die Datei
    /// dahinter. Andernfalls löschte ein Restore aus der Zeit vor Claude Desktop
    /// die versionierte Datei im Dotfiles-Repo.
    #[cfg(unix)]
    #[test]
    fn restore_entfernt_den_symlink_nicht_sein_ziel() {
        use std::os::unix::fs::symlink;
        let base = tmp("restore-symlink-delete");
        let root = base.join("snapshots");
        let cfg_dir = base.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let link = cfg_dir.join("claude_desktop_config.json");

        // Snapshot, solange am Pfad noch gar nichts liegt.
        let m = create_in(&root, &[(link.clone(), false)], None, false, 20).unwrap();
        assert!(!m.files[0].existed);

        // Danach richtet der Nutzer sein Dotfiles-Setup ein.
        let repo = base.join("dotfiles");
        std::fs::create_dir_all(&repo).unwrap();
        let real = repo.join("claude_desktop_config.json");
        write(&real, "VERSIONIERT");
        symlink(&real, &link).unwrap();

        restore_in(&root, &m.id, None, 20).unwrap();

        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "der Link muss verschwinden"
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "VERSIONIERT",
            "das Ziel hinter dem Link darf nicht angefasst werden"
        );
    }

    /// Auch ein **toter** Link wird entfernt – sonst bliebe er stehen und der
    /// nächste Schreibvorgang ersetzte ihn durch eine reguläre Datei.
    #[cfg(unix)]
    #[test]
    fn restore_entfernt_auch_einen_toten_symlink() {
        use std::os::unix::fs::symlink;
        let base = tmp("restore-symlink-dead");
        let root = base.join("snapshots");
        let cfg_dir = base.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let link = cfg_dir.join("claude_desktop_config.json");

        let m = create_in(&root, &[(link.clone(), false)], None, false, 20).unwrap();
        symlink(base.join("gibt-es-nicht"), &link).unwrap();

        restore_in(&root, &m.id, None, 20).unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
    }

    /// Beim Sichern eine reguläre Datei, beim Restore ein Symlink: der Restore
    /// bricht ab, statt entweder den Link zu ersetzen oder blind in eine nie
    /// gelesene Datei zu schreiben. Gegenprobe zum Altmanifest-Test weiter
    /// unten: hier ist `link_target: None` ein **Beweis**, weil das Manifest
    /// `link_targets_recorded` gesetzt hat.
    #[cfg(unix)]
    #[test]
    fn restore_bricht_ab_wenn_der_pfad_neuerdings_ein_symlink_ist() {
        use std::os::unix::fs::symlink;
        let base = tmp("restore-symlink-neu");
        let root = base.join("snapshots");
        let cfg = base.join("config/claude_desktop_config.json");
        write(&cfg, "ALT");

        let m = create_in(&root, &[(cfg.clone(), false)], None, false, 20).unwrap();
        assert!(m.files[0].link_target.is_none());
        assert!(m.link_targets_recorded, "neues Manifest erhebt das Feld");

        // Der Pfad wird nachträglich zu einem Link auf eine fremde Datei.
        let fremd = base.join("fremd.txt");
        write(&fremd, "FREMD");
        std::fs::remove_file(&cfg).unwrap();
        symlink(&fremd, &cfg).unwrap();

        assert!(restore_in(&root, &m.id, None, 20).is_err());
        assert_eq!(std::fs::read_to_string(&fremd).unwrap(), "FREMD");
        assert!(std::fs::symlink_metadata(&cfg)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(leftover_temps(base.join("config").as_path()).is_empty());
    }

    /// Beim Sichern über Link A, beim Restore zeigt derselbe Pfad auf B: Abbruch.
    #[cfg(unix)]
    #[test]
    fn restore_bricht_ab_wenn_der_symlink_umgehaengt_wurde() {
        use std::os::unix::fs::symlink;
        let base = tmp("restore-symlink-umgehaengt");
        let root = base.join("snapshots");
        let a = base.join("a.txt");
        let b = base.join("b.txt");
        write(&a, "A");
        write(&b, "B");
        let cfg_dir = base.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let link = cfg_dir.join("claude_desktop_config.json");
        symlink(&a, &link).unwrap();

        let m = create_in(&root, &[(link.clone(), false)], None, false, 20).unwrap();
        assert!(m.files[0].link_target.is_some());

        std::fs::remove_file(&link).unwrap();
        symlink(&b, &link).unwrap();

        assert!(restore_in(&root, &m.id, None, 20).is_err());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "B");
    }

    /// Beim Sichern ein Symlink, beim Restore eine reguläre Datei: geschrieben
    /// wird an den **sichtbaren** Ort. Das ist bewusst so (siehe
    /// `restore_write_target`) – der Nutzer hat sein Dotfiles-Setup aufgelöst,
    /// also gehört die Datei dorthin und nicht mehr ins alte Repo.
    #[cfg(unix)]
    #[test]
    fn restore_schreibt_an_den_sichtbaren_ort_wenn_der_symlink_verschwunden_ist() {
        use std::os::unix::fs::symlink;
        let base = tmp("restore-symlink-aufgeloest");
        let root = base.join("snapshots");
        let repo = base.join("dotfiles");
        std::fs::create_dir_all(&repo).unwrap();
        let real = repo.join("claude_desktop_config.json");
        write(&real, "ALT");

        let cfg_dir = base.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let link = cfg_dir.join("claude_desktop_config.json");
        symlink(&real, &link).unwrap();

        let m = create_in(&root, &[(link.clone(), false)], None, false, 20).unwrap();
        assert!(m.files[0].link_target.is_some());

        // Der Nutzer löst das Dotfiles-Setup auf: aus dem Link wird eine
        // eigenständige Datei, das Repo lebt unabhängig weiter.
        std::fs::remove_file(&link).unwrap();
        write(&link, "NEU");
        write(&real, "REPO");

        assert!(restore_in(&root, &m.id, None, 20).unwrap().is_empty());
        assert!(
            !std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "es darf kein Link entstehen"
        );
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "ALT");
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "REPO",
            "das ehemalige Linkziel bleibt unangetastet"
        );
    }

    /// Altmanifest (geschrieben, bevor es `link_targets_recorded` gab): ein
    /// fehlendes `link_target` heißt dort „nicht aufgezeichnet“, nicht „war kein
    /// Symlink“. Richtet der Nutzer danach sein Dotfiles-Setup ein, darf das
    /// nicht den **ganzen** Restore kippen – nur die betroffene Datei wird
    /// ausgelassen und gemeldet.
    #[cfg(unix)]
    #[test]
    fn altmanifest_ueberspringt_nur_die_symlink_datei() {
        use std::os::unix::fs::symlink;
        let base = tmp("restore-altmanifest");
        let root = base.join("snapshots");
        let cfg_dir = base.join("config");
        let a = cfg_dir.join("a.json");
        let b = cfg_dir.join("b.json");
        write(&a, "A-ALT");
        write(&b, "B-ALT");

        let m = create_in(
            &root,
            &[(a.clone(), false), (b.clone(), false)],
            None,
            false,
            20,
        )
        .unwrap();

        // Manifest auf den Stand von vor diesem Feld zurückdrehen.
        let mp = root.join(&m.id).join("manifest.json");
        let mut doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mp).unwrap()).unwrap();
        assert!(
            doc.as_object_mut()
                .unwrap()
                .remove("link_targets_recorded")
                .is_some(),
            "das Feld muss überhaupt geschrieben worden sein"
        );
        std::fs::write(&mp, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        // Erst danach richtet der Nutzer sein Dotfiles-Setup für `a` ein.
        let repo = base.join("dotfiles");
        std::fs::create_dir_all(&repo).unwrap();
        let real = repo.join("a.json");
        write(&real, "REPO");
        std::fs::remove_file(&a).unwrap();
        symlink(&real, &a).unwrap();
        write(&b, "B-NEU");

        let skipped = restore_in(&root, &m.id, None, 20).unwrap();
        assert_eq!(skipped, vec![a.to_string_lossy().to_string()]);
        assert!(
            std::fs::symlink_metadata(&a)
                .unwrap()
                .file_type()
                .is_symlink(),
            "der Link bleibt ein Link"
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "REPO",
            "das Ziel hinter dem Link darf nicht überschrieben werden"
        );
        assert_eq!(
            std::fs::read_to_string(&b).unwrap(),
            "B-ALT",
            "alle übrigen Dateien werden normal wiederhergestellt"
        );
        assert!(leftover_temps(&cfg_dir).is_empty());
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
        // Keine Temp-Dateien zurückgelassen (Namen sind zufällig -> Ordner scannen).
        assert!(
            leftover_temps(&base).is_empty(),
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
    fn id_validierung_lehnt_pfad_traversal_ab() {
        assert!(valid_id("1234-000000000"), "erzeugtes Format ist gültig");
        assert!(valid_id("1"));
        for bad in [
            "",
            ".",
            "..",
            "../..",
            "../opfer",
            "/etc",
            "a/b",
            "1234-000000000/..",
            "beschädigt",
            "abc",
            ".hidden",
        ] {
            assert!(!valid_id(bad), "muss abgelehnt werden: {bad:?}");
        }
        assert!(
            !valid_id(&"1".repeat(MAX_ID_LEN + 1)),
            "Längenlimit greift"
        );
    }

    #[test]
    fn loeschen_und_restore_lehnen_traversal_id_ab() {
        let base = tmp("traversal");
        let root = base.join("snapshots");
        std::fs::create_dir_all(&root).unwrap();
        let opfer = base.join("opfer");
        write(&opfer.join("wichtig.txt"), "bleibt");

        assert!(
            delete_in(&root, "../opfer").is_err(),
            "Pfad-Traversal beim Löschen abgelehnt"
        );
        assert!(
            opfer.join("wichtig.txt").exists(),
            "fremdes Verzeichnis unangetastet"
        );
        assert!(
            restore_in(&root, "../opfer", None, 20).is_err(),
            "Pfad-Traversal beim Restore abgelehnt"
        );
    }

    #[test]
    fn liste_nimmt_die_id_aus_dem_verzeichnisnamen() {
        // Regression: eine kopierte/manipulierte manifest.json darf die Id nicht
        // bestimmen – sonst zeigte „Löschen" auf ein fremdes Verzeichnis.
        let base = tmp("id-aus-dirname");
        let root = base.join("snapshots");
        let a = base.join("a.json");
        write(&a, "A");
        let m = create_in(&root, &[(a.clone(), true)], None, false, 20).unwrap();

        let manifest_path = root.join(&m.id).join("manifest.json");
        let mut roh: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        roh["id"] = serde_json::json!("../../opfer");
        std::fs::write(&manifest_path, serde_json::to_string(&roh).unwrap()).unwrap();

        let listed = list_in(&root).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, m.id, "Id kommt aus dem Verzeichnisnamen");
        // ... und ist damit auch löschbar.
        delete_in(&root, &listed[0].id).unwrap();
        assert!(list_in(&root).unwrap().is_empty());
    }

    #[test]
    fn auflisten_und_loeschen_sind_konsistent() {
        let base = tmp("konsistenz");
        let root = base.join("snapshots");
        // Gültiger Verzeichnisname, kaputtes Manifest -> gelistet UND löschbar.
        let kaputt = root.join("1234-000000000");
        std::fs::create_dir_all(&kaputt).unwrap();
        std::fs::write(kaputt.join("manifest.json"), "{ kaputt").unwrap();
        // Unzulässiger Verzeichnisname -> weder gelistet noch löschbar.
        let fremd = root.join("fremdes-verzeichnis");
        std::fs::create_dir_all(&fremd).unwrap();

        let listed = list_in(&root).unwrap();
        assert_eq!(listed.len(), 1, "nur der gültige Eintrag wird angeboten");
        assert_eq!(listed[0].id, "1234-000000000");
        for m in &listed {
            delete_in(&root, &m.id).expect("jeder gelistete Snapshot ist löschbar");
        }
        assert!(delete_in(&root, "fremdes-verzeichnis").is_err());
        assert!(fremd.is_dir(), "unbekanntes Verzeichnis bleibt liegen");
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

    /// Reale Umgebung: ist Claude Desktop installiert, muss dessen Config im
    /// Snapshot-Umfang stehen – mit `create_parent == false`, damit ein Restore
    /// kein Client-Verzeichnis neu entstehen lässt. `cargo test -- --ignored`.
    /// (Ein nicht-ignorierter Zwilling dieses Tests wäre ohne installierten
    /// Client eine leere Schleife und sicherte deshalb nichts zu.)
    #[test]
    #[ignore]
    fn real_env_collect_source_paths_enthaelt_client_config() {
        let clients = client_source_paths();
        if clients.is_empty() {
            eprintln!("kein Datei-Client erkannt – Test übersprungen");
            return;
        }
        let all = collect_source_paths();
        for (path, _) in clients {
            let found = all.iter().find(|(p, _)| *p == path);
            let (_, create_parent) = found.unwrap_or_else(|| {
                panic!("{} fehlt im Snapshot-Umfang", path.display());
            });
            assert!(!create_parent, "{}: create_parent=false", path.display());
        }
    }
}
