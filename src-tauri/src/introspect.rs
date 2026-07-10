//! Minimaler MCP-Client für die **Introspektion** eines stdio-Servers.
//!
//! Das Backend spricht sonst kein MCP – es delegiert an die `claude`-CLI. Für
//! Issue #7 (anzeigen, was ein Server bereitstellt) genügt ein kurzer,
//! handgeschriebener JSON-RPC-2.0-Handshake über stdin/stdout, ganz ohne neue
//! Abhängigkeiten:
//!   initialize -> notifications/initialized -> tools/list / resources/list /
//!   prompts/list.
//!
//! Prozess-Handling analog zu `claude_cli.rs`: eigene Prozessgruppe (killpg beim
//! Aufräumen/Timeout), stderr wird nebenläufig geleert (kein Pipe-Deadlock),
//! stdout wird zeilenweise über einen Channel gelesen. Nur stdio wird
//! unterstützt; HTTP/SSE behandelt der Command-Layer separat.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::models::{Introspection, McpPrompt, McpResource, McpTool, ServerEntry};

/// Protokoll-Version, die wir im Handshake anbieten. Server, die eine andere
/// Version fahren, antworten dennoch auf die read-only Listen-Aufrufe.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Obergrenze für `nextCursor`-Paginierung, damit ein fehlerhafter Server die
/// Schleife nicht unendlich laufen lässt.
const MAX_PAGES: usize = 10;

/// Obergrenze für die insgesamt von stdout gelesenen Bytes. Schützt davor, dass
/// ein fehlerhafter/bösartiger Server durch eine riesige (ggf. zeilenlose)
/// Ausgabe unbegrenzt Speicher belegt (OOM).
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Obergrenze für den erfassten stderr-Auszug (Diagnose). Klein gehalten – das
/// UI zeigt nur den relevanten Start-/Fehlertext, kein volles Log.
const STDERR_CAP: u64 = 64 * 1024;

/// Ausgang eines einzelnen JSON-RPC-Requests.
enum RpcOutcome {
    /// Erfolgreiches `result`-Objekt.
    Result(Value),
    /// JSON-RPC-`error` (z. B. „Method not found") als lesbare Meldung.
    RpcError(String),
}

/// Introspiziert einen stdio-Server. `entry.command` muss gesetzt sein.
///
/// Gibt IMMER eine `Introspection` zurück: Start-/Handshake-Fehler landen in
/// `error` (und ggf. erfasster stderr in `logs`), statt als `Err` verloren zu
/// gehen. So kann die Detail-Ansicht den echten Fehlergrund zeigen.
pub fn introspect_stdio(entry: &ServerEntry, timeout: Duration) -> Introspection {
    let mut notes: Vec<String> = Vec::new();
    let Some(command) = entry.command.as_ref() else {
        return error_introspection("Kein command für stdio-Introspektion".into(), None, notes);
    };

    let started = Instant::now();
    let deadline = started + timeout;

    // --- Prozess starten -----------------------------------------------------
    let mut cmd = Command::new(command);
    if let Some(args) = &entry.args {
        cmd.args(args);
    }
    // Konfigurierte env über die geerbte Umgebung legen (npx/uvx/docker brauchen PATH).
    if let Some(env) = &entry.env {
        for (k, v) in env {
            cmd.env(k, v);
        }
    }
    cmd.process_group(0);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = if e.kind() == std::io::ErrorKind::NotFound {
                format!("Befehl nicht gefunden: {command}")
            } else {
                e.to_string()
            };
            return error_introspection(msg, None, notes);
        }
    };

    let Some(mut stdin) = child.stdin.take() else {
        cleanup(&mut child);
        return error_introspection("stdin nicht verfügbar".into(), None, notes);
    };

    // stderr nebenläufig lesen und einmalig über einen Channel bereitstellen.
    // Bewusst NICHT gejoint (detached), analog zum stdout-Reader: ein Kindeskind,
    // das die Pipe erbt und offen hält, könnte join() sonst blockieren.
    //
    // WICHTIG: Die Pipe wird VOLLSTÄNDIG geleert – behalten wird aber nur der erste
    // STDERR_CAP-Anteil. Würde man nach STDERR_CAP aufhören zu lesen (z. B.
    // `take(STDERR_CAP)`), blockierte ein gesprächiger Server beim Schreiben, sobald
    // der Pipe-Puffer voll ist; er antwortete dann nicht mehr auf stdout und der
    // Handshake liefe fälschlich in den Timeout. Weiterlesen schützt zugleich vor OOM
    // (Überschuss wird verworfen, nicht akkumuliert).
    let (err_tx, err_rx) = mpsc::channel::<Vec<u8>>();
    if let Some(mut err_pipe) = child.stderr.take() {
        std::thread::spawn(move || {
            let cap = STDERR_CAP as usize;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let mut truncated = false;
            loop {
                match err_pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if buf.len() < cap {
                            let take = (cap - buf.len()).min(n);
                            buf.extend_from_slice(&chunk[..take]);
                            if take < n {
                                truncated = true;
                            }
                        } else {
                            truncated = true;
                        }
                        // Überschuss bewusst verwerfen, aber weiterlesen (Pipe leeren).
                    }
                }
            }
            if truncated {
                buf.extend_from_slice(b"\n\xe2\x80\xa6 (stderr gekuerzt)");
            }
            let _ = err_tx.send(buf);
        });
    }

    // stdout zeilenweise in einen Channel lesen. `take(MAX_RESPONSE_BYTES)`
    // deckelt den Gesamtspeicher (OOM-Schutz gegen riesige/zeilenlose Ausgaben).
    let Some(stdout) = child.stdout.take() else {
        cleanup(&mut child);
        return error_introspection("stdout nicht verfügbar".into(), None, notes);
    };
    let (tx, rx) = mpsc::channel::<String>();
    // Bewusst NICHT gejoint (detached): killpg schließt zwar die Pipe, aber ein
    // Kindeskind, das die Pipe erbt und offen hält, könnte join() sonst blockieren.
    // Der Thread endet von selbst bei EOF, Byte-Limit oder wenn der Receiver wegfällt.
    std::thread::spawn(move || {
        let mut buf = BufReader::new(stdout.take(MAX_RESPONSE_BYTES));
        let mut line = String::new();
        loop {
            line.clear();
            match buf.read_line(&mut line) {
                Ok(0) => break, // EOF oder Byte-Limit erreicht
                Ok(_) => {
                    if tx.send(line.clone()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // --- Handshake -----------------------------------------------------------
    // `connect_ms` wird im Handshake gesetzt, sobald `initialize` beantwortet ist
    // (Prozessstart bis initialize = echte Verbindungs-/Startzeit).
    let mut connect_ms: Option<u64> = None;
    let result = run_handshake(&mut stdin, &rx, &deadline, started, &mut connect_ms, &mut notes);

    // stdin schließen, Prozessgruppe hart beenden. Der Reader-Thread wird nicht
    // gejoint (siehe oben); rx fällt beim Verlassen der Funktion weg.
    drop(stdin);
    cleanup(&mut child);

    // Erfassten stderr einsammeln: killpg hat die Pipe geschlossen -> der Reader
    // sieht EOF und sendet. Kurzer, gedeckelter Wait; hält ein Kindeskind die Pipe
    // offen, gibt es eben keine Logs (best effort). Roh belassen – die zentrale
    // Maskierung (`mask_introspection`) redigiert vor Verlassen des Backends.
    let logs = err_rx
        .recv_timeout(Duration::from_millis(300))
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
        .filter(|s| !s.is_empty());

    match result {
        Ok((tools, resources, prompts, server_name, server_version)) => Introspection {
            tools,
            resources,
            prompts,
            server_name,
            server_version,
            notes,
            logs,
            error: None,
            connect_ms,
            introspected_at: unix_now(),
        },
        Err(e) => {
            // `initialize` kann bereits gelungen sein (connect_ms gesetzt), bevor ein
            // späterer Listen-Aufruf scheiterte. Messung erhalten – gerade langsame
            // Server (Issue-Use-Case) laufen so ggf. erst nach dem Handshake ins Timeout.
            let mut intro = error_introspection(e.to_string(), logs, notes);
            intro.connect_ms = connect_ms;
            intro
        }
    }
}

/// Baut eine `Introspection` für einen Fehlerfall: leere Listen, `error` gesetzt,
/// ggf. erfasster stderr in `logs`, bereits gesammelte `notes` beibehalten.
fn error_introspection(error: String, logs: Option<String>, notes: Vec<String>) -> Introspection {
    Introspection {
        tools: Vec::new(),
        resources: Vec::new(),
        prompts: Vec::new(),
        server_name: None,
        server_version: None,
        notes,
        logs,
        error: Some(error),
        connect_ms: None,
        introspected_at: unix_now(),
    }
}

type HandshakeData = (
    Vec<McpTool>,
    Vec<McpResource>,
    Vec<McpPrompt>,
    Option<String>,
    Option<String>,
);

/// Führt initialize + Listen-Aufrufe durch. Nutzt `notes` für nicht-fatale Hinweise.
/// `started`/`connect_ms`: sobald `initialize` beantwortet ist, wird die bis dahin
/// verstrichene Zeit (Prozessstart bis initialize) als Verbindungs-/Startzeit gesetzt.
fn run_handshake(
    stdin: &mut ChildStdin,
    rx: &Receiver<String>,
    deadline: &Instant,
    started: Instant,
    connect_ms: &mut Option<u64>,
    notes: &mut Vec<String>,
) -> Result<HandshakeData, crate::models::AppError> {
    let mut next_id: i64 = 1;

    // 1) initialize
    let init_params = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "mcp-manager", "version": env!("CARGO_PKG_VERSION") },
    });
    let init = match request(stdin, rx, &mut next_id, deadline, "initialize", init_params)? {
        RpcOutcome::Result(v) => v,
        RpcOutcome::RpcError(msg) => {
            return Err(crate::models::AppError::Io(format!(
                "initialize fehlgeschlagen: {msg}"
            )))
        }
    };
    // Verbindungs-/Startzeit: Prozessstart bis erfolgreiche initialize-Antwort.
    *connect_ms = Some(started.elapsed().as_millis() as u64);

    let server_name = init
        .get("serverInfo")
        .and_then(|s| s.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_string);
    let server_version = init
        .get("serverInfo")
        .and_then(|s| s.get("version"))
        .and_then(|n| n.as_str())
        .map(str::to_string);

    // 2) notifications/initialized (Notification, ohne id)
    notify(stdin, "notifications/initialized")?;

    // 3) Listen abrufen. Wir fragen bewusst alle drei ab (statt uns nur auf die
    //    angekündigten capabilities zu verlassen) – reale Server deklarieren nicht
    //    immer sauber, und nicht unterstützte Methoden landen ohnehin sauber als
    //    Notiz (RPC-„Method not found").
    let mut tools = Vec::new();
    let mut resources = Vec::new();
    let mut prompts = Vec::new();

    collect_pages(stdin, rx, &mut next_id, deadline, "tools/list", "tools", notes, |item| {
        tools.push(parse_tool(item));
    })?;
    collect_pages(stdin, rx, &mut next_id, deadline, "resources/list", "resources", notes, |item| {
        resources.push(parse_resource(item));
    })?;
    collect_pages(stdin, rx, &mut next_id, deadline, "prompts/list", "prompts", notes, |item| {
        prompts.push(parse_prompt(item));
    })?;

    Ok((tools, resources, prompts, server_name, server_version))
}

/// Ruft eine Listen-Methode ggf. über mehrere `nextCursor`-Seiten ab und reicht
/// jedes Element an `sink`. RPC-Fehler werden als Notiz vermerkt (nicht fatal).
#[allow(clippy::too_many_arguments)]
fn collect_pages(
    stdin: &mut ChildStdin,
    rx: &Receiver<String>,
    next_id: &mut i64,
    deadline: &Instant,
    method: &str,
    field: &str,
    notes: &mut Vec<String>,
    mut sink: impl FnMut(&Value),
) -> Result<(), crate::models::AppError> {
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let params = match &cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        match request(stdin, rx, next_id, deadline, method, params)? {
            RpcOutcome::Result(res) => {
                if let Some(arr) = res.get(field).and_then(|v| v.as_array()) {
                    for item in arr {
                        sink(item);
                    }
                }
                cursor = res
                    .get("nextCursor")
                    .and_then(|c| c.as_str())
                    .map(str::to_string);
                if cursor.is_none() {
                    return Ok(());
                }
            }
            RpcOutcome::RpcError(msg) => {
                notes.push(format!("{method} nicht verfügbar: {msg}"));
                return Ok(());
            }
        }
    }
    notes.push(format!("{method}: Paginierung nach {MAX_PAGES} Seiten abgebrochen."));
    Ok(())
}

/// Sendet einen Request und wartet (bis `deadline`) auf die Antwort mit passender id.
fn request(
    stdin: &mut ChildStdin,
    rx: &Receiver<String>,
    next_id: &mut i64,
    deadline: &Instant,
    method: &str,
    params: Value,
) -> Result<RpcOutcome, crate::models::AppError> {
    let id = *next_id;
    *next_id += 1;
    let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    send(stdin, &msg)?;
    read_response(rx, id, deadline)
}

/// Sendet eine Notification (kein `id`, keine Antwort erwartet).
fn notify(stdin: &mut ChildStdin, method: &str) -> Result<(), crate::models::AppError> {
    let msg = json!({ "jsonrpc": "2.0", "method": method });
    send(stdin, &msg)
}

fn send(stdin: &mut ChildStdin, msg: &Value) -> Result<(), crate::models::AppError> {
    let mut line = serde_json::to_string(msg).map_err(|e| crate::models::AppError::Parse(e.to_string()))?;
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .map_err(|e| crate::models::AppError::Io(e.to_string()))?;
    stdin
        .flush()
        .map_err(|e| crate::models::AppError::Io(e.to_string()))
}

/// Liest Zeilen bis zur Antwort mit `id`; überspringt Nicht-JSON, Notifications
/// und fremde Antworten. Timeout/Disconnect werden zu `AppError`.
fn read_response(
    rx: &Receiver<String>,
    id: i64,
    deadline: &Instant,
) -> Result<RpcOutcome, crate::models::AppError> {
    loop {
        let now = Instant::now();
        if now >= *deadline {
            return Err(crate::models::AppError::Timeout);
        }
        match rx.recv_timeout(*deadline - now) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
                    continue; // Nicht-JSON (z. B. versehentliche Log-Zeile) ignorieren.
                };
                if !id_matches(val.get("id"), id) {
                    continue; // Notification oder fremde Antwort.
                }
                if let Some(err) = val.get("error") {
                    let msg = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unbekannter RPC-Fehler")
                        .to_string();
                    return Ok(RpcOutcome::RpcError(msg));
                }
                return Ok(RpcOutcome::Result(val.get("result").cloned().unwrap_or(Value::Null)));
            }
            Err(RecvTimeoutError::Timeout) => return Err(crate::models::AppError::Timeout),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(crate::models::AppError::Io(
                    "MCP-Server hat die Verbindung geschlossen".into(),
                ))
            }
        }
    }
}

/// Prüft, ob die `id` einer Antwort zu unserer gesendeten (numerischen) `id`
/// passt. JSON-RPC erlaubt Zahl oder String; wir senden Zahlen, tolerieren aber
/// eine als String zurückgegebene id (nicht-spec-treue Server).
fn id_matches(value: Option<&Value>, id: i64) -> bool {
    match value {
        Some(Value::Number(n)) => n.as_i64() == Some(id),
        Some(Value::String(s)) => s == &id.to_string(),
        _ => false,
    }
}

/// Beendet die gesamte Prozessgruppe hart (SIGKILL) und erntet den Prozess.
fn cleanup(child: &mut Child) {
    let pgid = child.id() as libc::pid_t;
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
    let _ = child.wait();
}

fn str_field(item: &Value, key: &str) -> Option<String> {
    item.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn parse_tool(item: &Value) -> McpTool {
    McpTool {
        name: str_field(item, "name").unwrap_or_default(),
        description: str_field(item, "description"),
        input_schema: item.get("inputSchema").cloned(),
    }
}

fn parse_resource(item: &Value) -> McpResource {
    McpResource {
        uri: str_field(item, "uri").unwrap_or_default(),
        name: str_field(item, "name"),
        description: str_field(item, "description"),
        mime_type: str_field(item, "mimeType"),
    }
}

fn parse_prompt(item: &Value) -> McpPrompt {
    McpPrompt {
        name: str_field(item, "name").unwrap_or_default(),
        description: str_field(item, "description"),
    }
}

/// Unix-Zeitstempel (Sekunden), 0 bei Uhr-Fehlern. Auch vom Command-Layer genutzt.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ServerEntry;

    /// Opt-in-Integrationstest: echter Handshake gegen den offiziellen
    /// „everything"-Testserver (via npx, netz-/toolchain-abhängig, lädt beim
    /// ersten Lauf herunter). Nur mit `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn introspect_everything_server() {
        let entry = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("npx".into()),
            args: Some(vec![
                "-y".into(),
                "@modelcontextprotocol/server-everything".into(),
            ]),
            ..Default::default()
        };
        let intro = introspect_stdio(&entry, Duration::from_secs(60));
        assert!(intro.error.is_none(), "Handshake sollte gelingen: {:?}", intro.error);
        assert!(
            intro.connect_ms.is_some(),
            "erfolgreicher Handshake muss connect_ms setzen"
        );
        eprintln!(
            "server={:?} v{:?} | {} tools, {} resources, {} prompts | connect={:?} ms",
            intro.server_name,
            intro.server_version,
            intro.tools.len(),
            intro.resources.len(),
            intro.prompts.len(),
            intro.connect_ms,
        );
        for n in &intro.notes {
            eprintln!("note: {n}");
        }
        assert!(!intro.tools.is_empty(), "everything-Server sollte Tools liefern");
    }

    /// Deterministisch (kein Netz): ein Prozess, der sofort nach stderr schreibt
    /// und mit Fehler endet. Der Handshake schlägt fehl -> `error` gesetzt, der
    /// stderr-Text landet (roh) in `logs`. Redaction passiert erst im Command-Layer.
    #[test]
    fn introspect_captures_stderr_on_failure() {
        let entry = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("sh".into()),
            args: Some(vec![
                "-c".into(),
                "echo 'token=ghp_ABC123def456ghi789 boom' >&2; exit 1".into(),
            ]),
            ..Default::default()
        };
        let intro = introspect_stdio(&entry, Duration::from_secs(5));
        assert!(intro.error.is_some(), "fehlgeschlagener Start muss error setzen");
        assert!(intro.tools.is_empty());
        assert!(
            intro.connect_ms.is_none(),
            "ohne initialize darf keine connect_ms gesetzt sein"
        );
        let logs = intro.logs.expect("stderr sollte erfasst sein");
        assert!(logs.contains("boom"), "stderr-Text fehlt: {logs}");
    }

    /// Deterministisch (kein Netz): der Server beantwortet `initialize`, schließt
    /// aber danach stdout, sodass der folgende `tools/list`-Aufruf scheitert. Der
    /// Handshake endet mit `error` – die vor dem Fehler gemessene Verbindungs-/
    /// Startzeit (`connect_ms`) muss dennoch erhalten bleiben.
    #[test]
    fn introspect_keeps_connect_ms_when_listing_fails() {
        let entry = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("sh".into()),
            args: Some(vec![
                "-c".into(),
                // Erste Zeile (initialize-Request) lesen, gültige Antwort mit id=1
                // senden, dann enden -> stdout schließt, tools/list läuft ins Leere.
                "read line; printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"serverInfo\":{\"name\":\"t\",\"version\":\"1\"}}}\\n'"
                    .into(),
            ]),
            ..Default::default()
        };
        let intro = introspect_stdio(&entry, Duration::from_secs(5));
        assert!(intro.error.is_some(), "Folgefehler nach initialize muss error setzen");
        assert!(
            intro.connect_ms.is_some(),
            "connect_ms muss trotz Folgefehler erhalten bleiben"
        );
    }

    /// Nicht existierendes Kommando -> `error` mit „Befehl nicht gefunden", keine Logs.
    #[test]
    fn introspect_missing_command() {
        let entry = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("mcpmgr-nonexistent-binary-xyz".into()),
            ..Default::default()
        };
        let intro = introspect_stdio(&entry, Duration::from_secs(5));
        assert!(
            intro.error.as_deref().unwrap_or_default().contains("Befehl nicht gefunden"),
            "error: {:?}",
            intro.error
        );
        assert!(intro.logs.is_none());
    }

    /// Regression: ein Kommando, das WEIT mehr als STDERR_CAP nach stderr schreibt
    /// (und nichts nach stdout). Der Reader muss die Pipe komplett leeren – sonst
    /// blockierte der Schreiber und der Handshake liefe erst in den Timeout. Beweis
    /// über die Laufzeit (deutlich unter dem Timeout) und den gedeckelten Auszug.
    #[test]
    fn introspect_drains_oversized_stderr() {
        // 20000 Zeilen à 11 Bytes ≈ 220 KiB, rein POSIX-sh (keine externen Tools).
        let entry = ServerEntry {
            transport: Some("stdio".into()),
            command: Some("sh".into()),
            args: Some(vec![
                "-c".into(),
                "i=0; while [ $i -lt 20000 ]; do echo AAAAAAAAAA >&2; i=$((i+1)); done; exit 1"
                    .into(),
            ]),
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let intro = introspect_stdio(&entry, Duration::from_secs(20));
        // Muss klar vor dem 20-s-Timeout fertig sein (kein Backpressure-Deadlock).
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "verdächtig langsam ({:?}) – stderr wurde evtl. nicht geleert",
            started.elapsed()
        );
        assert!(intro.error.is_some());
        // Behaltener Auszug ist gedeckelt (Cap + kurze Kürzungs-Marke).
        let logs = intro.logs.unwrap_or_default();
        assert!(
            logs.len() <= STDERR_CAP as usize + 64,
            "Auszug nicht gedeckelt: {} Bytes",
            logs.len()
        );
    }
}
