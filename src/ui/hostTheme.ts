/** The host's own faces and theme tokens, for documents the app renders on the
 *  agent's behalf: the isolated HTML preview frame and inert SVG image
 *  documents. Also the one place a theme is read off the live app.
 *
 *  Only the declared token is trusted. `font-family` inherits, so whenever the
 *  app's own stylesheet has not applied, *both* `html` and `body` report the
 *  user agent's default — a serif — and reading either one (which is what this
 *  did) is why every generated document, and every `<text>` in a generated
 *  diagram, arrived in Times instead of the app's own face. Losing the token
 *  means losing the stylesheet, so the honest answer then is a plain system
 *  stack rather than a family inherited from nowhere. */
export function hostFontSans(): string {
  if (typeof document === "undefined") return "system-ui, sans-serif";
  return getComputedStyle(document.documentElement).getPropertyValue("--font-sans").trim() || "system-ui, sans-serif";
}

/** The app's monospace face, so a generated document's `--font-mono` is Geist
 *  Mono rather than whatever the platform happens to call monospace. */
export function hostFontMono(): string {
  if (typeof document === "undefined") return "ui-monospace, monospace";
  return getComputedStyle(document.documentElement).getPropertyValue("--mono").trim() || "ui-monospace, monospace";
}

/** The app's own theme tokens, as a declaration list a generated document can
 *  carry, and the reader that fills it in.
 *
 *  Both the isolated preview frame and an image-context diagram need the same
 *  thing: CSS custom properties the app renders with, from a document the app
 *  does not style. Kept here, beside the text faces, because this module is the
 *  leaf the other two build on. */
export function captureTheme(): Record<string, string> {
  const s = getComputedStyle(document.documentElement);
  const get = (name: string, fallback: string) => s.getPropertyValue(`--${name}`).trim() || fallback;
  return { bg: get("bg", "#111"), fg: get("fg", "#eee"), card: get("panel", "#222"), muted: get("mut", "#999"), accent: get("accent", "#a78bfa"), primary: get("accent", "#a78bfa"), border: get("line", "#444"),
    ...Object.fromEntries(["#a78bfa", "#60a5fa", "#34d399", "#fbbf24", "#f472b6", "#fb923c"].map((c, i) => [`chart${i + 1}`, get(`chart${i + 1}`, c)])),
    "font-sans": hostFontSans(), "font-mono": hostFontMono(), radius: "10px" };
}

/** CSS values are host-owned, but escape structural characters defensively: the
 *  list is interpolated into a `<style>` element of a document that also runs
 *  generated markup, so a value must not be able to close it. */
export function themeCss(theme: Record<string, string>): string {
  return Object.entries(theme).filter(([k]) => /^[a-z0-9-]+$/.test(k)).map(([k, v]) => `--${k}:${v.replace(/[<>;{}\\]/g, "")};`).join("");
}

/** An inert `data:` URL for an agent-authored SVG.
 *
 *  An `<img>` renders the SVG as its own document with no host CSS, so two
 *  things have to be carried into it:
 *
 *  1. The text face. A `<text>` that names no family falls back to the user
 *     agent's default, which is a serif. A presentation attribute on the root
 *     inherits to every text node and is outranked by anything the source sets
 *     itself, so a document that already names a family is passed through
 *     untouched.
 *
 *  2. The theme tokens. The app tells agents to draw with `--bg`, `--card`,
 *     `--accent` and the rest, and those are exactly the names an image document
 *     does not define: `fill="var(--card)"` resolved to nothing and the diagram
 *     arrived as an empty black rectangle — the worst failure mode there is,
 *     because it looks like the diagram was never written. The tokens are
 *     injected as a `:root` rule only when the source asks for one, so a
 *     document written with literal colours is still byte-identical, and only
 *     the documented names travel. This stays inert: styles are not scripts, and
 *     an image document still cannot reach the network or the host page. */
export function svgImageUrl(source: string): string {
  const url = (svg: string) => `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
  const open = /<svg\b[^>]*>/i.exec(source)?.[0];
  if (!open) return url(source);
  const style = /var\(\s*--/.test(source) ? `<style>:root{${themeCss(captureTheme())}}</style>` : "";
  // The attribute is double-quoted, so quotes inside the family list are dropped:
  // `Geist, system-ui, Segoe UI, sans-serif` is the same CSS value unquoted.
  const family = /\bfont-family\s*=/i.test(open) ? "" : hostFontSans().replace(/["<>]/g, "");
  // A self-closing root has to be opened when the theme goes in — markup after
  // `/>` would sit outside the root element and the document would not parse.
  const selfClosing = /\/>$/.test(open);
  const head = open.replace(/\/?>$/, (end) => (family ? ` font-family="${family}"` : "") + (selfClosing && style ? ">" : end));
  return url(source.replace(open, () => head + style + (selfClosing && style ? "</svg>" : "")));
}
