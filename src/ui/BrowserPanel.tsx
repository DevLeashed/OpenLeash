import { useCallback, useEffect, useRef, useState } from "react";
import { Input } from "./primitives/Controls";
import { Modal } from "./primitives/Modal";
import { browserPanel, type BrowserPanelAction, type BrowserPanelFrame } from "../api";

export function browserPoint(clientX: number, clientY: number, rect: Pick<DOMRect, "left" | "top" | "width" | "height">, frame: BrowserPanelFrame) {
  return { x: (clientX - rect.left) / rect.width * frame.viewport_width, y: (clientY - rect.top) / rect.height * frame.viewport_height };
}

export function BrowserPanel({ taskId, onClose }: { taskId: string; onClose: () => void }) {
  const [frame, setFrame] = useState<BrowserPanelFrame | null>(null);
  const [url, setUrl] = useState("");
  const [text, setText] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const editingUrl = useRef(false);
  const pending = useRef(false);
  const alive = useRef(true);
  const run = useCallback(async (action: BrowserPanelAction) => {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    try {
      const next = await browserPanel(taskId, action);
      if (alive.current) { setFrame(next); if (!editingUrl.current) setUrl(next.url); setError(""); if (action.kind === "type") setText(""); }
    } catch (e) { if (alive.current) setError(String(e)); }
    finally { pending.current = false; if (alive.current) setBusy(false); }
  }, [taskId]);
  useEffect(() => {
    alive.current = true;
    void run({ kind: "snapshot" });
    const timer = window.setInterval(() => { void run({ kind: "snapshot" }); }, 2000);
    return () => { alive.current = false; window.clearInterval(timer); };
  }, [run]);
  return <Modal onClose={onClose} style={{ width: "min(1100px, 95vw)", maxHeight: "90vh", overflow: "auto" }}><section className="mb" aria-label="Agent browser">
    <div style={{ display: "flex", gap: 8 }}><strong>Main agent browser</strong><button className="btn" onClick={onClose}>Close browser panel</button></div>
    <p className="desc">Same page and cookies as the main agent. Your clicks and typing act on websites immediately. The agent may act at the same time.</p>
    <p className="desc">Closing this panel does not close the browser. Run cleanup closes it; the next refresh may open a fresh session.</p>
    <form onSubmit={e => { e.preventDefault(); void run({ kind: "navigate", url }); }} style={{ display: "flex", gap: 8 }}>
      <button className="btn" type="button" disabled={busy} onClick={() => void run({ kind: "back" })}>Back</button>
      <button className="btn" type="button" disabled={busy} onClick={() => void run({ kind: "refresh" })}>Reload page</button>
      <Input aria-label="Browser URL" placeholder="https://example.com" type="url" value={url} onFocus={() => { editingUrl.current = true; }} onBlur={() => { editingUrl.current = false; }} onChange={e => setUrl(e.target.value)} style={{ flex: 1 }} />
      <button className="btn" disabled={busy || !/^https?:\/\//i.test(url)}>Go</button>
    </form>
    <div>{frame?.title} {frame?.url} {busy ? " · Updating…" : " · Live view (2s)"}</div>
    {error && <div role="alert">{error}</div>}
    {frame && <img alt="Main agent browser page — click to interact" src={`data:image/png;base64,${frame.png}`} width={frame.width} height={frame.height} style={{ display: "block", width: "100%", height: "auto", cursor: busy ? "wait" : "pointer" }} onClick={e => { if (!busy) void run({ kind: "click", ...browserPoint(e.clientX, e.clientY, e.currentTarget.getBoundingClientRect(), frame) }); }} />}
    <form onSubmit={e => { e.preventDefault(); void run({ kind: "type", text }); }} style={{ display: "flex", gap: 8 }}>
      <Input aria-label="Text for focused browser field" placeholder="Click a field above, then type here" value={text} onChange={e => setText(e.target.value)} style={{ flex: 1 }} />
      <button className="btn" disabled={busy || !text}>Type in page</button>
    </form>
    <div style={{ display: "flex", gap: 8 }}>{(["Enter", "Tab"] as const).map(key => <button className="btn" key={key} disabled={busy} onClick={() => void run({ kind: "key", key })}>{key} in page</button>)}<button className="btn" disabled={busy} onClick={() => void run({ kind: "scroll", delta_y: -500 })}>Scroll up</button><button className="btn" disabled={busy} onClick={() => void run({ kind: "scroll", delta_y: 500 })}>Scroll down</button></div>
  </section></Modal>;
}
