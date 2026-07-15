// Kleiner DOM-Helfer. Baut Elemente ausschließlich über textContent /
// createTextNode, damit Fremddaten (Servernamen, args, env-Werte) niemals als
// HTML interpretiert werden (kein XSS über innerHTML).

type EventHandler = (e: Event) => void;
type Attrs = Record<string, string | number | boolean | EventHandler | undefined>;

export function h(
  tag: string,
  attrs?: Attrs,
  ...children: Array<Node | string | null | undefined>
): HTMLElement {
  const el = document.createElement(tag);
  if (attrs) {
    for (const [key, value] of Object.entries(attrs)) {
      if (value === undefined || value === false) continue;
      if (key.startsWith("on") && typeof value === "function") {
        el.addEventListener(key.slice(2).toLowerCase(), value as EventHandler);
      } else if (key === "class") {
        el.className = String(value);
      } else if (key === "text") {
        el.textContent = String(value);
      } else if (key === "title") {
        el.title = String(value);
      } else if (value === true) {
        el.setAttribute(key, "");
      } else {
        el.setAttribute(key, String(value));
      }
    }
  }
  for (const child of children) {
    if (child === null || child === undefined) continue;
    el.append(typeof child === "string" ? document.createTextNode(child) : child);
  }
  return el;
}

export function clear(el: Element): void {
  while (el.firstChild) el.removeChild(el.firstChild);
}

// SVG-Elemente brauchen den SVG-Namespace (createElementNS) – `h()` erzeugt nur
// HTML-Elemente. Attribute sind hier rein numerisch/statisch (kein XSS).
export function svgEl(
  tag: string,
  attrs?: Record<string, string | number>,
  ...children: Array<Node | null | undefined>
): SVGElement {
  const el = document.createElementNS("http://www.w3.org/2000/svg", tag);
  if (attrs) {
    for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, String(v));
  }
  for (const c of children) {
    if (c) el.append(c);
  }
  return el;
}
