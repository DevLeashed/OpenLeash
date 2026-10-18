/** Tweaks: the adjustment controls an agent declares for an HTML artifict.
 *
 *  A design carries a manifest the browser never executes —
 *  `<script type="application/openleash-tweaks+json">[…]</script>` — and the
 *  studio reads it out of the source text, host-side, without running anything.
 *  Each entry binds one control to one CSS custom property. Moving a control
 *  sends the new value into the isolated frame, whose bridge sets the property
 *  on `:root` (plus a `data-*` attribute for selects and toggles), so the design
 *  answers live without a reload. Nothing here touches the project or reaches
 *  the agent: applying the values is a separate, explicit request.
 *
 *  The manifest is agent-authored and therefore untrusted. Every field is
 *  bounded and typed here, and an entry that does not parse is dropped rather
 *  than guessed at. */

export type Tweak =
  | { type: "range"; name: string; label: string; min: number; max: number; step: number; unit: string; value: number }
  | { type: "color"; name: string; label: string; value: string }
  | { type: "select"; name: string; label: string; options: { label: string; value: string }[]; value: string }
  | { type: "toggle"; name: string; label: string; on: string; off: string; value: boolean }
  | { type: "text"; name: string; label: string; value: string };

export type TweakValue = number | string | boolean;
/** What the frame bridge receives for one control. */
export type TweakMessage = { name: string; value: string; attr?: string; raw: TweakValue };

export const MAX_TWEAKS = 24;
const MAX_TEXT = 200;
const NAME = /^--[A-Za-z0-9_-]{1,64}$/;
const MANIFEST = /<script\b[^>]*\btype\s*=\s*["']?application\/openleash-tweaks\+json["']?[^>]*>([\s\S]*?)<\/script\s*>/i;

const str = (v: unknown, max = MAX_TEXT): string | null => typeof v === "string" && v.trim() && v.length <= max ? v.trim() : null;
const num = (v: unknown): number | null => typeof v === "number" && Number.isFinite(v) ? v : typeof v === "string" && v.trim() && Number.isFinite(Number(v)) ? Number(v) : null;

/** `--accent`, `accent` → `--accent`; anything else is not a custom property. */
function propertyName(raw: unknown): string | null {
  const text = str(raw, 66);
  if (!text) return null;
  const name = text.startsWith("--") ? text : `--${text}`;
  return NAME.test(name) ? name : null;
}

/** A human label from the property when the manifest gave none. */
function labelFor(name: string): string {
  const words = name.slice(2).replace(/[-_]+/g, " ").trim();
  return words ? words[0]!.toUpperCase() + words.slice(1) : name;
}

/** `#abc` → `#aabbcc`, so the native colour input can show it. */
export function hexColor(value: string): string | null {
  const v = value.trim();
  if (/^#[0-9a-f]{6}$/i.test(v)) return v.toLowerCase();
  if (/^#[0-9a-f]{3}$/i.test(v)) return `#${[...v.slice(1)].map((c) => c + c).join("")}`.toLowerCase();
  return null;
}

function parseOne(entry: unknown): Tweak | null {
  if (!entry || typeof entry !== "object" || Array.isArray(entry)) return null;
  const e = entry as Record<string, unknown>;
  const name = propertyName(e.var ?? e.name ?? e.property);
  if (!name) return null;
  const label = str(e.label, 80) ?? labelFor(name);
  const type = str(e.type, 20)?.toLowerCase();
  if (type === "range" || type === "number" || type === "slider") {
    const min = num(e.min) ?? 0, max = num(e.max) ?? 100;
    if (!(max > min)) return null;
    const stepRaw = num(e.step);
    const step = stepRaw && stepRaw > 0 ? stepRaw : (max - min) / 100;
    const unit = typeof e.unit === "string" && /^[a-z%]{0,8}$/i.test(e.unit) ? e.unit : "";
    const value = Math.min(max, Math.max(min, num(e.value) ?? min));
    return { type: "range", name, label, min, max, step, unit, value };
  }
  if (type === "color" || type === "colour") {
    const value = str(e.value, 80) ?? "#000000";
    return { type: "color", name, label, value };
  }
  if (type === "select" || type === "choice" || type === "options") {
    if (!Array.isArray(e.options)) return null;
    const options = e.options.slice(0, 20).map((o) => {
      if (typeof o === "string" || typeof o === "number") { const v = String(o); return v.length <= MAX_TEXT ? { label: v.slice(0, 80), value: v } : null; }
      if (o && typeof o === "object") {
        const r = o as Record<string, unknown>;
        const value = typeof r.value === "number" ? String(r.value) : str(r.value);
        return value === null ? null : { label: str(r.label, 80) ?? value.slice(0, 80), value };
      }
      return null;
    }).filter((o): o is { label: string; value: string } => !!o);
    if (!options.length) return null;
    const wanted = typeof e.value === "number" ? String(e.value) : typeof e.value === "string" ? e.value : "";
    const value = options.some((o) => o.value === wanted) ? wanted : options[0]!.value;
    return { type: "select", name, label, options, value };
  }
  if (type === "toggle" || type === "boolean" || type === "switch" || type === "checkbox") {
    const on = typeof e.on === "string" && e.on.length <= MAX_TEXT ? e.on : "1";
    const off = typeof e.off === "string" && e.off.length <= MAX_TEXT ? e.off : "0";
    return { type: "toggle", name, label, on, off, value: e.value === true || e.value === "true" || e.value === 1 };
  }
  if (type === "text" || type === "string") {
    return { type: "text", name, label, value: typeof e.value === "string" ? e.value.slice(0, MAX_TEXT) : "" };
  }
  return null;
}

/** Read the manifest out of an HTML source. No manifest, an unreadable one, or
 *  one with no valid entries all mean the same thing: this design has no tweaks. */
export function parseTweaks(source: string): Tweak[] {
  const match = MANIFEST.exec(source);
  if (!match) return [];
  let data: unknown;
  try { data = JSON.parse(match[1]!.trim()); } catch { return []; }
  const list = Array.isArray(data) ? data : data && typeof data === "object" && Array.isArray((data as { tweaks?: unknown }).tweaks) ? (data as { tweaks: unknown[] }).tweaks : [];
  const seen = new Set<string>();
  const out: Tweak[] = [];
  for (const entry of list.slice(0, MAX_TWEAKS * 2)) {
    const tweak = parseOne(entry);
    if (!tweak || seen.has(tweak.name)) continue;
    seen.add(tweak.name);
    out.push(tweak);
    if (out.length >= MAX_TWEAKS) break;
  }
  return out;
}

/** The CSS value a control's value becomes on `:root`. */
export function tweakCssValue(tweak: Tweak, value: TweakValue): string {
  switch (tweak.type) {
    case "range": return `${Number(value)}${tweak.unit}`;
    case "toggle": return value ? tweak.on : tweak.off;
    default: return String(value).slice(0, MAX_TEXT);
  }
}

/** The values the user moved away from the manifest's defaults. */
export function changedTweaks(tweaks: Tweak[], values: Record<string, TweakValue>): Tweak[] {
  return tweaks.filter((t) => t.name in values && values[t.name] !== t.value);
}

/** What the bridge is sent: every control's current value, so a reloaded frame
 *  comes back exactly as the user left it. */
export function tweakMessages(tweaks: Tweak[], values: Record<string, TweakValue>): TweakMessage[] {
  return tweaks.map((t) => {
    const raw = t.name in values ? values[t.name]! : t.value;
    return {
      name: t.name,
      value: tweakCssValue(t, raw),
      ...(t.type === "select" ? { attr: String(raw) } : t.type === "toggle" ? { attr: raw ? "true" : "false" } : {}),
      raw,
    };
  });
}

/** The tweak part of a request to the agent: property → applied CSS value. */
export function tweakState(tweaks: Tweak[], values: Record<string, TweakValue>): Record<string, string> {
  return Object.fromEntries(changedTweaks(tweaks, values).map((t) => [t.name, tweakCssValue(t, values[t.name]!)]));
}
