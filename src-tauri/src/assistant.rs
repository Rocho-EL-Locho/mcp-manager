//! Claude-Assistent: „Server per Link einrichten".
//!
//! Ruft headless `claude -p` mit read-only Tools (WebFetch/WebSearch) auf, lässt
//! die Quelle (Repo/npm/PyPI/Doku) inspizieren und einen Konfigurationsvorschlag
//! als JSON erzeugen. Es wird NICHTS geschrieben – der Vorschlag landet im
//! Formular zur Bestätigung durch den Nutzer.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::claude_cli::{resolve_claude, run_claude};
use crate::models::{AppError, ServerEntry};

const ASSISTANT_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Default, Deserialize)]
struct Candidate {
    name: Option<String>,
    #[serde(rename = "type")]
    transport: Option<String>,
    command: Option<String>,
    args: Option<Vec<String>>,
    env: Option<BTreeMap<String, String>>,
    url: Option<String>,
    headers: Option<BTreeMap<String, String>>,
    notes: Option<String>,
}

/// Ergebnis für das Frontend. `entry`/`name` befüllen das Formular; bei Fehlern
/// bleibt `raw` zur Einsicht.
#[derive(Debug, Serialize)]
pub struct AssistantResult {
    pub name: Option<String>,
    pub entry: Option<ServerEntry>,
    pub notes: Option<String>,
    pub raw: String,
    pub error: Option<String>,
}

fn build_prompt(url: &str, extra: Option<&str>) -> String {
    let extra = extra.map(|e| format!("\nZusätzlicher Kontext des Nutzers: {e}\n")).unwrap_or_default();
    format!(
        "You are configuring an MCP (Model Context Protocol) server for Claude Code.\n\
         Inspect this source and figure out how its MCP server is started: {url}\n{extra}\
         Use WebFetch/WebSearch to read the README and docs. Determine the exact launch method.\n\
         Output ONLY a single JSON object (no prose, no markdown fences) with these keys:\n\
         - name: suggested server name (lowercase-kebab)\n\
         - type: \"stdio\" | \"http\" | \"sse\"\n\
         - command: for stdio, the executable (e.g. npx, uvx, docker, node)\n\
         - args: array of arguments for stdio\n\
         - env: object of REQUIRED environment variables; include the KEYS with EMPTY string values, NEVER invent secret values\n\
         - url: for http/sse servers\n\
         - headers: object for http/sse\n\
         - notes: short string explaining how you determined this and which secrets the user must fill in\n\
         Use null for keys that do not apply. For stdio use command/args/env; for http/sse use url/headers.\n\
         Do not include any real secret values."
    )
}

/// Extrahiert das erste balancierte JSON-Objekt aus einem Text (toleriert Prosa/Fences).
fn extract_json_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = text.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for i in start..bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else {
            match c {
                '"' => in_str = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&text[start..=i]);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Holt aus der `--output-format json`-Hülle den eigentlichen Ergebnistext.
fn unwrap_result_text(stdout: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) {
        if let Some(r) = v.get("result") {
            return match r.as_str() {
                Some(s) => s.to_string(),
                None => r.to_string(),
            };
        }
    }
    stdout.to_string()
}

pub fn run_assistant(url: &str, extra_context: Option<&str>) -> Result<AssistantResult, AppError> {
    if url.trim().is_empty() {
        return Err(AppError::Io("Bitte einen Link angeben.".into()));
    }
    let claude = resolve_claude().ok_or(AppError::ClaudeNotFound)?;
    let prompt = build_prompt(url, extra_context);

    // -p = Print-Flag (boolean), Prompt ist Positional; --allowedTools ist
    // variadisch und steht daher am Ende.
    let out = run_claude(
        &claude,
        &[
            "-p",
            &prompt,
            "--output-format",
            "json",
            "--permission-mode",
            "default",
            "--allowedTools",
            "WebFetch",
            "WebSearch",
        ],
        None,
        ASSISTANT_TIMEOUT,
    )?;

    let raw = if out.stdout.trim().is_empty() {
        out.stderr.clone()
    } else {
        out.stdout.clone()
    };

    if !out.success() {
        return Ok(AssistantResult {
            name: None,
            entry: None,
            notes: None,
            raw,
            error: Some(format!("claude beendete mit {:?}", out.code)),
        });
    }

    let result_text = unwrap_result_text(&out.stdout);
    let candidate: Option<Candidate> =
        extract_json_object(&result_text).and_then(|j| serde_json::from_str(j).ok());

    match candidate {
        Some(c) => {
            // Absicherung gegen Leaks aus gefetchten Seiten: env/headers-Werte
            // NIE vorbefüllen. Keys bleiben erhalten, Werte werden geleert – der
            // Nutzer füllt Secrets selbst im Formular. (raw/notes bleiben, da
            // hinter einem Toggle.)
            let env = c.env.map(|m| m.into_keys().map(|k| (k, String::new())).collect());
            let headers = c
                .headers
                .map(|m| m.into_keys().map(|k| (k, String::new())).collect());
            let entry = ServerEntry {
                transport: c.transport,
                command: c.command,
                args: c.args,
                env,
                url: c.url,
                headers,
            };
            Ok(AssistantResult {
                name: c.name,
                entry: Some(entry),
                notes: c.notes,
                raw,
                error: None,
            })
        }
        None => Ok(AssistantResult {
            name: None,
            entry: None,
            notes: None,
            raw,
            error: Some("Konnte keine Konfiguration aus der Antwort extrahieren.".into()),
        }),
    }
}
