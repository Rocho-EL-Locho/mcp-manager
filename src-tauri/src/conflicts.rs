//! Namenskonflikte zwischen Scopes: derselbe Servername ist in mehr als einem
//! Scope definiert.
//!
//! Claude Code nutzt bei Namensgleichheit **genau eine** Definition (kein Merge),
//! nämlich die des höchstpriorisierten Scopes (local > project > user). Dieses
//! Modul findet solche Fälle, bestimmt den effektiven Scope und meldet, ob alle
//! Definitionen inhaltsgleich sind – der Command-Layer liest nur die Definitionen
//! ein und reicht sie hier hinein.

use std::collections::BTreeMap;

use crate::config_read::ScopedEntry;
use crate::mask::{mask_summary, summarize_entry};
use crate::models::{ConflictDefinition, ConflictInfo, Scope, ServerEntry};

/// Präzedenz für den effektiven Scope bei Namenskonflikten (kleiner = gewinnt):
/// local > project > user. Quelle: Claude-Code-Doku
/// (https://code.claude.com/docs/en/mcp) – bei gleichem Namen nutzt Claude Code
/// genau eine Definition (kein Merge), die des höchstpriorisierten Scopes.
/// Achtung: NICHT `commands::scope_rank` (das ist nur die Sortier-Reihenfolge
/// der Liste).
pub fn scope_precedence(scope: Scope) -> u8 {
    match scope {
        Scope::Local => 0,
        Scope::Project => 1,
        Scope::User => 2,
    }
}

/// Hash über die normalisierte Definition. `env`/`headers` sind `BTreeMap` →
/// deterministische JSON-Serialisierung → stabiler Fingerprint (unabhängig von
/// der Key-Reihenfolge). Kein Krypto nötig, nur Gleichheitsvergleich.
///
/// **Bleibt backend-intern.** Der Wert entsteht aus der UNmaskierten Definition
/// (inkl. env-/header-Klartext) mit einem nicht randomisierten Hasher; da
/// Kommando/args/env-Keys ohnehin im Payload stehen, ließe sich ein kurzes
/// Secret offline gegen den Hash durchprobieren. Deshalb steht er nicht in
/// `ConflictDefinition` – das Webview bekommt nur das Ergebnis (`identical`).
pub fn definition_fingerprint(entry: &ServerEntry) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let json = serde_json::to_string(entry).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    hasher.finish()
}

/// Findet die Namenskonflikte in `defs`: Namen, die in mehreren Scopes vorkommen.
/// Liefert je Konflikt die Definitionen (maskiert), den effektiven Scope und ob
/// alle Definitionen inhaltsgleich sind. Die Reihenfolge ist alphabetisch stabil
/// (BTreeMap-Gruppierung).
///
/// `defs` muss bereits alles enthalten, was der Aufrufer berücksichtigt haben
/// will – insbesondere die deaktivierten user-Server aus dem Stash (sonst
/// entstünde der Konflikt beim Reaktivieren überraschend).
pub fn find(defs: &[ScopedEntry]) -> Vec<ConflictInfo> {
    let mut by_name: BTreeMap<&str, Vec<&ScopedEntry>> = BTreeMap::new();
    for d in defs {
        by_name.entry(d.name.as_str()).or_default().push(d);
    }

    let mut out = Vec::new();
    for (name, group) in by_name {
        if group.len() < 2 {
            continue; // kein Konflikt
        }
        let definitions: Vec<ConflictDefinition> = group
            .iter()
            .map(|d| ConflictDefinition {
                scope: d.scope,
                project_path: d.project_path.clone(),
                summary: mask_summary(&summarize_entry(&d.entry), false),
            })
            .collect();
        // Der Fingerprint bleibt im Backend (siehe definition_fingerprint) –
        // nach außen geht nur das Ergebnis des Vergleichs.
        let fingerprints: Vec<u64> = group
            .iter()
            .map(|d| definition_fingerprint(&d.entry))
            .collect();
        let identical = fingerprints.iter().all(|fp| *fp == fingerprints[0]);
        out.push(ConflictInfo {
            name: name.to_string(),
            definitions,
            effective_scope: effective_scope(&group),
            identical,
        });
    }
    out
}

/// Effektiver Scope einer Konfliktgruppe = höchste Präzedenz (kleinster Wert).
fn effective_scope(group: &[&ScopedEntry]) -> Scope {
    group
        .iter()
        .map(|d| d.scope)
        .min_by_key(|s| scope_precedence(*s))
        .unwrap_or(Scope::User)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn entry(command: &str) -> ServerEntry {
        ServerEntry {
            command: Some(command.into()),
            ..Default::default()
        }
    }

    fn scoped(name: &str, scope: Scope, entry: ServerEntry) -> ScopedEntry {
        ScopedEntry {
            scope,
            name: name.to_string(),
            entry,
            project_path: None,
        }
    }

    #[test]
    fn precedence_local_beats_project_beats_user() {
        let eff = |scopes: &[Scope]| -> Scope {
            let defs: Vec<ScopedEntry> = scopes
                .iter()
                .map(|s| scoped("srv", *s, entry("srv")))
                .collect();
            let refs: Vec<&ScopedEntry> = defs.iter().collect();
            effective_scope(&refs)
        };
        assert_eq!(eff(&[Scope::User, Scope::Project]), Scope::Project);
        assert_eq!(eff(&[Scope::User, Scope::Local]), Scope::Local);
        assert_eq!(eff(&[Scope::User, Scope::Local, Scope::Project]), Scope::Local);
        assert_eq!(eff(&[Scope::Project, Scope::User, Scope::Local]), Scope::Local);
    }

    #[test]
    fn fingerprint_ignores_env_insertion_order() {
        let mut e1 = entry("srv");
        let mut env_a = BTreeMap::new();
        env_a.insert("A".to_string(), "1".to_string());
        env_a.insert("B".to_string(), "2".to_string());
        e1.env = Some(env_a);

        let mut e2 = entry("srv");
        let mut env_b = BTreeMap::new();
        env_b.insert("B".to_string(), "2".to_string());
        env_b.insert("A".to_string(), "1".to_string());
        e2.env = Some(env_b);

        assert_eq!(
            definition_fingerprint(&e1),
            definition_fingerprint(&e2),
            "gleiche Definition (andere Insert-Reihenfolge) -> gleicher Fingerprint"
        );
        assert_ne!(
            definition_fingerprint(&e1),
            definition_fingerprint(&entry("anders")),
            "abweichende Definition -> anderer Fingerprint"
        );
    }

    /// `find` liefert nur mehrfach definierte Namen, mit effektivem Scope und
    /// Gleichheits-Flag – rein in-memory, ohne echte Umgebung.
    #[test]
    fn find_reports_only_multi_scope_names() {
        let defs = vec![
            scoped("allein", Scope::User, entry("a")),
            scoped("doppelt", Scope::User, entry("b")),
            scoped("doppelt", Scope::Local, entry("b")),
            scoped("abweichend", Scope::User, entry("c")),
            scoped("abweichend", Scope::Project, entry("d")),
        ];
        let out = find(&defs);
        let names: Vec<&str> = out.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["abweichend", "doppelt"], "alphabetisch, ohne Einzelgänger");

        let doppelt = out.iter().find(|c| c.name == "doppelt").unwrap();
        assert_eq!(doppelt.effective_scope, Scope::Local, "local gewinnt");
        assert!(doppelt.identical, "gleiche Definition -> identical");

        let abweichend = out.iter().find(|c| c.name == "abweichend").unwrap();
        assert_eq!(abweichend.effective_scope, Scope::Project, "project schlägt user");
        assert!(!abweichend.identical, "andere Definition -> nicht identical");
    }
}
