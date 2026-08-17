//! Abstraktion für **dateibasierte** MCP-Clients (Claude Desktop; später Cursor,
//! VS Code, …). Solche Clients haben keine CLI: ihre Konfiguration wird direkt,
//! atomar und snapshot-gesichert geschrieben.
//!
//! Claude **Code** wird bewusst NICHT in diesen Trait gepresst — es hat Scopes,
//! eine CLI und einen Stash; eine erzwungene Vereinheitlichung würde beide
//! Seiten verbiegen.

pub mod claude_desktop;

use std::path::PathBuf;

use serde::Serialize;

use crate::models::{AppError, ServerEntry};

/// Was ein Client kann. Steuert Transport-Auswahl im Formular, die Ziel-Optionen
/// beim Kopieren und die Prüfung in `validate_entry`.
///
/// Bewusst auf das eine Merkmal beschränkt, das heute tatsächlich unterschieden
/// wird: stdio kann jeder Datei-Client (sonst gäbe es die Datei nicht), und ob
/// ein Server ein-/ausschaltbar ist, entscheidet weiterhin der Scope
/// (`main.ts::canToggle`). Weitere Fähigkeiten kommen mit Feature 17, sobald ein
/// zweiter Adapter sie wirklich anders beantwortet.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ClientCaps {
    /// Remote-Server (url/headers/type != stdio) werden aus der Datei geladen.
    pub remote: bool,
}

/// Ein erkannter Client für die Seitenleiste.
#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub id: String,
    pub label: String,
    pub config_path: String,
    pub server_count: usize,
    pub caps: ClientCaps,
    /// Gesetzt, wenn die Konfigurationsdatei existiert, aber nicht lesbar ist
    /// (von Hand kaputt editiert). Die Ansicht zeigt dann statt der Liste einen
    /// Fehler — geschrieben wird in diesem Zustand nie.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
}

/// Ein dateibasierter MCP-Client.
pub trait ClientAdapter {
    /// Stabile Id, z. B. "claude-desktop". Schlüssel aller Client-Commands.
    fn id(&self) -> &'static str;
    /// Anzeigename, z. B. "Claude Desktop".
    fn label(&self) -> &'static str;
    /// Config-Pfad, wenn der Client installiert ist — sonst `None`.
    /// Legt niemals etwas an.
    fn detect(&self) -> Option<PathBuf>;
    fn list(&self) -> Result<Vec<(String, ServerEntry)>, AppError>;
    fn upsert(&self, name: &str, entry: &ServerEntry) -> Result<(), AppError>;
    fn remove(&self, name: &str) -> Result<(), AppError>;
    fn capabilities(&self) -> ClientCaps;
}

/// Alle bekannten Datei-Clients (unabhängig davon, ob sie installiert sind).
pub fn adapters() -> Vec<Box<dyn ClientAdapter>> {
    vec![Box::new(claude_desktop::ClaudeDesktop::new())]
}

/// Adapter zu einer Id. Unbekannte Id ist ein Fehler (kein stiller Fallback —
/// sonst schriebe ein Tippfehler in einen fremden Client).
pub fn adapter(id: &str) -> Result<Box<dyn ClientAdapter>, AppError> {
    adapters()
        .into_iter()
        .find(|a| a.id() == id)
        .ok_or_else(|| AppError::Io(format!("Unbekannter Client: {id}")))
}

pub fn validate_entry(entry: &ServerEntry, caps: ClientCaps) -> Result<(), AppError> {
    // Eine einzige Quelle für „ist das ein Remote-Server?": `transport_of`
    // (Spiegel im Frontend: `src/transport.ts::transportOfEntry`). Eine eigene
    // Regel an dieser Stelle würde das Frontend Ziel-Optionen freischalten
    // lassen, die das Backend danach mit anderer Begründung ablehnt.
    let is_remote = crate::commands::transport_of(entry) != "stdio";
    // Spiegel der Begründung: `src/views/serverPicker.ts::remoteBlocked` — dort
    // erklärt derselbe Text vorab, warum Remote-Vorlagen ausgegraut sind.
    // Änderungen bitte auf beiden Seiten (wie `transport_of` ↔ `transport.ts`).
    if !caps.remote && is_remote {
        return Err(AppError::Io(
            "Claude Desktop lädt Remote-Server nicht aus der Konfigurationsdatei – dort als \
             Connector einrichten. Alternative: als stdio-Server über die Bridge \
             „npx mcp-remote <url>“."
                .into(),
        ));
    }

    // Unvollständige Definitionen gar nicht erst in die Datei lassen: stdio
    // braucht einen Befehl, Remote eine URL.
    let nonempty = |v: Option<&String>| v.map(|s| s.trim()).is_some_and(|s| !s.is_empty());
    if is_remote {
        if !nonempty(entry.url.as_ref()) {
            return Err(AppError::Io("URL darf nicht leer sein".into()));
        }
    } else if !nonempty(entry.command.as_ref()) {
        return Err(AppError::Io("Command darf nicht leer sein".into()));
    }
    Ok(())
}

/// Darf ein **bestehender** Eintrag dieses Namens überschrieben werden?
///
/// Nein, wenn er von Hand als Remote-Server eingetragen wurde und der Client
/// Remote gar nicht aus der Datei lädt: das Formular zeigt für diesen Client
/// keine Remote-Felder an, `buildEntry()` verwürfe `url`/`headers` — der Eintrag
/// würde beim Speichern still zu einem stdio-Server umgeschrieben und die echte
/// Definition wäre weg.
///
/// Bewusst getrennt von [`validate_entry`], das den **neuen** Wert prüft, und
/// bewusst an zwei Stellen aufgerufen: in `commands::update_client_server` VOR
/// dem Auto-Snapshot (sonst entstünde ein Waisen-Snapshot für eine Aktion, die
/// gleich darauf abgelehnt wird) und in `ClientAdapter::upsert` als letzte
/// Schranke unmittelbar vor dem Schreiben.
pub fn ensure_overwritable(
    name: &str,
    existing: &ServerEntry,
    label: &str,
    caps: ClientCaps,
) -> Result<(), AppError> {
    // Dieselbe eine Quelle für „ist das ein Remote-Server?“ wie in
    // `validate_entry`.
    if !caps.remote && crate::commands::transport_of(existing) != "stdio" {
        return Err(AppError::Io(format!(
            "Server „{name}“ ist in {label} von Hand als Remote-Server eingetragen. \
             {label} lädt aus der Datei nur stdio-Server; der Eintrag wird deshalb \
             nicht überschrieben – bitte direkt in der Datei bereinigen."
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn caps_stdio_only() -> ClientCaps {
        ClientCaps { remote: false }
    }

    fn stdio_entry() -> ServerEntry {
        ServerEntry {
            command: Some("npx".into()),
            args: Some(vec!["-y".into(), "server".into()]),
            ..Default::default()
        }
    }

    #[test]
    fn adapters_enthaelt_claude_desktop() {
        assert!(adapters().iter().any(|a| a.id() == "claude-desktop"));
    }

    #[test]
    fn adapter_unbekannte_id_ist_fehler() {
        assert!(adapter("gibt-es-nicht").is_err());
        assert!(adapter("claude-desktop").is_ok());
    }

    #[test]
    fn validate_entry_akzeptiert_stdio() {
        assert!(validate_entry(&stdio_entry(), caps_stdio_only()).is_ok());
        // Explizites type: "stdio" ist ebenfalls in Ordnung.
        let mut e = stdio_entry();
        e.transport = Some("stdio".into());
        assert!(validate_entry(&e, caps_stdio_only()).is_ok());
    }

    #[test]
    fn validate_entry_lehnt_remote_ab_wenn_client_es_nicht_kann() {
        let mit_url = ServerEntry {
            url: Some("https://example.test/mcp".into()),
            ..Default::default()
        };

        let mut mit_typ = stdio_entry();
        mit_typ.transport = Some("http".into());
        mit_typ.url = Some("https://example.test/mcp".into());

        for (fall, entry) in [("url", mit_url), ("type", mit_typ)] {
            let err = validate_entry(&entry, caps_stdio_only())
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("Connector"),
                "{fall}: Begründung fehlt ({err})"
            );
        }
    }

    /// Die Remote-Erkennung stammt aus `commands::transport_of` (Spiegel:
    /// `src/transport.ts`). Ein stdio-Server mit Kopfzeilen bleibt dort stdio –
    /// das Frontend schaltet die Ziel-Option genau so frei, also darf das
    /// Backend hier nicht strenger urteilen.
    #[test]
    fn validate_entry_folgt_transport_of_bei_headers_und_unbekanntem_typ() {
        let mut mit_headers = stdio_entry();
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), "Bearer x".to_string());
        mit_headers.headers = Some(headers);
        assert!(validate_entry(&mit_headers, caps_stdio_only()).is_ok());

        // Unbekanntes `type` gewinnt nicht gegen den fehlenden url-Hinweis.
        let mut unbekannter_typ = stdio_entry();
        unbekannter_typ.transport = Some("SSE".into());
        assert!(validate_entry(&unbekannter_typ, caps_stdio_only()).is_ok());
    }

    #[test]
    fn validate_entry_erlaubt_remote_wenn_client_es_kann() {
        let caps = ClientCaps { remote: true };
        let entry = ServerEntry {
            transport: Some("http".into()),
            url: Some("https://example.test/mcp".into()),
            ..Default::default()
        };
        assert!(validate_entry(&entry, caps).is_ok());

        // Remote ohne URL ist unvollständig und wird abgelehnt.
        let ohne_url = ServerEntry {
            transport: Some("http".into()),
            ..Default::default()
        };
        assert!(validate_entry(&ohne_url, caps).is_err());
    }

    /// Der Guard, der in `update_client_server` VOR dem Auto-Snapshot läuft:
    /// ein von Hand eingetragener Remote-Server wird nicht überschrieben.
    #[test]
    fn ensure_overwritable_lehnt_bestehenden_remote_eintrag_ab() {
        let remote = ServerEntry {
            transport: Some("http".into()),
            url: Some("https://example.test/mcp".into()),
            ..Default::default()
        };
        let err = ensure_overwritable("foo", &remote, "Claude Desktop", caps_stdio_only())
            .unwrap_err()
            .to_string();
        assert!(err.contains("foo"), "Servername fehlt ({err})");
        assert!(err.contains("von Hand"), "Begründung fehlt ({err})");
    }

    #[test]
    fn ensure_overwritable_laesst_stdio_und_remote_faehige_clients_durch() {
        let caps_remote = ClientCaps { remote: true };
        let remote = ServerEntry {
            transport: Some("http".into()),
            url: Some("https://example.test/mcp".into()),
            ..Default::default()
        };
        // Bestehender stdio-Eintrag: immer überschreibbar.
        assert!(
            ensure_overwritable("foo", &stdio_entry(), "Claude Desktop", caps_stdio_only()).is_ok()
        );
        // Kann der Client Remote, gibt es nichts zu schützen.
        assert!(ensure_overwritable("foo", &remote, "Cursor", caps_remote).is_ok());
    }

    #[test]
    fn validate_entry_lehnt_leeren_command_ab() {
        let entry = ServerEntry {
            command: Some("   ".into()),
            ..Default::default()
        };
        assert!(validate_entry(&entry, caps_stdio_only()).is_err());
        assert!(validate_entry(&ServerEntry::default(), caps_stdio_only()).is_err());
    }
}
