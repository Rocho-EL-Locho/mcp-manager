//! Zentraler, sicherer Wrapper um jeden Aufruf der `claude`-CLI.
//!
//! Wichtige Eigenschaften:
//! - Immer Argument-Vektoren, nie ein Shell-String (keine Command-Injection).
//! - Eigene Prozessgruppe, damit von `claude` gestartete Kindprozesse
//!   (docker/npx/uvx) bei einem Timeout mitgekillt werden.
//! - Nebenläufiges Auslesen von stdout/stderr, damit volle Pipe-Puffer nicht
//!   zum Deadlock führen.
//! - Harte Zeitüberschreitung mit SIGKILL auf die Prozessgruppe.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::models::AppError;

/// Ergebnis eines CLI-Aufrufs.
pub struct CliOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CliOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Home-Verzeichnis des Nutzers (für Pfad-Fallbacks und user-scope cwd).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Ermittelt den absoluten Pfad zur `claude`-CLI.
///
/// Reihenfolge: Env-Var (`MCP_MANAGER_CLAUDE_PATH`, stärkster Override, wichtig
/// für Tests/Scripting) > `configured` (Einstellung) > `which` > gängige
/// Installationspfade. Der absolute Pfad ist wichtig, weil die App aus einem
/// Desktop-Eintrag ohne vollständiges Shell-PATH gestartet werden kann.
pub fn resolve_claude(configured: Option<&str>) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MCP_MANAGER_CLAUDE_PATH") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }

    // Konfigurierter Pfad aus den Einstellungen. Ist er gesetzt, aber (z. B. nach
    // Löschen des Binaries) ungültig, fällt die Kette bewusst auf die automatische
    // Auflösung zurück, statt hart zu scheitern.
    if let Some(p) = configured.map(str::trim).filter(|p| !p.is_empty()) {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }

    if let Ok(out) = Command::new("which").arg("claude").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                let pb = PathBuf::from(&s);
                if pb.is_file() {
                    return Some(pb);
                }
            }
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = home_dir() {
        candidates.push(home.join(".local/bin/claude"));
        candidates.push(home.join(".npm-global/bin/claude"));
    }
    candidates.push(PathBuf::from("/usr/local/bin/claude"));
    candidates.push(PathBuf::from("/usr/bin/claude"));
    candidates.into_iter().find(|p| p.is_file())
}

/// Führt `claude <args>` aus. `cwd = None` bedeutet Home-Verzeichnis
/// (relevant für scope-abhängige Aufrufe: user = Home, local/project = Projektpfad).
pub fn run_claude(
    claude: &Path,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<CliOutput, AppError> {
    let mut cmd = Command::new(claude);
    cmd.args(args);

    match cwd {
        Some(dir) => {
            cmd.current_dir(dir);
        }
        None => {
            if let Some(home) = home_dir() {
                cmd.current_dir(home);
            }
        }
    }

    // Eigene Prozessgruppe -> killpg trifft auch Kindprozesse von claude.
    cmd.process_group(0);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::ClaudeNotFound
        } else {
            AppError::Io(e.to_string())
        }
    })?;

    // stdout/stderr nebenläufig leeren, damit volle Puffer nicht blockieren.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = stdout_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = stderr_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let (code, timed_out) = match child
        .wait_timeout(timeout)
        .map_err(|e| AppError::Io(e.to_string()))?
    {
        Some(status) => (status.code(), false),
        None => {
            // Timeout: gesamte Prozessgruppe hart beenden.
            let pgid = child.id() as libc::pid_t;
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
            let _ = child.wait();
            (None, true)
        }
    };

    let stdout = String::from_utf8_lossy(&out_handle.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_handle.join().unwrap_or_default()).into_owned();

    if timed_out {
        return Err(AppError::Timeout);
    }

    Ok(CliOutput {
        code,
        stdout,
        stderr,
    })
}
