//! Fachlich neutrale Kleinhelfer, die mehrere Module brauchen.
//!
//! Bewusst KEIN Fachmodul: hier steht nur, was keinem Feature gehört (Ablageort
//! der App-Dateien, Wanduhr). Alles mit Fachlogik gehört in das jeweilige Modul.

use std::path::{Path, PathBuf};

use crate::claude_cli::home_dir;
use crate::models::AppError;

/// Nutzer-privates Config-Verzeichnis dieser App (`$XDG_CONFIG_HOME/mcp-manager`
/// bzw. `~/.config/mcp-manager`). Gemeinsame Ablage für `settings.json`,
/// `stash.json`, `metrics.json` und `snapshots/`.
pub(crate) fn config_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(x).join("mcp-manager")
    } else {
        home_dir().unwrap_or_default().join(".config/mcp-manager")
    }
}

/// Unix-Zeitstempel (Sekunden), 0 bei Uhr-Fehlern.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Legt eine Datei **exklusiv neu** an (0600) und schreibt `bytes` hinein.
///
/// `create_new` (O_CREAT|O_EXCL) scheitert, wenn der Pfad schon existiert –
/// auch als Symlink; `O_NOFOLLOW` schließt die Lücke zusätzlich. Anders als
/// `create(true).truncate(true)` wird so weder ein vorab platzierter Symlink
/// verfolgt noch eine fremde Datei mit laxen Rechten weiterbenutzt (`mode()`
/// wirkt nur beim Neuanlegen).
pub(crate) fn create_private_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)
}

/// Schreibt eine eindeutig benannte, private (0600) Temp-Datei neben das
/// spätere Ziel und gibt deren Pfad zurück.
///
/// Der Name enthält PID und Nanosekunden, damit ihn niemand vorhersagen kann;
/// bei Kollision (fremd angelegte Datei/Symlink) wird ein neuer Name probiert.
/// Wichtig, weil das Zielverzeichnis ein beliebiger, ggf. gemeinsam nutzbarer
/// Pfad sein kann (z. B. `/tmp/projekt/.mcp.json`) und `rename` die Rechte der
/// Temp-Datei auf das Ziel zieht – deshalb ist 0600 hier auch die Antwort auf
/// „Klartext-Secrets in `~/.claude.json`".
pub(crate) fn write_private_temp(
    parent: &Path,
    fname: &str,
    tag: &str,
    bytes: &[u8],
) -> Result<PathBuf, AppError> {
    let pid = std::process::id();
    for attempt in 0..8u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let tmp = parent.join(format!(".{fname}.{tag}.{pid}-{nanos:09}-{attempt}.tmp"));
        match create_private_new(&tmp, bytes) {
            Ok(()) => return Ok(tmp),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(AppError::Io(e.to_string())),
        }
    }
    Err(AppError::Io(format!(
        "Konnte keine Temp-Datei für {fname} anlegen (Zielverzeichnis belegt)"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Eindeutiges, frisches Temp-Verzeichnis pro Test.
    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mcpmgr-util-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Die Härtungsregel in einem Satz: 0600 gilt ab dem ersten Byte, es gibt
    /// also kein Fenster, in dem Klartext-Secrets world-readable auf der Platte
    /// liegen.
    #[test]
    #[cfg(unix)]
    fn create_private_new_schreibt_mit_0600() {
        let dir = tmp("create-mode");
        let p = dir.join("geheim.json");
        create_private_new(&p, b"{}").unwrap();
        assert_eq!(mode_of(&p), 0o600);
        assert_eq!(std::fs::read(&p).unwrap(), b"{}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `create_new` statt `create(true).truncate(true)`: eine bereits vorhandene
    /// Datei wird NICHT weiterbenutzt (deren laxe Rechte blieben sonst stehen,
    /// weil `mode()` nur beim Neuanlegen greift).
    #[test]
    fn create_private_new_weist_vorhandene_datei_ab() {
        let dir = tmp("create-exists");
        let p = dir.join("belegt.json");
        std::fs::write(&p, b"alt").unwrap();
        let err = create_private_new(&p, b"neu").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&p).unwrap(), b"alt", "Inhalt unangetastet");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Kern der Symlink-Härtung: ein vorab platzierter Symlink darf nicht
    /// verfolgt werden – sonst schriebe ein Angreifer über den Temp-Namen in ein
    /// beliebiges Ziel des Nutzers.
    #[test]
    #[cfg(unix)]
    fn create_private_new_folgt_keinem_symlink() {
        let dir = tmp("create-symlink");
        let ziel = dir.join("opfer.txt");
        std::fs::write(&ziel, b"unberuehrt").unwrap();
        let link = dir.join("link.json");
        std::os::unix::fs::symlink(&ziel, &link).unwrap();

        assert!(create_private_new(&link, b"boese").is_err());
        assert_eq!(std::fs::read(&ziel).unwrap(), b"unberuehrt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Der Temp-Name ist unvorhersagbar (PID + Nanosekunden + Versuch) und die
    /// Datei privat – zwei Aufrufe kollidieren nicht.
    #[test]
    fn write_private_temp_erzeugt_eindeutige_private_datei() {
        let dir = tmp("temp-name");
        let a = write_private_temp(&dir, "stash.json", "mcpmgr", b"a").unwrap();
        let b = write_private_temp(&dir, "stash.json", "mcpmgr", b"b").unwrap();
        assert_ne!(a, b, "Namen müssen sich unterscheiden");
        assert_ne!(
            a.file_name().unwrap(),
            std::ffi::OsStr::new(".stash.json.tmp"),
            "kein fester, vorhersagbarer Name"
        );
        assert_eq!(std::fs::read(&a).unwrap(), b"a");
        assert_eq!(std::fs::read(&b).unwrap(), b"b");
        #[cfg(unix)]
        {
            assert_eq!(mode_of(&a), 0o600);
            assert_eq!(mode_of(&b), 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
