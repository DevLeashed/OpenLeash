// Pure helpers behind the Browser view: address-bar parsing, history
// suggestions, keyboard mapping, and the annotate payload. Kept apart from the
// component so each rule is pinned by a unit test rather than by a click path.
import type { BrowserAnnotation, BrowserElement, BrowserKey, BrowserModifiers, BrowserRect } from "../api";

/** What the address bar should open, or why it won't. It never searches: a
 *  word that is not an address is an error, not a query sent to a search
 *  engine with whatever the user happened to type. */
export function normalizeAddress(raw: string): { url: string } | { error: string } {
  const text = raw.trim();
  if (!text) return { error: "Type an address to open." };
  if (/\s/.test(text)) return { error: "That isn't an address. The address bar opens pages; it doesn't search the web." };
  let candidate: string;
  if (/^https?:\/\//i.test(text)) candidate = text;
  // Another scheme (file:, javascript:, data:, chrome://…). `localhost:3000`
  // also starts with "word:", which is why a digit after the colon is a port.
  else if (/^[a-z][a-z0-9+.-]*:(?!\d)/i.test(text)) return { error: "Only http and https pages can be opened here." };
  else {
    const host = text.split(/[/?#]/, 1)[0]!.toLowerCase();
    const bare = host.startsWith("[") ? host.slice(0, host.indexOf("]") + 1) : host.replace(/:\d+$/, "");
    const local = bare === "localhost" || bare.endsWith(".localhost") || /^\d{1,3}(\.\d{1,3}){3}$/.test(bare) || bare.startsWith("[");
    const port = /:\d+$/.test(host);
    if (local || (port && !bare.includes("."))) candidate = `http://${text}`;
    else if (bare.includes(".") && !bare.startsWith(".") && !bare.endsWith(".")) candidate = `https://${text}`;
    else return { error: `“${text}” isn't an address. Type a full one, like example.com or localhost:3000 — the address bar doesn't search the web.` };
  }
  try {
    const u = new URL(candidate);
    if ((u.protocol !== "http:" && u.protocol !== "https:") || !u.hostname) return { error: "Only http and https pages can be opened here." };
    return { url: u.href };
  } catch {
    return { error: "That address can't be opened." };
  }
}

/** Visited pages matching what is typed, newest first. */
export function historySuggestions(history: string[] | undefined, typed: string, current: string, limit = 8): string[] {
  const q = typed.trim().toLowerCase();
  const all = history ?? [];
  // Untouched address (still the page's own URL): offer the history as is.
  const hits = !q || typed === current ? all : all.filter((u) => u.toLowerCase().includes(q));
  return hits.filter((u) => u !== typed).slice(0, limit);
}

const SPECIAL: Record<string, BrowserKey> = {
  Enter: "Enter", Tab: "Tab", Backspace: "Backspace", Delete: "Delete", Escape: "Escape",
  ArrowUp: "ArrowUp", ArrowDown: "ArrowDown", ArrowLeft: "ArrowLeft", ArrowRight: "ArrowRight",
  Home: "Home", End: "End", PageUp: "PageUp", PageDown: "PageDown",
};

export type KeyIntent = { kind: "key"; key: BrowserKey; modifiers?: BrowserModifiers } | { kind: "text"; text: string } | null;

/** What a keystroke on the focused live view sends to the page. `typing` says
 *  whether printable text is mid-batch, so a space joins the word being typed
 *  instead of becoming a separate Space press. */
export function keyIntent(e: Pick<KeyboardEvent, "key" | "ctrlKey" | "shiftKey" | "altKey" | "metaKey">, typing: boolean): KeyIntent {
  const chord = e.ctrlKey || e.altKey || e.metaKey;
  const modifiers: BrowserModifiers = {};
  if (e.ctrlKey) modifiers.ctrl = true;
  if (e.shiftKey) modifiers.shift = true;
  if (e.altKey) modifiers.alt = true;
  if (e.metaKey) modifiers.meta = true;
  const mods = Object.keys(modifiers).length ? { modifiers } : {};
  const special = SPECIAL[e.key];
  if (special) return { kind: "key", key: special, ...mods };
  if (e.key === " ") return !chord && typing ? { kind: "text", text: " " } : { kind: "key", key: "Space", ...mods };
  // A chord with a printable key is an app shortcut (Ctrl+F, Ctrl+W…), and the
  // panel has no closed-enum key for it: leave it to the app.
  if (e.key.length === 1 && !chord) return { kind: "text", text: e.key };
  return null;
}

/** The styles Annotate offers, in the order the controls appear. */
export const ANNOTATE_STYLES: { property: string; label: string; kind: "text" | "color" | "weight" }[] = [
  { property: "font-size", label: "Font size", kind: "text" },
  { property: "font-weight", label: "Font weight", kind: "weight" },
  { property: "line-height", label: "Line height", kind: "text" },
  { property: "letter-spacing", label: "Letter spacing", kind: "text" },
  { property: "color", label: "Text color", kind: "color" },
  { property: "background-color", label: "Background", kind: "color" },
  { property: "padding", label: "Padding", kind: "text" },
  { property: "margin", label: "Margin", kind: "text" },
  { property: "border-radius", label: "Border radius", kind: "text" },
];

/** Mirrors the backend's style value check, so an invalid value is flagged at
 *  the field rather than bounced by the preview. The backend stays the gate. */
export function styleValueOk(value: string): boolean {
  const v = value.trim();
  if (!v || v.length > 200) return false;
  if ([...v].some((c) => c.charCodeAt(0) < 32 || c.charCodeAt(0) === 127) || /[;{}<>\\@]/.test(v)) return false;
  return !/url\(|expression|javascript:|image-set\(|image\(|element\(|\/\*/i.test(v);
}

/** `rgb(…)`/`rgba(…)` as `#rrggbb` for a colour input; `null` when it isn't one. */
export function toHex(value: string | undefined): string | null {
  if (!value) return null;
  if (/^#[0-9a-f]{6}$/i.test(value)) return value.toLowerCase();
  const m = value.match(/^rgba?\(\s*(\d+)[\s,]+(\d+)[\s,]+(\d+)/i);
  if (!m) return null;
  return "#" + [m[1], m[2], m[3]].map((n) => Math.min(255, Number(n)).toString(16).padStart(2, "0")).join("");
}

export interface PendingAnnotation {
  id: number;
  comment: string;
  bbox: BrowserRect;
  element?: BrowserElement;
  /** property → new value; only properties the user changed. */
  styles: Record<string, string>;
}

/** The wire shape `browser_annotate` takes. */
export function annotationPayload(list: PendingAnnotation[]): BrowserAnnotation[] {
  return list.map((a) => {
    const out: BrowserAnnotation = { comment: a.comment.trim(), bbox: a.bbox };
    if (a.element) {
      const e = a.element;
      out.element = { ref: e.ref, selector: e.selector, tag: e.tag, role: e.role, name: e.name };
      const styles = Object.entries(a.styles).filter(([p, v]) => v.trim() && v.trim() !== (e.styles[p] ?? "").trim());
      if (styles.length) out.styles = styles.map(([property, after]) => ({ property, before: e.styles[property] ?? "", after: after.trim() }));
    }
    return out;
  });
}

/** The changed, valid styles of one annotation, as a preview call's list. */
export function previewStyles(a: Pick<PendingAnnotation, "element" | "styles">) {
  if (!a.element) return [];
  const base = a.element.styles;
  return Object.entries(a.styles).filter(([p, v]) => styleValueOk(v) && v.trim() !== (base[p] ?? "").trim()).map(([property, value]) => ({ property, value: value.trim() }));
}

/** A rectangle from two corner points, in either drag direction, to 0.01px. */
export function rectFrom(a: { x: number; y: number }, b: { x: number; y: number }): BrowserRect {
  const r = (v: number) => Math.round(v * 100) / 100;
  return { x: r(Math.min(a.x, b.x)), y: r(Math.min(a.y, b.y)), width: r(Math.abs(a.x - b.x)), height: r(Math.abs(a.y - b.y)) };
}

/** A short label for an element: tag, role, accessible name. */
export function elementLabel(e: Pick<BrowserElement, "tag" | "role" | "name">): string {
  const name = e.name.trim();
  return [e.tag ? `<${e.tag.toLowerCase()}>` : "", e.role && e.role !== e.tag ? e.role : "", name ? `“${name.length > 60 ? name.slice(0, 60) + "…" : name}”` : ""].filter(Boolean).join(" ");
}
