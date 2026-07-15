//! Persistente Status-/Verfügbarkeits-Historie pro Server (Feature 09).
//!
//! Pro Server-Schlüssel ein Ring von max. [`MAX_POINTS`] Messpunkten
//! `{ ts, status_kind, connect_ms? }`. Geschrieben wird EINMAL pro Status-Refresh
//! (nicht pro Server). Die Datei enthält **keine Secrets** (nur Name/Scope/
//! Status/Zahl) – daher normale Dateirechte und dasselbe robuste Muster wie
//! `settings.rs`: atomar schreiben, tolerant laden (fehlend/korrupt ⇒ leer, nie
//! ein Fehler), `#[serde(default)]` gegen fehlende/zukünftige Felder.
//!
//! Latenz (`connect_ms`) wird nur gemessen, wenn ein Server introspiziert wurde;
//! der normale Health-Check liefert nur den Status. Die Historie ist daher
//! primär eine Verfügbarkeits-Zeitleiste mit opportunistischen Latenzpunkten.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::models::{AppError, ServerStatus};

/// Maximale Anzahl Messpunkte pro Server (Ring).
const MAX_POINTS: usize = 200;
/// Verwaiste Server (nicht mehr in der Liste) nach dieser Zeit verwerfen.
const ORPHAN_MAX_AGE_SECS: u64 = 30 * 24 * 60 * 60;

/// Ein einzelner Messpunkt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricPoint {
    pub ts: u64,
    #[serde(rename = "statusKind")]
    pub status_kind: String,
    #[serde(rename = "connectMs", default, skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u64>,
}

/// Historie aller Server. Key: `"scope::name::<projektpfad>"` (wie `introspection_key`).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Metrics {
    pub servers: BTreeMap<String, Vec<MetricPoint>>,
}

/// Serde-Tag eines Status als kompakter String (ServerStatus hat kein Deserialize
/// und trägt teils Daten – daher hier nur der Kind-String).
pub fn status_kind(s: &ServerStatus) -> &'static str {
    match s {
        ServerStatus::Connected => "connected",
        ServerStatus::Failed { .. } => "failed",
        ServerStatus::NeedsAuth => "needs_auth",
        ServerStatus::PendingApproval => "pending_approval",
        ServerStatus::Disabled => "disabled",
        ServerStatus::Unknown => "unknown",
    }
}

fn metrics_path() -> PathBuf {
    crate::stash::config_dir().join("metrics.json")
}

/// Lädt die Historie. Fehlende Datei ⇒ leer; korrupte Datei ⇒ leer + Log-Notiz
/// (die Historie wird dann neu begonnen). Niemals ein Fehler.
pub fn load() -> Metrics {
    match std::fs::read_to_string(metrics_path()) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!("mcp-manager: metrics.json nicht lesbar ({e}) – Historie wird neu begonnen.");
            Metrics::default()
        }),
        Err(_) => Metrics::default(),
    }
}

fn save(m: &Metrics) -> Result<(), AppError> {
    let value = serde_json::to_value(m).map_err(|e| AppError::Parse(e.to_string()))?;
    crate::toggles::atomic_write_json(&metrics_path(), &value)
}

/// Reine In-Memory-Aufnahme eines Batches (ohne Persistenz – dadurch testbar):
/// pro Key anhängen + Ring auf [`MAX_POINTS`] trimmen; verwaiste Keys (nicht im
/// Batch und letzter Punkt älter als 30 Tage) verwerfen.
pub fn append_batch(m: &mut Metrics, now: u64, batch: Vec<(String, MetricPoint)>) {
    let seen: HashSet<String> = batch.iter().map(|(k, _)| k.clone()).collect();

    // Verwaiste (nicht mehr existierende) Server nach Ablauf verwerfen.
    m.servers.retain(|k, points| {
        seen.contains(k)
            || points
                .last()
                .map(|p| now.saturating_sub(p.ts) < ORPHAN_MAX_AGE_SECS)
                .unwrap_or(false)
    });

    for (key, point) in batch {
        let ring = m.servers.entry(key).or_default();
        ring.push(point);
        let len = ring.len();
        if len > MAX_POINTS {
            ring.drain(0..len - MAX_POINTS);
        }
    }
}

/// Nimmt einen Batch auf und persistiert (Produktionspfad). Fehler beim Schreiben
/// werden zurückgegeben – der Aufrufer ignoriert sie (Refresh darf nie blockieren).
pub fn record(
    m: &mut Metrics,
    now: u64,
    batch: Vec<(String, MetricPoint)>,
) -> Result<(), AppError> {
    append_batch(m, now, batch);
    save(m)
}

/// Historie eines Servers (Kopie), leer wenn keine vorhanden.
pub fn points_for(m: &Metrics, key: &str) -> Vec<MetricPoint> {
    m.servers.get(key).cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(ts: u64, kind: &str) -> MetricPoint {
        MetricPoint {
            ts,
            status_kind: kind.into(),
            connect_ms: None,
        }
    }

    #[test]
    fn ring_trims_to_max() {
        let mut m = Metrics::default();
        for i in 0..(MAX_POINTS as u64 + 5) {
            append_batch(
                &mut m,
                1000,
                vec![("k".into(), point(1000 + i, "connected"))],
            );
        }
        let ring = &m.servers["k"];
        assert_eq!(ring.len(), MAX_POINTS, "Ring auf MAX_POINTS begrenzt");
        // Der älteste (ts=1000) wurde verdrängt; jüngster bleibt.
        assert_eq!(ring.first().unwrap().ts, 1000 + 5);
        assert_eq!(ring.last().unwrap().ts, 1000 + MAX_POINTS as u64 + 4);
    }

    #[test]
    fn orphan_keys_expire_after_30_days() {
        let mut m = Metrics::default();
        // Alter, verwaister Key (letzter Punkt weit in der Vergangenheit).
        m.servers.insert("old".into(), vec![point(0, "connected")]);
        // Junger, verwaister Key (kürzlich gesehen) bleibt.
        let now = ORPHAN_MAX_AGE_SECS + 100;
        m.servers
            .insert("young".into(), vec![point(now - 10, "connected")]);

        // Batch enthält weder old noch young -> Aufräumen greift.
        append_batch(
            &mut m,
            now,
            vec![("active".into(), point(now, "connected"))],
        );

        assert!(!m.servers.contains_key("old"), "alter Verwaister verworfen");
        assert!(m.servers.contains_key("young"), "junger Verwaister bleibt");
        assert!(m.servers.contains_key("active"), "aktiver Key vorhanden");
    }

    #[test]
    fn corrupt_json_is_ignored() {
        // load() gibt bei kaputtem JSON Default zurück (kein Panic) – hier direkt
        // die Deserialisierung prüfen (load() liest eine feste Pfad-Datei).
        assert!(serde_json::from_str::<Metrics>("{ kaputt").is_err());
        let ok: Metrics = serde_json::from_str(r#"{"servers":{}}"#).unwrap();
        assert!(ok.servers.is_empty());
    }

    #[test]
    fn partial_json_defaults() {
        // Fehlendes `servers`-Feld -> leere Historie (serde default).
        let m: Metrics = serde_json::from_str("{}").unwrap();
        assert!(m.servers.is_empty());
    }
}
