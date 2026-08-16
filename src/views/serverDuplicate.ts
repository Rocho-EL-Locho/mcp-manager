// Dialog „Server duplizieren": Kopie unter neuem Namen in einem (ggf. anderen)
// Scope und Projekt anlegen. Eigene Datei, weil das ein vollständiges Formular
// mit Ziel-Auswahl, Sichtbarkeitslogik, Validierung und IPC ist – in
// `serverDetail.ts` war es nur der Aufruf eines Knopfes.
import { h } from "../dom";
import type { MergedServer, ProjectInfo, Scope } from "../ipc";
import { cloneServer, listProjects } from "../ipc";
import { openModal } from "../modal";
import { toast } from "../toast";
import { field } from "../form";
import { scopeSelect } from "../scope";

/// Öffnet den Duplizieren-Dialog für `server`. `onDone` läuft nur nach einer
/// erfolgreich angelegten Kopie (der Aufrufer schließt damit die Detailansicht
/// und lädt neu).
export function openDuplicateModal(server: MergedServer, onDone: () => void): void {
  const currentScope = server.scope as Scope;

  const nameInput = h("input", { class: "inp" }) as HTMLInputElement;
  nameInput.value = `${server.name}-kopie`;

  const scopeSel = scopeSelect(currentScope);

  // Projekt-Auswahl: Dropdown aus bekannten Projekten + Freitext-Pfad.
  const projSelect = h("select", { class: "inp" }, h("option", { value: "" }, "– Projekt wählen –")) as HTMLSelectElement;
  const projInput = h("input", { class: "inp mono", placeholder: "/pfad/zum/projekt" }) as HTMLInputElement;
  void listProjects()
    .then((projs: ProjectInfo[]) => {
      for (const p of projs) {
        const label = p.exists ? p.path : `${p.path} (fehlt)`;
        const opt = h("option", { value: p.path }, label) as HTMLOptionElement;
        if (!p.exists) opt.disabled = true; // fehlende Verzeichnisse nicht auswählbar
        projSelect.appendChild(opt);
      }
      // Standard: aktuelles Projekt des Servers vorbelegen, falls vorhanden.
      if (server.project_path) {
        projSelect.value = server.project_path;
        projInput.value = server.project_path;
      }
    })
    .catch(() => {
      projSelect.appendChild(
        h("option", { value: "" }, "(Projekte nicht ladbar – Pfad manuell eingeben)"),
      );
    });
  projSelect.addEventListener("change", () => {
    if (projSelect.value) projInput.value = projSelect.value;
  });

  const projField = field(
    "Projekt",
    h("div", { class: "scope-row" }, projSelect, projInput),
    "Ziel-Verzeichnis für local/project (Dropdown oder eigener Pfad).",
  );
  const scopeHint = h("div", { class: "field-hint", text: "" });

  const syncScopeUi = () => {
    const isProjectScoped = scopeSel.value === "local" || scopeSel.value === "project";
    projField.style.display = isProjectScoped ? "" : "none";
    scopeHint.textContent =
      scopeSel.value === "project"
        ? "Der Server wird in .mcp.json des Zielprojekts angelegt und muss dort ggf. erst bestätigt werden."
        : "";
  };
  scopeSel.addEventListener("change", syncScopeUi);
  syncScopeUi();

  const status = h("p", { class: "form-status" });
  const cancelBtn = h("button", { class: "btn" }, "Abbrechen") as HTMLButtonElement;
  const okBtn = h("button", { class: "btn btn-primary" }, "Duplizieren") as HTMLButtonElement;

  const form = h(
    "div",
    { class: "server-form" },
    field("Neuer Name", nameInput),
    field("Ziel-Scope", scopeSel),
    scopeHint,
    projField,
    status,
  );

  const modal = openModal(`Duplizieren: ${server.name}`, form, [cancelBtn, okBtn]);
  cancelBtn.addEventListener("click", () => modal.close());
  nameInput.focus();
  nameInput.select();

  okBtn.addEventListener("click", async () => {
    const newName = nameInput.value.trim();
    const toScope = scopeSel.value as Scope;
    const isProjectScoped = toScope === "local" || toScope === "project";
    const toProject = isProjectScoped ? projInput.value.trim() : undefined;

    status.className = "form-status";
    if (!newName) {
      status.className = "form-status error";
      status.textContent = "Bitte einen neuen Namen angeben.";
      return;
    }
    if (isProjectScoped && !toProject) {
      status.className = "form-status error";
      status.textContent = "Bitte ein Zielprojekt wählen oder einen Pfad angeben.";
      return;
    }

    okBtn.disabled = true;
    cancelBtn.disabled = true;
    status.textContent = "wird angelegt…";
    try {
      await cloneServer(
        server.name,
        currentScope,
        newName,
        toScope,
        server.project_path ?? undefined,
        toProject,
      );
      toast(`„${newName}" in ${toScope} angelegt`);
      modal.close();
      onDone();
    } catch (e) {
      okBtn.disabled = false;
      cancelBtn.disabled = false;
      status.className = "form-status error";
      status.textContent = "Fehler: " + String(e);
    }
  });
}
