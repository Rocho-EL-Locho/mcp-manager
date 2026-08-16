// Pfad-Darstellung für die UI. Die Anzeige ist rein kosmetisch – gearbeitet
// wird im Backend immer mit dem vollen Pfad.

/// Ersetzt das Home-Verzeichnis am Anfang durch `~`. Ist `path` genau das
/// Home-Verzeichnis, kommt `~` heraus; sonst `~/rest`. Leeres `home` oder ein
/// Pfad außerhalb bleibt unverändert.
export function homeRelative(path: string, home: string): string {
  if (!home) return path;
  if (path === home) return "~";
  if (path.startsWith(home + "/")) return "~/" + path.slice(home.length + 1);
  return path;
}
