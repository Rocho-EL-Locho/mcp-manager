// Desktop-Benachrichtigungen bei Statusverschlechterung (Feature 09).
// Die Opt-in-Prüfung (Einstellung `notifications`) macht der Aufrufer; hier nur
// Permission-Handling und Versand.
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

let granted: boolean | null = null;

/// Fordert die Benachrichtigungs-Berechtigung bei Bedarf einmalig an (lazy).
async function ensurePermission(): Promise<boolean> {
  if (granted !== null) return granted;
  try {
    let ok = await isPermissionGranted();
    if (!ok) ok = (await requestPermission()) === "granted";
    granted = ok;
  } catch {
    granted = false;
  }
  return granted;
}

/// Zeigt eine Benachrichtigung für einen verschlechterten Serverstatus an.
export async function notifyStatusChange(name: string, toKind: string): Promise<void> {
  if (!(await ensurePermission())) return;
  const label = toKind === "needs_auth" ? "benötigt jetzt eine Anmeldung" : "ist nicht mehr erreichbar";
  try {
    sendNotification({ title: "MCP-Manager", body: `„${name}" ${label}.` });
  } catch {
    /* Versand best effort – Fehler ignorieren. */
  }
}
