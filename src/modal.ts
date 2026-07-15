import { h, clear } from "./dom";
import { icon } from "./icons";

export interface ModalHandle {
  close(): void;
  setBody(node: Node): void;
}

/// Ist gerade ein Modal (Overlay) offen? Genutzt vom Auto-Refresh, um einen
/// Full-Rebuild zu unterlassen, solange der Nutzer in einem Dialog arbeitet.
export function modalsOpen(): boolean {
  return document.querySelector(".modal-overlay") !== null;
}

/// Öffnet ein modales Overlay. Schließen per ✕, Klick auf den Hintergrund oder Escape.
export function openModal(title: string, body: Node, footer?: Node[]): ModalHandle {
  const bodyWrap = h("div", { class: "modal-body" }, body);
  const overlay = h("div", { class: "modal-overlay" });

  const close = () => {
    document.removeEventListener("keydown", onKey);
    overlay.remove();
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key !== "Escape") return;
    // Bei gestapelten Modals nur das oberste schließen: das ist das zuletzt
    // an document.body angehängte .modal-overlay.
    const overlays = document.querySelectorAll(".modal-overlay");
    if (overlays[overlays.length - 1] !== overlay) return;
    close();
  };

  const header = h(
    "div",
    { class: "modal-header" },
    h("h2", { text: title }),
    h("button", { class: "btn btn-icon", title: "Schließen", onclick: close }, icon("x")),
  );

  const dialog = h(
    "div",
    { class: "modal" },
    header,
    bodyWrap,
    footer && footer.length ? h("div", { class: "modal-footer" }, ...footer) : null,
  );

  overlay.append(dialog);
  overlay.addEventListener("click", (e) => {
    if (e.target === overlay) close();
  });
  document.addEventListener("keydown", onKey);
  document.body.append(overlay);

  return {
    close,
    setBody(node: Node) {
      clear(bodyWrap);
      bodyWrap.append(node);
    },
  };
}
