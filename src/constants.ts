// Gemeinsame Frontend-Konstanten.
//
// TIMEOUT_MIN/MAX spiegeln nur die Anzeige (Feld-`min`/`max`, Hinweistext) wider.
// **Maßgeblich ist das Backend** (`settings.rs::validate`): dort wird der Bereich
// erzwungen und eine klare Fehlermeldung geliefert. Das Frontend prüft daher
// clientseitig nur die Basissanität (ganze Zahl > 0) und lässt die exakte
// Bereichsprüfung dem Backend – so können Front-/Backend nicht in Annahme/
// Ablehnung auseinanderlaufen (ein veralteter Hinweistext bliebe rein kosmetisch).

/** Untergrenze der konfigurierbaren Timeouts (Sekunden) – Anzeige. */
export const TIMEOUT_MIN = 5;
/** Obergrenze der konfigurierbaren Timeouts (Sekunden) – Anzeige. */
export const TIMEOUT_MAX = 600;

/** Obergrenze für das Auto-Refresh-Intervall (Minuten, = 1 Woche). Clientseitig
 *  geklemmt, weil `auto_refresh_minutes` als u32 serialisiert wird und ein
 *  Überlauf sonst nur einen kryptischen Deserialisierungsfehler erzeugte. */
export const AUTO_REFRESH_MAX = 10080;

/** Grenzen der Snapshot-Aufbewahrung – Anzeige. Maßgeblich ist das Backend
 *  (`settings.rs::validate`, `RETENTION_MIN`/`RETENTION_MAX`). */
export const RETENTION_MIN = 1;
export const RETENTION_MAX = 500;

/** Zeilen-Obergrenze der Live-Diagnose-Ansicht – Spiegel von
 *  `logview.rs::RING_CAPACITY`. Das Backend deckelt seinen Ring, emittiert aber
 *  jede Zeile; ohne dieselbe Grenze im Webview sammeln Array und DOM unbegrenzt
 *  (ein Server in der stderr-Schleife erzeugt so hunderttausende Knoten, und
 *  stirbt der Webview daran, läuft der Exit-Hook nicht mehr → verwaiste
 *  Prozessgruppe). Beim Anhängen wird vorne verworfen. */
export const LOG_RING_CAPACITY = 2000;
