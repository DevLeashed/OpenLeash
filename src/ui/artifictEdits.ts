/** Direct edits: what the user changes on the canvas in Edit mode, and how
 *  those changes become the source of a new revision.
 *
 *  The frame reports what was picked (a CSS path, the element's own text when
 *  it has no child elements, a few computed styles). Everything the user then
 *  types or drags lives here, host-side; the frame is only told the current
 *  edits so the preview can show them, and it can never add one itself.
 *
 *  Saving is deliberately conservative, in the spirit of the artifict
 *  `edits` contract: a text change is placed only where the original text
 *  occurs exactly once between two tags in the source, or the save is refused
 *  and says why. Guessing which of two identical headings was meant would
 *  silently edit the wrong one. Style changes go into one marked block,
 *  `<style data-openleash-edits>`, keyed by the picked element's selector, so
 *  the agent can see exactly what the user changed and fold it into the
 *  design's own CSS later. */

export type EditPropKind = "color" | "px" | "select";
export interface EditPropDef { prop: EditProp; label: string; kind: EditPropKind; min?: number; max?: number; options?: readonly string[] }
export type EditProp = "color" | "background-color" | "font-size" | "font-weight" | "padding" | "border-radius";

/** The properties the edit panel offers. Kept short on purpose: anything
 *  structural is a comment for the agent, not a knob. */
export const EDIT_PROPS: readonly EditPropDef[] = [
  { prop: "color", label: "Text colour", kind: "color" },
  { prop: "background-color", label: "Background", kind: "color" },
  { prop: "font-size", label: "Font size", kind: "px", min: 6, max: 200 },
  { prop: "font-weight", label: "Weight", kind: "select", options: ["300", "400", "500", "600", "700", "800", "900"] },
  { prop: "padding", label: "Padding", kind: "px", min: 0, max: 400 },
  { prop: "border-radius", label: "Corner radius", kind: "px", min: 0, max: 400 },
];
const PROP_NAMES = new Set<string>(EDIT_PROPS.map((p) => p.prop));
export const isEditProp = (name: string): name is EditProp => PROP_NAMES.has(name);

/** The element the frame reported for Edit mode. `text` is the element's own
 *  text when it has no child elements (so replacing it cannot drop markup),
 *  otherwise null and only its styles can be edited. */
export interface InspectPick { selector: string; label: string; text: string | null; styles: Partial<Record<EditProp, string>> }

/** One element's pending change. `originalText` is what the frame reported
 *  when it was picked; `text` is set only once the user changes it. */
export interface ElementEdit {
  selector: string;
  label: string;
  originalText: string | null;
  text?: string;
  styles: Partial<Record<EditProp, string>>;
}
export type EditSet = Record<string, ElementEdit>;

export const MAX_EDIT_ELEMENTS = 64;
export const MAX_EDIT_TEXT = 2000;

/** A selector as the bridge builds it — tag names, `:nth-of-type()`, `#id`,
 *  `>` — never anything that could close the style element or open a block. */
export function safeSelector(selector: string): boolean {
  return selector.length > 0 && selector.length <= 300 && !/[<{};@]|\/\*/.test(selector);
}

/** A validated CSS value for one property, or null. Only shapes the panel can
 *  produce are accepted, so nothing typed here can break out of a declaration. */
export function cssValue(prop: EditProp, raw: string): string | null {
  const value = raw.trim();
  const def = EDIT_PROPS.find((p) => p.prop === prop)!;
  if (def.kind === "color") {
    if (/^#[0-9a-f]{3}([0-9a-f]{3})?([0-9a-f]{2})?$/i.test(value)) return value.toLowerCase();
    if (value === "transparent") return value;
    return /^rgba?\(\s*[\d.]+%?\s*,\s*[\d.]+%?\s*,\s*[\d.]+%?\s*(,\s*[\d.]+%?\s*)?\)$/.test(value) ? value : null;
  }
  if (def.kind === "select") return def.options!.includes(value) ? value : null;
  const n = Number(value.replace(/px$/, ""));
  if (!value || !Number.isFinite(n) || n < (def.min ?? 0) || n > (def.max ?? 1000)) return null;
  return `${Math.round(n * 10) / 10}px`;
}

/** `rgb(17, 24, 39)` → `#111827`, for a colour input. Transparent and
 *  anything unparseable come back null. */
export function colorToHex(value: string | undefined): string | null {
  if (!value) return null;
  const v = value.trim();
  if (/^#[0-9a-f]{6}$/i.test(v)) return v.toLowerCase();
  if (/^#[0-9a-f]{3}$/i.test(v)) return `#${v.slice(1).split("").map((c) => c + c).join("")}`.toLowerCase();
  const m = v.match(/^rgba?\(\s*(\d+(?:\.\d+)?)\s*,\s*(\d+(?:\.\d+)?)\s*,\s*(\d+(?:\.\d+)?)\s*(?:,\s*([\d.]+)\s*)?\)$/);
  if (!m) return null;
  if (m[4] !== undefined && Number(m[4]) === 0) return null;
  return `#${[m[1], m[2], m[3]].map((c) => Math.max(0, Math.min(255, Math.round(Number(c)))).toString(16).padStart(2, "0")).join("")}`;
}

/** The first pixel number of a computed value (`"8px 16px"` → 8). */
export function pxNumber(value: string | undefined): number | null {
  const m = value?.match(/^\s*(-?\d+(?:\.\d+)?)px/);
  return m ? Math.round(Number(m[1]) * 10) / 10 : null;
}

/** Whether the user changed an element's text (surrounding whitespace aside:
 *  the field shows the text trimmed). */
export function textChanged(edit: ElementEdit): boolean {
  return edit.text !== undefined && edit.text.trim() !== (edit.originalText ?? "").trim();
}

/** Whether an element's edit changes anything. */
export function editChanges(edit: ElementEdit): boolean {
  return textChanged(edit) || Object.keys(edit.styles).length > 0;
}

/** What the frame needs to show the current edits live. */
export function liveEdits(edits: EditSet): { selector: string; styles: Record<string, string>; text?: string }[] {
  return Object.values(edits).filter(editChanges).map((edit) => ({
    selector: edit.selector,
    styles: { ...edit.styles } as Record<string, string>,
    ...(textChanged(edit) ? { text: edit.text } : {}),
  }));
}

const escapeHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
const escapeRegExp = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

const BLOCK_OPEN = "<style data-openleash-edits>";
const BLOCK_RE = /<style data-openleash-edits>([\s\S]*?)<\/style>\n?/i;
const BLOCK_NOTE = "/* Direct edits made in the OpenLeash studio. Keep them, or fold them into the design's own CSS. */";

/** The rules an existing edits block already holds, so a second round of edits
 *  adds to the first instead of replacing it. Only the shape this module writes
 *  is read back; anything else in the block is left as it was. */
function readBlock(body: string): { rules: Map<string, Map<string, string>>; other: string[] } {
  const rules = new Map<string, Map<string, string>>();
  const other: string[] = [];
  for (const line of body.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed || trimmed === BLOCK_NOTE) continue;
    const m = trimmed.match(/^(.+?)\s*\{([^{}]*)\}$/);
    if (!m || !safeSelector(m[1]!)) { other.push(line); continue; }
    const decls = rules.get(m[1]!) ?? new Map<string, string>();
    for (const decl of m[2]!.split(";")) {
      const at = decl.indexOf(":");
      if (at < 0) continue;
      const prop = decl.slice(0, at).trim();
      const value = decl.slice(at + 1).replace(/!important/g, "").trim();
      if (prop && value) decls.set(prop, value);
    }
    rules.set(m[1]!, decls);
  }
  return { rules, other };
}

function writeBlock(rules: Map<string, Map<string, string>>, other: string[]): string {
  const lines = [...rules].filter(([, decls]) => decls.size).map(([selector, decls]) => `${selector} { ${[...decls].map(([p, v]) => `${p}: ${v} !important;`).join(" ")} }`);
  return `${BLOCK_OPEN}\n${BLOCK_NOTE}\n${[...other, ...lines].join("\n")}\n</style>\n`;
}

export interface ApplyResult { content: string; text: number; styles: number; failed: { label: string; reason: string }[] }

/** Apply every pending edit to `source`. All or nothing for the caller: when
 *  `failed` is non-empty the content is still returned (for inspection), but
 *  the studio refuses to save it. */
export function applyEditsToSource(source: string, edits: EditSet): ApplyResult {
  let content = source;
  let text = 0, styles = 0;
  const failed: ApplyResult["failed"] = [];
  const list = Object.values(edits).filter(editChanges);
  for (const edit of list) {
    if (edit.text === undefined || !textChanged(edit)) continue;
    const before = (edit.originalText ?? "").trim();
    const after = edit.text.trim();
    if (!before) { failed.push({ label: edit.label, reason: "it has no text of its own to replace" }); continue; }
    if (!after) { failed.push({ label: edit.label, reason: "its new text is empty" }); continue; }
    // Between two tags: `>  Pricing  <`. The raw text and its HTML-escaped form
    // are both tried, because the frame reports text as rendered (`&` rather
    // than `&amp;`).
    const candidates = [...new Set([before, escapeHtml(before)])];
    let placed = false, seen = 0;
    for (const candidate of candidates) {
      const re = new RegExp(`(>\\s*)${escapeRegExp(candidate)}(\\s*<)`, "g");
      const hits = content.match(re)?.length ?? 0;
      seen = Math.max(seen, hits);
      if (hits === 1) {
        content = content.replace(re, (_m, lead: string, tail: string) => `${lead}${escapeHtml(after)}${tail}`);
        placed = true;
        break;
      }
    }
    if (placed) text++;
    else failed.push({ label: edit.label, reason: seen > 1 ? `its text appears ${seen} times in the source, so which one you meant is ambiguous` : "its text is not written literally in the source (a script may render it)" });
  }
  const styled = list.filter((edit) => Object.keys(edit.styles).length > 0);
  if (styled.length) {
    const existing = content.match(BLOCK_RE);
    const { rules, other } = readBlock(existing?.[1] ?? "");
    for (const edit of styled) {
      if (!safeSelector(edit.selector)) { failed.push({ label: edit.label, reason: "its selector cannot be written into a style rule" }); continue; }
      const decls = rules.get(edit.selector) ?? new Map<string, string>();
      for (const [prop, raw] of Object.entries(edit.styles)) {
        const value = isEditProp(prop) && raw !== undefined ? cssValue(prop, raw) : null;
        if (value === null) { failed.push({ label: edit.label, reason: `“${prop}” has an invalid value` }); continue; }
        decls.set(prop, value);
        styles++;
      }
      rules.set(edit.selector, decls);
    }
    const block = writeBlock(rules, other);
    if (existing) content = content.replace(BLOCK_RE, block);
    else {
      // Last in the document, so it follows the design's own styles; inside
      // `<body>`/`<html>` when the source is a whole document.
      const lower = content.toLowerCase();
      const bodyAt = lower.lastIndexOf("</body>");
      const insert = bodyAt >= 0 ? bodyAt : lower.lastIndexOf("</html>");
      content = insert >= 0 ? `${content.slice(0, insert)}${block}${content.slice(insert)}` : `${content.replace(/\s*$/, "\n")}${block}`;
    }
  }
  return { content, text, styles, failed };
}

/** The version note a canvas edit is saved with: what changed, briefly. */
export function editNote(edits: EditSet, result: ApplyResult): string {
  const labels = Object.values(edits).filter(editChanges).map((e) => e.label);
  const parts = [result.text ? `${result.text} text change${result.text === 1 ? "" : "s"}` : "", result.styles ? `${result.styles} style change${result.styles === 1 ? "" : "s"}` : ""].filter(Boolean);
  let note = `Edited on the canvas: ${parts.join(", ") || "changes"}${labels.length ? ` (${labels.join("; ")})` : ""}`;
  if (new TextEncoder().encode(note).byteLength > 300) {
    while (new TextEncoder().encode(`${note}…`).byteLength > 300) note = note.slice(0, -1);
    note = `${note}…`;
  }
  return note;
}
