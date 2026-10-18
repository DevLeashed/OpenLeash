import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent as ReactMouseEvent, type PointerEvent as ReactPointerEvent } from "react";
import { ArrowLeft, ArrowRight, Maximize2, MessageSquarePlus, Minimize2, Plus, RotateCw, X } from "lucide-react";
import { Button, IconButton, Input, TextArea } from "./primitives/Controls";
import { Modal } from "./primitives/Modal";
import { browserAnnotate, browserPanel, type BrowserElement, type BrowserPanelAction, type BrowserPanelFrame, type BrowserRect } from "../api";
import { useStore } from "../store";
import { ANNOTATE_STYLES, annotationPayload, elementLabel, historySuggestions, keyIntent, normalizeAddress, previewStyles, rectFrom, styleValueOk, toHex, type PendingAnnotation } from "./browserPanelModel";

export function browserPoint(clientX: number, clientY: number, rect: Pick<DOMRect, "left" | "top" | "width" | "height">, frame: Pick<BrowserPanelFrame, "viewport_width" | "viewport_height">) {
  return { x: (clientX - rect.left) / rect.width * frame.viewport_width, y: (clientY - rect.top) / rect.height * frame.viewport_height };
}

/** How often the view refreshes while the agent is driving, and otherwise. */
export const POLL_ACTIVE_MS = 700;
export const POLL_IDLE_MS = 2500;
/** Printable keys typed into the focused view are sent together after this pause. */
const TYPE_BATCH_MS = 150;
/** Style edits preview after the user stops changing a field for this long. */
export const PREVIEW_DEBOUNCE_MS = 300;
const WHEEL_BATCH_MS = 120;
export const MAX_ANNOTATIONS = 10;
/** A pointer that moves less than this (screen px) is a click, not a drag. */
const DRAG_SLOP = 6;

const AGENT_VERBS: Record<string, string> = { navigate: "opening a page", read: "reading", screenshot: "taking a screenshot", click: "clicking", type: "typing", js: "running script", back: "going back", info: "looking", snapshot: "reading the page", scroll: "scrolling", tabs: "managing tabs" };

/** The action of the main agent's running `browser` call in this chat, if any. A
 *  primitive, so the store selector is stable across unrelated updates. */
function useAgentBrowserAction(taskId: string): string | null {
  return useStore((s) => {
    const list = s.items[taskId];
    if (!list) return null;
    for (let i = list.length - 1; i >= Math.max(0, list.length - 60); i--) {
      const it = list[i]!;
      if (it.kind === "tool" && it.data.name === "browser" && it.data.status === "running") return String(it.data.input?.action ?? "");
    }
    return null;
  });
}

interface Draft { bbox: BrowserRect; element?: BrowserElement; comment: string; styles: Record<string, string>; showStyles: boolean }

/** The agent's own browser, over the same page and cookies. `embedded` drops the
 *  modal frame so the workspace pane can host it as one of its views — the pane
 *  is the frame then, and switching views is the close. */
export function BrowserPanel({ taskId, onClose, embedded = false, onExpandedChange }: { taskId: string; onClose: () => void; embedded?: boolean; onExpandedChange?: (expanded: boolean) => void }) {
  const [frame, setFrame] = useState<BrowserPanelFrame | null>(null);
  const [address, setAddress] = useState("");
  const [editing, setEditing] = useState(false);
  const [suggestOpen, setSuggestOpen] = useState(false);
  const [highlight, setHighlight] = useState(-1);
  const [error, setError] = useState("");
  // Kept apart from `error`, which the next poll clears: what the user typed
  // stays explained until they change it.
  const [addrError, setAddrError] = useState("");
  const [busy, setBusy] = useState(false);
  const [viewFocused, setViewFocused] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [annotating, setAnnotating] = useState(false);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [pending, setPending] = useState<PendingAnnotation[]>([]);
  const [band, setBand] = useState<BrowserRect | null>(null);
  const [sending, setSending] = useState(false);
  const [sent, setSent] = useState("");
  const [promptText, setPromptText] = useState<string | null>(null);
  const alive = useRef(true);
  const chain = useRef<Promise<unknown>>(Promise.resolve());
  const inflight = useRef(0);
  const editingRef = useRef(false);
  const typed = useRef("");
  const typeTimer = useRef<number | undefined>(undefined);
  const wheel = useRef({ dx: 0, dy: 0, x: 0, y: 0, timer: undefined as number | undefined });
  const previewTimer = useRef<number | undefined>(undefined);
  const drag = useRef<{ startX: number; startY: number; at: { x: number; y: number }; moved: boolean } | null>(null);
  const nextId = useRef(1);
  const viewRef = useRef<HTMLDivElement | null>(null);
  const imgRef = useRef<HTMLImageElement | null>(null);
  const agentAction = useAgentBrowserAction(taskId);
  const agentActive = agentAction !== null;
  const agentActiveRef = useRef(agentActive);
  agentActiveRef.current = agentActive;

  /** Every action goes through one queue, so keystrokes arrive in order and
   *  nothing the user did is dropped behind a poll. Polls are skipped while
   *  anything else is in flight. Never rejects: failures land in `error`. */
  const run = useCallback((action: BrowserPanelAction): Promise<BrowserPanelFrame | null> => {
    if (action.kind === "snapshot" && inflight.current > 0) return Promise.resolve(null);
    inflight.current++;
    setBusy(true);
    const p = chain.current.then(async () => {
      try {
        const next = await browserPanel(taskId, action);
        if (alive.current) {
          // A dialog blocks the renderer, so that frame carries no picture:
          // keep showing the last one rather than a blank.
          setFrame((prev) => (!next.png && prev ? { ...next, png: prev.png, width: prev.width, height: prev.height, viewport_width: prev.viewport_width, viewport_height: prev.viewport_height } : next));
          if (!editingRef.current) setAddress(next.url);
          setError("");
        }
        return next;
      } catch (e) {
        if (alive.current) setError(String(e));
        return null;
      } finally {
        inflight.current--;
        if (alive.current && inflight.current === 0) setBusy(false);
      }
    });
    chain.current = p;
    return p;
  }, [taskId]);

  // Live view: fast while the agent is driving, slow otherwise, and not at all
  // while the window is hidden.
  useEffect(() => {
    alive.current = true;
    let timer: number | undefined;
    const tick = () => {
      if (!document.hidden) void run({ kind: "snapshot" });
      timer = window.setTimeout(tick, agentActiveRef.current ? POLL_ACTIVE_MS : POLL_IDLE_MS);
    };
    void run({ kind: "snapshot" });
    timer = window.setTimeout(tick, agentActiveRef.current ? POLL_ACTIVE_MS : POLL_IDLE_MS);
    const onVisible = () => { if (!document.hidden) void run({ kind: "snapshot" }); };
    document.addEventListener("visibilitychange", onVisible);
    const pendingWheel = wheel.current;
    return () => {
      alive.current = false;
      window.clearTimeout(timer);
      window.clearTimeout(typeTimer.current);
      window.clearTimeout(pendingWheel.timer);
      window.clearTimeout(previewTimer.current);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [run]);

  // When the agent starts, look right away instead of waiting out the slow poll.
  useEffect(() => { if (agentActive) void run({ kind: "snapshot" }); }, [agentActive, run]);

  useEffect(() => { onExpandedChange?.(expanded); }, [expanded, onExpandedChange]);
  // Leaving the view must not leave the chat collapsed behind it.
  useEffect(() => () => onExpandedChange?.(false), [onExpandedChange]);

  const flushTyped = useCallback(() => {
    window.clearTimeout(typeTimer.current);
    typeTimer.current = undefined;
    const text = typed.current;
    typed.current = "";
    if (text) void run({ kind: "type", text });
  }, [run]);

  // Wheel needs a non-passive listener to keep the pane itself from scrolling.
  useEffect(() => {
    const el = viewRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      const img = imgRef.current;
      if (!img || !frame || annotating) return;
      e.preventDefault();
      const w = wheel.current;
      const p = browserPoint(e.clientX, e.clientY, img.getBoundingClientRect(), frame);
      w.dx += e.deltaX; w.dy += e.deltaY; w.x = p.x; w.y = p.y;
      if (w.timer !== undefined) return;
      w.timer = window.setTimeout(() => {
        const clamp = (v: number) => Math.max(-2000, Math.min(2000, Math.round(v)));
        const action: BrowserPanelAction = { kind: "scroll", delta_y: clamp(w.dy), ...(w.dx ? { delta_x: clamp(w.dx) } : {}), ...(Number.isFinite(w.x) && w.x >= 0 && w.y >= 0 ? { x: Math.round(w.x), y: Math.round(w.y) } : {}) };
        w.dx = 0; w.dy = 0; w.timer = undefined;
        void run(action);
      }, WHEEL_BATCH_MS);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [frame, annotating, run]);

  const navigate = (target: string) => {
    const r = normalizeAddress(target);
    if ("error" in r) { setAddrError(r.error); return; }
    setAddrError("");
    editingRef.current = false;
    setEditing(false);
    setSuggestOpen(false);
    setHighlight(-1);
    setAddress(r.url);
    void run({ kind: "navigate", url: r.url });
  };

  const suggestions = useMemo(() => (suggestOpen ? historySuggestions(frame?.history, address, frame?.url ?? "") : []), [suggestOpen, frame?.history, frame?.url, address]);

  const onAddressKey = (e: ReactKeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (!suggestOpen) { setSuggestOpen(true); setHighlight(0); return; }
      const n = suggestions.length;
      if (!n) return;
      setHighlight((h) => (e.key === "ArrowDown" ? (h + 1) % n : (h <= 0 ? n - 1 : h - 1)));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const pick = highlight >= 0 ? suggestions[highlight] : undefined;
      navigate(pick ?? address);
    } else if (e.key === "Escape") {
      e.preventDefault();
      if (suggestOpen && suggestions.length) { setSuggestOpen(false); setHighlight(-1); return; }
      editingRef.current = false;
      setEditing(false);
      setAddress(frame?.url ?? "");
      e.currentTarget.blur();
    }
  };

  const point = (clientX: number, clientY: number) => {
    const img = imgRef.current;
    if (!img || !frame) return null;
    const p = browserPoint(clientX, clientY, img.getBoundingClientRect(), frame);
    if (!(p.x >= 0 && p.y >= 0 && p.x < frame.viewport_width && p.y < frame.viewport_height)) return null;
    return p;
  };

  const onViewKey = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.target !== e.currentTarget || annotating) return;
    const intent = keyIntent(e.nativeEvent, typed.current.length > 0);
    if (!intent) return;
    e.preventDefault();
    if (intent.kind === "text") {
      typed.current += intent.text;
      window.clearTimeout(typeTimer.current);
      typeTimer.current = window.setTimeout(flushTyped, TYPE_BATCH_MS);
      return;
    }
    flushTyped();
    void run(intent);
  };

  // ── Annotate ──
  const reapply = useCallback(async (list: PendingAnnotation[]) => {
    await run({ kind: "clear_preview" });
    for (const a of list) {
      const styles = previewStyles(a);
      if (a.element && styles.length) await run({ kind: "preview_style", selector: a.element.selector, styles });
    }
  }, [run]);

  const schedulePreview = useCallback((d: Draft) => {
    window.clearTimeout(previewTimer.current);
    previewTimer.current = window.setTimeout(() => {
      const styles = previewStyles(d);
      if (d.element && styles.length) void run({ kind: "preview_style", selector: d.element.selector, styles });
    }, PREVIEW_DEBOUNCE_MS);
  }, [run]);

  const setStyle = (property: string, value: string) => {
    if (!draft) return;
    const next = { ...draft, styles: { ...draft.styles, [property]: value } };
    setDraft(next);
    schedulePreview(next);
  };

  const revertDraftStyles = () => {
    if (!draft) return;
    window.clearTimeout(previewTimer.current);
    const hadPreview = previewStyles(draft).length > 0;
    setDraft({ ...draft, styles: {} });
    if (hadPreview) void reapply(pending);
  };

  const cancelDraft = () => {
    window.clearTimeout(previewTimer.current);
    if (draft && previewStyles(draft).length) void reapply(pending);
    setDraft(null);
  };

  const addDraft = () => {
    if (!draft || !draft.comment.trim() || pending.length >= MAX_ANNOTATIONS) return;
    window.clearTimeout(previewTimer.current);
    const added: PendingAnnotation = { id: nextId.current++, comment: draft.comment, bbox: draft.bbox, element: draft.element, styles: draft.styles };
    // The debounced preview may not have fired yet; apply it now so what the
    // user saw is what the screenshot shows.
    const styles = previewStyles(added);
    if (added.element && styles.length) void run({ kind: "preview_style", selector: added.element.selector, styles });
    setPending((list) => [...list, added]);
    setDraft(null);
  };

  const removePending = (id: number) => {
    const next = pending.filter((a) => a.id !== id);
    const removed = pending.find((a) => a.id === id);
    setPending(next);
    if (removed && previewStyles(removed).length) void reapply(next);
  };

  const discardAll = () => {
    window.clearTimeout(previewTimer.current);
    const hadPreview = pending.some((a) => previewStyles(a).length) || (draft ? previewStyles(draft).length > 0 : false);
    setPending([]);
    setDraft(null);
    if (hadPreview) void run({ kind: "clear_preview" });
  };

  const sendAll = async () => {
    if (!pending.length || sending) return;
    setSending(true);
    setSent("");
    try {
      await browserAnnotate(taskId, annotationPayload(pending));
      if (!alive.current) return;
      const hadPreview = pending.some((a) => previewStyles(a).length);
      setPending([]);
      setDraft(null);
      setSent("Sent · the reply arrives in the chat");
      setError("");
      // The previews were a sketch for the agent; the real fix goes in the source.
      if (hadPreview) void run({ kind: "clear_preview" });
    } catch (e) {
      if (alive.current) setError(String(e));
    } finally {
      if (alive.current) setSending(false);
    }
  };

  const selectAt = async (p: { x: number; y: number }) => {
    const next = await run({ kind: "inspect", x: p.x, y: p.y });
    const el = next?.element;
    if (!el || !alive.current) return;
    setSent("");
    setDraft({ bbox: el.bbox, element: el, comment: "", styles: {}, showStyles: false });
  };

  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!annotating || e.button !== 0 || draft) return;
    const p = point(e.clientX, e.clientY);
    if (!p) return;
    e.preventDefault();
    drag.current = { startX: e.clientX, startY: e.clientY, at: p, moved: false };
    e.currentTarget.setPointerCapture?.(e.pointerId);
  };
  const onPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d || !frame) return;
    if (!d.moved && Math.hypot(e.clientX - d.startX, e.clientY - d.startY) < DRAG_SLOP) return;
    d.moved = true;
    const img = imgRef.current;
    if (!img) return;
    const p = browserPoint(e.clientX, e.clientY, img.getBoundingClientRect(), frame);
    const clamped = { x: Math.max(0, Math.min(frame.viewport_width, p.x)), y: Math.max(0, Math.min(frame.viewport_height, p.y)) };
    setBand(rectFrom(d.at, clamped));
  };
  const onPointerUp = () => {
    const d = drag.current;
    drag.current = null;
    if (!d) return;
    if (d.moved && band) {
      setBand(null);
      if (band.width >= 4 && band.height >= 4) { setSent(""); setDraft({ bbox: band, comment: "", styles: {}, showStyles: false }); }
      return;
    }
    setBand(null);
    void selectAt(d.at);
  };

  const onViewClick = (e: ReactMouseEvent<HTMLDivElement>) => {
    if (annotating || !frame) return;
    viewRef.current?.focus();
    const p = point(e.clientX, e.clientY);
    if (!p) return;
    flushTyped();
    void run({ kind: "click", x: p.x, y: p.y });
  };
  const onContextMenu = (e: ReactMouseEvent<HTMLDivElement>) => {
    if (annotating || !frame) return;
    const p = point(e.clientX, e.clientY);
    if (!p) return;
    e.preventDefault();
    void run({ kind: "click", x: p.x, y: p.y, button: "right" });
  };

  const pct = (r: BrowserRect) => frame ? { left: `${r.x / frame.viewport_width * 100}%`, top: `${r.y / frame.viewport_height * 100}%`, width: `${r.width / frame.viewport_width * 100}%`, height: `${r.height / frame.viewport_height * 100}%` } : {};

  const tabs = frame?.tabs ?? [];
  const dialog = frame?.dialog ?? null;
  const agentVerb = agentAction ? (AGENT_VERBS[agentAction] ?? agentAction) : "";
  const listId = `browser-history-${taskId}`;

  const chrome = <>
    {tabs.length > 0 && <div className="browser-tabs" role="tablist" aria-label="Browser tabs">
      {tabs.map((t) => {
        const label = t.title || t.url || "New tab";
        return <div key={t.id} className={"browser-tab" + (t.active ? " on" : "")}>
          <button type="button" role="tab" aria-selected={t.active} title={t.url} className="browser-tab-title" onClick={() => { if (!t.active) void run({ kind: "tab_switch", id: t.id }); }}>{label}</button>
          <button type="button" className="browser-tab-close" aria-label={`Close tab ${label}`} onClick={() => void run({ kind: "tab_close", id: t.id })}><X size={11} aria-hidden="true" /></button>
        </div>;
      })}
      <IconButton label="New tab" className="browser-tab-new" onClick={() => void run({ kind: "tab_new" })}><Plus size={13} /></IconButton>
    </div>}
    <div className="browser-toolbar">
      <IconButton label="Back" disabled={!frame?.can_back} onClick={() => void run({ kind: "back" })}><ArrowLeft size={14} /></IconButton>
      <IconButton label="Forward" disabled={!frame?.can_forward} onClick={() => void run({ kind: "forward" })}><ArrowRight size={14} /></IconButton>
      <IconButton label="Reload page" onClick={() => void run({ kind: "refresh" })}><RotateCw size={13} /></IconButton>
      <div className="browser-address">
        <Input aria-label="Browser URL" role="combobox" aria-expanded={suggestions.length > 0} aria-controls={listId} aria-autocomplete="list" aria-activedescendant={highlight >= 0 && suggestions[highlight] ? `${listId}-${highlight}` : undefined} placeholder="Type an address · localhost:3000, example.com" spellCheck={false} autoComplete="off" value={address}
          onFocus={(e) => { editingRef.current = true; setEditing(true); setSuggestOpen(true); setHighlight(-1); e.currentTarget.select(); }}
          onBlur={() => { editingRef.current = false; setEditing(false); setSuggestOpen(false); setHighlight(-1); }}
          onChange={(e) => { setAddress(e.target.value); setAddrError(""); setSuggestOpen(true); setHighlight(-1); }}
          onKeyDown={onAddressKey} />
        {suggestions.length > 0 && <ul className="browser-suggest" role="listbox" id={listId} aria-label="Visited pages">
          {suggestions.map((u, i) => <li key={u} id={`${listId}-${i}`} role="option" aria-selected={i === highlight} className={i === highlight ? "on" : undefined} onMouseDown={(e) => { e.preventDefault(); navigate(u); }}>{u}</li>)}
        </ul>}
      </div>
      <Button className="browser-annotate-toggle" variant={annotating ? "primary" : "ghost"} aria-pressed={annotating} onClick={() => { setAnnotating((v) => !v); setDraft(null); setBand(null); }}><MessageSquarePlus size={13} aria-hidden="true" />Annotate</Button>
      {embedded && onExpandedChange && <IconButton label={expanded ? "Collapse browser" : "Expand browser"} onClick={() => setExpanded((v) => !v)}>{expanded ? <Minimize2 size={14} /> : <Maximize2 size={14} />}</IconButton>}
    </div>
    <div className="browser-status" aria-live="polite">
      {agentActive
        ? <span className="browser-agent on"><span className="browser-agent-dot" aria-hidden="true" />Agent is using the browser · {agentVerb}</span>
        : <span className="browser-agent">You and the agent share this page</span>}
      <span className="browser-status-page">{frame ? (frame.title ? `${frame.title} · ` : "") + frame.url : "Opening the agent's browser…"}</span>
      {busy && !editing && <span className="browser-status-busy">Updating…</span>}
    </div>
    {addrError && <div role="alert" className="browser-error">{addrError}</div>}
    {error && <div role="alert" className="browser-error">{error}</div>}
    {dialog && <div className="browser-dialog" role="group" aria-label="Page dialog">
      <div><strong>{dialog.kind === "prompt" ? "The page asks" : dialog.kind === "confirm" ? "The page wants confirmation" : dialog.kind === "beforeunload" ? "Leave this page?" : "The page says"}</strong></div>
      <div className="browser-dialog-msg">{dialog.message}</div>
      {dialog.kind === "prompt" && <Input aria-label="Dialog answer" value={promptText ?? dialog.default_prompt} onChange={(e) => setPromptText(e.target.value)} />}
      <div className="browser-row">
        <Button variant="primary" onClick={() => { const text = dialog.kind === "prompt" ? (promptText ?? dialog.default_prompt) : undefined; setPromptText(null); void run({ kind: "dialog", accept: true, ...(text !== undefined ? { text } : {}) }); }}>{dialog.kind === "alert" ? "OK" : "Accept"}</Button>
        {dialog.kind !== "alert" && <Button onClick={() => { setPromptText(null); void run({ kind: "dialog", accept: false }); }}>Dismiss</Button>}
      </div>
    </div>}
    {annotating && <div className="browser-annotate-hint desc">{draft ? "Describe what should change, then add it." : "Click an element or drag over a region to annotate it. Notes collect below; send them together."}</div>}
    <div ref={viewRef} className={"browser-view" + (annotating ? " annotating" : "") + (viewFocused ? " focused" : "")} tabIndex={0} role="application"
      aria-label={annotating ? "Live page — click an element or drag a region to annotate" : "Live page — click to interact; while focused, typing and scrolling go to the page"}
      onFocus={() => setViewFocused(true)} onBlur={() => { setViewFocused(false); flushTyped(); }} onKeyDown={onViewKey}
      onClick={onViewClick} onContextMenu={onContextMenu} onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={() => { drag.current = null; setBand(null); }}>
      {frame?.png
        ? <img ref={imgRef} draggable={false} alt="Main agent browser page — click to interact" src={`data:image/png;base64,${frame.png}`} width={frame.width} height={frame.height} />
        : <div className="browser-view-empty">{error ? "No picture yet." : "Starting the browser…"}</div>}
      {frame?.png && annotating && pending.map((a, i) => <div key={a.id} className="browser-mark" style={pct(a.bbox)} aria-hidden="true"><span className="browser-pin">{i + 1}</span></div>)}
      {frame?.png && draft && <div className="browser-mark draft" style={pct(draft.bbox)} aria-hidden="true"><span className="browser-pin">{pending.length + 1}</span></div>}
      {band && <div className="browser-band" style={pct(band)} aria-hidden="true" />}
    </div>
    {annotating && draft && <div className="browser-note" role="group" aria-label="New annotation">
      <div className="browser-note-head"><span className="browser-pin static">{pending.length + 1}</span>
        {draft.element ? <span className="mono">{elementLabel(draft.element)}{draft.element.ref ? <span className="browser-ref"> · {draft.element.ref}</span> : null}</span> : <span>Region · {Math.round(draft.bbox.width)}×{Math.round(draft.bbox.height)}</span>}
      </div>
      <TextArea aria-label="Annotation comment" autoFocus rows={3} maxLength={4000} placeholder="What should change here?" value={draft.comment} onChange={(e) => setDraft({ ...draft, comment: e.target.value })} onKeyDown={(e) => { if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); addDraft(); } if (e.key === "Escape") { e.preventDefault(); cancelDraft(); } }} />
      {draft.element && <>
        <Button variant="ghost" aria-expanded={draft.showStyles} onClick={() => setDraft({ ...draft, showStyles: !draft.showStyles })}>Adjust styles {draft.showStyles ? "▾" : "▸"}</Button>
        {draft.showStyles && <div className="browser-styles">
          {ANNOTATE_STYLES.map(({ property, label, kind }) => {
            const base = draft.element!.styles[property] ?? "";
            const value = draft.styles[property] ?? base;
            const bad = value.trim() !== "" && !styleValueOk(value);
            return <label key={property} className="browser-style">
              <span>{label}</span>
              {kind === "weight"
                ? <select className="input" aria-label={label} value={/^\d+$/.test(value) ? value : base} onChange={(e) => setStyle(property, e.target.value)}>
                  {Array.from(new Set([base, "100", "200", "300", "400", "500", "600", "700", "800", "900"].filter(Boolean))).map((w) => <option key={w} value={w}>{w}</option>)}
                </select>
                : <span className="browser-style-field">
                  {kind === "color" && <input type="color" aria-label={`${label} picker`} value={toHex(value) ?? "#000000"} onChange={(e) => setStyle(property, e.target.value)} />}
                  <Input aria-label={label} aria-invalid={bad} className="input mono" value={value} onChange={(e) => setStyle(property, e.target.value)} />
                </span>}
            </label>;
          })}
          <div className="browser-row"><span className="desc">Previewed live, inline in the page only. The agent gets before → after and fixes the source.</span><Button onClick={revertDraftStyles}>Revert styles</Button></div>
        </div>}
      </>}
      <div className="browser-row">
        <Button variant="primary" disabled={!draft.comment.trim() || pending.length >= MAX_ANNOTATIONS} onClick={addDraft}>Add note</Button>
        <Button onClick={cancelDraft}>Cancel</Button>
      </div>
    </div>}
    {pending.length > 0 && <div className="browser-pending" role="group" aria-label="Pending annotations">
      <ol>
        {pending.map((a, i) => <li key={a.id}><span className="browser-pin static">{i + 1}</span><span className="browser-pending-text">{a.element ? `${elementLabel(a.element)} — ` : "Region — "}{a.comment.trim().split("\n")[0]}{previewStyles(a).length ? ` · ${previewStyles(a).length} style change${previewStyles(a).length === 1 ? "" : "s"}` : ""}</span><IconButton label={`Remove note ${i + 1}`} onClick={() => removePending(a.id)}><X size={12} /></IconButton></li>)}
      </ol>
      <div className="browser-row">
        <Button variant="primary" disabled={sending} onClick={() => void sendAll()}>{sending ? "Sending…" : `Send to agent (${pending.length})`}</Button>
        <Button disabled={sending} onClick={discardAll}>Discard</Button>
        {pending.length >= MAX_ANNOTATIONS && <span className="desc">{MAX_ANNOTATIONS} notes is the most one send carries.</span>}
      </div>
    </div>}
    {sent && <div role="status" className="browser-sent">{sent}</div>}
  </>;

  if (embedded) return <section className="browser-panel" aria-label="Agent browser">
    {chrome}
    <p className="desc browser-foot">Same page and cookies as the main agent. Your clicks and typing act on websites immediately, and the agent may act at the same time. Leaving this view does not close the browser.</p>
  </section>;
  return <Modal onClose={onClose} style={{ width: "min(1100px, 95vw)", maxHeight: "90vh", overflow: "auto" }}><section className="mb browser-panel" aria-label="Agent browser">
    <div className="browser-row"><strong style={{ flex: 1 }}>Main agent browser</strong><Button onClick={onClose}>Close browser panel</Button></div>
    <p className="desc">Same page and cookies as the main agent. Your clicks and typing act on websites immediately. The agent may act at the same time. Closing this panel does not close the browser.</p>
    {chrome}
  </section></Modal>;
}
