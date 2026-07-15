// Feine Linien-Icons (Lucide-Stil) als Inline-SVG.
// Die SVG-Strings sind statische, entwickler-definierte Konstanten (keine
// Fremddaten) — daher ist innerHTML hier unbedenklich.

const PATHS: Record<string, string> = {
  menu: '<line x1="3" y1="6" x2="21" y2="6"/><line x1="3" y1="12" x2="21" y2="12"/><line x1="3" y1="18" x2="21" y2="18"/>',
  sparkles:
    '<path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z"/><path d="M18.6 13.6l.6 1.8 1.8.6-1.8.6-.6 1.8-.6-1.8-1.8-.6 1.8-.6z"/>',
  plus: '<line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/>',
  refresh: '<path d="M21 12a9 9 0 11-3-6.7"/><path d="M21 4v5h-5"/>',
  eye: '<path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7-10-7-10-7z"/><circle cx="12" cy="12" r="3"/>',
  "eye-off":
    '<path d="M9.9 5.2A10.4 10.4 0 0112 5c6.4 0 10 7 10 7a17.6 17.6 0 01-3.3 4M6.6 6.6A17.4 17.4 0 002 12s3.6 7 10 7a10 10 0 004.2-.9"/><line x1="4" y1="4" x2="20" y2="20"/>',
  globe:
    '<circle cx="12" cy="12" r="9"/><line x1="3" y1="12" x2="21" y2="12"/><path d="M12 3a14 14 0 010 18M12 3a14 14 0 000 18"/>',
  lock: '<rect x="5" y="11" width="14" height="9" rx="2"/><path d="M8 11V7a4 4 0 018 0v4"/>',
  alert:
    '<path d="M10.3 4.3l-7.5 13A1.5 1.5 0 004.1 19.5h15.8a1.5 1.5 0 001.3-2.2l-7.5-13a1.5 1.5 0 00-2.6 0z"/><line x1="12" y1="9.5" x2="12" y2="13.5"/><line x1="12" y1="16.7" x2="12" y2="16.7"/>',
  x: '<line x1="6" y1="6" x2="18" y2="18"/><line x1="18" y1="6" x2="6" y2="18"/>',
  archive:
    '<rect x="3" y="4" width="18" height="4" rx="1"/><path d="M5 8v11a1 1 0 001 1h12a1 1 0 001-1V8"/><line x1="10" y1="12" x2="14" y2="12"/>',
  terminal: '<polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/>',
  settings:
    '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 11-2.83 2.83l-.06-.06a1.65 1.65 0 00-1.82-.33 1.65 1.65 0 00-1 1.51V21a2 2 0 01-4 0v-.09A1.65 1.65 0 009 19.4a1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 11-2.83-2.83l.06-.06a1.65 1.65 0 00.33-1.82 1.65 1.65 0 00-1.51-1H3a2 2 0 010-4h.09A1.65 1.65 0 004.6 9a1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 112.83-2.83l.06.06a1.65 1.65 0 001.82.33H9a1.65 1.65 0 001-1.51V3a2 2 0 014 0v.09a1.65 1.65 0 001 1.51 1.65 1.65 0 001.82-.33l.06-.06a2 2 0 112.83 2.83l-.06.06a1.65 1.65 0 00-.33 1.82V9a1.65 1.65 0 001.51 1H21a2 2 0 010 4h-.09a1.65 1.65 0 00-1.51 1z"/>',
};

function markup(name: string): string {
  return (
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" ' +
    'stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
    (PATHS[name] ?? "") +
    "</svg>"
  );
}

export function icon(name: string, className = "", title?: string): HTMLElement {
  const span = document.createElement("span");
  span.className = "icon" + (className ? " " + className : "");
  span.setAttribute("aria-hidden", "true");
  if (title) span.title = title;
  span.innerHTML = markup(name); // statischer String, kein XSS-Risiko
  return span;
}

/// Wechselt das Icon eines bestehenden Icon-Spans (z. B. eye <-> eye-off).
export function setIcon(span: HTMLElement, name: string): void {
  span.innerHTML = markup(name);
}
