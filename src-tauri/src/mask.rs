//! Maskierung von Geheimnissen, BEVOR Daten das Backend Richtung Webview verlassen.
//!
//! Grundsatz: `claude mcp list/get` und die Config-Dateien enthalten Tokens im
//! Klartext (env-Werte, headers, inline in args wie `-e TOKEN=...`). Standardmäßig
//! wird alles maskiert; Klartext gibt es nur bei explizitem `reveal = true`.

use crate::models::ServerEntry;

pub const MASK: &str = "••••••••";

/// Schlüssel-Namen, deren Wert als geheim gilt (case-insensitive, Teilstring).
const SECRET_KEY_HINTS: &[&str] = &[
    "TOKEN", "KEY", "SECRET", "PASSWORD", "PASSWD", "PASS", "AUTH", "CREDENTIAL", "COOKIE",
];

fn key_looks_secret(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SECRET_KEY_HINTS.iter().any(|h| upper.contains(h))
}

/// Bekannte Token-Präfixe (case-sensitive geprüft, wie in freier Wildbahn).
const SECRET_TOKEN_PREFIXES: &[&str] = &[
    "sk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "xoxb-", "xoxp-", "xoxa-",
    "xoxr-", "glpat-", "AKIA",
];

/// Query-Parameter-Namen, deren Wert als geheim gilt (case-insensitive, Teilstring).
const SECRET_QUERY_HINTS: &[&str] = &[
    "token", "key", "secret", "apikey", "api_key", "password", "passwd", "auth", "access_token",
    "credential", "sig", "signature",
];

/// Sieht ein einzelner Wert wie ein Geheimnis aus
/// (JWT/Bearer/Basic/bekannte Präfixe/langer opaker String)?
fn value_looks_secret(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return false;
    }
    if v.starts_with("eyJ") {
        return true; // JWT
    }
    let lower = v.to_ascii_lowercase();
    if lower.starts_with("bearer ") || lower.starts_with("basic ") {
        return true;
    }
    // Bekannte Token-Präfixe.
    if SECRET_TOKEN_PREFIXES.iter().any(|p| v.starts_with(p)) {
        return true;
    }
    // Lange opake Strings ohne Whitespace, überwiegend base64/hex-artig.
    if v.len() >= 24 && !v.chars().any(|c| c.is_whitespace()) && looks_opaque(v) {
        return true;
    }
    false
}

/// Heuristik für ein bare Token (base64url/hex-artig). Bewusst KONSERVATIV, um
/// legitime Werte nicht zu übermaskieren: erlaubt sind nur Alnum, `-` und `_`
/// (also KEIN `/`, `.`, `:` …). Damit fallen Dateipfade (`/home/…`), Domains
/// (`.`) und URLs heraus. Zusätzlich wird sowohl mindestens eine Ziffer ALS AUCH
/// mindestens ein Buchstabe verlangt, damit reine Wörter/Pfad-Segmente und reine
/// Zahlen nicht greifen. Strukturierte Secrets (JWT, Bearer/Basic, bekannte
/// Präfixe, KEY=VALUE) werden ohnehin separat erkannt.
fn looks_opaque(v: &str) -> bool {
    let mut has_digit = false;
    let mut has_alpha = false;
    for c in v.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
            if c.is_ascii_digit() {
                has_digit = true;
            } else if c.is_ascii_alphabetic() {
                has_alpha = true;
            }
        } else {
            return false; // Fremdzeichen (/, ., :, …) -> kein bare Token
        }
    }
    has_digit && has_alpha
}

/// Sieht ein Token wie eine URL mit Query-Anteil aus?
fn looks_like_url_with_query(token: &str) -> bool {
    let scheme = token.starts_with("http://")
        || token.starts_with("https://")
        || token.contains("://");
    scheme && token.contains('?')
}

/// Maskiert in einer URL die Werte geheim wirkender Query-Parameter.
/// Gibt (maskierte_url, wurde_etwas_maskiert) zurück. Der Basis-URL-Teil bleibt.
fn mask_url_query(url: &str) -> (String, bool) {
    let Some(qpos) = url.find('?') else {
        return (url.to_string(), false);
    };
    let (base, query) = url.split_at(qpos);
    let query = &query[1..]; // '?' überspringen
    let mut masked_any = false;
    let mut out_pairs: Vec<String> = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            out_pairs.push(pair.to_string());
            continue;
        }
        if let Some(eq) = pair.find('=') {
            let (k, v) = pair.split_at(eq);
            let v = &v[1..];
            let key_lower = k.to_ascii_lowercase();
            let key_secret = SECRET_QUERY_HINTS.iter().any(|h| key_lower.contains(h));
            if !v.is_empty() && (key_secret || value_looks_secret(v)) {
                out_pairs.push(format!("{k}={MASK}"));
                masked_any = true;
                continue;
            }
        }
        out_pairs.push(pair.to_string());
    }
    if masked_any {
        (format!("{base}?{}", out_pairs.join("&")), true)
    } else {
        (url.to_string(), false)
    }
}

/// Maskiert den Wertteil eines `KEY=VALUE`-Arguments, wenn der Schlüssel geheim wirkt.
/// Gibt (maskiertes_arg, war_geheim) zurück.
fn mask_kv_arg(arg: &str) -> (String, bool) {
    // URL mit geheimem Query-Anteil? -> Query-Werte maskieren, Basis behalten.
    if looks_like_url_with_query(arg) {
        let (masked, changed) = mask_url_query(arg);
        if changed {
            return (masked, true);
        }
    }
    if let Some(eq) = arg.find('=') {
        let (key, val) = arg.split_at(eq);
        let val = &val[1..];
        if !val.is_empty() && (key_looks_secret(key) || value_looks_secret(val)) {
            return (format!("{key}={MASK}"), true);
        }
    }
    if value_looks_secret(arg) {
        return (MASK.to_string(), true);
    }
    (arg.to_string(), false)
}

/// Enthält die Definition Geheimnisse (env/headers/verdächtige args)?
pub fn entry_has_secrets(entry: &ServerEntry) -> bool {
    if entry.env.as_ref().is_some_and(|m| !m.is_empty()) {
        return true;
    }
    if entry.headers.as_ref().is_some_and(|m| !m.is_empty()) {
        return true;
    }
    if let Some(args) = &entry.args {
        if args.iter().any(|a| mask_kv_arg(a).1) {
            return true;
        }
    }
    false
}

/// Liefert eine (ggf.) maskierte Kopie der Definition.
pub fn mask_entry(entry: &ServerEntry, reveal: bool) -> ServerEntry {
    if reveal {
        return entry.clone();
    }
    let mut out = entry.clone();
    if let Some(env) = out.env.as_mut() {
        for v in env.values_mut() {
            *v = MASK.to_string();
        }
    }
    if let Some(headers) = out.headers.as_mut() {
        for v in headers.values_mut() {
            *v = MASK.to_string();
        }
    }
    if let Some(args) = out.args.as_mut() {
        for a in args.iter_mut() {
            *a = mask_kv_arg(a).0;
        }
    }
    out
}

/// Maskiert eine freie Zusammenfassungszeile (z. B. aus `claude mcp list`).
pub fn mask_summary(summary: &str, reveal: bool) -> String {
    if reveal {
        return summary.to_string();
    }
    summary
        .split_whitespace()
        .map(|tok| mask_kv_arg(tok).0)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Ersetzt in beliebigem Freitext geheim aussehende Fragmente durch `MASK`.
///
/// Erfasst zwei Klassen: `KEY=VALUE`-Vorkommen mit geheimem Key (bzw. geheimem
/// Wert) sowie einzelne Tokens, die `value_looks_secret` erfüllen
/// (Bearer/Basic/JWT/bekannte Präfixe/lange opake Strings). Robust und ohne
/// Panik – arbeitet whitespace-tokenweise und lässt Nicht-Secrets unangetastet.
pub fn redact_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    // Über Whitespace-Grenzen iterieren, dabei die originalen Trenner erhalten.
    while !rest.is_empty() {
        // Führenden Whitespace unverändert übernehmen.
        let ws_end = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());
        if ws_end > 0 {
            out.push_str(&rest[..ws_end]);
            rest = &rest[ws_end..];
            if rest.is_empty() {
                break;
            }
        }
        // Nächstes Token bis zum nächsten Whitespace.
        let tok_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..tok_end];
        out.push_str(&redact_token(token));
        rest = &rest[tok_end..];
    }
    // Zweiter Durchlauf: eingebettete Signatur-Tokens (JWT, bekannte Präfixe)
    // auch OHNE Whitespace-Trennung maskieren – z. B. in kompaktem JSON wie
    // {"env":{"TOKEN":"ghp_…"}}, das eine fehlschlagende CLI zurückgeben könnte.
    redact_embedded_signatures(&out)
}

/// Zeichen, die zu einem Token-Kern gehören (ASCII, secret-typisch).
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '=')
}

/// Maskiert eingebettete Secrets, die an einem JWT- (`eyJ`) oder bekannten
/// Präfix (ghp_, sk-, …) beginnen – unabhängig von umgebenden Trennzeichen.
/// Erfasst so auch in dichten Strings (kompaktes JSON) verborgene Tokens.
fn redact_embedded_signatures(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut idx = 0usize;
    while idx < text.len() {
        let rest = &text[idx..];
        let hit = rest.starts_with("eyJ")
            || SECRET_TOKEN_PREFIXES.iter().any(|p| rest.starts_with(p));
        if hit {
            // Token-Kern ab hier bis zum ersten Nicht-Token-Zeichen konsumieren.
            let mut end = idx;
            for (off, c) in rest.char_indices() {
                if is_token_char(c) {
                    end = idx + off + c.len_utf8();
                } else {
                    break;
                }
            }
            if end - idx >= 8 {
                out.push_str(MASK);
                idx = end;
                continue;
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        idx += c.len_utf8();
    }
    out
}

/// Redigiert ein einzelnes (whitespace-freies) Token; behält umgebende
/// Satzzeichen/Klammern bei und maskiert nur den geheimen Kern.
fn redact_token(token: &str) -> String {
    // Umgebende „Rand"-Zeichen (Anführungszeichen, Klammern, Satzzeichen) abschälen.
    let trim_chars: &[char] = &['"', '\'', '`', '(', ')', '[', ']', '{', '}', ',', ';', '<', '>'];
    let stripped_front = token.trim_start_matches(|c| trim_chars.contains(&c));
    let core = stripped_front.trim_end_matches(|c| trim_chars.contains(&c));
    if core.is_empty() {
        return token.to_string();
    }
    // Byte-Offsets des Kerns im Original bestimmen (Ränder bleiben erhalten).
    let lead_len = token.len() - stripped_front.len();
    let lead = &token[..lead_len];
    let trail = &token[lead_len + core.len()..];

    // Der (ggf. schon bestehende) mask_kv_arg-Pfad deckt KEY=VALUE, URL-Query
    // und einzelne Secret-Tokens ab.
    let (masked, changed) = mask_kv_arg(core);
    if changed {
        format!("{lead}{masked}{trail}")
    } else {
        token.to_string()
    }
}

/// Kurzbeschreibung für die Listenzeile aus einer Definition ableiten.
pub fn summarize_entry(entry: &ServerEntry) -> String {
    if let Some(url) = &entry.url {
        return url.clone();
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(cmd) = &entry.command {
        parts.push(cmd.clone());
    }
    if let Some(args) = &entry.args {
        parts.extend(args.iter().cloned());
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pfade_bleiben_sichtbar() {
        // Regression #3: normale Pfade/Wörter dürfen NICHT maskiert werden.
        for s in [
            "/home/user/projects/mcp-manager",
            "/usr/bin/python3",
            "@modelcontextprotocol/server-filesystem",
            "mcp/grafana:latest",
        ] {
            assert_eq!(mask_kv_arg(s).0, s, "sollte unverändert bleiben: {s}");
            assert!(!value_looks_secret(s), "kein Secret: {s}");
        }
    }

    #[test]
    fn echte_secrets_werden_erkannt() {
        assert!(value_looks_secret("ghp_1234567890abcdefGHIJ"));
        assert!(value_looks_secret("EXAMPLEexample1234567890abcdef"));
        assert!(value_looks_secret("eyJhbGciOiJIUzI1NiJ9.abc.def"));
        assert_eq!(mask_kv_arg("API_TOKEN=EXAMPLEexample1234").0, "API_TOKEN=••••••••");
    }

    #[test]
    fn redact_secrets_erfasst_kompaktes_json() {
        // Regression #4: Token in whitespace-freiem JSON muss redigiert werden.
        let leaked = "error: invalid config {\"env\":{\"TOKEN\":\"ghp_ABC123def456ghi789\"}}";
        let red = redact_secrets(leaked);
        assert!(!red.contains("ghp_ABC123def456ghi789"), "Token darf nicht durchrutschen: {red}");
        assert!(red.contains(MASK));
        // Freitext mit Pfad bleibt lesbar.
        let plain = "spawn failed for /home/user/mcp-servers/example-mcp";
        assert_eq!(redact_secrets(plain), plain);
    }
}
