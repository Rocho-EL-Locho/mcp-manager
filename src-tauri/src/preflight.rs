//! Laufzeit-Preflight: prüft, ob der von einem Server benötigte Befehl
//! (`node`/`npx`, `python`/`uvx`, `docker`, …) tatsächlich auf PATH verfügbar
//! ist, und liefert – falls nicht – einen umsetzbaren Hinweis (+ optional die
//! erkannte Version).
//!
//! Ein großer Teil der als „Fehler" gemeldeten Server scheitert schlicht an
//! einer fehlenden Laufzeit (oder einem unvollständigen PATH, wenn die App aus
//! einem Desktop-Eintrag gestartet wurde). Diesen Fall explizit zu erkennen
//! verwandelt einen kryptischen Fehlschlag in eine klare Handlungsanweisung.
//!
//! PATH-Treue: Auflösung und Versionsabfrage nutzen dasselbe effektive PATH,
//! das der Server beim echten Start via `introspect_stdio` sähe (`entry.env`
//! wird dort über die geerbte Umgebung gelegt). So ist „nicht gefunden" für den
//! Kontext dieser App korrekt.

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Serialize;
use wait_timeout::ChildExt;

use crate::models::ServerEntry;

/// Zeitbudget für die (best-effort) Versionsabfrage. Klein gehalten – ein
/// `--version` antwortet sofort; ein Befehl, der stattdessen blockiert (z. B.
/// eine REPL, obwohl stdin auf null steht), wird hart abgebrochen.
const VERSION_TIMEOUT: Duration = Duration::from_secs(3);

/// Obergrenze für die angezeigte Versionszeile (Diagnose, kein volles Log).
const VERSION_MAX_LEN: usize = 120;

/// Obergrenze für die aus `--version` gelesenen Bytes. Der Befehl ist
/// user-konfiguriert (potentiell fehlerhaft/bösartig); eine Flut auf `--version`
/// darf keinen unbegrenzten Speicher belegen (OOM-Schutz, analog zu
/// `introspect.rs`). Reale Versionsausgaben sind wenige Bytes.
const VERSION_READ_CAP: usize = 64 * 1024;

/// Ergebnis des Preflights für einen Server.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimePreflight {
    /// Der zu prüfende Befehl, exakt wie in der Definition ("npx", "/usr/bin/python3").
    pub command: String,
    /// Menschlicher Name der Laufzeit ("Node.js", "Python", "uv", "Docker", …)
    /// bzw. der Befehl selbst, wenn es keine bekannte Laufzeit ist.
    pub runtime: String,
    /// Auf PATH gefunden bzw. (bei Pfad-Befehl) existent und ausführbar.
    pub found: bool,
    /// Aufgelöster Pfad, falls gefunden.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Erkannte Version (`<cmd> --version`, nur bei Exit 0), falls ermittelbar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Umsetzbarer Hinweis – nur gesetzt, wenn der Befehl NICHT gefunden wurde.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Voller Preflight inklusive Versionsabfrage. Gibt `None` für Server ohne
/// lokalen Befehl zurück (HTTP/SSE): dann gibt es keine Laufzeit zu prüfen.
pub fn check(entry: &ServerEntry) -> Option<RuntimePreflight> {
    let command = entry.command.as_deref()?.trim().to_string();
    if command.is_empty() {
        return None;
    }

    let runtime = classify(&command);
    let known = known_runtime(&basename(&command));
    let resolved = resolve(&command, entry.env.as_ref());
    let found = resolved.is_some();

    // Version NUR für bekannte Laufzeiten abfragen: für node/npx/python/uv/docker/…
    // ist `--version` eine sichere, etablierte Konvention. Ein beliebiges
    // Server-Binary könnte bei `--version` stattdessen seinen stdio-Loop starten –
    // das wäre ein ungewollter Server-Start (der Preflight verspricht das Gegenteil).
    let version = match (&resolved, known) {
        (Some(path), Some(_)) => {
            detect_version(path, effective_path(entry.env.as_ref()).as_deref())
        }
        _ => None,
    };
    let hint = (!found).then(|| hint_for(&command, known));

    Some(RuntimePreflight {
        command,
        runtime,
        found,
        path: resolved.map(|p| p.to_string_lossy().into_owned()),
        version,
        hint,
    })
}

/// Billige Auflösung OHNE Subprozess (nur Dateisystem-Stat). Für die
/// Listenanreicherung, die pro Server nur „vorhanden ja/nein" braucht.
pub fn resolve_command(entry: &ServerEntry) -> Option<PathBuf> {
    let command = entry.command.as_deref()?.trim();
    if command.is_empty() {
        return None;
    }
    resolve(command, entry.env.as_ref())
}

/// Effektives PATH: Definition-Override (`entry.env["PATH"]`) hat Vorrang,
/// sonst das geerbte Prozess-PATH – exakt die Sicht des gestarteten Servers.
fn effective_path(env: Option<&BTreeMap<String, String>>) -> Option<String> {
    if let Some(p) = env.and_then(|m| m.get("PATH")) {
        if !p.trim().is_empty() {
            return Some(p.clone());
        }
    }
    std::env::var("PATH").ok()
}

/// Löst einen Befehl auf: enthält er ein `/`, wird er als Pfad behandelt
/// (führendes `~/` auf `$HOME` expandiert); sonst wird das effektive PATH
/// durchsucht. Es zählt nur eine existierende, ausführbare Datei.
fn resolve(command: &str, env: Option<&BTreeMap<String, String>>) -> Option<PathBuf> {
    if command.contains('/') {
        let p = PathBuf::from(expand_tilde(command));
        return is_executable_file(&p).then_some(p);
    }
    let path = effective_path(env)?;
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let cand = Path::new(dir).join(command);
        if is_executable_file(&cand) {
            return Some(cand);
        }
    }
    None
}

/// Expandiert ein führendes `~/` auf das Home-Verzeichnis (best effort).
fn expand_tilde(command: &str) -> String {
    if let Some(rest) = command.strip_prefix("~/") {
        if let Some(home) = crate::claude_cli::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    command.to_string()
}

/// Ist der Pfad eine reguläre, für den aufrufenden Nutzer ausführbare Datei?
/// Prüft die Ausführbarkeit via `access(X_OK)` (berücksichtigt reale UID/GID und
/// Gruppen) statt roher Mode-Bits – ein nur fremd-ausführbares Binary (z. B.
/// mode 0700, root) gilt so korrekt als NICHT nutzbar. Die vorgeschaltete
/// `is_file`-Prüfung schließt ein Verzeichnis mit gesetztem +x-Bit aus.
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    if !std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false) {
        return false;
    }
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: gültiger, NUL-terminierter Pfad; access() hat keine Nebenwirkungen.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// Letztes Pfadsegment eines Befehls ("/usr/bin/python3" -> "python3").
fn basename(command: &str) -> String {
    command.rsplit('/').next().unwrap_or(command).to_string()
}

/// Bekannte Laufzeit: Anzeigename + Installations-/PATH-Hinweis (falls fehlend).
/// Einzige Registry – `classify` (Label) und `hint_for` (Hinweis) leiten sich
/// hieraus ab, damit die beiden Sichten nicht auseinanderlaufen.
#[derive(Clone, Copy)]
struct KnownRuntime {
    label: &'static str,
    hint: &'static str,
}

/// Ordnet einem Befehls-Basename eine bekannte Laufzeit zu (`None` = generisch).
/// Für bekannte Laufzeiten ist `--version` eine sichere, etablierte Konvention –
/// nur für sie fragt `check` die Version ab.
fn known_runtime(base: &str) -> Option<KnownRuntime> {
    let (label, hint) = match base {
        "node" | "npx" | "npm" => (
            "Node.js",
            "Node.js/npx nicht auf PATH. Installiere Node.js (z. B. via nvm, fnm oder den \
             Paketmanager) und starte die App neu.",
        ),
        "python" | "python3" | "python2" | "pythonw" => (
            "Python",
            "Python nicht auf PATH. Installiere Python 3 über den Paketmanager.",
        ),
        "uv" | "uvx" => (
            "uv",
            "uv/uvx nicht auf PATH. Installiere uv: curl -LsSf https://astral.sh/uv/install.sh | sh",
        ),
        "pipx" => (
            "pipx",
            "pipx nicht auf PATH. Installiere pipx: python3 -m pip install --user pipx",
        ),
        "pip" | "pip3" => (
            "pip",
            "pip nicht auf PATH. Installiere Python 3 inklusive pip über den Paketmanager.",
        ),
        "docker" => (
            "Docker",
            "Docker nicht auf PATH. Installiere Docker und stelle sicher, dass der Docker-Daemon läuft.",
        ),
        "podman" => (
            "Podman",
            "Podman nicht auf PATH. Installiere Podman über den Paketmanager.",
        ),
        "deno" => (
            "Deno",
            "Deno nicht auf PATH. Installiere Deno: https://deno.land/#installation",
        ),
        "bun" | "bunx" => ("Bun", "Bun nicht auf PATH. Installiere Bun: https://bun.sh"),
        "ruby" => ("Ruby", "Ruby nicht auf PATH. Installiere Ruby über den Paketmanager."),
        "go" => ("Go", "Go nicht auf PATH. Installiere Go: https://go.dev/dl/"),
        _ => return None,
    };
    Some(KnownRuntime { label, hint })
}

/// Menschenlesbares Laufzeit-Label. Unbekannte Befehle liefern ihren eigenen
/// Basename (die „Laufzeit" ist dann der Befehl selbst).
fn classify(command: &str) -> String {
    let base = basename(command);
    known_runtime(&base).map(|k| k.label.to_string()).unwrap_or(base)
}

/// Umsetzbarer Hinweis, wenn ein Befehl nicht auflösbar war.
fn hint_for(command: &str, known: Option<KnownRuntime>) -> String {
    // Pfad-Befehl: konkret auf Existenz/Ausführbarkeit hinweisen.
    if command.contains('/') {
        return format!(
            "Datei nicht gefunden oder nicht ausführbar: {command}. Prüfe den Pfad in der \
             Definition und die Ausführbar-Rechte."
        );
    }
    match known {
        Some(k) => k.hint.to_string(),
        None => format!(
            "Befehl „{command}\" nicht auf PATH gefunden. Installiere ihn oder gib in der \
             Definition einen absoluten Pfad an. (Wird die App über einen Desktop-Eintrag \
             gestartet, kann PATH unvollständig sein.)"
        ),
    }
}

/// Fragt `<path> --version` sicher ab: eigene Prozessgruppe (killpg beim
/// Timeout), stdin auf null (eine REPL bekäme sofort EOF), stdout/stderr
/// nebenläufig gelesen. Nur bei Exit 0 wird die erste sinnvolle Zeile
/// zurückgegeben – sonst gälten Fehlerausgaben (`dash --version`) als „Version".
fn detect_version(path: &Path, env_path: Option<&str>) -> Option<String> {
    let mut cmd = Command::new(path);
    cmd.arg("--version");
    if let Some(pp) = env_path {
        cmd.env("PATH", pp);
    }
    cmd.process_group(0);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().ok()?;

    // stdout/stderr nebenläufig UND gedeckelt leeren: die Pipe wird vollständig
    // geleert (kein Backpressure-Deadlock), aber nur die ersten VERSION_READ_CAP
    // Bytes behalten (OOM-Schutz gegen einen flutenden Befehl).
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_handle = std::thread::spawn(move || read_capped(out_pipe.as_mut()));
    let err_handle = std::thread::spawn(move || read_capped(err_pipe.as_mut()));

    let ok = match child.wait_timeout(VERSION_TIMEOUT) {
        Ok(Some(status)) => status.success(),
        Ok(None) | Err(_) => {
            // Timeout (oder wait-Fehler): gesamte Prozessgruppe hart beenden.
            let pgid = child.id() as libc::pid_t;
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
            let _ = child.wait();
            false
        }
    };

    // Reader-Threads in ALLEN Pfaden ernten (Kind ist beendet/gekillt -> Pipes
    // geschlossen -> EOF), analog zu claude_cli.rs.
    let stdout = String::from_utf8_lossy(&out_handle.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_handle.join().unwrap_or_default()).into_owned();
    if !ok {
        return None;
    }
    // Die meisten Tools schreiben die Version nach stdout; ein paar nach stderr.
    let combined = if stdout.trim().is_empty() { stderr } else { stdout };
    first_version_line(&combined)
}

/// Liest eine Pipe vollständig leer, behält aber nur die ersten
/// `VERSION_READ_CAP` Bytes. Weiterlesen (statt früh abbrechen) verhindert, dass
/// ein gesprächiger Befehl beim Schreiben blockiert; das Deckeln schützt vor OOM.
fn read_capped<R: Read>(pipe: Option<&mut R>) -> Vec<u8> {
    let mut buf = Vec::new();
    let Some(p) = pipe else { return buf };
    let mut chunk = [0u8; 4096];
    loop {
        match p.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if buf.len() < VERSION_READ_CAP {
                    let take = (VERSION_READ_CAP - buf.len()).min(n);
                    buf.extend_from_slice(&chunk[..take]);
                }
                // Überschuss bewusst verwerfen, aber weiterlesen (Pipe leeren).
            }
        }
    }
    buf
}

/// Erste nicht-leere Zeile, getrimmt und auf `VERSION_MAX_LEN` Zeichen gekappt.
fn first_version_line(output: &str) -> Option<String> {
    let line = output.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(VERSION_MAX_LEN).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(command: &str) -> ServerEntry {
        ServerEntry {
            command: Some(command.into()),
            ..Default::default()
        }
    }

    #[test]
    fn classify_maps_known_runtimes() {
        assert_eq!(classify("npx"), "Node.js");
        assert_eq!(classify("node"), "Node.js");
        assert_eq!(classify("/usr/bin/python3"), "Python");
        assert_eq!(classify("uvx"), "uv");
        assert_eq!(classify("docker"), "Docker");
        // Unbekannt -> Basename selbst.
        assert_eq!(classify("mein-mcp-server"), "mein-mcp-server");
        assert_eq!(classify("/opt/tools/mein-mcp-server"), "mein-mcp-server");
    }

    #[test]
    fn resolve_finds_and_misses() {
        // Absoluter Pfad auf ein sicher vorhandenes, ausführbares Binary.
        assert!(resolve("/bin/sh", None).is_some());
        // Bare command über PATH (sh liegt auf jedem POSIX-System auf PATH).
        assert!(resolve("sh", None).is_some());
        // Nicht existierendes Binary.
        assert!(resolve("mcpmgr-nonexistent-binary-xyz", None).is_none());
        // Absoluter Pfad, der nicht existiert.
        assert!(resolve("/usr/bin/mcpmgr-nonexistent-xyz", None).is_none());
    }

    #[test]
    fn resolve_respects_entry_path() {
        // Ein PATH, das nur ins Leere zeigt -> bare command wird nicht gefunden.
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), "/nonexistent-dir-xyz".to_string());
        assert!(resolve("sh", Some(&env)).is_none());
    }

    #[test]
    fn check_found_has_no_hint() {
        let pf = check(&entry("sh")).expect("stdio -> Some");
        assert!(pf.found, "sh sollte auf PATH sein");
        assert!(pf.hint.is_none());
        assert!(pf.path.is_some());
        // „sh" ist keine bekannte Laufzeit -> keine (ungewollte) Versionsabfrage.
        assert!(pf.version.is_none(), "generischer Befehl: keine Versionsabfrage");
    }

    #[test]
    fn check_missing_sets_hint() {
        let pf = check(&entry("mcpmgr-nonexistent-binary-xyz")).expect("stdio -> Some");
        assert!(!pf.found);
        assert!(pf.hint.is_some());
        assert_eq!(pf.runtime, "mcpmgr-nonexistent-binary-xyz");
        assert!(pf.version.is_none());
    }

    #[test]
    fn check_none_for_http_or_empty() {
        let http = ServerEntry {
            url: Some("https://example/mcp".into()),
            ..Default::default()
        };
        assert!(check(&http).is_none());
        // Leeres/whitespace command ebenfalls None.
        assert!(check(&entry("   ")).is_none());
    }

    #[test]
    fn hint_for_mentions_runtime_and_path() {
        assert!(hint_for("npx", known_runtime("npx")).contains("Node"));
        assert!(hint_for("uvx", known_runtime("uvx")).contains("uv"));
        // Pfad-Befehl -> Pfad-Hinweis (unabhängig von der Laufzeit).
        assert!(hint_for("/opt/x/bar", None).contains("Pfad"));
        // Generischer Befehl -> allgemeiner PATH-Hinweis.
        assert!(hint_for("mystery", None).contains("PATH"));
    }

    /// Opt-in-Integrationstest: echter `check` gegen real installierte Laufzeiten
    /// (umgebungsabhängig, startet Subprozesse). Beweist, dass die Versionsabfrage
    /// end-to-end funktioniert. Nur mit `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn check_real_runtimes() {
        for cmd in ["node", "npx", "python3", "uv", "uvx", "docker", "deno"] {
            let pf = check(&entry(cmd)).expect("stdio -> Some");
            eprintln!(
                "{cmd:8} runtime={:8} found={} version={:?} path={:?} hint={:?}",
                pf.runtime, pf.found, pf.version, pf.path, pf.hint
            );
            // Wenn das Binary auf PATH liegt, muss found stimmen und (fast immer)
            // eine Version erkannt werden; sonst greift der Hinweis.
            if which_on_path(cmd) {
                assert!(pf.found, "{cmd} sollte gefunden werden");
                assert!(pf.hint.is_none(), "gefundene Runtime braucht keinen Hinweis");
            } else {
                assert!(!pf.found);
                assert!(pf.hint.is_some());
            }
        }
    }

    /// Kleiner PATH-Check nur für den Integrationstest oben.
    fn which_on_path(cmd: &str) -> bool {
        std::env::var("PATH")
            .ok()
            .map(|p| p.split(':').any(|d| is_executable_file(&Path::new(d).join(cmd))))
            .unwrap_or(false)
    }

    #[test]
    fn first_version_line_picks_first_nonempty() {
        assert_eq!(first_version_line(""), None);
        assert_eq!(first_version_line("\n\n"), None);
        assert_eq!(
            first_version_line("  v1.2.3  \nmore"),
            Some("v1.2.3".to_string())
        );
        let long = "x".repeat(500);
        assert_eq!(first_version_line(&long).unwrap().chars().count(), VERSION_MAX_LEN);
    }
}
