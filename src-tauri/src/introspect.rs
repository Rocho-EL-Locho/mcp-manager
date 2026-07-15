//! Minimaler MCP-Client für die **Introspektion** eines Servers (stdio + HTTP/SSE).
//!
//! Das Backend spricht sonst kein MCP – es delegiert an die `claude`-CLI. Für
//! „anzeigen, was ein Server bereitstellt" genügt ein kurzer, handgeschriebener
//! JSON-RPC-2.0-Handshake:
//!   initialize -> notifications/initialized -> tools/list / resources/list /
//!   prompts/list.
//!
//! Die Handshake-/Auswertungslogik ist transportneutral (`RpcTransport`-Trait);
//! es gibt zwei Transporte:
//!   - **stdio** (`StdioTransport`/`introspect_stdio`): Subprozess, eigene
//!     Prozessgruppe (killpg beim Aufräumen/Timeout), stderr nebenläufig geleert
//!     (kein Pipe-Deadlock), stdout zeilenweise über einen Channel.
//!   - **HTTP/SSE** (`HttpTransport`/`introspect_http`, Feature 06): Streamable
//!     HTTP gemäß MCP-Spec 2025-06-18 (POST je Nachricht, JSON- oder SSE-Antwort,
//!     `Mcp-Session-Id`), synchron via `ureq`.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::models::{AppError, Introspection, McpPrompt, McpResource, McpTool, ServerEntry};

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

/// Transport eines JSON-RPC-Austauschs. Damit sind Handshake und Auswertung
/// (initialize, Listen, Pagination) für stdio und HTTP/SSE identisch – eine
/// Quelle der Wahrheit. `dyn`-tauglich (nur `&mut self`-Methoden).
trait RpcTransport {
    /// Sendet ein Request-Objekt und liefert die Antwort mit passender `id`.
    fn exchange(
        &mut self,
        msg: &Value,
        id: i64,
        deadline: &Instant,
    ) -> Result<RpcOutcome, AppError>;
    /// Sendet eine Notification (kein `id`, keine Antwort erwartet).
    fn notification(&mut self, msg: &Value) -> Result<(), AppError>;
}

/// Wertet ein JSON-RPC-Antwortobjekt aus (result vs. error) – transportneutral.
fn parse_rpc_response(val: &Value) -> RpcOutcome {
    if let Some(err) = val.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unbekannter RPC-Fehler")
            .to_string();
        return RpcOutcome::RpcError(msg);
    }
    RpcOutcome::Result(val.get("result").cloned().unwrap_or(Value::Null))
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

    let Some(stdin) = child.stdin.take() else {
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
    let result = {
        // stdin + rx in den Transport verschieben; am Blockende wird er gedroppt
        // -> stdin schließt (EOF ans Kind), rx fällt weg.
        let mut transport = StdioTransport { stdin, rx };
        run_handshake(
            &mut transport,
            &deadline,
            started,
            &mut connect_ms,
            &mut notes,
        )
    };

    // Prozessgruppe hart beenden. Der Reader-Thread wird nicht gejoint (siehe oben).
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
    transport: &mut dyn RpcTransport,
    deadline: &Instant,
    started: Instant,
    connect_ms: &mut Option<u64>,
    notes: &mut Vec<String>,
) -> Result<HandshakeData, AppError> {
    let mut next_id: i64 = 1;

    // 1) initialize
    let init_params = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "mcp-manager", "version": env!("CARGO_PKG_VERSION") },
    });
    let init = match request(transport, &mut next_id, deadline, "initialize", init_params)? {
        RpcOutcome::Result(v) => v,
        RpcOutcome::RpcError(msg) => {
            return Err(AppError::Io(format!("initialize fehlgeschlagen: {msg}")))
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
    notify(transport, "notifications/initialized")?;

    // 3) Listen abrufen. Wir fragen bewusst alle drei ab (statt uns nur auf die
    //    angekündigten capabilities zu verlassen) – reale Server deklarieren nicht
    //    immer sauber, und nicht unterstützte Methoden landen ohnehin sauber als
    //    Notiz (RPC-„Method not found").
    let mut tools = Vec::new();
    let mut resources = Vec::new();
    let mut prompts = Vec::new();

    collect_pages(
        transport,
        &mut next_id,
        deadline,
        "tools/list",
        "tools",
        notes,
        |item| {
            tools.push(parse_tool(item));
        },
    )?;
    collect_pages(
        transport,
        &mut next_id,
        deadline,
        "resources/list",
        "resources",
        notes,
        |item| {
            resources.push(parse_resource(item));
        },
    )?;
    collect_pages(
        transport,
        &mut next_id,
        deadline,
        "prompts/list",
        "prompts",
        notes,
        |item| {
            prompts.push(parse_prompt(item));
        },
    )?;

    Ok((tools, resources, prompts, server_name, server_version))
}

/// Ruft eine Listen-Methode ggf. über mehrere `nextCursor`-Seiten ab und reicht
/// jedes Element an `sink`. RPC-Fehler werden als Notiz vermerkt (nicht fatal).
fn collect_pages(
    transport: &mut dyn RpcTransport,
    next_id: &mut i64,
    deadline: &Instant,
    method: &str,
    field: &str,
    notes: &mut Vec<String>,
    mut sink: impl FnMut(&Value),
) -> Result<(), AppError> {
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let params = match &cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        match request(transport, next_id, deadline, method, params)? {
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
    notes.push(format!(
        "{method}: Paginierung nach {MAX_PAGES} Seiten abgebrochen."
    ));
    Ok(())
}

/// Baut ein Request-Objekt (mit fortlaufender id) und führt den Austausch über
/// den Transport aus.
fn request(
    transport: &mut dyn RpcTransport,
    next_id: &mut i64,
    deadline: &Instant,
    method: &str,
    params: Value,
) -> Result<RpcOutcome, AppError> {
    let id = *next_id;
    *next_id += 1;
    let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    transport.exchange(&msg, id, deadline)
}

/// Sendet eine Notification (kein `id`, keine Antwort erwartet).
fn notify(transport: &mut dyn RpcTransport, method: &str) -> Result<(), AppError> {
    let msg = json!({ "jsonrpc": "2.0", "method": method });
    transport.notification(&msg)
}

// ---------------------------------------------------------------------------
// stdio-Transport
// ---------------------------------------------------------------------------

/// stdio-Transport: newline-delimited JSON über stdin/stdout des Subprozesses.
/// Besitzt stdin und den stdout-Zeilen-Receiver; beim Drop schließt stdin (EOF).
struct StdioTransport {
    stdin: ChildStdin,
    rx: Receiver<String>,
}

impl RpcTransport for StdioTransport {
    fn exchange(
        &mut self,
        msg: &Value,
        id: i64,
        deadline: &Instant,
    ) -> Result<RpcOutcome, AppError> {
        send(&mut self.stdin, msg)?;
        read_response(&self.rx, id, deadline)
    }

    fn notification(&mut self, msg: &Value) -> Result<(), AppError> {
        send(&mut self.stdin, msg)
    }
}

fn send(stdin: &mut ChildStdin, msg: &Value) -> Result<(), AppError> {
    let mut line = serde_json::to_string(msg).map_err(|e| AppError::Parse(e.to_string()))?;
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .map_err(|e| AppError::Io(e.to_string()))?;
    stdin.flush().map_err(|e| AppError::Io(e.to_string()))
}

/// Liest Zeilen bis zur Antwort mit `id`; überspringt Nicht-JSON, Notifications
/// und fremde Antworten. Timeout/Disconnect werden zu `AppError`.
fn read_response(
    rx: &Receiver<String>,
    id: i64,
    deadline: &Instant,
) -> Result<RpcOutcome, AppError> {
    loop {
        let now = Instant::now();
        if now >= *deadline {
            return Err(AppError::Timeout);
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
                return Ok(parse_rpc_response(&val));
            }
            Err(RecvTimeoutError::Timeout) => return Err(AppError::Timeout),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(AppError::Io(
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

// ---------------------------------------------------------------------------
// HTTP/SSE-Transport (Streamable HTTP, MCP-Spec 2025-06-18)
// ---------------------------------------------------------------------------

/// HTTP-Transport: jede JSON-RPC-Nachricht ist ein POST an den MCP-Endpoint.
/// Antworten kommen als einzelnes JSON-Objekt oder als SSE-Stream zurück.
struct HttpTransport {
    agent: ureq::Agent,
    url: String,
    /// Zusätzliche, vom Nutzer konfigurierte Header (z. B. Authorization).
    headers: Vec<(String, String)>,
    /// Vom Server bei `initialize` vergebene Session-Id (danach mitgesendet).
    session_id: Option<String>,
}

impl HttpTransport {
    /// Setzt Protokoll-Version, Session-Id (falls vorhanden) und die
    /// konfigurierten Header auf einen Request.
    fn apply_headers(&self, mut req: ureq::Request) -> ureq::Request {
        req = req.set("MCP-Protocol-Version", PROTOCOL_VERSION);
        if let Some(sid) = &self.session_id {
            req = req.set("Mcp-Session-Id", sid);
        }
        for (k, v) in &self.headers {
            req = req.set(k, v);
        }
        req
    }

    /// Session am Ende best effort schließen (Server darf 405 antworten).
    fn close_session(&self) {
        if self.session_id.is_none() {
            return;
        }
        let req = self.apply_headers(self.agent.delete(&self.url));
        let _ = req.call();
    }
}

impl RpcTransport for HttpTransport {
    fn exchange(
        &mut self,
        msg: &Value,
        id: i64,
        deadline: &Instant,
    ) -> Result<RpcOutcome, AppError> {
        let body = serde_json::to_string(msg).map_err(|e| AppError::Parse(e.to_string()))?;
        let req = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream");
        let resp = match self.apply_headers(req).send_string(&body) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, _)) => return Err(http_status_error(code)),
            Err(e) => return Err(AppError::Io(format!("HTTP-Verbindungsfehler: {e}"))),
        };

        // Bei redirects(0) liefert ureq einen 3xx als Ok zurück – nicht folgen,
        // sondern als klaren Fehler melden (kein Header-Leak an ein Redirect-Ziel).
        if (300..=399).contains(&resp.status()) {
            return Err(http_status_error(resp.status()));
        }

        // Session-Id beim ersten Mal übernehmen.
        if self.session_id.is_none() {
            if let Some(sid) = resp.header("Mcp-Session-Id") {
                self.session_id = Some(sid.to_string());
            }
        }

        if resp.content_type().contains("event-stream") {
            read_sse_response(resp, id, deadline)
        } else {
            let mut buf = String::new();
            resp.into_reader()
                .take(MAX_RESPONSE_BYTES)
                .read_to_string(&mut buf)
                .map_err(|e| AppError::Io(e.to_string()))?;
            let val: Value = serde_json::from_str(buf.trim())
                .map_err(|e| AppError::Parse(format!("ungültige JSON-Antwort: {e}")))?;
            // Fehlerantworten (dürfen laut Spec id=null tragen) durchreichen; eine
            // Erfolgsantwort mit fremder id ablehnen (Konsistenz mit stdio/SSE).
            if val.get("error").is_some() || id_matches(val.get("id"), id) {
                Ok(parse_rpc_response(&val))
            } else {
                Err(AppError::Io("Antwort-id passt nicht zum Request".into()))
            }
        }
    }

    fn notification(&mut self, msg: &Value) -> Result<(), AppError> {
        let body = serde_json::to_string(msg).map_err(|e| AppError::Parse(e.to_string()))?;
        let req = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream");
        match self.apply_headers(req).send_string(&body) {
            Ok(resp) if (300..=399).contains(&resp.status()) => {
                Err(http_status_error(resp.status()))
            }
            Ok(_) => Ok(()), // 2xx (v. a. 202 Accepted)
            Err(ureq::Error::Status(code, _)) => Err(http_status_error(code)),
            Err(e) => Err(AppError::Io(format!("HTTP-Verbindungsfehler: {e}"))),
        }
    }
}

/// Übersetzt HTTP-Fehlerstatus in verständliche Meldungen (kein Stacktrace).
fn http_status_error(code: u16) -> AppError {
    match code {
        401 | 403 => AppError::Io("Authentifizierung erforderlich".into()),
        404 => AppError::Io("HTTP 404 – MCP-Endpoint nicht gefunden".into()),
        405 => AppError::Io("HTTP 405 – Methode am Endpoint nicht erlaubt".into()),
        300..=399 => AppError::Io(format!(
            "Server antwortete mit Redirect ({code}); wird aus Sicherheitsgründen nicht gefolgt \
             (konfigurierte Header könnten sonst an ein fremdes Ziel gelangen). Bitte die \
             endgültige URL direkt konfigurieren."
        )),
        _ => AppError::Io(format!("HTTP-Fehlerstatus {code}")),
    }
}

/// Liest einen SSE-Antwortstrom, bis das JSON-RPC-Objekt mit passender `id`
/// auftaucht. Sammelt `data:`-Zeilen je Event; respektiert Byte-Limit/Deadline.
fn read_sse_response(
    resp: ureq::Response,
    id: i64,
    deadline: &Instant,
) -> Result<RpcOutcome, AppError> {
    let mut reader = BufReader::new(resp.into_reader().take(MAX_RESPONSE_BYTES));
    let mut data = String::new();
    let mut line = String::new();

    // Versucht, das aktuell gesammelte `data` als passende Antwort zu deuten.
    let try_data = |data: &str| -> Option<RpcOutcome> {
        let t = data.trim();
        if t.is_empty() {
            return None;
        }
        let val = serde_json::from_str::<Value>(t).ok()?;
        if id_matches(val.get("id"), id) {
            Some(parse_rpc_response(&val))
        } else {
            None
        }
    };

    loop {
        if Instant::now() >= *deadline {
            return Err(AppError::Timeout);
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                // Stream-Ende: letztes Event (ohne abschließende Leerzeile) prüfen.
                if let Some(outcome) = try_data(&data) {
                    return Ok(outcome);
                }
                return Err(AppError::Io(
                    "SSE-Stream endete ohne passende Antwort".into(),
                ));
            }
            Ok(_) => {
                let l = line.trim_end_matches(['\r', '\n']);
                if l.is_empty() {
                    // Event-Ende.
                    if let Some(outcome) = try_data(&data) {
                        return Ok(outcome);
                    }
                    data.clear();
                } else if let Some(rest) = l.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
                }
                // event:/id:/retry:/Kommentarzeilen (":") werden ignoriert.
            }
            Err(_) => return Err(AppError::Io("Fehler beim Lesen des SSE-Streams".into())),
        }
    }
}

/// Introspiziert einen HTTP/SSE-Server (Streamable HTTP). `entry.url` muss
/// gesetzt sein. Gibt – wie stdio – IMMER eine `Introspection` zurück.
pub fn introspect_http(entry: &ServerEntry, timeout: Duration) -> Introspection {
    let mut notes: Vec<String> = Vec::new();
    let Some(url) = entry
        .url
        .as_ref()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
    else {
        return error_introspection("Keine URL für HTTP-Introspektion".into(), None, notes);
    };
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return error_introspection(format!("Ungültige HTTP(S)-URL: {url}"), None, notes);
    }

    let started = Instant::now();
    let deadline = started + timeout;
    // Redirects bewusst ABschalten: MCP-Endpoints sind exakte URLs. Würde ureq
    // Redirects folgen, könnte es die konfigurierten Header (inkl. Authorization)
    // an ein fremdes Ziel bzw. über ein http-Downgrade weitersenden. Ein 3xx wird
    // so als Fehler mit klarer Meldung sichtbar (http_status_error).
    let agent = ureq::AgentBuilder::new()
        .timeout(timeout)
        .redirects(0)
        .build();
    let headers: Vec<(String, String)> = entry
        .headers
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();

    let mut transport = HttpTransport {
        agent,
        url,
        headers,
        session_id: None,
    };
    let mut connect_ms: Option<u64> = None;
    let result = run_handshake(
        &mut transport,
        &deadline,
        started,
        &mut connect_ms,
        &mut notes,
    );

    transport.close_session();

    match result {
        Ok((tools, resources, prompts, server_name, server_version)) => Introspection {
            tools,
            resources,
            prompts,
            server_name,
            server_version,
            notes,
            logs: None,
            error: None,
            connect_ms,
            introspected_at: unix_now(),
        },
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("Authentifizierung") {
                notes.push(
                    "Es werden nur die konfigurierten Header gesendet; OAuth-Token der \
                     claude-CLI sind hier nicht nutzbar. Ggf. über die Anmelden-Funktion \
                     einloggen oder einen Authorization-Header setzen."
                        .into(),
                );
            }
            // Legacy-SSE-Entscheidung (siehe PR): reines GET-SSE wird nicht unterstützt.
            if entry.transport.as_deref() == Some("sse") {
                notes.push(
                    "Reines Legacy-SSE (GET-Stream mit endpoint-Event) wird nicht \
                     unterstützt – nur Streamable HTTP (POST)."
                        .into(),
                );
            }
            let mut intro = error_introspection(msg, None, notes);
            intro.connect_ms = connect_ms;
            intro
        }
    }
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
        assert!(
            intro.error.is_none(),
            "Handshake sollte gelingen: {:?}",
            intro.error
        );
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
        assert!(
            !intro.tools.is_empty(),
            "everything-Server sollte Tools liefern"
        );
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
        assert!(
            intro.error.is_some(),
            "fehlgeschlagener Start muss error setzen"
        );
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
        assert!(
            intro.error.is_some(),
            "Folgefehler nach initialize muss error setzen"
        );
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
            intro
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("Befehl nicht gefunden"),
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

#[cfg(test)]
mod http_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    /// Antwort, die der Test-HTTP-Server auf einen Request gibt. Der Server baut
    /// aus `Json`/`Sse` selbst die JSON-RPC-Hülle mit der echten Request-`id`.
    enum Reply {
        /// 200 application/json mit `{jsonrpc,id,result}`.
        Json(Value),
        /// 200 application/json + `Mcp-Session-Id`-Header.
        JsonWithSession(Value, String),
        /// 200 text/event-stream mit einem `data:`-Frame `{jsonrpc,id,result}`.
        Sse(Value),
        /// 202 ohne Body.
        Accepted,
        /// Fehlerstatus ohne verwertbaren Body.
        Status(u16),
        /// 200 application/json mit exakt diesem Body (keine id-Einsetzung).
        Raw(String),
    }

    /// Startet einen minimalen HTTP-Server (ein Request pro Verbindung dank
    /// `Connection: close`). Der Handler bekommt HTTP-Methode, das geparste
    /// Request-JSON und ob ein `Mcp-Session-Id`-Header vorhanden war.
    fn spawn_server<F>(handler: F) -> String
    where
        F: Fn(&str, &Value, bool) -> Reply + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handler = Arc::new(handler);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut http_method = String::new();
                let mut content_length = 0usize;
                let mut has_session = false;
                let mut first = true;
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let t = line.trim_end();
                    if first {
                        http_method = t.split_whitespace().next().unwrap_or("").to_string();
                        first = false;
                    }
                    if t.is_empty() {
                        break; // Header-Ende
                    }
                    let lower = t.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                    if lower.starts_with("mcp-session-id:") {
                        has_session = true;
                    }
                }
                let mut body = vec![0u8; content_length];
                if content_length > 0 {
                    let _ = reader.read_exact(&mut body);
                }
                let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let id = req.get("id").cloned().unwrap_or(json!(0));

                let reply = handler(&http_method, &req, has_session);
                let (status, ctype, resp_body, session): (u16, &str, String, Option<String>) =
                    match reply {
                        Reply::Json(result) => (
                            200,
                            "application/json",
                            json!({"jsonrpc":"2.0","id":id,"result":result}).to_string(),
                            None,
                        ),
                        Reply::JsonWithSession(result, sid) => (
                            200,
                            "application/json",
                            json!({"jsonrpc":"2.0","id":id,"result":result}).to_string(),
                            Some(sid),
                        ),
                        Reply::Sse(result) => {
                            let obj = json!({"jsonrpc":"2.0","id":id,"result":result});
                            (200, "text/event-stream", format!("data: {obj}\n\n"), None)
                        }
                        Reply::Accepted => (202, "text/plain", String::new(), None),
                        Reply::Status(code) => (code, "application/json", String::new(), None),
                        Reply::Raw(body) => (200, "application/json", body, None),
                    };

                let mut head = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    resp_body.len()
                );
                if let Some(sid) = session {
                    head.push_str(&format!("Mcp-Session-Id: {sid}\r\n"));
                }
                head.push_str("\r\n");
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(resp_body.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://{addr}/mcp")
    }

    fn http_entry(url: &str) -> ServerEntry {
        ServerEntry {
            transport: Some("http".into()),
            url: Some(url.into()),
            ..Default::default()
        }
    }

    #[test]
    fn http_json_session_and_pagination() {
        let url = spawn_server(|http_method, req, has_session| {
            if http_method == "DELETE" {
                return Reply::Accepted;
            }
            match req.get("method").and_then(|m| m.as_str()).unwrap_or("") {
                "initialize" => Reply::JsonWithSession(
                    json!({"serverInfo": {"name": "srv", "version": "9"}}),
                    "sess-1".into(),
                ),
                "notifications/initialized" => Reply::Accepted,
                "tools/list" => {
                    // Verlangt die Session-Id auf Folge-Requests.
                    if !has_session {
                        return Reply::Status(400);
                    }
                    let has_cursor = req.get("params").and_then(|p| p.get("cursor")).is_some();
                    if has_cursor {
                        Reply::Json(json!({"tools": [{"name": "b"}]}))
                    } else {
                        Reply::Json(json!({"tools": [{"name": "a"}], "nextCursor": "p2"}))
                    }
                }
                "resources/list" => Reply::Json(json!({"resources": []})),
                "prompts/list" => Reply::Json(json!({"prompts": []})),
                _ => Reply::Status(404),
            }
        });

        let intro = introspect_http(&http_entry(&url), Duration::from_secs(10));
        assert!(
            intro.error.is_none(),
            "kein Fehler erwartet: {:?}",
            intro.error
        );
        assert_eq!(intro.server_name.as_deref(), Some("srv"));
        assert!(intro.connect_ms.is_some(), "connect_ms muss gesetzt sein");
        // Zwei Seiten -> zwei Tools; beweist Session-Weitergabe (sonst 400) + Pagination.
        let names: Vec<&str> = intro.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn http_sse_response_is_parsed() {
        let url = spawn_server(|_m, req, _s| {
            match req.get("method").and_then(|m| m.as_str()).unwrap_or("") {
                "initialize" => Reply::JsonWithSession(
                    json!({"serverInfo": {"name": "sse-srv", "version": "1"}}),
                    "s".into(),
                ),
                "notifications/initialized" => Reply::Accepted,
                "tools/list" => Reply::Sse(json!({"tools": [{"name": "streamed"}]})),
                "resources/list" => Reply::Sse(json!({"resources": []})),
                "prompts/list" => Reply::Sse(json!({"prompts": []})),
                _ => Reply::Status(404),
            }
        });

        let intro = introspect_http(&http_entry(&url), Duration::from_secs(10));
        assert!(intro.error.is_none(), "kein Fehler: {:?}", intro.error);
        assert_eq!(intro.tools.len(), 1);
        assert_eq!(intro.tools[0].name, "streamed");
    }

    #[test]
    fn http_401_reports_auth_required() {
        let url = spawn_server(|_m, _req, _s| Reply::Status(401));
        let intro = introspect_http(&http_entry(&url), Duration::from_secs(10));
        assert!(
            intro
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("Authentifizierung erforderlich"),
            "error: {:?}",
            intro.error
        );
        assert!(
            intro
                .notes
                .iter()
                .any(|n| n.contains("konfigurierten Header")),
            "Hinweis auf Header/OAuth erwartet: {:?}",
            intro.notes
        );
    }

    #[test]
    fn http_redirect_is_not_followed() {
        // Server antwortet auf initialize mit einem Redirect (302). Der Client darf
        // dem NICHT folgen (sonst Header-Leak) und muss klar fehlschlagen.
        let url = spawn_server(|_m, _req, _s| Reply::Status(302));
        let intro = introspect_http(&http_entry(&url), Duration::from_secs(10));
        assert!(
            intro
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("Redirect"),
            "Redirect-Fehler erwartet: {:?}",
            intro.error
        );
    }

    #[test]
    fn http_wrong_id_is_rejected() {
        // initialize ok, aber tools/list liefert eine Antwort mit falscher id.
        let url = spawn_server(|_m, req, _s| {
            match req.get("method").and_then(|m| m.as_str()).unwrap_or("") {
                "initialize" => {
                    Reply::JsonWithSession(json!({"serverInfo": {"name": "x"}}), "s".into())
                }
                "notifications/initialized" => Reply::Accepted,
                // Rohe JSON-Antwort mit fest falscher id (nicht die des Requests):
                "tools/list" => Reply::Raw(
                    r#"{"jsonrpc":"2.0","id":999,"result":{"tools":[{"name":"x"}]}}"#.into(),
                ),
                _ => Reply::Json(json!({"resources": [], "prompts": []})),
            }
        });
        let intro = introspect_http(&http_entry(&url), Duration::from_secs(10));
        assert!(
            intro
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("id passt nicht"),
            "Fremd-id sollte abgelehnt werden: {:?}",
            intro.error
        );
    }

    #[test]
    fn http_timeout_yields_error() {
        // Server liest den Request, wartet dann länger als das Timeout.
        let url = spawn_server(|_m, _req, _s| {
            std::thread::sleep(Duration::from_secs(3));
            Reply::Json(json!({"serverInfo": {}}))
        });
        let intro = introspect_http(&http_entry(&url), Duration::from_secs(1));
        assert!(intro.error.is_some(), "Timeout muss einen Fehler liefern");
    }

    /// Opt-in-Smoke gegen einen echten öffentlichen MCP-Endpoint. Netzabhängig,
    /// daher `#[ignore]`. Mit `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn http_smoke_deepwiki() {
        let entry = http_entry("https://mcp.deepwiki.com/mcp");
        let intro = introspect_http(&entry, Duration::from_secs(30));
        eprintln!(
            "server={:?} v{:?} | {} tools | connect={:?} ms | error={:?}",
            intro.server_name,
            intro.server_version,
            intro.tools.len(),
            intro.connect_ms,
            intro.error,
        );
        for n in &intro.notes {
            eprintln!("note: {n}");
        }
        assert!(
            intro.error.is_none(),
            "Handshake sollte gelingen: {:?}",
            intro.error
        );
        assert!(!intro.tools.is_empty(), "deepwiki sollte Tools liefern");
    }
}
