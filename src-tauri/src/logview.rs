//! Live-Diagnose-Session für einen stdio-Server (Feature 08).
//!
//! Startet den Server als **eigene, langlebige Instanz** (nicht den Prozess, den
//! Claude Code benutzt), streamt stderr und den JSON-RPC-Verkehr live als
//! Tauri-Events (`mcp-log`) und hält einen Ring der letzten [`RING_CAPACITY`]
//! Zeilen (Backfill beim Wieder-Öffnen). Eine Session gleichzeitig; harter
//! killpg beim Stop, Timeout und App-Exit (kein Zombie-npx).
//!
//! Architektur: zwei Reader-Threads (stderr/stdout) und der Handshake schieben
//! `(kind, text)` in einen Channel; ein Emitter-Thread batcht ~[`BATCH_INTERVAL`],
//! redigiert jede Zeile, schreibt den Ring und emittiert einen Batch. Ein
//! Monitor-Thread wartet auf das Prozessende und emittiert `closed`.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use crate::mask::redact_secrets;
use crate::models::{AppError, ServerEntry};

/// Maximale Zeilen im Ring (Backfill + Speicherobergrenze).
const RING_CAPACITY: usize = 2000;
/// Automatischer Stop nach dieser Laufzeit.
const SESSION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Sammelfenster für Emit-Batches (verhindert Webview-Einfrieren bei Flut).
const BATCH_INTERVAL: Duration = Duration::from_millis(100);
/// Höchstens so viele Zeilen pro Emit-Batch.
const MAX_BATCH: usize = 200;
/// Einzelne Zeilen kappen (Schutz gegen riesige JSON-/Log-Zeilen).
const MAX_LINE: usize = 8192;
/// Kapazität des Reader→Emitter-Channels. Voll ⇒ Reader blockiert (Backpressure):
/// ein flutender Server wird gedrosselt statt den Speicher zu sprengen.
const CHANNEL_CAPACITY: usize = 4096;
/// Event-Kanal (nur eine Session gleichzeitig → fester Name; `seq` dedupliziert).
pub const EVENT_NAME: &str = "mcp-log";
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Eine Trace-/Log-Zeile.
#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub seq: u64,
    pub ts: u64,
    /// `stderr | rpc_out | rpc_in | stdout | closed`
    pub kind: String,
    pub text: String,
}

/// Handle einer laufenden Session (im `AppState` unter der Session-Id gehalten).
pub struct LogSessionHandle {
    pgid: i32,
    stop: Arc<AtomicBool>,
    ring: Arc<Mutex<VecDeque<LogLine>>>,
    /// stdin offen halten – ein Drop gäbe dem Server EOF auf stdin (viele beenden
    /// sich dann). Wird beim Kill mit der Prozessgruppe ohnehin geschlossen.
    _stdin: ChildStdin,
}

impl LogSessionHandle {
    /// Aktueller Ring-Inhalt (Backfill beim Wieder-Öffnen der Ansicht).
    pub fn buffer(&self) -> Vec<LogLine> {
        self.ring
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Beendet die gesamte Prozessgruppe hart (idempotent).
    pub fn kill(&self) {
        self.stop.store(true, Ordering::SeqCst);
        unsafe {
            libc::killpg(self.pgid, libc::SIGKILL);
        }
    }
}

fn truncate_line(mut s: String) -> String {
    if s.len() > MAX_LINE {
        s.truncate(MAX_LINE);
        s.push_str("… (gekürzt)");
    }
    s
}

/// Klassifiziert eine stdout-Zeile: gültiges JSON-RPC (`result`/`error`/`method`)
/// ⇒ `rpc_in`, sonst rohe Ausgabe ⇒ `stdout`.
fn classify_stdout(line: &str) -> &'static str {
    match serde_json::from_str::<Value>(line) {
        Ok(v)
            if v.get("result").is_some()
                || v.get("error").is_some()
                || v.get("method").is_some() =>
        {
            "rpc_in"
        }
        _ => "stdout",
    }
}

/// Liest eine Pipe zeilenweise (lossy UTF-8, gekappt) und schiebt jede nicht-leere
/// Zeile mit dem von `classify` bestimmten Kind in den Channel.
fn spawn_reader<R: Read + Send + 'static>(
    pipe: R,
    tx: SyncSender<(String, String)>,
    classify: impl Fn(&str) -> &'static str + Send + 'static,
) {
    std::thread::spawn(move || {
        let mut buf = BufReader::new(pipe);
        let mut bytes: Vec<u8> = Vec::new();
        loop {
            bytes.clear();
            match buf.read_until(b'\n', &mut bytes) {
                Ok(0) => break, // EOF (Prozess/Pipe zu)
                Ok(_) => {
                    let text = String::from_utf8_lossy(&bytes)
                        .trim_end_matches(['\r', '\n'])
                        .to_string();
                    if text.is_empty() {
                        continue;
                    }
                    let kind = classify(&text).to_string();
                    if tx.send((kind, truncate_line(text))).is_err() {
                        break; // Emitter weg
                    }
                }
                Err(_) => break,
            }
        }
    });
}

fn push_ring_and_emit(
    ring: &Mutex<VecDeque<LogLine>>,
    on_batch: &dyn Fn(Vec<LogLine>),
    batch: Vec<LogLine>,
) {
    {
        let mut r = ring.lock().unwrap_or_else(|e| e.into_inner());
        for line in &batch {
            if r.len() == RING_CAPACITY {
                r.pop_front();
            }
            r.push_back(line.clone());
        }
    }
    on_batch(batch);
}

fn send_line(stdin: &mut ChildStdin, msg: &Value) {
    if let Ok(mut line) = serde_json::to_string(msg) {
        line.push('\n');
        let _ = stdin.write_all(line.as_bytes());
        let _ = stdin.flush();
    }
}

/// Startet eine Diagnose-Session für einen stdio-`entry`. Jede (redigierte,
/// gebatchte) Zeilengruppe wird an `on_batch` gereicht (Produktion: `app.emit`;
/// Tests: Sammler) – so ist `logview` von Tauri entkoppelt und testbar. Gibt das
/// Handle zurück (der Aufrufer legt es im `AppState` ab).
pub fn start(
    entry: &ServerEntry,
    on_batch: impl Fn(Vec<LogLine>) + Send + 'static,
) -> Result<LogSessionHandle, AppError> {
    let command = entry
        .command
        .as_ref()
        .ok_or_else(|| AppError::Io("Diagnose-Session nur für stdio-Server".into()))?;

    let mut cmd = Command::new(command);
    if let Some(args) = &entry.args {
        cmd.args(args);
    }
    if let Some(env) = &entry.env {
        for (k, v) in env {
            cmd.env(k, v);
        }
    }
    cmd.process_group(0);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::Io(format!("Befehl nicht gefunden: {command}"))
        } else {
            AppError::Io(e.to_string())
        }
    })?;

    let pgid = child.id() as i32;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::Io("stdin nicht verfügbar".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Io("stdout nicht verfügbar".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Io("stderr nicht verfügbar".into()))?;

    let ring = Arc::new(Mutex::new(VecDeque::with_capacity(RING_CAPACITY)));
    let stop = Arc::new(AtomicBool::new(false));
    let seq = Arc::new(AtomicU64::new(0));

    let (tx, rx) = mpsc::sync_channel::<(String, String)>(CHANNEL_CAPACITY);

    // Emitter-Thread: batcht, redigiert, schreibt Ring, reicht an on_batch.
    {
        let ring = ring.clone();
        let seq = seq.clone();
        std::thread::spawn(move || loop {
            let mut batch: Vec<LogLine> = Vec::new();
            let deadline = Instant::now() + BATCH_INTERVAL;
            loop {
                let timeout = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(timeout) {
                    Ok((kind, text)) => {
                        batch.push(LogLine {
                            seq: seq.fetch_add(1, Ordering::SeqCst),
                            ts: crate::introspect::unix_now(),
                            kind,
                            text: redact_secrets(&text),
                        });
                        if batch.len() >= MAX_BATCH || Instant::now() >= deadline {
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => {
                        if !batch.is_empty() {
                            push_ring_and_emit(&ring, &on_batch, batch);
                        }
                        return; // alle Sender weg -> Session vorbei
                    }
                }
            }
            if !batch.is_empty() {
                push_ring_and_emit(&ring, &on_batch, batch);
            }
        });
    }

    // Reader-Threads.
    spawn_reader(stderr, tx.clone(), |_| "stderr");
    spawn_reader(stdout, tx.clone(), classify_stdout);

    // Handshake fire-and-forget: senden + als rpc_out loggen; Antworten kommen
    // automatisch über den stdout-Reader als rpc_in.
    let init = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "mcp-manager", "version": env!("CARGO_PKG_VERSION") },
        }
    });
    let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    send_line(&mut stdin, &init);
    let _ = tx.send(("rpc_out".into(), init.to_string()));
    send_line(&mut stdin, &initialized);
    let _ = tx.send(("rpc_out".into(), initialized.to_string()));

    // Monitor-Thread: besitzt den Child, wartet aufs Ende, meldet `closed`.
    {
        let stop = stop.clone();
        let tx_close = tx.clone();
        std::thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code());
            let text = if stop.load(Ordering::SeqCst) {
                "Diagnose-Session beendet.".to_string()
            } else {
                match code {
                    Some(c) => format!("Prozess beendet (Code {c})."),
                    None => "Prozess beendet.".to_string(),
                }
            };
            let _ = tx_close.send(("closed".into(), text));
            // tx_close fällt hier weg; die Reader-tx fallen bei Pipe-EOF weg
            // -> Emitter-Thread erhält Disconnected und endet.
        });
    }

    // Watchdog: nach SESSION_TIMEOUT killen, falls nicht schon gestoppt.
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + SESSION_TIMEOUT;
            while Instant::now() < deadline {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            if !stop.swap(true, Ordering::SeqCst) {
                unsafe {
                    libc::killpg(pgid, libc::SIGKILL);
                }
            }
        });
    }

    // Original-`tx` hier fallen lassen (Reader/Monitor haben Klone), damit der
    // Channel schließt, sobald alle Threads enden.
    drop(tx);

    Ok(LogSessionHandle {
        pgid,
        stop,
        ring,
        _stdin: stdin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_stdout_distinguishes_rpc() {
        assert_eq!(
            classify_stdout(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
            "rpc_in"
        );
        assert_eq!(
            classify_stdout(r#"{"jsonrpc":"2.0","method":"x/y"}"#),
            "rpc_in"
        );
        assert_eq!(
            classify_stdout(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-1}}"#),
            "rpc_in"
        );
        assert_eq!(classify_stdout("Server läuft auf Port 3000"), "stdout");
        assert_eq!(classify_stdout("{ kaputtes json"), "stdout");
    }

    #[test]
    fn truncate_caps_long_lines() {
        let short = "kurz".to_string();
        assert_eq!(truncate_line(short.clone()), short);
        let long = "a".repeat(MAX_LINE + 500);
        let out = truncate_line(long);
        assert!(out.len() <= MAX_LINE + 32);
        assert!(out.ends_with("(gekürzt)"));
    }

    /// Opt-in (startet einen echten Prozess): Session gegen ein sh-Skript starten,
    /// das eine initialize-Antwort + stderr ausgibt und dann auf stdin wartet.
    /// Prüft, dass Zeilen im Ring landen und der Prozess nach `kill()` weg ist
    /// (kein Zombie). Nur mit `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn session_start_collects_and_kills() {
        use std::sync::atomic::AtomicUsize;

        let entry = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("sh".into()),
            args: Some(vec![
                "-c".into(),
                // initialize beantworten, etwas auf stderr, dann offen bleiben (stdin lesen).
                "read a; printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\\n'; \
                 echo 'server bereit' >&2; cat >/dev/null"
                    .into(),
            ]),
            ..Default::default()
        };

        let count = Arc::new(AtomicUsize::new(0));
        let c2 = count.clone();
        let handle = start(&entry, move |lines| {
            c2.fetch_add(lines.len(), Ordering::SeqCst);
        })
        .expect("start");

        // Kurz warten, bis Handshake + stderr durch den 100-ms-Batcher sind.
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            count.load(Ordering::SeqCst) >= 2,
            "es sollten Zeilen geflossen sein"
        );
        assert!(!handle.buffer().is_empty(), "Ring darf nicht leer sein");

        let pgid = handle.pgid;
        handle.kill();
        std::thread::sleep(Duration::from_millis(300));
        // Prozessgruppe ist weg: killpg mit Signal 0 schlägt fehl (ESRCH).
        let alive = unsafe { libc::killpg(pgid, 0) } == 0;
        assert!(
            !alive,
            "Prozessgruppe {pgid} sollte beendet sein (kein Zombie)"
        );
    }

    #[test]
    fn ring_push_trims_to_capacity() {
        // Ring-Trimm-Logik isoliert nachbauen (wie in push_ring_and_emit).
        let mut r: VecDeque<u64> = VecDeque::with_capacity(RING_CAPACITY);
        for i in 0..(RING_CAPACITY as u64 + 50) {
            if r.len() == RING_CAPACITY {
                r.pop_front();
            }
            r.push_back(i);
        }
        assert_eq!(r.len(), RING_CAPACITY);
        assert_eq!(*r.front().unwrap(), 50);
        assert_eq!(*r.back().unwrap(), RING_CAPACITY as u64 + 49);
    }
}
