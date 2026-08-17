//! Adapter für **Claude Desktop** (`claude_desktop_config.json`).
//!
//! Claude Desktop hat keine CLI: die Datei wird direkt geschrieben — atomar
//! (`toggles::atomic_write_json`: private Temp-Datei 0600 + `rename`) und nur im
//! Schlüssel `mcpServers`. Alle anderen Top-Level-Keys (`coworkUserFilesPath`,
//! `preferences`, `globalShortcut`, …) bleiben inhalts- und reihenfolgengleich
//! erhalten (`serde_json` mit `preserve_order`).
//!
//! Aus der Datei lädt Claude Desktop ausschließlich **stdio**-Server; Remote-MCP
//! läuft dort über „Connectors“ in der eigenen Oberfläche. Daher
//! `ClientCaps { remote: false }`.
//!
//! Eine von Hand kaputt editierte Datei wird **nie** „reparierend“ überschrieben:
//! `read_root` (invalides JSON) bzw. `parse_servers` (unverständlicher Eintrag)
//! liefern einen Fehler, und alle Schreibpfade brechen davor ab. Die Ansicht
//! zeigt in diesem Zustand ein Fehler-Banner statt einer Liste.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{ensure_overwritable, validate_entry, ClientAdapter, ClientCaps};
use crate::models::{AppError, ServerEntry};

const ID: &str = "claude-desktop";
const LABEL: &str = "Claude Desktop";
const CONFIG_FILE: &str = "claude_desktop_config.json";
const SERVERS_KEY: &str = "mcpServers";
/// Die Keys, die `ServerEntry` auf einem Server-Eintrag modelliert (serde-Namen).
/// Nur diese schreibt `upsert`; alle anderen bleiben unangetastet. Der Test
/// `entry_keys_deckt_server_entry_ab` hält die Liste mit `ServerEntry` synchron.
const ENTRY_KEYS: [&str; 6] = ["type", "command", "args", "env", "url", "headers"];

/// Home-Verzeichnis; ohne `HOME` ein bewusst nicht existierender Pfad, damit
/// `detect()` sauber `None` liefert statt relativ im Arbeitsverzeichnis zu raten.
fn home() -> PathBuf {
    crate::claude_cli::home_dir().unwrap_or_else(|| PathBuf::from("/nonexistent"))
}

#[cfg(target_os = "linux")]
fn config_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(x) => PathBuf::from(x).join("Claude"),
        None => home().join(".config").join("Claude"),
    }
}

#[cfg(target_os = "macos")]
fn config_dir() -> PathBuf {
    home().join("Library").join("Application Support").join("Claude")
}

#[cfg(target_os = "windows")]
fn config_dir() -> PathBuf {
    match std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
        Some(a) => PathBuf::from(a).join("Claude"),
        None => home().join("AppData").join("Roaming").join("Claude"),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn config_dir() -> PathBuf {
    home().join(".config").join("Claude")
}

/// Grobe Fehlerklasse einer Serde-Meldung, ohne den beanstandeten **Wert**.
/// Der Wert selbst darf das Webview nie erreichen (Boundary-Regel): er kann ein
/// Secret sein, und die Maskierungs-Heuristik in `mask.rs` erkennt nicht jedes.
fn serde_category(e: &serde_json::Error) -> &'static str {
    match e.classify() {
        serde_json::error::Category::Data => "unpassender Datentyp",
        serde_json::error::Category::Syntax => "Syntaxfehler",
        serde_json::error::Category::Eof => "unerwartetes Ende",
        serde_json::error::Category::Io => "Lesefehler",
    }
}

/// Geladene Konfigurationswurzel samt dem Pfad, aus dem sie stammt.
///
/// Lesen und Schreiben benutzen **dieselbe** `PathBuf`; die Symlink-Auflösung
/// passiert genau einmal in [`ClaudeDesktop::read_root`]. Ohne das läge zwischen
/// Prüfung und Schreibvorgang ein TOCTOU-Fenster (Link umhängen ⇒ Schreiben auf
/// ein nie gelesenes Ziel).
struct Loaded {
    root: Map<String, Value>,
    write_path: PathBuf,
}

pub struct ClaudeDesktop {
    config_path: PathBuf,
}

impl ClaudeDesktop {
    pub fn new() -> Self {
        Self {
            config_path: config_dir().join(CONFIG_FILE),
        }
    }

    /// Adapter gegen einen frei gewählten Pfad — für Unit-Tests (Muster
    /// `snapshot::create_in`), damit kein Test die echte Konfiguration anfasst.
    #[cfg(test)]
    pub fn with_path(config_path: PathBuf) -> Self {
        Self { config_path }
    }

    /// Liest die Datei als JSON-Objekt — zusammen mit dem Pfad, in den
    /// zurückgeschrieben werden darf.
    ///
    /// Fehlende (oder leere) Datei ⇒ leere Map: „Verzeichnis da, Datei noch
    /// nicht“ ist ein gültiger Anfangszustand. Vorhandene, aber invalide Datei ⇒
    /// Fehler mit Pfad und Grund — **niemals** reparieren.
    ///
    /// **Symlink-Semantik** (Dotfiles-Setups): Der Pfad wird genau **einmal**
    /// aufgelöst; derselbe `write_path` wird gelesen und später beschrieben, es
    /// gibt also kein Zeitfenster, in dem ein umgehängter Link den
    /// Schreibvorgang auf ein nie gelesenes Ziel lenken könnte. Gefolgt wird nur
    /// einem Link, hinter dem tatsächlich ein Konfigurationsdokument steht:
    /// * tot / kein reguläres Ziel ⇒ Fehler (weder blind folgen noch den Link
    ///   durch eine reguläre Datei ersetzen),
    /// * Ziel vorhanden, aber **leer** ⇒ Fehler; eine leere Datei (frische
    ///   Lockdatei, `~/.hushlogin`, …) wurde nie als Konfiguration gelesen und
    ///   darf auch nicht als solche beschrieben werden.
    ///
    /// Ohne Symlink bleibt alles wie gehabt: leere Datei = gültiger Anfang.
    fn read_root(&self) -> Result<Loaded, AppError> {
        let is_link = std::fs::symlink_metadata(&self.config_path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        // `resolve_link_target` liefert den Pfad unverändert zurück, wenn er
        // kein Symlink ist ODER der Link nicht auf eine reguläre Datei zeigt.
        let path = crate::toggles::resolve_link_target(&self.config_path);
        if is_link && path == self.config_path {
            return Err(AppError::Io(format!(
                "{} ist ein Symlink, dessen Ziel keine reguläre Datei ist – \
                 bitte den Link reparieren oder entfernen.",
                self.config_path.display()
            )));
        }

        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Loaded {
                    root: Map::new(),
                    write_path: path,
                })
            }
            Err(e) => {
                return Err(AppError::Io(format!(
                    "{} nicht lesbar: {e}",
                    self.config_path.display()
                )))
            }
        };
        if text.trim().is_empty() {
            if is_link {
                return Err(AppError::Parse(format!(
                    "{} ist ein Symlink auf eine leere Datei – bitte „{{}}“ hineinschreiben \
                     oder den Link entfernen.",
                    self.config_path.display()
                )));
            }
            return Ok(Loaded {
                root: Map::new(),
                write_path: path,
            });
        }
        let value: Value = serde_json::from_str(&text).map_err(|e| {
            AppError::Parse(format!(
                "{} enthält kein gültiges JSON: {e}",
                self.config_path.display()
            ))
        })?;
        match value {
            Value::Object(root) => Ok(Loaded {
                root,
                write_path: path,
            }),
            _ => Err(AppError::Parse(format!(
                "{} enthält kein JSON-Objekt",
                self.config_path.display()
            ))),
        }
    }

    /// Schreibt die Wurzel zurück — atomar und **ausschließlich** in den Pfad,
    /// aus dem sie zuvor gelesen wurde (`Loaded::write_path`). Die Symlink-
    /// Auflösung ist damit schon passiert; siehe [`ClaudeDesktop::read_root`]
    /// und `toggles::resolve_link_target`.
    fn write_root(&self, write_path: &Path, root: Map<String, Value>) -> Result<(), AppError> {
        crate::toggles::atomic_write_json(write_path, &Value::Object(root))
    }

    /// Das `mcpServers`-Objekt der Datei. Fehlt der Key, ist das der gültige
    /// Anfangszustand (`None`); ist er vorhanden, aber **kein Objekt**, ist die
    /// Datei von Hand kaputt editiert worden — dann ein Fehler statt einer
    /// stillschweigend leeren Liste (sonst meldete die Ansicht „keine Server"
    /// und erst das Anlegen bräche ab).
    fn servers_of<'a>(
        &self,
        root: &'a Map<String, Value>,
    ) -> Result<Option<&'a Map<String, Value>>, AppError> {
        match root.get(SERVERS_KEY) {
            None => Ok(None),
            Some(Value::Object(obj)) => Ok(Some(obj)),
            Some(_) => Err(AppError::Parse(format!(
                "{}: „{SERVERS_KEY}“ ist kein Objekt",
                self.config_path.display()
            ))),
        }
    }

    /// Alle Server-Einträge, **streng** geparst.
    ///
    /// Bewusst nicht tolerant – anders als `config_read::parse_servers_map`, wo
    /// die CLI der Schreiber ist. Hier ist dieser Adapter der einzige Schreiber:
    /// ein `unwrap_or_default()` machte aus einem nicht verstandenen Eintrag eine
    /// leere Definition, das Bearbeiten-Formular öffnete leer, der Secret-Schutz
    /// (`has_secrets`) fiele aus — und Speichern ersetzte die echte Definition
    /// samt Klartext-Secrets. Ein Typfehler in der Datei ist damit derselbe Fall
    /// wie kaputtes JSON: Fehler-Banner, keine Schreibvorgänge.
    ///
    /// Deshalb rufen **alle** Schreibpfade diese Prüfung vorab auf: geschrieben
    /// wird nur in eine Datei, die vollständig verstanden wurde.
    fn parse_servers(&self, root: &Map<String, Value>) -> Result<Vec<(String, ServerEntry)>, AppError> {
        let Some(servers) = self.servers_of(root)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(servers.len());
        for (name, def) in servers {
            let entry = serde_json::from_value::<ServerEntry>(def.clone()).map_err(|e| {
                // Die Serde-Meldung wird BEWUSST nicht durchgereicht: sie nennt
                // den unpassenden Wert im Klartext, und `redact_secrets` erkennt
                // ihn nicht zuverlässig (ein rein numerisches Token wie
                // `1234567890123456` sieht für die Heuristik harmlos aus). Nach
                // draußen geht nur Eintragsname + Fehlerklasse.
                AppError::Parse(format!(
                    "{}: Eintrag „{name}“ ist keine gültige Server-Definition ({}) – \
                     bitte die Felder command/args/env/url/headers prüfen.",
                    self.config_path.display(),
                    serde_category(&e)
                ))
            })?;
            out.push((name.clone(), entry));
        }
        Ok(out)
    }
}

impl ClientAdapter for ClaudeDesktop {
    fn id(&self) -> &'static str {
        ID
    }

    fn label(&self) -> &'static str {
        LABEL
    }

    fn detect(&self) -> Option<PathBuf> {
        // Maßgeblich ist das Verzeichnis: existiert es nicht, ist Claude Desktop
        // nicht installiert. Die Datei selbst darf fehlen (noch kein Server).
        let parent = self.config_path.parent()?;
        parent.is_dir().then(|| self.config_path.clone())
    }

    fn list(&self) -> Result<Vec<(String, ServerEntry)>, AppError> {
        let loaded = self.read_root()?;
        self.parse_servers(&loaded.root)
    }

    fn upsert(&self, name: &str, entry: &ServerEntry) -> Result<(), AppError> {
        let caps = self.capabilities();
        validate_entry(entry, caps)?;
        let Loaded {
            mut root,
            write_path,
        } = self.read_root()?;
        // Erst vollständig verstehen, dann schreiben: eine kaputte Datei (auch
        // nur ein unverständlicher Eintrag) wird nie „repariert“ überschrieben.
        let existing_entries = self.parse_servers(&root)?;

        // Von Hand eingetragener Remote-Server: nicht anfassen (Begründung in
        // `ensure_overwritable`). Hier steht die **letzte** Schranke direkt vor
        // dem Schreiben; `commands::update_client_server` prüft dasselbe bereits
        // vor dem Auto-Snapshot.
        if let Some((_, old)) = existing_entries.iter().find(|(n, _)| n == name) {
            ensure_overwritable(name, old, LABEL, caps)?;
        }

        // `"type": "stdio"` ist in dieser Datei der Standard und wird dort nicht
        // geschrieben — normalisieren, damit die Datei idiomatisch bleibt.
        let mut normalized = entry.clone();
        if normalized.transport.as_deref() == Some("stdio") {
            normalized.transport = None;
        }
        let fields = match serde_json::to_value(&normalized)
            .map_err(|e| AppError::Parse(e.to_string()))?
        {
            Value::Object(map) => map,
            // `ServerEntry` serialisiert immer als Objekt; defensiv statt Panik.
            _ => return Err(AppError::Parse("unerwartete Serialisierung".into())),
        };

        let servers = root
            .entry(SERVERS_KEY.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(obj) = servers.as_object_mut() else {
            return Err(AppError::Parse(format!(
                "{}: „{SERVERS_KEY}“ ist kein Objekt",
                self.config_path.display()
            )));
        };

        // Bestehenden Eintrag als Basis nehmen: `ServerEntry` modelliert nur
        // `ENTRY_KEYS`; alles andere auf diesem Eintrag (`disabled`, `cwd`,
        // Zusatzfelder handgepflegter Dateien) gehört uns nicht und bleibt
        // stehen — dieselbe Invariante wie für fremde Top-Level-Keys, nur eine
        // Ebene tiefer. Abgewählte bekannte Felder verschwinden gezielt,
        // verbleibende behalten dank `preserve_order` ihre Position.
        let mut merged = match obj.get(name) {
            Some(Value::Object(existing)) => existing.clone(),
            _ => Map::new(),
        };
        merged.retain(|k, _| !ENTRY_KEYS.contains(&k.as_str()) || fields.contains_key(k));
        for (key, value) in fields {
            merged.insert(key, value);
        }
        obj.insert(name.to_string(), Value::Object(merged));
        self.write_root(&write_path, root)
    }

    fn remove(&self, name: &str) -> Result<(), AppError> {
        let Loaded {
            mut root,
            write_path,
        } = self.read_root()?;
        // Wie in `upsert`: nur in eine vollständig verstandene Datei schreiben.
        self.parse_servers(&root)?;
        let removed = root
            .get_mut(SERVERS_KEY)
            .and_then(|v| v.as_object_mut())
            .and_then(|obj| obj.remove(name))
            .is_some();
        if !removed {
            // Ohne Schreibvorgang abbrechen: nichts zu tun heißt nichts anfassen.
            return Err(AppError::Io(format!(
                "Server „{name}“ in {LABEL} nicht gefunden"
            )));
        }
        // Ein dann ggf. leeres `mcpServers: {}` bleibt bewusst stehen.
        self.write_root(&write_path, root)
    }

    fn capabilities(&self) -> ClientCaps {
        // Aus der Datei lädt Claude Desktop nur stdio-Server; Remote läuft dort
        // über „Connectors“ in der eigenen Oberfläche.
        ClientCaps { remote: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;

    /// Eindeutiges Temp-Verzeichnis pro Test (Muster aus `toggles.rs`).
    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mcpmgr-desktop-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Namen aller in `dir` zurückgebliebenen Temp-Dateien.
    fn leftover_temps(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".mcpmgr-") && n.ends_with(".tmp"))
            .collect()
    }

    fn adapter_in(dir: &Path) -> ClaudeDesktop {
        ClaudeDesktop::with_path(dir.join(CONFIG_FILE))
    }

    fn stdio_entry(command: &str) -> ServerEntry {
        ServerEntry {
            command: Some(command.into()),
            args: Some(vec!["-y".into(), "beispiel".into()]),
            ..Default::default()
        }
    }

    fn read_root_value(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn detect_ohne_verzeichnis_ist_none() {
        let dir = tmp("detect-none");
        let a = ClaudeDesktop::with_path(dir.join("gibt-es-nicht").join(CONFIG_FILE));
        assert!(a.detect().is_none());
    }

    #[test]
    fn detect_mit_verzeichnis_ohne_datei_ist_some() {
        let dir = tmp("detect-some");
        let a = adapter_in(&dir);
        assert_eq!(a.detect(), Some(dir.join(CONFIG_FILE)));
        assert!(!dir.join(CONFIG_FILE).exists(), "detect legt nichts an");
    }

    #[test]
    fn list_bei_fehlender_datei_ist_leer() {
        let dir = tmp("list-fehlt");
        assert!(adapter_in(&dir).list().unwrap().is_empty());
    }

    #[test]
    fn list_ohne_mcpservers_key_ist_leer() {
        let dir = tmp("list-ohne-key");
        std::fs::write(dir.join(CONFIG_FILE), r#"{"preferences":{"a":1}}"#).unwrap();
        assert!(adapter_in(&dir).list().unwrap().is_empty());
    }

    #[test]
    fn upsert_list_remove_roundtrip() {
        let dir = tmp("roundtrip");
        let a = adapter_in(&dir);

        let mut entry = stdio_entry("npx");
        let mut env = BTreeMap::new();
        env.insert("TOKEN".to_string(), "s3cr3t".to_string());
        entry.env = Some(env);

        a.upsert("demo", &entry).unwrap();
        let listed = a.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, "demo");
        assert_eq!(listed[0].1.command.as_deref(), Some("npx"));
        assert_eq!(
            listed[0].1.env.as_ref().unwrap().get("TOKEN").unwrap(),
            "s3cr3t",
            "env-Werte überleben den Roundtrip"
        );

        a.remove("demo").unwrap();
        assert!(a.list().unwrap().is_empty());
        // Der (leere) mcpServers-Key bleibt stehen.
        let root = read_root_value(&dir.join(CONFIG_FILE));
        assert!(root.get(SERVERS_KEY).unwrap().as_object().unwrap().is_empty());
    }

    #[test]
    fn upsert_erhaelt_fremde_top_level_keys() {
        let dir = tmp("fremde-keys");
        let path = dir.join(CONFIG_FILE);
        std::fs::write(
            &path,
            r#"{"mcpServers":{"alt":{"command":"alt-cmd"}},"coworkUserFilesPath":"/tmp/x","preferences":{"b":2,"a":1}}"#,
        )
        .unwrap();

        adapter_in(&dir).upsert("neu", &stdio_entry("npx")).unwrap();

        let root = read_root_value(&path);
        let obj = root.as_object().unwrap();
        assert_eq!(
            obj.keys().collect::<Vec<_>>(),
            vec!["mcpServers", "coworkUserFilesPath", "preferences"],
            "Reihenfolge der Top-Level-Keys unverändert"
        );
        assert_eq!(obj.get("coworkUserFilesPath").unwrap(), &json_str("/tmp/x"));
        assert_eq!(
            obj.get("preferences")
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec!["b", "a"],
            "Reihenfolge innerhalb fremder Objekte unverändert"
        );
        // Bestehender Server bleibt, neuer kam dazu.
        let servers = obj.get(SERVERS_KEY).unwrap().as_object().unwrap();
        assert!(servers.contains_key("alt") && servers.contains_key("neu"));
    }

    fn json_str(s: &str) -> Value {
        Value::String(s.to_string())
    }

    #[test]
    fn upsert_legt_fehlendes_mcpservers_an_ohne_fremde_keys_zu_beruehren() {
        let dir = tmp("kein-mcpservers");
        let path = dir.join(CONFIG_FILE);
        std::fs::write(&path, r#"{"globalShortcut":"Ctrl+Space"}"#).unwrap();

        adapter_in(&dir).upsert("neu", &stdio_entry("uvx")).unwrap();

        let root = read_root_value(&path);
        let obj = root.as_object().unwrap();
        assert_eq!(obj.get("globalShortcut").unwrap(), &json_str("Ctrl+Space"));
        assert!(obj
            .get(SERVERS_KEY)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("neu"));
        assert_eq!(
            obj.keys().collect::<Vec<_>>(),
            vec!["globalShortcut", "mcpServers"],
            "neuer Key wird angehängt, fremder bleibt vorn"
        );
    }

    #[test]
    fn invalides_json_wird_abgelehnt() {
        let dir = tmp("kaputt");
        let path = dir.join(CONFIG_FILE);
        let kaputt = "{ \"mcpServers\": { \"a\": { \"command\": \"x\" }, }";
        std::fs::write(&path, kaputt).unwrap();
        let a = adapter_in(&dir);

        assert!(a.list().is_err());
        assert!(a.upsert("neu", &stdio_entry("npx")).is_err());
        assert!(a.remove("a").is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            kaputt,
            "kaputte Datei bleibt byteidentisch"
        );
        assert!(leftover_temps(&dir).is_empty());
    }

    #[test]
    fn json_kein_objekt_wird_abgelehnt() {
        let dir = tmp("kein-objekt");
        let path = dir.join(CONFIG_FILE);
        std::fs::write(&path, "[]").unwrap();
        let a = adapter_in(&dir);
        assert!(a.list().is_err());
        assert!(a.upsert("neu", &stdio_entry("npx")).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");
    }

    #[test]
    fn upsert_lehnt_remote_entry_ab() {
        let dir = tmp("remote");
        let path = dir.join(CONFIG_FILE);
        let a = adapter_in(&dir);

        let mit_url = ServerEntry {
            url: Some("https://example.test/mcp".into()),
            ..Default::default()
        };
        assert!(a.upsert("remote", &mit_url).is_err());

        // Kopfzeilen ALLEIN machen keinen Remote-Server – maßgeblich ist
        // `commands::transport_of` (Spiegel `src/transport.ts`), siehe
        // `clients::validate_entry`.
        let mut mit_typ = stdio_entry("npx");
        mit_typ.transport = Some("http".into());
        mit_typ.url = Some("https://example.test/mcp".into());
        assert!(a.upsert("remote", &mit_typ).is_err());

        assert!(!path.exists(), "kein Schreibvorgang bei abgelehnter Prüfung");
    }

    /// `mcpServers` vorhanden, aber kein Objekt: das ist eine kaputte Datei und
    /// kein leerer Zustand – sonst meldete die Ansicht „keine Server" und erst
    /// das Anlegen bräche ab.
    #[test]
    fn mcpservers_kein_objekt_wird_abgelehnt() {
        for inhalt in [
            r#"{"mcpServers":[]}"#,
            r#"{"mcpServers":null}"#,
            r#"{"mcpServers":"nichts"}"#,
        ] {
            let dir = tmp("mcpservers-kein-objekt");
            let path = dir.join(CONFIG_FILE);
            std::fs::write(&path, inhalt).unwrap();
            let a = adapter_in(&dir);

            assert!(a.list().is_err(), "{inhalt}: list muss scheitern");
            assert!(
                a.upsert("neu", &stdio_entry("npx")).is_err(),
                "{inhalt}: upsert muss scheitern"
            );
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                inhalt,
                "{inhalt}: Datei bleibt byteidentisch"
            );
            assert!(leftover_temps(&dir).is_empty());
        }
    }

    /// Ein Eintrag, der keine Server-Definition ist (Handeditier-Fehler wie
    /// `"args": "npx foo"`), darf NICHT still zu einer leeren Definition werden:
    /// das Bearbeiten-Formular öffnete sonst leer und ein Speichern ersetzte die
    /// echte Definition samt Klartext-Secrets.
    #[test]
    fn list_lehnt_unverstaendlichen_eintrag_ab() {
        for inhalt in [
            r#"{"mcpServers":{"gut":{"command":"x"},"kaputt":42}}"#,
            r#"{"mcpServers":{"kaputt":{"command":"x","args":"npx foo"}}}"#,
            r#"{"mcpServers":{"kaputt":{"command":"x","env":{"PORT":8080}}}}"#,
        ] {
            let dir = tmp("eintrag-kaputt");
            let path = dir.join(CONFIG_FILE);
            std::fs::write(&path, inhalt).unwrap();
            let a = adapter_in(&dir);

            let err = a.list().unwrap_err().to_string();
            assert!(
                err.contains("kaputt"),
                "{inhalt}: Fehler nennt den Eintrag ({err})"
            );
            // Kein Schreibpfad darf über den Fehler hinweggehen.
            assert!(a.upsert("neu", &stdio_entry("npx")).is_err());
            assert!(a.remove("kaputt").is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), inhalt);
            assert!(leftover_temps(&dir).is_empty());
        }
    }

    /// Unbekannte Keys auf einem Server-Eintrag (`disabled`, `cwd`, …) gehören
    /// uns nicht und bleiben beim Bearbeiten stehen – dieselbe Invariante wie für
    /// fremde Top-Level-Keys. Abgewählte bekannte Felder verschwinden dagegen.
    #[test]
    fn upsert_erhaelt_unbekannte_keys_am_server() {
        let dir = tmp("fremde-server-keys");
        let path = dir.join(CONFIG_FILE);
        std::fs::write(
            &path,
            r#"{"mcpServers":{"demo":{"command":"alt","args":["--x"],"cwd":"/tmp","disabled":true}}}"#,
        )
        .unwrap();

        // Neue Definition ohne args (bewusst abgewählt).
        let neu = ServerEntry {
            command: Some("neu".into()),
            ..Default::default()
        };
        adapter_in(&dir).upsert("demo", &neu).unwrap();

        let root = read_root_value(&path);
        let server = root
            .get(SERVERS_KEY)
            .unwrap()
            .get("demo")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(server.get("command").unwrap(), &json_str("neu"));
        assert_eq!(server.get("cwd").unwrap(), &json_str("/tmp"));
        assert_eq!(server.get("disabled").unwrap(), &Value::Bool(true));
        assert!(
            !server.contains_key("args"),
            "abgewähltes bekanntes Feld verschwindet"
        );
    }

    /// Wächter für `ENTRY_KEYS`: käme ein Feld zu `ServerEntry` hinzu, ohne die
    /// Liste zu ergänzen, ließe `upsert` beim Bearbeiten den Altwert stehen.
    #[test]
    fn entry_keys_deckt_server_entry_ab() {
        let voll = ServerEntry {
            transport: Some("http".into()),
            command: Some("npx".into()),
            args: Some(vec!["-y".into()]),
            env: Some(BTreeMap::from([("A".to_string(), "B".to_string())])),
            url: Some("https://example.test".into()),
            headers: Some(BTreeMap::from([("H".to_string(), "V".to_string())])),
        };
        let value = serde_json::to_value(&voll).unwrap();
        let mut keys: Vec<String> = value
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.to_string())
            .collect();
        keys.sort();
        let mut erwartet: Vec<String> = ENTRY_KEYS.iter().map(|k| k.to_string()).collect();
        erwartet.sort();
        assert_eq!(
            keys, erwartet,
            "ENTRY_KEYS und ServerEntry sind auseinandergelaufen"
        );
    }

    #[test]
    fn upsert_schreibt_kein_type_stdio() {
        let dir = tmp("kein-type");
        let path = dir.join(CONFIG_FILE);
        let mut entry = stdio_entry("npx");
        entry.transport = Some("stdio".into());
        adapter_in(&dir).upsert("demo", &entry).unwrap();

        let root = read_root_value(&path);
        let server = root
            .get(SERVERS_KEY)
            .unwrap()
            .get("demo")
            .unwrap()
            .as_object()
            .unwrap();
        assert!(!server.contains_key("type"), "Datei bleibt idiomatisch");
        assert!(server.contains_key("command"));
    }

    #[test]
    fn remove_unbekannter_name_ist_fehler_ohne_schreibvorgang() {
        let dir = tmp("remove-unbekannt");
        let path = dir.join(CONFIG_FILE);
        let inhalt = r#"{"mcpServers":{"a":{"command":"x"}}}"#;
        std::fs::write(&path, inhalt).unwrap();

        assert!(adapter_in(&dir).remove("gibt-es-nicht").is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            inhalt,
            "Datei unverändert"
        );
        assert!(leftover_temps(&dir).is_empty());
    }

    #[test]
    fn schreiben_hinterlaesst_keine_temp_dateien() {
        let dir = tmp("temps");
        let a = adapter_in(&dir);
        a.upsert("eins", &stdio_entry("npx")).unwrap();
        a.upsert("zwei", &stdio_entry("uvx")).unwrap();
        a.remove("eins").unwrap();
        assert!(leftover_temps(&dir).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn schreiben_folgt_symlink_statt_ihn_zu_ersetzen() {
        let dir = tmp("symlink");
        let echt_dir = dir.join("dotfiles");
        std::fs::create_dir_all(&echt_dir).unwrap();
        let echt = echt_dir.join("desktop.json");
        std::fs::write(&echt, r#"{"mcpServers":{}}"#).unwrap();

        let link = dir.join(CONFIG_FILE);
        std::os::unix::fs::symlink(&echt, &link).unwrap();

        ClaudeDesktop::with_path(link.clone())
            .upsert("demo", &stdio_entry("npx"))
            .unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "Symlink bleibt ein Symlink"
        );
        let root = read_root_value(&echt);
        assert!(root
            .get(SERVERS_KEY)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("demo"));
        assert!(leftover_temps(&echt_dir).is_empty());
        assert!(leftover_temps(&dir).is_empty());
    }

    /// Symlink auf eine **leere** reguläre Datei (frische Lockdatei,
    /// `~/.hushlogin`, …): sie wurde nie als Konfiguration gelesen und darf auch
    /// nicht als solche beschrieben werden. Ohne Symlink bleibt eine leere Datei
    /// dagegen der gültige Anfangszustand.
    #[cfg(unix)]
    #[test]
    fn symlink_auf_leere_datei_wird_weder_gelesen_noch_beschrieben() {
        let dir = tmp("symlink-leer");
        let fremd_dir = dir.join("fremd");
        std::fs::create_dir_all(&fremd_dir).unwrap();
        let fremd = fremd_dir.join("hushlogin");
        std::fs::write(&fremd, "").unwrap();

        let link = dir.join(CONFIG_FILE);
        std::os::unix::fs::symlink(&fremd, &link).unwrap();
        let a = ClaudeDesktop::with_path(link);

        assert!(a.list().is_err());
        assert!(a.upsert("demo", &stdio_entry("npx")).is_err());
        assert!(a.remove("demo").is_err());
        assert_eq!(std::fs::read_to_string(&fremd).unwrap(), "");
        assert!(leftover_temps(&fremd_dir).is_empty());
        assert!(leftover_temps(&dir).is_empty());
    }

    /// Ohne Symlink bleibt die leere Datei ein gültiger Anfangszustand
    /// (Gegenprobe zum Test darüber).
    #[test]
    fn leere_datei_ohne_symlink_ist_gueltiger_anfangszustand() {
        let dir = tmp("leer-ohne-link");
        std::fs::write(dir.join(CONFIG_FILE), "").unwrap();
        let a = adapter_in(&dir);
        assert!(a.list().unwrap().is_empty());
        a.upsert("demo", &stdio_entry("npx")).unwrap();
        assert_eq!(a.list().unwrap().len(), 1);
    }

    /// Toter Symlink: weder blind folgen (das Ziel gibt es nicht) noch den Link
    /// durch eine reguläre Datei ersetzen — stattdessen eine klare Meldung.
    #[cfg(unix)]
    #[test]
    fn toter_symlink_wird_nicht_ersetzt() {
        let dir = tmp("symlink-tot");
        let link = dir.join(CONFIG_FILE);
        std::os::unix::fs::symlink(dir.join("gibt-es-nicht"), &link).unwrap();
        let a = ClaudeDesktop::with_path(link.clone());

        assert!(a.list().is_err());
        assert!(a.upsert("demo", &stdio_entry("npx")).is_err());
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "der Link bleibt ein Link"
        );
        assert!(leftover_temps(&dir).is_empty());
    }

    /// Die Serde-Meldung darf den beanstandeten **Wert** nicht nach draußen
    /// tragen: ein rein numerisches Token entkommt jeder Masken-Heuristik.
    #[test]
    fn fehlermeldung_nennt_den_unpassenden_wert_nicht() {
        let dir = tmp("kein-wert-leak");
        std::fs::write(
            dir.join(CONFIG_FILE),
            r#"{"mcpServers":{"demo":{"command":"npx","env":{"API_KEY":1234567890123456}}}}"#,
        )
        .unwrap();

        let err = adapter_in(&dir).list().unwrap_err().to_string();
        assert!(err.contains("demo"), "Eintragsname wird genannt: {err}");
        assert!(
            !err.contains("1234567890123456"),
            "der Wert darf nicht auftauchen: {err}"
        );
    }

    /// Ein von Hand eingetragener Remote-Server wird beim Speichern NICHT still
    /// zu stdio umgeschrieben (das Formular zeigt für diesen Client gar keine
    /// Remote-Felder an) — `upsert` bricht mit Begründung ab.
    #[test]
    fn upsert_schreibt_handgepflegten_remote_eintrag_nicht_um() {
        let dir = tmp("remote-nicht-umschreiben");
        let path = dir.join(CONFIG_FILE);
        let inhalt = r#"{"mcpServers":{"remote":{"type":"http","url":"https://example.test/mcp"}}}"#;
        std::fs::write(&path, inhalt).unwrap();

        let err = adapter_in(&dir)
            .upsert("remote", &stdio_entry("npx"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Remote"), "Begründung nennt den Grund: {err}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            inhalt,
            "die Datei bleibt byteidentisch"
        );
        // Ein anderer Name bleibt davon unberührt.
        adapter_in(&dir).upsert("neu", &stdio_entry("npx")).unwrap();
    }
}
