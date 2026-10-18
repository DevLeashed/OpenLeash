/** The document the isolated preview frame runs, and the theme it is handed.
 *
 *  Split out of the viewer component so a bare frame can publish the same
 *  document — the gallery renders a miniature of each artifact inside a card,
 *  and that miniature is the very same isolated runtime, not a screenshot. */
import { captureTheme, themeCss } from "./hostTheme";

/** Re-exported for callers that treat the frame document as the home of the
 *  theme: it moved to `hostTheme`, the leaf module an SVG image document reads
 *  it from too. */
export { captureTheme };

/** The bridge every preview document carries, and the whole of what a
 *  generated document can say to the host or hear from it.
 *
 *  It runs inside the sandboxed, opaque-origin frame, so everything it posts is
 *  untrusted and re-validated host-side (`artifictBridge.ts`,
 *  `visualizationFeedback.ts`). It can only *propose*: a picked element, where
 *  saved comment anchors currently sit, or a state draft. Nothing it sends is
 *  saved or reaches the agent without an explicit user action in the host.
 *
 *  What it does:
 *  - Comment mode (host → `openleash:mode`): hovering outlines the element under
 *    the pointer, and a click is swallowed — the prototype's own handler never
 *    runs — and becomes an `openleash:pick` naming that element (a CSS path, a
 *    readable label, the point as a percentage of the document and of the
 *    element). In interact mode it does nothing and the prototype is just itself.
 *  - Anchors (host → `openleash:anchors`): resolves each saved comment's element
 *    and reports where it sits in the frame's viewport as the page scrolls,
 *    resizes or animates, so the host can draw the numbered pin on top of it.
 *  - Tweaks (host → `openleash:tweaks`): sets CSS custom properties on `:root`,
 *    a `data-*` attribute for selects/toggles, and fires `openleash:tweak`.
 *  - Edit mode (host → `openleash:mode` `edit`): picking works as in comment
 *    mode, but a click becomes an `openleash:inspect` naming the element, its
 *    own text when it has no child elements, and a few computed styles.
 *  - Live edits (host → `openleash:edits`): shows the user's pending text and
 *    style changes in a marked style element and in the picked elements' text;
 *    a change the host no longer sends is put back. The frame never invents an
 *    edit: the host holds them and only the host's Save writes one.
 *  - `window.openleashFeedbackDraft(draft)` stays the documented way for a
 *    prototype to propose interactive state. */
const BRIDGE = String.raw`(() => {
  if (window.__openleashBridge) return;
  window.__openleashBridge = true;
  const post = (message) => { try { parent.postMessage(Object.assign({ version: 1 }, message), "*"); } catch (_) {} };
  const pct = (value, total) => { if (!total || !isFinite(value)) return 0; const n = Math.round((value / total) * 1000) / 10; return n < 0 ? 0 : n > 100 ? 100 : n; };
  window.openleashFeedbackDraft = (draft) => { try { parent.postMessage({ type: "openleash:feedback-draft", version: 1, ...draft }, "*"); } catch (_) {} };
  window.openleashTweaks = {};

  let mode = "interact";
  let anchors = [];
  let lastReport = "";
  let hoverEl = null;
  let pickedEl = null;
  let hoverBox = null;
  let pickBox = null;
  let cursorStyle = null;

  const accent = () => getComputedStyle(document.documentElement).getPropertyValue("--accent").trim() || "#a78bfa";
  const makeBox = () => {
    const box = document.createElement("div");
    box.setAttribute("data-openleash-overlay", "");
    box.setAttribute("aria-hidden", "true");
    box.style.cssText = "position:fixed;pointer-events:none;z-index:2147483647;border-radius:5px;box-sizing:border-box;display:none;transition:left .08s,top .08s,width .08s,height .08s;";
    document.documentElement.appendChild(box);
    return box;
  };
  const place = (box, el, picked) => {
    if (!box) return;
    if (!el || !el.isConnected) { box.style.display = "none"; return; }
    const r = el.getBoundingClientRect();
    const color = accent();
    box.style.display = "block";
    box.style.left = (r.left - 3) + "px";
    box.style.top = (r.top - 3) + "px";
    box.style.width = (r.width + 6) + "px";
    box.style.height = (r.height + 6) + "px";
    box.style.border = picked ? "2px solid " + color : "1.5px dashed " + color;
    box.style.background = picked && /^#[0-9a-f]{6}$/i.test(color) ? color + "1f" : "transparent";
  };
  const showHover = (el) => { if (!hoverBox) hoverBox = makeBox(); place(hoverBox, el, false); };
  const showPick = (el) => { if (!pickBox) pickBox = makeBox(); place(pickBox, el, true); };

  const escapeCss = (value) => (window.CSS && CSS.escape) ? CSS.escape(value) : String(value).replace(/[^a-zA-Z0-9_-]/g, "\\$&");
  const selectorFor = (el) => {
    const parts = [];
    let node = el;
    while (node && node.nodeType === 1 && node !== document.documentElement && parts.length < 12) {
      if (node.id && /^[A-Za-z][\w-]*$/.test(node.id) && document.querySelectorAll("#" + escapeCss(node.id)).length === 1) { parts.unshift("#" + escapeCss(node.id)); break; }
      const tag = node.localName;
      if (tag === "body") { parts.unshift("body"); break; }
      const parentEl = node.parentElement;
      let part = tag;
      if (parentEl) {
        const same = Array.prototype.filter.call(parentEl.children, (child) => child.localName === tag);
        if (same.length > 1) part += ":nth-of-type(" + (same.indexOf(node) + 1) + ")";
      }
      parts.unshift(part);
      node = parentEl;
    }
    let selector = parts.join(" > ");
    while (selector.length > 300 && parts.length > 1) { parts.shift(); selector = parts.join(" > "); }
    return selector.slice(0, 300);
  };
  const KIND = { a: "link", button: "button", img: "image", svg: "graphic", input: "field", textarea: "field", select: "menu", h1: "heading", h2: "heading", h3: "heading", h4: "heading", h5: "heading", h6: "heading", p: "text", li: "list item", nav: "navigation", header: "header", footer: "footer", section: "section", article: "card", aside: "sidebar", form: "form", label: "label", table: "table", ul: "list", ol: "list", body: "page" };
  const labelFor = (el) => {
    const marked = el.getAttribute && el.getAttribute("data-openleash-target");
    if (marked) return marked.slice(0, 120);
    const named = el.getAttribute("aria-label") || el.getAttribute("alt") || el.getAttribute("title") || el.getAttribute("placeholder") || "";
    const kind = KIND[el.localName] || el.localName;
    if (el.localName === "body") return "page";
    const text = (named || el.innerText || el.textContent || "").replace(/\s+/g, " ").trim();
    if (!text) return kind;
    return kind + " \u201c" + (text.length > 60 ? text.slice(0, 57) + "\u2026" : text) + "\u201d";
  };
  const pickable = (target) => {
    let el = target instanceof Element ? target : target && target.parentElement;
    if (!el || el.closest("[data-openleash-overlay]")) return null;
    const marked = el.closest("[data-openleash-target]");
    if (marked) return marked;
    if (el === document.documentElement) return document.body;
    return el;
  };

  const picking = () => mode === "comment" || mode === "edit";
  const setMode = (next) => {
    mode = next === "comment" || next === "edit" ? next : "interact";
    if (picking()) {
      if (!cursorStyle) { cursorStyle = document.createElement("style"); cursorStyle.textContent = "*,*::before,*::after{cursor:crosshair!important}"; }
      (document.head || document.documentElement).appendChild(cursorStyle);
    } else {
      if (cursorStyle && cursorStyle.parentNode) cursorStyle.parentNode.removeChild(cursorStyle);
      hoverEl = null;
      showHover(null);
    }
  };
  const swallow = (event) => {
    if (!picking()) return;
    event.preventDefault();
    event.stopPropagation();
    event.stopImmediatePropagation();
  };
  ["mousedown", "mouseup", "pointerdown", "pointerup", "dblclick", "auxclick", "submit", "dragstart", "touchstart", "touchend"].forEach((type) => window.addEventListener(type, swallow, { capture: true, passive: false }));
  const STYLE_KEYS = ["color", "background-color", "font-size", "font-weight", "padding", "border-radius"];
  const inspect = (el) => {
    const computed = getComputedStyle(el);
    const styles = {};
    STYLE_KEYS.forEach((key) => { const value = computed.getPropertyValue(key); if (value) styles[key] = String(value).slice(0, 100); });
    // Only an element with no child elements has text of its own: replacing
    // anything richer would drop its markup.
    const own = el.children.length === 0 ? (el.textContent || "") : "";
    post({ type: "openleash:inspect", selector: selectorFor(el), label: labelFor(el), text: own.trim() && own.length <= 2000 ? own : null, styles });
  };
  window.addEventListener("click", (event) => {
    if (!picking()) return;
    swallow(event);
    const el = pickable(event.target);
    if (!el) return;
    if (mode === "edit") { pickedEl = el; showPick(el); inspect(el); return; }
    const r = el.getBoundingClientRect();
    const root = document.documentElement;
    const width = Math.max(root.scrollWidth, innerWidth), height = Math.max(root.scrollHeight, innerHeight);
    pickedEl = el;
    showPick(el);
    post({
      type: "openleash:pick",
      selector: selectorFor(el),
      label: labelFor(el),
      x: pct(event.clientX + scrollX, width),
      y: pct(event.clientY + scrollY, height),
      ox: pct(event.clientX - r.left, r.width),
      oy: pct(event.clientY - r.top, r.height),
      vx: Math.round(event.clientX),
      vy: Math.round(event.clientY),
    });
  }, true);
  window.addEventListener("mousemove", (event) => {
    if (!picking()) return;
    const el = pickable(event.target);
    if (el !== hoverEl) { hoverEl = el; showHover(el); }
  }, true);
  document.addEventListener("mouseleave", () => { hoverEl = null; showHover(null); });
  window.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && (picking() || pickedEl)) post({ type: "openleash:escape" });
  }, true);

  const report = () => {
    if (pickedEl) showPick(pickedEl);
    if (hoverEl) showHover(hoverEl);
    if (!anchors.length && lastReport === "[]") return;
    const root = document.documentElement;
    const width = Math.max(root.scrollWidth, innerWidth), height = Math.max(root.scrollHeight, innerHeight);
    const positions = anchors.map((anchor) => {
      let el = null;
      if (anchor.selector) { try { el = document.querySelector(anchor.selector); } catch (_) { el = null; } }
      let x = 0, y = 0;
      if (el) {
        const r = el.getBoundingClientRect();
        if (!r.width && !r.height) el = null;
        else { x = r.left + ((typeof anchor.ox === "number" ? anchor.ox : 50) / 100) * r.width; y = r.top + ((typeof anchor.oy === "number" ? anchor.oy : 50) / 100) * r.height; }
      }
      if (!el) {
        if (typeof anchor.x !== "number" || typeof anchor.y !== "number") return { id: anchor.id, x: 0, y: 0, visible: false };
        x = (anchor.x / 100) * width - scrollX;
        y = (anchor.y / 100) * height - scrollY;
      }
      return { id: anchor.id, x: Math.round(x), y: Math.round(y), visible: x >= -4 && y >= -4 && x <= innerWidth + 4 && y <= innerHeight + 4 };
    });
    const text = JSON.stringify(positions);
    if (text === lastReport) return;
    lastReport = text;
    post({ type: "openleash:positions", positions });
  };
  let queued = false;
  const schedule = () => { if (queued) return; queued = true; requestAnimationFrame(() => { queued = false; report(); }); };
  window.addEventListener("scroll", schedule, { capture: true, passive: true });
  window.addEventListener("resize", schedule);
  document.addEventListener("DOMContentLoaded", schedule);
  window.addEventListener("load", schedule);
  setInterval(() => { if (anchors.length || pickedEl) schedule(); }, 400);

  const applyTweaks = (values) => {
    const root = document.documentElement;
    values.slice(0, 64).forEach((tweak) => {
      if (!tweak || typeof tweak.name !== "string" || !/^--[A-Za-z0-9_-]{1,64}$/.test(tweak.name) || typeof tweak.value !== "string") return;
      const key = tweak.name.slice(2);
      root.style.setProperty(tweak.name, tweak.value.slice(0, 400));
      if (typeof tweak.attr === "string") root.setAttribute("data-" + key.toLowerCase(), tweak.attr.slice(0, 200));
      window.openleashTweaks[key] = tweak.raw === undefined ? tweak.value : tweak.raw;
      try { window.dispatchEvent(new CustomEvent("openleash:tweak", { detail: { name: key, value: window.openleashTweaks[key] } })); } catch (_) {}
    });
    schedule();
  };

  let liveStyle = null;
  const originals = new Map();
  const applyEdits = (rules) => {
    if (!liveStyle) { liveStyle = document.createElement("style"); liveStyle.setAttribute("data-openleash-live-edits", ""); }
    if (!liveStyle.isConnected) (document.head || document.documentElement).appendChild(liveStyle);
    let css = "";
    const texts = new Set();
    rules.slice(0, 64).forEach((rule) => {
      if (!rule || typeof rule.selector !== "string" || rule.selector.length > 300 || /[<{};@]|\/\*/.test(rule.selector)) return;
      let el = null;
      try { el = document.querySelector(rule.selector); } catch (_) { return; }
      const decls = [];
      const styles = rule.styles && typeof rule.styles === "object" ? rule.styles : {};
      Object.keys(styles).slice(0, 16).forEach((prop) => {
        const value = styles[prop];
        if (STYLE_KEYS.indexOf(prop) >= 0 && typeof value === "string" && value.length <= 100 && !/[<>{};\\]/.test(value)) decls.push(prop + ":" + value + " !important");
      });
      if (decls.length) css += rule.selector + "{" + decls.join(";") + "}\n";
      if (el && typeof rule.text === "string" && el.children.length === 0) {
        if (!originals.has(rule.selector)) originals.set(rule.selector, { el: el, text: el.textContent });
        el.textContent = rule.text.slice(0, 2000);
        texts.add(rule.selector);
      }
    });
    originals.forEach((saved, selector) => { if (!texts.has(selector)) { saved.el.textContent = saved.text; originals.delete(selector); } });
    liveStyle.textContent = css;
    if (pickedEl) showPick(pickedEl);
    schedule();
  };

  window.addEventListener("message", (event) => {
    if (event.source !== parent) return;
    const data = event.data;
    if (!data || typeof data !== "object" || data.version !== 1) return;
    if (data.type === "openleash:mode") setMode(data.mode);
    else if (data.type === "openleash:anchors" && Array.isArray(data.anchors)) {
      anchors = data.anchors.slice(0, 256).filter((anchor) => anchor && typeof anchor.id === "string");
      lastReport = "";
      schedule();
    } else if (data.type === "openleash:clear") { pickedEl = null; showPick(null); }
    else if (data.type === "openleash:tweaks" && Array.isArray(data.values)) applyTweaks(data.values);
    else if (data.type === "openleash:edits" && Array.isArray(data.rules)) applyEdits(data.rules);
  });
  post({ type: "openleash:ready" });
})();`;

/** The document the isolated preview frame runs, given the theme it was handed.
 *  The wrapper precedes any supplied head/body so tokens exist when scripts run,
 *  and the bridge is in place before the design's own scripts. */
export function buildVisualizationDocument(source: string, theme: Record<string, string>): string {
  const css = themeCss(theme);
  return `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><style>:root{${css}}body{margin:0;padding:16px;background:var(--bg);color:var(--fg);font-family:var(--font-sans)}*{box-sizing:border-box}</style><script>${BRIDGE}</script></head><body>${source}</body></html>`;
}
