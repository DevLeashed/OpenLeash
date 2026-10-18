import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { RefreshCw } from "lucide-react";
import type { ArtifictAnnotation, ArtifictFormat } from "./artifictApi";
import { ArtifictBody, type ArtifictReadableFormat } from "./ArtifictCanvas";
import { ArtifictPinLayer, pinStyle, type PinPoint } from "./ArtifictPinLayer";
import { listenToFrame, sendToFrame, type BridgePick, type FrameAnchor } from "./artifictBridge";
import type { TweakMessage } from "./artifictTweaks";
import type { InspectPick } from "./artifictEdits";
import { buildVisualizationDocument, captureTheme } from "./vizDocument";
import type { VisualizationFeedbackDraft } from "./visualizationFeedback";
import { Button, TextArea } from "./primitives";

/** The widths a design is looked at in. `fit` fills the pane; the others are
 *  real device sizes, scaled down to fit rather than reflowed narrower, so a
 *  desktop layout is seen as a desktop layout even in a narrow pane. */
export type StudioDevice = "fit" | "desktop" | "tablet" | "phone";
export const STUDIO_DEVICES: Record<Exclude<StudioDevice, "fit">, { w: number; h: number; label: string }> = {
  desktop: { w: 1280, h: 800, label: "Desktop · 1280" },
  tablet: { w: 820, h: 1180, label: "Tablet · 820" },
  phone: { w: 390, h: 844, label: "Phone · 390" },
};
const STAGE_PAD = 20;

export type StudioMode = "interact" | "comment" | "edit";

function useElementSize(el: HTMLElement | null) {
  const [size, setSize] = useState({ w: 0, h: 0 });
  useLayoutEffect(() => {
    if (!el || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => {
      if (!entry) return;
      const next = { w: Math.round(entry.contentRect.width), h: Math.round(entry.contentRect.height) };
      setSize((old) => old.w === next.w && old.h === next.h ? old : next);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [el]);
  return size;
}

/** Where the frame sits inside the stage, and at what scale. */
export function artboardLayout(device: StudioDevice, stage: { w: number; h: number }) {
  if (!stage.w || !stage.h) return null;
  if (device === "fit") return { frameW: stage.w, frameH: stage.h, scale: 1 };
  const { w, h } = STUDIO_DEVICES[device];
  const availW = Math.max(120, stage.w - STAGE_PAD * 2), availH = Math.max(120, stage.h - STAGE_PAD * 2);
  if (device === "desktop") {
    // A desktop page is long: fill the height at the scale the width allows.
    const scale = Math.min(1, availW / w);
    return { frameW: w, frameH: Math.round(availH / scale), scale };
  }
  const scale = Math.min(1, availW / w, availH / h);
  return { frameW: w, frameH: h, scale };
}

/** The comment being written, opened right where the user pointed. */
export function CommentComposer({ target, value, onChange, onSubmit, onCancel, busy }: {
  target?: string; value: string; onChange: (value: string) => void; onSubmit: () => void; onCancel: () => void; busy?: boolean;
}) {
  return <div className="artifict-composer" role="dialog" aria-label="New comment" onClick={(event) => event.stopPropagation()}>
    {target && <div className="artifict-composer-target" title={target}>{target}</div>}
    <TextArea
      aria-label="Comment text"
      autoFocus
      rows={3}
      value={value}
      placeholder="What should change here?"
      onChange={(event) => onChange(event.currentTarget.value)}
      onKeyDown={(event) => {
        if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) { event.preventDefault(); if (value.trim()) onSubmit(); }
        else if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); onCancel(); }
      }}
    />
    <div className="artifict-composer-actions">
      <span className="artifict-composer-hint">Ctrl ↵ to add</span>
      <Button variant="ghost" onClick={onCancel}>Cancel</Button>
      <Button variant="primary" disabled={busy || !value.trim()} onClick={onSubmit}>Add comment</Button>
    </div>
  </div>;
}

/** A saved annotation's anchor in the shape the frame bridge resolves. */
export function frameAnchor(annotation: ArtifictAnnotation): FrameAnchor {
  return {
    id: annotation.id,
    ...(annotation.selector ? { selector: annotation.selector } : {}),
    ...(annotation.point ? { x: annotation.point.x, y: annotation.point.y } : {}),
    ...(annotation.offset ? { ox: annotation.offset.x, oy: annotation.offset.y } : {}),
  };
}

/** A live HTML design on the studio stage: its own isolated runtime, sized to a
 *  device, with the saved comments pinned on top and the composer opened at
 *  the point the user picked. The frame keeps every interaction of its own in
 *  interact mode; comment mode is the bridge's job, not an overlay that would
 *  steal the frame's clicks. */
export function HtmlArtboard({ source, title, device, mode, annotations, activeId, onSelectAnnotation, onPick, pending, composer, onEscape, onDraft, tweaks, onInspect, edits, editing }: {
  source: string;
  title: string;
  device: StudioDevice;
  mode: StudioMode;
  annotations: ArtifictAnnotation[];
  activeId?: string | null;
  onSelectAnnotation?: (annotation: ArtifictAnnotation) => void;
  onPick?: (pick: BridgePick) => void;
  pending?: { vx: number; vy: number } | null;
  composer?: ReactNode;
  onEscape?: () => void;
  onDraft?: (draft: VisualizationFeedbackDraft) => void;
  tweaks?: TweakMessage[];
  /** Edit mode: the element the user picked to edit. */
  onInspect?: (pick: InspectPick) => void;
  /** The user's pending edits, shown live in the frame. */
  edits?: { selector: string; styles: Record<string, string>; text?: string }[];
  /** Whether an element is selected for editing; when it stops being, the
   *  frame's selection outline is cleared. */
  editing?: boolean;
}) {
  const [stage, setStage] = useState<HTMLDivElement | null>(null);
  const size = useElementSize(stage);
  const frame = useRef<HTMLIFrameElement>(null);
  const [id, setId] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);
  const [positions, setPositions] = useState<Record<string, { x: number; y: number; visible: boolean }>>({});

  useEffect(() => {
    let alive = true;
    let token: string | null = null;
    setId(null); setError(""); setPositions({});
    invoke<string>("visualization_publish", { source: buildVisualizationDocument(source, captureTheme()) })
      .then((published) => {
        token = published;
        // Published after the stage went away: release it here, or it holds a
        // slot in a bounded registry nothing will ever free.
        if (!alive) { void invoke("visualization_release", { id: published }).catch(() => {}); return; }
        setId(published);
      })
      .catch((e) => { if (alive) setError(String(e)); });
    return () => {
      alive = false;
      if (token) void invoke("visualization_release", { id: token }).catch(() => {});
    };
  }, [source, attempt]);

  // The latest of everything the frame should know, for whenever it (re)loads.
  const anchors = annotations.map(frameAnchor);
  const anchorsKey = JSON.stringify(anchors);
  const tweaksKey = JSON.stringify(tweaks ?? []);
  const editsKey = JSON.stringify(edits ?? []);
  const latest = useRef({ mode, anchors, tweaks, edits });
  latest.current = { mode, anchors, tweaks, edits };
  const sync = useCallback(() => {
    const { mode: m, anchors: a, tweaks: t, edits: e } = latest.current;
    sendToFrame(frame.current, { type: "openleash:mode", mode: m });
    sendToFrame(frame.current, { type: "openleash:anchors", anchors: a });
    if (t?.length) sendToFrame(frame.current, { type: "openleash:tweaks", values: t });
    if (e?.length) sendToFrame(frame.current, { type: "openleash:edits", rules: e });
  }, []);
  useEffect(() => { sendToFrame(frame.current, { type: "openleash:mode", mode }); }, [mode, id]);
  useEffect(() => { sendToFrame(frame.current, { type: "openleash:anchors", anchors: JSON.parse(anchorsKey) as FrameAnchor[] }); }, [anchorsKey, id]);
  useEffect(() => { const values = JSON.parse(tweaksKey) as TweakMessage[]; if (values.length) sendToFrame(frame.current, { type: "openleash:tweaks", values }); }, [tweaksKey, id]);
  // Always sent, even empty: an edit the user discarded must be taken back out
  // of the running preview.
  useEffect(() => { sendToFrame(frame.current, { type: "openleash:edits", rules: JSON.parse(editsKey) as NonNullable<typeof edits> }); }, [editsKey, id]);
  const hasPending = !!pending || !!editing;
  useEffect(() => { if (!hasPending) sendToFrame(frame.current, { type: "openleash:clear" }); }, [hasPending, id]);

  const handlers = useRef({ onPick, onEscape, onDraft, onInspect });
  handlers.current = { onPick, onEscape, onDraft, onInspect };
  useEffect(() => {
    if (!id || !frame.current) return;
    return listenToFrame(frame.current, (event) => {
      if (event.type === "ready") sync();
      else if (event.type === "escape") handlers.current.onEscape?.();
      else if (event.type === "pick") handlers.current.onPick?.(event.pick);
      else if (event.type === "inspect") handlers.current.onInspect?.(event.pick);
      else setPositions(Object.fromEntries(event.positions.map((p) => [p.id, p])));
    }, (draft) => handlers.current.onDraft?.(draft));
  }, [id, sync]);

  const layout = artboardLayout(device, size);
  const scale = layout?.scale ?? 1;
  const boardW = layout ? Math.round(layout.frameW * scale) : undefined;
  const boardH = layout ? Math.round(layout.frameH * scale) : undefined;
  const pendingAt = pending ? { x: pending.vx * scale, y: pending.vy * scale } : null;
  return <div ref={setStage} className="studio-stage" data-device={device} data-mode={mode}>
    {error ? <div className="studio-stage-message" role="alert">
      <span>The preview could not start. {error}</span>
      <Button variant="ghost" onClick={() => setAttempt((n) => n + 1)}><RefreshCw size={13} />Retry</Button>
    </div> : !id ? <div className="studio-stage-message" role="status"><span className="studio-loading" aria-hidden="true" />Starting the preview…</div>
    : <div className="studio-artboard" data-device={device} style={{ width: boardW ?? "100%", height: boardH ?? "100%" }}>
      {/* The frame is laid out at device size and scaled down, so its layout box
          is bigger than the board; the clip keeps that from becoming scroll
          area, while the overlay beside it may let the composer spill out. */}
      <div className="studio-artboard-clip">
        <iframe
          ref={frame}
          key={id}
          title={`${title} preview`}
          sandbox="allow-scripts"
          referrerPolicy="no-referrer"
          src={convertFileSrc(id, "openleash-viz")}
          onLoad={sync}
          style={{ width: layout ? layout.frameW : "100%", height: layout ? layout.frameH : "100%", transform: scale !== 1 ? `scale(${scale})` : undefined }}
        />
      </div>
      <div className="studio-overlay">
        {annotations.map((annotation, index) => {
          const at = positions[annotation.id];
          if (!at?.visible) return null;
          return <button
            key={annotation.id}
            type="button"
            data-artifict-pin=""
            className="artifict-pin"
            data-sent={annotation.shared ? "true" : undefined}
            aria-label={`Comment ${index + 1}: ${annotation.text.slice(0, 80)}`}
            aria-pressed={annotation.id === activeId}
            title={annotation.text.slice(0, 200)}
            style={{ ...pinStyle(annotation.id === activeId), left: at.x * scale, top: at.y * scale }}
            onClick={() => onSelectAnnotation?.(annotation)}
          >{index + 1}</button>;
        })}
        {pendingAt && <span aria-hidden="true" className="artifict-pin" style={{ ...pinStyle(true), left: pendingAt.x, top: pendingAt.y, pointerEvents: "none" }}>+</span>}
        {pendingAt && composer && <div
          className="artifict-composer-anchor"
          data-flip-x={boardW && pendingAt.x > boardW - 300 ? "true" : undefined}
          data-flip-y={boardH && pendingAt.y > boardH - 200 ? "true" : undefined}
          style={{ left: pendingAt.x, top: pendingAt.y }}
        >{composer}</div>}
      </div>
    </div>}
  </div>;
}

/** Every other kind on the studio stage: the host draws it on a sheet sized to
 *  the device, and the pin layer takes the comment clicks directly. */
export function ReadableArtboard({ format, content, label, device, mode, annotations, activeId, onSelectAnnotation, onPoint, onQuote, pending, pendingQuote, composer, onDraft }: {
  format: ArtifictReadableFormat;
  content: string;
  label: string;
  device: StudioDevice;
  mode: StudioMode;
  annotations: ArtifictAnnotation[];
  activeId?: string | null;
  onSelectAnnotation?: (annotation: ArtifictAnnotation) => void;
  onPoint?: (point: PinPoint, target: string) => void;
  /** Selecting text comments on that text; pictures have none to select. */
  onQuote?: (point: PinPoint, quote: string) => void;
  pending?: PinPoint | null;
  pendingQuote?: string | null;
  composer?: ReactNode;
  onDraft?: (draft: VisualizationFeedbackDraft) => void;
}) {
  const bleed = format === "svg" || format === "image";
  return <div className="studio-stage readable" data-device={device} data-mode={mode}>
    <div className="studio-sheet" data-device={device} data-bleed={bleed ? "true" : undefined}>
      <ArtifictPinLayer
        label={label}
        annotations={annotations.filter((a) => a.point)}
        activeId={activeId}
        onSelect={onSelectAnnotation}
        onPoint={mode === "comment" ? onPoint : undefined}
        onQuote={bleed ? undefined : onQuote}
        commentMode={mode === "comment"}
        pending={pending ?? null}
        pendingQuote={pendingQuote}
        composer={composer}
      >
        <ArtifictBody format={format} content={content} label={label} onFeedbackDraft={onDraft} />
      </ArtifictPinLayer>
    </div>
  </div>;
}

/** The source of any kind, as the studio's code view. When `onSave` is given
 *  (the latest version, outside browse-only), the source can be edited in place
 *  and saved as the user's own revision. */
export function StudioSource({ content, label, format, onSave, busy, readOnlyReason }: { content: string; label: string; format: ArtifictFormat; onSave?: (content: string) => void; busy?: boolean; readOnlyReason?: string }) {
  const [draft, setDraft] = useState<string | null>(null);
  // A new revision on screen ends an edit begun on the old one rather than
  // offering to save it on top of the new content.
  useEffect(() => { setDraft(null); }, [content]);
  const changed = draft !== null && draft !== content;
  return <div className="studio-stage source">
    <div className="studio-source-bar">
      {draft === null ? <>
        {onSave ? <Button variant="ghost" onClick={() => setDraft(content)}>Edit source</Button> : readOnlyReason ? <span className="studio-fine">{readOnlyReason}</span> : null}
      </> : <>
        <span className="studio-fine">{changed ? "Unsaved changes" : "No changes yet"}</span>
        <Button variant="ghost" onClick={() => setDraft(null)}>Cancel</Button>
        <Button variant="primary" disabled={busy || !changed || !draft.trim()} onClick={() => onSave?.(draft)}>Save as new version</Button>
      </>}
    </div>
    {draft === null
      ? <pre className="studio-source" aria-label={`${label} source`} data-format={format}><code>{content}</code></pre>
      : <TextArea aria-label={`Edit ${label} source`} className="input studio-source studio-source-edit" spellCheck={false} value={draft} onChange={(event) => setDraft(event.currentTarget.value)}
          onKeyDown={(event) => { if (event.key === "s" && (event.ctrlKey || event.metaKey)) { event.preventDefault(); if (changed && draft.trim()) onSave?.(draft); } }} />}
  </div>;
}
