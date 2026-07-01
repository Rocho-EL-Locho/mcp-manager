import type { MergedServer } from "./ipc";

export interface CleanupHint {
  note: string;
  command?: string;
}

/// Leitet aus der Server-Definition nicht-destruktive Aufräum-Hinweise ab.
/// Nichts davon wird automatisch ausgeführt – der Nutzer entscheidet selbst.
export function cleanupHints(server: MergedServer): CleanupHint[] {
  const hints: CleanupHint[] = [];
  const entry = server.entry;
  if (!entry) return hints;

  const command = entry.command ?? "";
  const args = entry.args ?? [];

  if (command === "docker") {
    const image = args.find(
      (a) => !a.startsWith("-") && !a.includes("=") && a.includes("/") && !a.startsWith("/"),
    );
    if (image) {
      hints.push({
        note: "Docker-Image wird nicht automatisch entfernt:",
        command: `docker image rm ${image}`,
      });
    }
  }

  if (command === "npx" || args[0] === "npx") {
    hints.push({ note: "npx-Cache kann Reste enthalten (~/.npm/_npx)." });
  }

  if (command === "uvx" || command === "uv") {
    const di = args.indexOf("--directory");
    if (di >= 0 && args[di + 1]) {
      hints.push({
        note: "Projektverzeichnis NICHT löschen (evtl. eigener Code):",
        command: args[di + 1],
      });
    } else {
      hints.push({ note: "uv-Cache kann Reste enthalten:", command: "uv cache prune" });
    }
  }

  if (entry.url) {
    hints.push({
      note: "Gespeicherte OAuth-Anmeldung entfernen:",
      command: `claude mcp logout ${server.name}`,
    });
  }

  return hints;
}
