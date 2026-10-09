export interface VisualizationFeedbackDraft {
  target?: { selector?: string; label?: string };
  state?: Record<string, unknown>;
}

export const MAX_VIZ_FEEDBACK_BYTES = 8 * 1024;
export const MAX_VIZ_FEEDBACK_PER_PREVIEW = 100;
const MIN_VIZ_FEEDBACK_INTERVAL_MS = 150;

function plainRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value) && Object.getPrototypeOf(value) === Object.prototype;
}

function boundedDraftGraph(root: unknown): boolean {
  const pending: Array<{ value: unknown; depth: number }> = [{ value: root, depth: 0 }];
  const seen = new WeakSet<object>();
  let nodes = 0;
  let entries = 0;
  let textUnits = 0;
  while (pending.length) {
    const { value, depth } = pending.pop()!;
    if (typeof value === "string") { textUnits += value.length; if (textUnits > MAX_VIZ_FEEDBACK_BYTES) return false; continue; }
    if (value === null || typeof value === "boolean") continue;
    if (typeof value === "number") { if (!Number.isFinite(value)) return false; continue; }
    if (typeof value !== "object" || value === undefined || depth > 8 || seen.has(value)) return false;
    seen.add(value);
    if (++nodes > 500) return false;
    if (Array.isArray(value)) {
      entries += value.length;
      if (entries > 500) return false;
      for (const child of value) pending.push({ value: child, depth: depth + 1 });
      continue;
    }
    if (!plainRecord(value)) return false;
    const keys = Object.keys(value);
    entries += keys.length;
    if (entries > 500) return false;
    for (const key of keys) {
      textUnits += key.length;
      if (textUnits > MAX_VIZ_FEEDBACK_BYTES) return false;
      pending.push({ value: value[key], depth: depth + 1 });
    }
  }
  return true;
}

/** Messages from generated HTML are always untrusted drafts, never commands or submissions. */
export function parseVisualizationFeedbackDraft(value: unknown): VisualizationFeedbackDraft | null {
  if (!plainRecord(value) || value.type !== "openleash:feedback-draft" || value.version !== 1) return null;
  if (Object.keys(value).some((key) => !["type", "version", "target", "state"].includes(key))) return null;
  let encoded: string;
  try { encoded = JSON.stringify(value); } catch { return null; }
  if (new TextEncoder().encode(encoded).byteLength > MAX_VIZ_FEEDBACK_BYTES || !boundedDraftGraph(value)) return null;
  let target: VisualizationFeedbackDraft["target"];
  if (value.target !== undefined) {
    if (!plainRecord(value.target) || Object.keys(value.target).some((key) => key !== "selector" && key !== "label")) return null;
    const selector = value.target.selector;
    const label = value.target.label;
    if (selector !== undefined && (typeof selector !== "string" || selector.length > 300)) return null;
    if (label !== undefined && (typeof label !== "string" || label.length > 300)) return null;
    if (selector?.trim() || label?.trim()) target = { ...(selector?.trim() ? { selector: selector.trim() } : {}), ...(label?.trim() ? { label: label.trim() } : {}) };
  }
  let state: Record<string, unknown> | undefined;
  if (value.state !== undefined) {
    if (!plainRecord(value.state) || Object.keys(value.state).length > 100 || !boundedDraftGraph(value.state)) return null;
    state = value.state;
  }
  if (!target && !state) return null;
  return { ...(target ? { target } : {}), ...(state ? { state } : {}) };
}

/** Listen only to this opaque-origin iframe. A valid message stages review data;
 * it never invokes app commands, persists, or sends anything to the agent. */
export function listenForVisualizationDraft(
  frame: HTMLIFrameElement,
  onDraft: (draft: VisualizationFeedbackDraft) => void,
  target: Window = window,
  now: () => number = () => performance.now(),
): () => void {
  let count = 0;
  let lastAccepted = -Infinity;
  const onMessage = (event: MessageEvent) => {
    if (event.source !== frame.contentWindow || event.origin !== "null" || count >= MAX_VIZ_FEEDBACK_PER_PREVIEW) return;
    const timestamp = now();
    if (timestamp - lastAccepted < MIN_VIZ_FEEDBACK_INTERVAL_MS) return;
    const draft = parseVisualizationFeedbackDraft(event.data);
    if (!draft) return;
    lastAccepted = timestamp;
    count++;
    onDraft(draft);
  };
  target.addEventListener("message", onMessage);
  return () => target.removeEventListener("message", onMessage);
}
