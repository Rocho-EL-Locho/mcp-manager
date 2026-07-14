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
