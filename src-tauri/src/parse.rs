//! Toleranter Parser für die menschenlesbare Ausgabe von `claude mcp list`.
//!
//! Zeilenformat: `NAME: <cmd-or-url> [(TYPE)] - <STATUS>`.
//! Bewusst robust: Namen enthalten Leerzeichen/Doppelpunkte (z. B.
//! `claude.ai Microsoft 365`, `plugin:github:github`), der Status wird über
//! bekannte Textbausteine erkannt (nicht über Glyphen, die je nach Terminal
//! variieren). Eine unparsbare Zeile wird zu `Unknown` – niemals ein Abbruch.

use crate::models::{ListItem, ServerStatus};

/// Bekannte Status-Textbausteine (Reihenfolge = Priorität bei Prüfung).
fn detect_status(rest: &str) -> ServerStatus {
    let lower = rest.to_ascii_lowercase();
    if lower.contains("needs authentication") {
        ServerStatus::NeedsAuth
    } else if lower.contains("pending approval") {
        ServerStatus::PendingApproval
    } else if lower.contains("failed to connect") || lower.contains("failed") {
        ServerStatus::Failed { detail: None }
    } else if lower.contains("connected") {
        ServerStatus::Connected
    } else {
        ServerStatus::Unknown
    }
}

/// Erkennt den Status aus der Ausgabe von `claude mcp get`.
///
/// Bevorzugt die Zeile, die (getrimmt) mit "Status:" beginnt, und wertet
/// detect_status NUR auf dieser Zeile aus. So wird ein verbundener Server mit
/// "failed"/"pending approval"/"needs authentication" im Pfad/Namen nicht mehr
/// falsch klassifiziert. Existiert keine solche Zeile, wird auf den Gesamttext
/// zurückgefallen.
pub fn status_from_text(text: &str) -> ServerStatus {
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("Status:") {
            return detect_status(rest);
        }
    }
    detect_status(text)
}

/// Trennt die Status-Angabe von der Zusammenfassung ab.
/// Sucht das letzte " - " und behandelt den Rest als Statustext.
fn split_summary_status(rest: &str) -> (String, ServerStatus) {
    if let Some(pos) = rest.rfind(" - ") {
        let summary = rest[..pos].trim().to_string();
        let status_text = &rest[pos + 3..];
        let status = detect_status(status_text);
        // Nur abtrennen, wenn wir den Status auch erkannt haben – sonst ganze
        // Zeile als summary behalten (verhindert falsches Abschneiden bei URLs
        // mit " - " im Pfad).
        if !matches!(status, ServerStatus::Unknown) {
            return (summary, status);
        }
    }
    (rest.trim().to_string(), detect_status(rest))
}

/// Parst die komplette stdout von `claude mcp list`.
pub fn parse_list(stdout: &str) -> Vec<ListItem> {
    let mut items = Vec::new();
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // Kopfzeile des Health-Checks überspringen.
        if line.starts_with("Checking MCP server health") {
            continue;
        }
        // Am ERSTEN ": " splitten (Name kann Doppelpunkte enthalten).
        let Some(idx) = line.find(": ") else {
            continue;
        };
        let name = line[..idx].trim().to_string();
        let rest = line[idx + 2..].trim();
        if name.is_empty() {
            continue;
        }
        let (summary, status) = split_summary_status(rest);
        items.push(ListItem {
            name,
            summary,
            status,
        });
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_output() {
        let input = "Checking MCP server health…\n\n\
            freecad: /usr/bin/python3 /home/x/bridge.py - √ Connected\n\
            grafana-home: https://mcp.example/sse (SSE) - × Failed to connect\n\
            claude.ai Notion: https://mcp.notion.com/mcp - ! Needs authentication\n\
            plugin:github:github: https://api.githubcopilot.com/mcp/ (HTTP) - × Failed to connect\n\
            blender: uvx blender-mcp - ⏸ Pending approval (run `claude` to approve)\n";
        let items = parse_list(input);
        assert_eq!(items.len(), 5);
        assert_eq!(items[0].name, "freecad");
        assert!(matches!(items[0].status, ServerStatus::Connected));
        assert!(matches!(items[1].status, ServerStatus::Failed { .. }));
        assert_eq!(items[2].name, "claude.ai Notion");
        assert!(matches!(items[2].status, ServerStatus::NeedsAuth));
        assert_eq!(items[3].name, "plugin:github:github");
        assert!(matches!(items[4].status, ServerStatus::PendingApproval));
    }
}
