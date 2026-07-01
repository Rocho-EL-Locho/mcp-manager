import { h } from "./dom";

let container: HTMLElement | null = null;

export function toast(message: string, kind: "ok" | "error" = "ok", ms = 3500): void {
  if (!container) {
    container = h("div", { class: "toast-container" });
    document.body.append(container);
  }
  const el = h("div", { class: `toast toast-${kind}`, text: message });
  container.append(el);
  window.setTimeout(() => el.remove(), ms);
}
