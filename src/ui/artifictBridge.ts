/** Host side of the preview frame's bridge (see `vizDocument.ts`).
 *
 *  Everything a frame posts is untrusted: it is generated code in an opaque-
 *  origin sandbox. A message is accepted only from that exact frame's window,
 *  only with the opaque origin, only in a known shape, and only within bounds.
 *  An accepted message can move a pin, open a comment composer or select an
 *  element for Edit mode; it can never save, send or invoke anything by itself. */
import { listenForVisualizationDraft, type VisualizationFeedbackDraft } from "./visualizationFeedback";
import type { TweakMessage } from "./artifictTweaks";
import { isEditProp, MAX_EDIT_TEXT, type EditProp, type InspectPick } from "./artifictEdits";

/** The element a click picked in comment mode. `x`/`y`: percentages of the
 *  document; `ox`/`oy`: of the element's own box; `vx`/`vy`: the frame-viewport
 *  pixel the click landed on, so the composer can open right there. */
export interface BridgePick { selector: string; label: string; x: number; y: number; ox: number; oy: number; vx: number; vy: number }
/** Where a saved anchor currently sits, in frame-viewport pixels. */
export interface BridgePosition { id: string; x: number; y: number; visible: boolean }
/** A saved comment's anchor, as the frame needs it to find the element again. */
export interface FrameAnchor { id: string; selector?: string; x?: number; y?: number; ox?: number; oy?: number }

export type BridgeEvent =
  | { type: "ready" }
  | { type: "escape" }
  | { type: "pick"; pick: BridgePick }
  | { type: "inspect"; pick: InspectPick }
  | { type: "positions"; positions: BridgePosition[] };

const MAX_ANCHORS = 256;
const MIN_PICK_INTERVAL_MS = 120;
const plain = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
const percent = (v: unknown) => typeof v === "number" && Number.isFinite(v) && v >= 0 && v <= 100 ? Math.round(v * 10) / 10 : null;
/** Frame pixels: finite and within a generous viewport, never a huge value that
 *  could push a pin (or the composer) somewhere absurd. */
const pixel = (v: unknown) => typeof v === "number" && Number.isFinite(v) && Math.abs(v) <= 100_000 ? Math.round(v) : null;
const text = (v: unknown, max: number) => typeof v === "string" && v.length <= max ? v.trim() : null;

export function parseBridgeMessage(data: unknown): BridgeEvent | null {
  if (!plain(data) || data.version !== 1 || typeof data.type !== "string") return null;
  switch (data.type) {
    case "openleash:ready": return { type: "ready" };
    case "openleash:escape": return { type: "escape" };
    case "openleash:pick": {
      const selector = text(data.selector, 300), label = text(data.label, 300);
      const x = percent(data.x), y = percent(data.y), ox = percent(data.ox), oy = percent(data.oy);
      const vx = pixel(data.vx), vy = pixel(data.vy);
      if (selector === null || label === null || x === null || y === null || ox === null || oy === null || vx === null || vy === null) return null;
      return { type: "pick", pick: { selector, label: label || "element", x, y, ox, oy, vx, vy } };
    }
    case "openleash:inspect": {
      // Only what Edit mode needs, each part bounded. The text is the element's
      // own text as rendered; it is shown to the user and only ever written to
      // the source by the user's own Save, as a match for that exact text.
      const selector = text(data.selector, 300), label = text(data.label, 300);
      if (!selector || label === null) return null;
      if (data.text !== null && (typeof data.text !== "string" || data.text.length > MAX_EDIT_TEXT)) return null;
      if (!plain(data.styles)) return null;
      const styles: Partial<Record<EditProp, string>> = {};
      for (const [key, value] of Object.entries(data.styles)) {
        if (isEditProp(key) && typeof value === "string" && value.length <= 100) styles[key] = value;
      }
      return { type: "inspect", pick: { selector, label: label || "element", text: data.text as string | null, styles } };
    }
    case "openleash:positions": {
      if (!Array.isArray(data.positions) || data.positions.length > MAX_ANCHORS) return null;
      const positions: BridgePosition[] = [];
      for (const raw of data.positions) {
        if (!plain(raw)) return null;
        const id = text(raw.id, 100), x = pixel(raw.x), y = pixel(raw.y);
        if (!id || x === null || y === null || typeof raw.visible !== "boolean") return null;
        positions.push({ id, x, y, visible: raw.visible });
      }
      return { type: "positions", positions };
    }
    default: return null;
  }
}

/** Listen to one frame's bridge. State drafts keep their own bounded,
 *  rate-limited path (`listenForVisualizationDraft`). */
export function listenToFrame(
  frame: HTMLIFrameElement,
  onEvent: (event: BridgeEvent) => void,
  onDraft?: (draft: VisualizationFeedbackDraft) => void,
  target: Window = window,
  now: () => number = () => performance.now(),
): () => void {
  let lastPick = -Infinity;
  const onMessage = (event: MessageEvent) => {
    if (event.source !== frame.contentWindow || event.origin !== "null") return;
    const parsed = parseBridgeMessage(event.data);
    if (!parsed) return;
    if (parsed.type === "pick" || parsed.type === "inspect") {
      const at = now();
      if (at - lastPick < MIN_PICK_INTERVAL_MS) return;
      lastPick = at;
    }
    onEvent(parsed);
  };
  target.addEventListener("message", onMessage);
  const stopDrafts = onDraft ? listenForVisualizationDraft(frame, onDraft, target, now) : undefined;
  return () => { target.removeEventListener("message", onMessage); stopDrafts?.(); };
}

/** Host → frame. The frame is opaque-origin, so `*` is the only target origin
 *  that reaches it; what is sent is only ever a mode, anchors the user made,
 *  tweak values or the user's own pending edits — nothing private. */
type HostMessage =
  | { type: "openleash:mode"; mode: "interact" | "comment" | "edit" }
  | { type: "openleash:anchors"; anchors: FrameAnchor[] }
  | { type: "openleash:clear" }
  | { type: "openleash:tweaks"; values: TweakMessage[] }
  | { type: "openleash:edits"; rules: { selector: string; styles: Record<string, string>; text?: string }[] };
export function sendToFrame(frame: HTMLIFrameElement | null, message: HostMessage) {
  try { frame?.contentWindow?.postMessage({ version: 1, ...message }, "*"); } catch { /* A frame mid-teardown has no window to reach. */ }
}
