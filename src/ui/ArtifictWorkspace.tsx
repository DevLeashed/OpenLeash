import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import {
  ChevronLeft, Download, Info, LayoutGrid, MessageSquare, PenLine, RotateCcw, SlidersHorizontal, SquarePen, ChevronRight, Code2, Columns2, Copy, Layers, Maximize2, MessageSquarePlus, Minimize2, Monitor, MousePointer2,
  Globe, RefreshCw, Scan, Send, Smartphone, Sparkles, Tablet, Trash2, WandSparkles, X,
} from "lucide-react";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { ArtifictAnnotation, ArtifictDetail, ArtifictFeedback, ArtifictFormat, ArtifictSummary, artifictApi, exportFileName, exportTarget, parseArtifictState } from "./artifictApi";
import { ArtifictOverview, ARTIFICT_BADGE } from "./ArtifictCanvas";
import { ArtifictEditPanel } from "./ArtifictEditPanel";
import { applyEditsToSource, editChanges, editNote, liveEdits, MAX_EDIT_ELEMENTS, type EditProp, type EditSet, type ElementEdit, type InspectPick } from "./artifictEdits";
import { ArtifictThumb } from "./ArtifictThumb";
import { Sandboxes } from "./Sandboxes";
import { getStudioView, patchStudioView } from "./studioViewState";
import { ArtifictTweaksPanel } from "./ArtifictTweaksPanel";
import { CommentComposer, HtmlArtboard, ReadableArtboard, StudioSource, type StudioDevice, type StudioMode } from "./ArtifictStudioStage";
import { parseTweaks, tweakMessages, tweakState, type TweakValue } from "./artifictTweaks";
import type { BridgePick } from "./artifictBridge";
import { quoteOf, type PinPoint } from "./ArtifictPinLayer";
import { Visualization } from "./Visualization";
import { captureTheme } from "./vizDocument";
import { Button, Dropdown, Input, TextArea, Tooltip } from "./primitives";
import { copyText } from "./mdx";
import { get as getStore, go, set as setStore, useStore } from "../store";
import { api } from "../api";
import { submit as submitChat } from "./Composer";
import type { VisualizationFeedbackDraft } from "./visualizationFeedback";

/** Where the next comment is being written: an element the frame's bridge
 *  picked, or a point on a host-drawn sheet. */
type Pending =
  | { kind: "frame"; pick: BridgePick }
  | { kind: "point"; point: PinPoint; target: string };

/** Everything the user is composing against one exact revision. Drafts never
 *  cross the submission boundary on their own; only Send does that. */
type ArtifictDraft = {
  pending: Pending | null;
  commentText: string;
  /** Unsent comments the user chose to hold back from the next request. */
  excluded: string[];
  stateText: string;
  feedbackDraft: VisualizationFeedbackDraft | null;
  tweakValues: Record<string, TweakValue>;
  /** The tweak state last sent, so a value is not asked for twice. */
  tweaksSent: string;
  /** Edit mode: the element picked for editing, and every pending change. */
  inspected: InspectPick | null;
  edits: EditSet;
};
const emptyDraft: ArtifictDraft = { pending: null, commentText: "", excluded: [], stateText: "", feedbackDraft: null, tweakValues: {}, tweaksSent: "{}", inspected: null, edits: {} };

/** The project's design system file, the one `prompt::MEMORY_FILES` reads. */
export const DESIGN_SYSTEM_FILE = ".openleash/DESIGN.md";

/** One-click requests from the studio, each sent against the exact revision on
 *  screen like any other studio request. They ask; they approve nothing. */
export const ASK_REQUESTS = {
  review: "Review this design as a design jury before I take it further. Score it 1–10 on layout and hierarchy, fit to the brief, consistency with the project's design system or brand, accessibility (contrast, focus states, semantics, hit targets) and copy. List the must-fix issues, fix them in a new revision, and tell me the scores before and after.",
  directions: "Explore three distinct directions for this design in one new revision — switchable tabs or side by side — each with its own palette, type and layout idea, keeping the real content. Say which one you would pick and why.",
  implement: "Implement this revision in the real project. Inspect the existing code and conventions first, build it with the project's own components and styles rather than pasting this HTML, run the relevant checks, and reply with the changed files and the verification results. Keep this artifict as the reference.",
  designSystemCreate: `Set up a design system for this project. Inspect the real styles (CSS, Tailwind or theme config, components) and this design, then write \`${DESIGN_SYSTEM_FILE}\` with concrete tokens (colours, type scale, spacing, radii, shadows), component rules and do/don't examples. Tell me what you based it on.`,
  designSystemCheck: `Check this design against the project's design system in \`${DESIGN_SYSTEM_FILE}\`. List every place it departs from the tokens or component rules, then fix them in a new revision.`,
} as const;

export type DiffLine = { kind: "same" | "remove" | "add"; text: string };
export function artifictDiff(before: string, after: string): DiffLine[] {
  const a = before.split("\n"), b = after.split("\n"); let prefix = 0, suffix = 0;
  while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) prefix++;
  while (suffix < a.length - prefix && suffix < b.length - prefix && a[a.length - 1 - suffix] === b[b.length - 1 - suffix]) suffix++;
  return [...a.slice(0, prefix).map((text) => ({ kind: "same" as const, text })), ...a.slice(prefix, a.length - suffix).map((text) => ({ kind: "remove" as const, text })), ...b.slice(prefix, b.length - suffix).map((text) => ({ kind: "add" as const, text })), ...(suffix ? a.slice(a.length - suffix).map((text) => ({ kind: "same" as const, text })) : [])];
}

/** The preview the inline chat card uses. An HTML artifict runs in the chat's
 *  own isolated viewer; every other format is drawn by the shared canvas. */
export function ArtifictContentPreview({ format, content, label, onFeedbackDraft, autoStart = false, commentMode, annotations = [], activeId, onSelectAnnotation, onPoint }: {
  format: ArtifictFormat;
  content: string;
  label?: string;
  onFeedbackDraft?: (draft: VisualizationFeedbackDraft) => void;
  autoStart?: boolean;
  commentMode?: boolean;
  annotations?: ArtifictAnnotation[];
  activeId?: string | null;
  onSelectAnnotation?: (annotation: ArtifictAnnotation) => void;
  onPoint?: (point: PinPoint, target: string) => void;
}) {
  if (format === "html") return <Visualization source={content} label={label} autoStart={autoStart} onFeedbackDraft={onFeedbackDraft} />;
  return <ArtifictOverview format={format} content={content} label={label} onFeedbackDraft={onFeedbackDraft} commentMode={commentMode} annotations={annotations} activeId={activeId} onSelectAnnotation={onSelectAnnotation} onPoint={onPoint} />;
}

/** Re-exported from its own module, where the composer fills it. */
export { studioHandoff } from "./studioHandoff";

const DEVICE_STORE = "openleash.artifict.device";
const DEVICES: { value: StudioDevice; label: string; icon: ReactNode }[] = [
  { value: "fit", label: "Fit to pane", icon: <Scan size={14} /> },
  { value: "desktop", label: "Desktop", icon: <Monitor size={14} /> },
  { value: "tablet", label: "Tablet", icon: <Tablet size={14} /> },
  { value: "phone", label: "Phone", icon: <Smartphone size={14} /> },
];
function savedDevice(): StudioDevice {
  try { const v = localStorage.getItem(DEVICE_STORE); return v === "desktop" || v === "tablet" || v === "phone" ? v : "fit"; } catch { return "fit"; }
}

/** A toolbar button: an icon with its name as a tooltip and an accessible name. */
function Tool({ label, pressed, onClick, disabled, children, className }: { label: string; pressed?: boolean; onClick: () => void; disabled?: boolean; children: ReactNode; className?: string }) {
  return <Tooltip content={label} side="bottom"><button type="button" className={`studio-tool${pressed ? " on" : ""}${className ? ` ${className}` : ""}`} aria-label={label} aria-pressed={pressed} disabled={disabled} onClick={onClick}>{children}</button></Tooltip>;
}

/** Project-scoped, persistent artifict studio. Preview frames get no app
 *  bridge beyond the bounded postMessage protocol. Comments, points, tweak
 *  values and state are host-entered, untrusted data; only the explicit Send
 *  makes them agent-readable, and that submission is also what starts the
 *  agent turn. */
export function ArtifictWorkspace({ taskId, taskTitle, initialSelection, onExpandedChange, toolbarSlot, browseOnly = false, startInGallery = false, onBrowser }: { taskId: string; taskTitle: string; initialSelection?: { artifictId: string; versionId: string } | null; onExpandedChange?: (expanded: boolean) => void; toolbarSlot?: HTMLElement | null;
  /** Switch the surface to the browser plugin's view; the mode switch offers it only when given. */
  onBrowser?: () => void;
  /** Looking, not acting: the new-task screen borrows another chat's studio to
   *  show the design, so nothing here may comment, tweak or send to that chat. */
  browseOnly?: boolean; startInGallery?: boolean }) {
  const [rows, setRows] = useState<ArtifictSummary[]>([]);
  const [artifictId, setArtifictId] = useState("");
  const [versionId, setVersionId] = useState("");
  const [detail, setDetail] = useState<ArtifictDetail | null>(null);
  const [compareVersion, setCompareVersion] = useState("");
  const [compareDetail, setCompareDetail] = useState<ArtifictDetail | null>(null);
  const [compare, setCompare] = useState(false);
  // The records are a view of their own rather than a rail beside the one you
  // opened: the studio starts on the record itself, and "All artificts" is the
  // way to the set.
  // The studio remounts when the chat changes (it is keyed by chat), so the view this chat left is read back here.
  // Browse-only embeds borrow another chat's studio and neither read nor write it.
  const saved = useMemo(() => (browseOnly ? {} : getStudioView(taskId)), []); // eslint-disable-line react-hooks/exhaustive-deps
  const [gallery, setGallery] = useState(saved.gallery ?? startInGallery);
  // The studio has two modes: the project's artificts, and the sandboxes (agent-owned computers).
  const [sbx, setSbx] = useState(saved.sbx ?? false);
  // When this chat's agent makes or picks a sandbox, show it rather than leaving the user on a design.
  useEffect(() => {
    if (browseOnly) return;
    let un: (() => void) | undefined; let dead = false;
    void Promise.resolve(listen<{ task?: string; attached?: boolean }>("ol://sandbox", ({ payload }) => {
      if (payload.attached && payload.task === taskId) { setSbx(true); setGallery(false); }
    })).then((u) => { if (typeof u === "function") { if (dead) u(); else un = u; } });
    return () => { dead = true; un?.(); };
  }, [browseOnly, taskId]);
  const [expanded, setExpanded] = useState(false);
  // Expand is a claim on the layout, and the layout belongs to the pane that
  // hosts this surface, so the state is reported out rather than acted on here.
  useEffect(() => { onExpandedChange?.(expanded); }, [expanded, onExpandedChange]);
  const [filter, setFilter] = useState("");
  const [mode, setMode] = useState<StudioMode>((saved.mode as StudioMode | undefined) ?? "interact");
  const [device, setDeviceState] = useState<StudioDevice>(savedDevice);
  const setDevice = (value: StudioDevice) => { setDeviceState(value); try { localStorage.setItem(DEVICE_STORE, value); } catch { /* Not persisted; still applied. */ } };
  const [showSource, setShowSource] = useState(false);
  // Comments, Tweaks and Details are cards that float over the canvas, opened
  // from the toolbar — never a column that takes the design's width. Closed
  // until asked for; the comments themselves are already on the canvas as pins.
  const [panel, setPanel] = useState<null | "comments" | "tweaks" | "details" | "edit" | "ask">((saved.panel as null | "comments" | "tweaks" | "details" | "edit" | "ask" | undefined) ?? null);
  const togglePanel = (which: "comments" | "tweaks" | "details" | "edit" | "ask") => setPanel((open) => open === which ? null : which);
  const [activeAnnotation, setActiveAnnotation] = useState<string | null>(null);
  // Drafts belong to an exact version, not whichever preview happens to be open.
  const [drafts, setDrafts] = useState<Record<string, ArtifictDraft>>({});
  const draftKey = `${taskId}:${artifictId}:${versionId}`;
  const draft = drafts[draftKey] ?? emptyDraft;
  const { pending, commentText, excluded, stateText, feedbackDraft, tweakValues, tweaksSent, inspected, edits } = draft;
  const patchDraft = useCallback((key: string, patch: Partial<ArtifictDraft> | ((old: ArtifictDraft) => Partial<ArtifictDraft>)) => setDrafts((old) => {
    const base = old[key] ?? emptyDraft;
    return { ...old, [key]: { ...base, ...(typeof patch === "function" ? patch(base) : patch) } };
  }), []);
  const updateDraft = (patch: Partial<ArtifictDraft> | ((old: ArtifictDraft) => Partial<ArtifictDraft>)) => patchDraft(draftKey, patch);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  useEffect(() => { if (!notice) return; const t = setTimeout(() => setNotice(""), 2600); return () => clearTimeout(t); }, [notice]);
  // An explicit selection (a card, or a handoff from another chat) wins; otherwise this chat's own last one.
  const initialArtifictId = initialSelection?.artifictId ?? saved.artifictId ?? "";
  const initialVersionId = initialSelection?.versionId ?? (initialSelection ? "" : saved.versionId ?? "");
  useEffect(() => {
    if (!browseOnly) patchStudioView(taskId, { artifictId, versionId, gallery, sbx, mode, panel });
  }, [browseOnly, taskId, artifictId, versionId, gallery, sbx, mode, panel]);
  // The backend publishes a new id every time an artifict is written — from the
  // agent's own tools as much as from here — so a revision the agent makes during
  // a turn lands in an open studio without a manual Refresh.
  const publishedVersion = useStore((s) => s.artificts[taskId]?.[artifictId]);
  const taskRunning = useStore((s) => s.tasks[taskId]?.status === "running");
  // Whether the project has a design system the agent is given. Read from the
  // same inventory Settings shows: `docs` is what exists, `memory` what the
  // next chat's prompt actually carries (an untrusted or switched-off file
  // exists but is not given to the agent).
  const projectPath = useStore((s) => s.tasks[taskId]?.project ?? "");
  const [designSystem, setDesignSystem] = useState<"given" | "unused" | "none" | null>(null);
  const readDesignSystem = useCallback(() => {
    if (!projectPath) { setDesignSystem(null); return; }
    api.project(projectPath).then((info) => setDesignSystem(info.memory.includes(DESIGN_SYSTEM_FILE) ? "given" : info.docs.includes(DESIGN_SYSTEM_FILE) ? "unused" : "none")).catch(() => setDesignSystem(null));
  }, [projectPath]);
  useEffect(() => { readDesignSystem(); }, [readDesignSystem]);
  // An artifict tool call in flight for this record (or a new one being made):
  // the canvas says so instead of sitting still while the agent writes.
  const building = useStore((s) => {
    const items = s.items[taskId] ?? [];
    for (let i = items.length - 1, seen = 0; i >= 0 && seen < 60; i--, seen++) {
      const item = items[i]!;
      if (item.kind !== "tool" || item.data?.status !== "running") continue;
      const name = item.data?.name;
      if (name === "artifict_create") return "create";
      if (name === "artifict_revise" && (!item.data?.input?.id || item.data.input.id === artifictId)) return "revise";
    }
    return "";
  });

  const load = useCallback(async (preferId?: string, preferVersion?: string) => {
    const list = await artifictApi.list(taskId);
    setRows(list);
    const id = preferId && list.some((item) => item.id === preferId) ? preferId : artifictId && list.some((item) => item.id === artifictId) ? artifictId : list[0]?.id ?? "";
    setArtifictId(id);
    const summary = list.find((item) => item.id === id);
    if (!summary) { setDetail(null); setVersionId(""); return; }
    const vid = preferVersion && summary.versions.some((item) => item.id === preferVersion) ? preferVersion : id === artifictId && summary.versions.some((item) => item.id === versionId) ? versionId : summary.latest_version_id;
    setVersionId(vid);
    const next = await artifictApi.get(taskId, id, vid);
    setDetail(next);
    const other = next.summary.versions.find((item) => item.id !== vid); setCompareVersion((old) => old !== vid && next.summary.versions.some((item) => item.id === old) ? old : other?.id ?? "");
  }, [taskId, artifictId, versionId]);

  useEffect(() => {
    let alive = true; setBusy(true); setError("");
    artifictApi.list(taskId).then(async (list) => {
      if (!alive) return;
      setRows(list);
      const id = initialArtifictId && list.some((item) => item.id === initialArtifictId) ? initialArtifictId : list[0]?.id ?? ""; setArtifictId(id);
      const summary = list.find((item) => item.id === id);
      if (summary) {
        const requestedVersion = initialArtifictId === id ? initialVersionId : undefined;
        const selectedVersion = requestedVersion && summary.versions.some((item) => item.id === requestedVersion) ? requestedVersion : summary.latest_version_id;
        const next = await artifictApi.get(taskId, summary.id, selectedVersion);
        if (!alive) return;
        setDetail(next); setVersionId(next.selected_version.id);
        setCompareVersion(next.summary.versions.filter((v) => v.id !== next.selected_version.id).at(-1)?.id ?? "");
      }
    }).catch((e) => { if (alive) setError(String(e)); }).finally(() => { if (alive) setBusy(false); });
    return () => { alive = false; };
  }, [taskId, initialArtifictId, initialVersionId]);
  useEffect(() => {
    let alive = true;
    if (artifictId && versionId) artifictApi.get(taskId, artifictId, versionId).then((next) => {
      if (!alive) return;
      setDetail(next);
    }).catch((e) => { if (alive) setError(String(e)); });
    else setDetail(null);
    return () => { alive = false; };
  }, [taskId, artifictId, versionId, publishedVersion]);
  useEffect(() => {
    let alive = true;
    if (compare && artifictId && compareVersion) artifictApi.get(taskId, artifictId, compareVersion).then((next) => { if (alive) setCompareDetail(next); }).catch((e) => { if (alive) setError(String(e)); });
    else setCompareDetail(null);
    return () => { alive = false; };
  }, [compare, taskId, artifictId, compareVersion]);
  // What the studio is showing, read by the publish handler below. Kept in a ref
  // because that handler must fire on the published version alone.
  const [panelPos, setPanelPos] = useState({ x: 0, y: 0 });
  const view = useRef<{ artifict: string; version: string; tip: string } | null>(null);
  useEffect(() => {
    view.current = detail ? { artifict: detail.summary.id, version: detail.selected_version.id, tip: detail.summary.latest_version_id } : null;
  }, [detail]);
  // Whatever the agent writes is what the user should see: a new revision of
  // the open artifict, or a new artifict altogether, opens at its latest
  // version the moment it is written. The version they were on becomes the
  // comparison, so going back is one click. Writes that were already published
  // when the studio opened are history, not news, and do not move it.
  const published = useStore((s) => s.artificts[taskId]);
  const seen = useRef<Record<string, string> | null>(null);
  useEffect(() => {
    const now = published ?? {};
    const before = seen.current;
    seen.current = { ...now };
    if (!before) return;
    const changed = Object.keys(now).filter((id) => now[id] !== before[id]);
    if (!changed.length) return;
    const target = changed.includes(artifictId) ? artifictId : changed[changed.length - 1]!;
    const shown = view.current;
    if (shown && shown.artifict === target && shown.version !== now[target]) setCompareVersion(shown.version);
    if (target !== artifictId) { setArtifictId(target); setGallery(false); setCompare(false); setShowSource(false); }
    setVersionId(now[target]!);
    let alive = true;
    artifictApi.list(taskId).then((list) => { if (alive) setRows(list); }).catch(() => {});
    return () => { alive = false; };
    // Keyed on the published map alone: the selection it changes must not
    // re-run it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [taskId, published]);

  useEffect(() => { setActiveAnnotation(null); }, [artifictId, versionId]);
  const parsedState = useMemo(() => parseArtifictState(stateText), [stateText]);
  const comparedDetail = compareDetail?.summary.id === artifictId && compareDetail.selected_version.id === compareVersion ? compareDetail : null;
  const previewDraft = useCallback((proposal: VisualizationFeedbackDraft) => patchDraft(draftKey, { feedbackDraft: proposal }), [draftKey, patchDraft]);
  const selectedDetail = detail?.summary.id === artifictId && detail.selected_version.id === versionId ? detail : null;
  const current = selectedDetail?.selected_version;
  const format = detail?.summary.format;
  // Edit mode is an HTML design's: a document, diagram or picture is edited
  // through its source instead.
  useEffect(() => { if (format && format !== "html") { setMode((m) => m === "edit" ? "interact" : m); setPanel((p) => p === "edit" ? null : p); } }, [format]);
  // Publish what is on screen so the chat can tell the agent; withdrawn when
  // the studio closes, the gallery is up, or nothing is selected.
  const shownId = !browseOnly && !gallery && !sbx && current && selectedDetail ? `${selectedDetail.summary.id}:${current.id}` : "";
  const shownTitle = selectedDetail?.summary.title ?? "";
  const shownNumber = current?.number ?? 0;
  useEffect(() => {
    const [aId, vId] = shownId.split(":");
    setStore((s) => {
      const next = { ...s.studioView };
      if (aId && vId) next[taskId] = { artifictId: aId, versionId: vId, title: shownTitle, number: shownNumber, kind: format ?? "" };
      else delete next[taskId];
      return { studioView: next };
    });
  }, [taskId, shownId, shownTitle, shownNumber, format]);
  useEffect(() => () => { if (!browseOnly) setStore((s) => { const next = { ...s.studioView }; delete next[taskId]; return { studioView: next }; }); }, [taskId, browseOnly]);
  const versions = detail?.summary.versions ?? [];
  const position = versions.findIndex((v) => v.id === versionId);
  const onTip = !!detail && versionId === detail.summary.latest_version_id;
  const tweaks = useMemo(() => format === "html" && current ? parseTweaks(current.content) : [], [format, current]);
  const tweakFrame = useMemo(() => tweakMessages(tweaks, tweakValues), [tweaks, tweakValues]);
  const tweakChanges = useMemo(() => tweakState(tweaks, tweakValues), [tweaks, tweakValues]);
  const unsentTweaks = Object.keys(tweakChanges).length > 0 && JSON.stringify(tweakChanges) !== tweaksSent ? tweakChanges : null;
  const annotations: ArtifictAnnotation[] = detail?.annotations.filter((item) => item.version_id === versionId) ?? [];
  const queued = annotations.filter((a) => !a.shared && !excluded.includes(a.id));
  const unsentCount = annotations.filter((a) => !a.shared).length;
  // The conversation is the chat beside the studio; here the agent's state is
  // only named, on the canvas, so there is never a second chat to read.
  const feedbackHistory: ArtifictFeedback[] = detail?.feedback ?? [];
  const outstanding = feedbackHistory.some((f) => f.status === "submitted");
  const lastReply = [...feedbackHistory].reverse().find((f) => f.response);
  const agentAsks = !outstanding && lastReply?.decision === "needs_clarification";
  const diff = current && comparedDetail ? artifictDiff(comparedDetail.selected_version.content, current.content) : [];

  const execute = async (fn: () => Promise<ArtifictDetail | undefined>, success: string, id = artifictId, vid?: string, onSaved?: () => void) => {
    if (busy) return;
    setBusy(true); setError(""); setNotice("");
    try {
      const result = await fn();
      onSaved?.();
      if (result) {
        setDetail(result);
        setRows((old) => old.map((row) => row.id === result.summary.id ? { ...row, ...result.summary, pendingFeedback: result.feedback.filter(f => f.status === "submitted" || f.decision === "needs_clarification").length } : row));
      } else await load(id, vid);
      if (id) setNotice(success);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const pickArtifict = (id: string) => { const row = rows.find((item) => item.id === id); setArtifictId(id); setVersionId(row?.latest_version_id ?? ""); setError(""); setNotice(""); setCompare(false); setGallery(false); setShowSource(false); };
  const chooseVersion = (id: string) => {
    if (!detail || !id) return;
    setVersionId(id);
    setCompareVersion((old) => old && old !== id ? old : detail.summary.versions.find((v) => v.id !== id)?.id ?? "");
    setError(""); setNotice("");
  };

  // Comment flow: point at something, write, add. A comment is saved private to
  // this exact revision and queued; sending it is a separate, explicit act.
  const onPick = (pick: BridgePick) => updateDraft({ pending: { kind: "frame", pick } });
  const onPoint = (point: PinPoint, target: string) => updateDraft({ pending: { kind: "point", point, target } });
  // A selected passage: the comment is about exactly that text, quoted as its
  // target so both the highlight and the agent get the passage itself.
  const onQuote = (point: PinPoint, quote: string) => { setShowSource(false); updateDraft({ pending: { kind: "point", point, target: `“${quote}”` } }); };
  const cancelComment = () => updateDraft({ pending: null });
  const saveComment = () => {
    if (!current || !pending) return;
    const text = commentText.trim();
    if (!text) return setError("Write a comment first.");
    if (text.length > 4000) return setError("A comment is limited to 4,000 characters.");
    const key = draftKey;
    const input = pending.kind === "frame"
      ? { version_id: current.id, text, selector: pending.pick.selector || undefined, target: pending.pick.label || undefined, point: { x: pending.pick.x, y: pending.pick.y }, offset: { x: pending.pick.ox, y: pending.pick.oy } }
      : { version_id: current.id, text, target: pending.target || undefined, point: pending.point };
    void execute(() => artifictApi.annotate(taskId, artifictId, input), `Comment added · it goes with your next Send`, artifictId, current.id,
      () => patchDraft(key, (old) => ({ commentText: old.commentText === commentText ? "" : old.commentText, pending: null })));
  };
  const removeComment = (annotationId: string) => {
    void execute(() => artifictApi.removeAnnotation(taskId, artifictId, annotationId, current?.id), "Comment removed.", artifictId, current?.id, () => updateDraft((old) => ({ excluded: old.excluded.filter((id) => id !== annotationId) })));
  };

  /** Send what the canvas collected — queued comments, moved tweaks, state.
   *  Free-form talk belongs in the chat beside the studio, not a second box. */
  const send = (text = "") => {
    if (!current) return setError("Select an exact artifict version first.");
    if (parsedState.error) return setError(parsedState.error);
    const state = unsentTweaks ? { ...(parsedState.value ?? {}), tweaks: unsentTweaks } : parsedState.value;
    if (!queued.length && !state) return setError("Add a comment or move a tweak first.");
    if (browseOnly) return void startChatWith(requestText(text, state));
    const key = draftKey, sentState = stateText, sentIds = queued.map((a) => a.id), sentTweaks = unsentTweaks ? JSON.stringify(unsentTweaks) : null;
    void execute(() => artifictApi.submitFeedback(taskId, artifictId, { version_id: current.id, feedback: text, annotation_ids: sentIds, interactive_state: state }), `Sent · the reply arrives in the chat`, artifictId, current.id,
      () => {
        patchDraft(key, (old) => ({
          stateText: old.stateText === sentState ? "" : old.stateText,
          excluded: old.excluded.filter((id) => !sentIds.includes(id)),
          ...(sentTweaks ? { tweaksSent: sentTweaks } : {}),
        }));
      });
  };
  const useProposal = () => {
    if (!feedbackDraft) return;
    updateDraft({
      feedbackDraft: null,
      ...(feedbackDraft.state ? { stateText: JSON.stringify(feedbackDraft.state, null, 2) } : {}),
    });
  };
  const filteredRows = rows.filter((row) => `${row.title} ${row.format} ${row.excerpt ?? ""} ${row.recordStage ?? ""}`.toLowerCase().includes(filter.toLowerCase()));

  // ── The user's own revisions: canvas edits, source edits, restore. Each is a
  // new version on top of the latest; nothing is rewritten, and the store's
  // parent check refuses one that raced an agent revision.
  const nextNumber = versions.length + 1;
  const toLatest = (result: ArtifictDetail) => { setVersionId(result.selected_version.id); return result; };
  const onInspect = (pick: InspectPick) => updateDraft({ inspected: pick });
  const editElement = (pick: InspectPick, patch: (edit: ElementEdit) => ElementEdit) => updateDraft((old) => {
    const base: ElementEdit = old.edits[pick.selector] ?? { selector: pick.selector, label: pick.label, originalText: pick.text, styles: {} };
    const next = patch(base);
    const changed: EditSet = { ...old.edits };
    if (editChanges(next)) changed[next.selector] = next; else delete changed[next.selector];
    if (Object.keys(changed).length > MAX_EDIT_ELEMENTS) { setError(`Save these edits first: at most ${MAX_EDIT_ELEMENTS} elements can be edited at once.`); return {}; }
    return { edits: changed };
  });
  const onEditText = (pick: InspectPick, value: string) => editElement(pick, (edit) => ({ ...edit, text: value }));
  const onEditStyle = (pick: InspectPick, prop: EditProp, value: string | null) => editElement(pick, (edit) => {
    const styles = { ...edit.styles };
    if (value === null) delete styles[prop]; else styles[prop] = value;
    return { ...edit, styles };
  });
  const resetElement = (selector: string) => updateDraft((old) => { const changed = { ...old.edits }; delete changed[selector]; return { edits: changed }; });
  const discardEdits = () => updateDraft({ edits: {}, inspected: null });
  const saveEdits = () => {
    if (!current || !detail || !onTip) return;
    const result = applyEditsToSource(current.content, edits);
    if (result.failed.length) return setError(`Not saved — ${result.failed.map((f) => `${f.label}: ${f.reason}`).join("; ")}. Reset that element, or leave a comment for the agent instead.`);
    if (result.content === current.content) return setError("These edits make no change to the source.");
    const key = draftKey;
    void execute(async () => toLatest(await artifictApi.edit(taskId, artifictId, current.id, result.content, editNote(edits, result))), `Saved as v${nextNumber}`, artifictId, undefined,
      () => patchDraft(key, { edits: {}, inspected: null }));
  };
  const saveSource = (content: string) => {
    if (!current || !onTip) return;
    void execute(async () => toLatest(await artifictApi.edit(taskId, artifictId, current.id, content, "Edited the source in the studio")), `Saved as v${nextNumber}`);
  };
  const restoreVersion = () => {
    if (!current || !detail || onTip) return;
    const from = current;
    void execute(async () => toLatest(await artifictApi.restore(taskId, artifictId, detail.summary.latest_version_id, from.id)), `v${from.number} restored as v${nextNumber}`);
  };
  const target = current && format ? exportTarget(format, current.content) : null;
  const exportVersion = async () => {
    if (!current || !detail || !target) return;
    const path = await saveDialog({ defaultPath: exportFileName(detail.summary.title, current.number, target.ext), filters: [{ name: target.name, extensions: [target.ext] }] }).catch((e) => { setError(String(e)); return null; });
    if (!path) return;
    try {
      const written = await artifictApi.export(taskId, artifictId, current.id, path, format === "html" ? captureTheme() : null);
      setNotice(`Exported v${current.number} to ${written}`);
    } catch (e) { setError(String(e)); }
  };
  /** A one-click request: text only, against the exact revision on screen.
   *  Queued comments stay queued for the next Send. */
  const ask = (text: string) => {
    if (!current) return;
    if (browseOnly) return void startChatWith(text);
    void execute(() => artifictApi.submitFeedback(taskId, artifictId, { version_id: current.id, feedback: text, annotation_ids: [], interactive_state: null }), "Sent · the reply arrives in the chat", artifictId, current.id, () => setPanel(null));
  };
  const setCanvasMode = (next: StudioMode) => {
    setShowSource(false);
    setMode(next);
    if (next === "edit") setPanel("edit");
    else if (panel === "edit") setPanel(null);
  };

  /** On the new-task screen this studio reads through a chat it does not own, so
   *  a request has no chat to go to: it becomes the first message of the new
   *  chat, about this exact revision, and sends now. Whatever was already typed
   *  in the composer stays, ahead of the request. */
  const requestText = (text: string, state: Record<string, unknown> | null) => {
    if (!current) return text;
    const parts = [text.trim()];
    if (queued.length) parts.push(`Comments on v${current.number}:\n` + queued.map((a, i) => `${i + 1}. ${a.target || a.selector ? `[${a.target || a.selector}] ` : ""}${a.text}`).join("\n"));
    if (state) parts.push(`Captured state / tweaks:\n${JSON.stringify(state, null, 2)}`);
    return parts.filter(Boolean).join("\n\n");
  };
  const startChatWith = (text: string) => {
    if (!current || !detail) return;
    const typed = getStore().draft.trim();
    setStore((s) => ({ draft: typed ? `${typed}\n\n${text}` : text, home: { ...s.home, artificts: true, studio: { taskId, artifictId: detail.summary.id, versionId: current.id, title: detail.summary.title, number: current.number, kind: detail.summary.format } } }));
    setPanel(null);
    void submitChat("home");
  };

  /** A new chat about the artifict on screen: the real new-task screen, in
   *  Artificts mode, with this revision beside it. The composer there has the
   *  model picker and everything else a new chat has; the chat it creates is
   *  told which artifict it was started from and opens on it. */
  const startChat = () => {
    if (!current || !detail) return;
    setStore((s) => ({ home: { ...s.home, artificts: true, studio: { taskId, artifictId: detail.summary.id, versionId: current.id, title: detail.summary.title, number: current.number, kind: detail.summary.format } } }));
    go("home");
  };

  // Keyboard: C toggles comment mode, Esc steps back out of it, [ and ] walk
  // the versions — the way a design tool's canvas answers, and never while the
  // user is typing.
  const onKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    const target = event.target as HTMLElement;
    if (target.closest("input,textarea,select,[role='combobox'],[contenteditable='true']") || event.ctrlKey || event.metaKey || event.altKey) return;
    if (gallery || !current) return;
    if ((event.key === "c" || event.key === "C")) { event.preventDefault(); setCanvasMode(mode === "comment" ? "interact" : "comment"); }
    else if (format === "html" && (event.key === "e" || event.key === "E")) { event.preventDefault(); setCanvasMode(mode === "edit" ? "interact" : "edit"); }
    else if (event.key === "[" && position > 0) chooseVersion(versions[position - 1]!.id);
    else if (event.key === "]" && position >= 0 && position < versions.length - 1) chooseVersion(versions[position + 1]!.id);
  };
  const onEscape = () => { if (pending) cancelComment(); else if (mode === "edit" && inspected) updateDraft({ inspected: null }); else if (mode !== "interact") setCanvasMode("interact"); };
  // Esc steps out of comment/edit mode wherever focus is (iframe, body, panel).
  // The section's onKeyDown only hears it with focus inside, and the app's window
  // handler would otherwise pause the running agent: so listen in the capture
  // phase and consume it, the way useEscapeClose does.
  const escapeRef = useRef(onEscape);
  escapeRef.current = onEscape;
  const escapeActive = !gallery && !sbx && !!current && (mode !== "interact" || !!pending);
  useEffect(() => {
    if (!escapeActive) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      escapeRef.current();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [escapeActive]);

  const composer = current && pending ? <CommentComposer
    target={pending.kind === "frame" ? pending.pick.label : pending.target || undefined}
    value={commentText}
    onChange={(value) => updateDraft({ commentText: value })}
    onSubmit={saveComment}
    onCancel={cancelComment}
    busy={busy}
  /> : null;

  const stage = (shown: ArtifictDetail, primary: boolean) => {
    const version = shown.selected_version;
    const label = `${shown.summary.title} · v${version.number}`;
    const shownAnnotations = shown.annotations.filter((a) => a.version_id === version.id);
    if (showSource) return <StudioSource content={version.content} label={label} format={shown.summary.format}
      busy={busy}
      onSave={primary && version.id === shown.summary.latest_version_id ? saveSource : undefined}
      readOnlyReason={primary && version.id !== shown.summary.latest_version_id ? "Go to the latest version to edit its source." : undefined} />;
    if (shown.summary.format === "html") return <HtmlArtboard
      key={`${shown.summary.id}:${version.id}`}
      source={version.content}
      title={label}
      device={device}
      mode={primary ? mode : "interact"}
      annotations={shownAnnotations}
      activeId={primary ? activeAnnotation : null}
      onSelectAnnotation={primary ? (a) => { setActiveAnnotation(a.id); setPanel("comments"); } : undefined}
      onPick={primary ? onPick : undefined}
      pending={primary && pending?.kind === "frame" ? pending.pick : null}
      composer={primary ? composer : null}
      onEscape={primary ? onEscape : undefined}
      onDraft={primary ? previewDraft : undefined}
      tweaks={primary ? tweakFrame : undefined}
      onInspect={primary && mode === "edit" ? onInspect : undefined}
      edits={primary ? liveEdits(edits) : undefined}
      editing={primary && !!inspected}
    />;
    return <ReadableArtboard
      key={`${shown.summary.id}:${version.id}`}
      format={shown.summary.format}
      content={version.content}
      label={label}
      device={device}
      mode={primary ? mode : "interact"}
      annotations={shownAnnotations}
      activeId={primary ? activeAnnotation : null}
      onSelectAnnotation={primary ? (a) => { setActiveAnnotation(a.id); setPanel("comments"); } : undefined}
      onPoint={primary ? onPoint : undefined}
      onQuote={primary ? onQuote : undefined}
      pending={primary && pending?.kind === "point" ? pending.point : null}
      pendingQuote={primary && pending?.kind === "point" ? quoteOf(pending.target) : null}
      composer={primary ? composer : null}
      onDraft={primary ? previewDraft : undefined}
    />;
  };

  const sendCount = queued.length + (unsentTweaks ? 1 : 0);
  // In the app the toolbar lives in the chat header (Session hands over a slot),
  // so the surface has one bar; standalone it keeps a bar of its own. React
  // events still bubble through the portal to this section, so the keyboard
  // shortcuts work wherever the toolbar is drawn.
  const portalOr = (node: ReactNode) => toolbarSlot ? createPortal(node, toolbarSlot) : node;
  return <section aria-label="Artifict workspace" className="studio" onKeyDown={onKeyDown}>
    {portalOr(<div role="toolbar" aria-label="Artifict tools" className={toolbarSlot ? "studio-bar in-header" : "studio-bar artifict-workspace-bar"}>
      <div className="studio-bar-group studio-title-group">
        {!sbx && rows.length > 0 && <Tooltip content={gallery ? "Back to the open artifict" : "All artificts in this project"} side="bottom"><button type="button" className={`studio-tool studio-gallery-toggle${gallery ? " on" : ""}`} aria-label={gallery ? "Back to artifict" : "All artificts"} aria-expanded={gallery} onClick={() => setGallery((value) => !value)}>{gallery ? <ChevronLeft size={15} aria-hidden="true" /> : <LayoutGrid size={14} aria-hidden="true" />}</button></Tooltip>}
        <b className="studio-title" title={detail?.summary.title}>{sbx ? "Sandboxes" : gallery ? "Artificts" : detail?.summary.title ?? "Artificts"}</b>
        {!gallery && !sbx && format && <span className="artifict-badge">{ARTIFICT_BADGE[format]}</span>}
        {!toolbarSlot && <span className="studio-task" title={taskTitle}>{taskTitle}</span>}
      </div>
      {(gallery || sbx || rows.length === 0) && <div className="studio-bar-group studio-seg" role="group" aria-label="Studio mode">
        <button type="button" aria-pressed={!sbx} className={!sbx ? "on" : ""} onClick={() => setSbx(false)}><Layers size={12} aria-hidden="true" />Artificts</button>
        <button type="button" aria-pressed={sbx} className={sbx ? "on" : ""} onClick={() => setSbx(true)}><Monitor size={12} aria-hidden="true" />Sandboxes</button>
        {onBrowser && <button type="button" aria-pressed={false} onClick={onBrowser}><Globe size={12} aria-hidden="true" />Browser</button>}
      </div>}
      {!gallery && !sbx && detail && current && <div className="studio-bar-group studio-versions" aria-label="Versions">
        <Tool label="Previous version ( [ )" disabled={busy || position <= 0} onClick={() => chooseVersion(versions[position - 1]!.id)}><ChevronLeft size={14} /></Tool>
        <Dropdown placeholder="Exact artifict version" search={versions.length > 10} disabled={busy} value={versionId} onChange={chooseVersion} style={{ height: 28, maxWidth: 230, padding: "0 8px", gap: 6, fontSize: 11.5 }}
          options={versions.map((v) => ({ value: v.id, label: `v${v.number}`, hint: `${new Date(v.created_at).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}${v.author === "user" ? " · yours" : ""}` }))} />
        <Tool label="Next version ( ] )" disabled={busy || position < 0 || position >= versions.length - 1} onClick={() => chooseVersion(versions[position + 1]!.id)}><ChevronRight size={14} /></Tool>
        {onTip ? <span className="studio-latest">Latest</span> : <>
          <button type="button" className="studio-latest jump" onClick={() => chooseVersion(detail.summary.latest_version_id)}>Go to latest</button>
          <Tool label={`Restore v${current.number} as v${nextNumber}`} disabled={busy} onClick={restoreVersion}><RotateCcw size={14} /></Tool>
        </>}
      </div>}
      <span className="studio-spacer" />
      {!gallery && !sbx && detail && current && <>
        <div className="studio-bar-group studio-seg" role="radiogroup" aria-label="Canvas mode">
          <Tooltip content="Use the design (Esc)" side="bottom"><button type="button" role="radio" aria-checked={mode === "interact"} aria-label="Interact" className={mode === "interact" ? "on" : undefined} onClick={() => setCanvasMode("interact")}><MousePointer2 size={14} /></button></Tooltip>
          <Tooltip content="Click anything to comment (C)" side="bottom"><button type="button" role="radio" aria-checked={mode === "comment"} aria-label="Comment" className={mode === "comment" ? "on" : undefined} onClick={() => setCanvasMode("comment")}><MessageSquarePlus size={14} /><span>Comment</span></button></Tooltip>
          {format === "html" && <Tooltip content="Click anything to edit its text or style (E)" side="bottom"><button type="button" role="radio" aria-checked={mode === "edit"} aria-label="Edit" className={mode === "edit" ? "on" : undefined} onClick={() => setCanvasMode("edit")}><PenLine size={14} /><span>Edit</span></button></Tooltip>}
        </div>
        <div className="studio-bar-group studio-seg" role="radiogroup" aria-label="Preview size">
          {DEVICES.map((d) => <Tooltip key={d.value} content={d.label} side="bottom"><button type="button" role="radio" aria-checked={device === d.value} aria-label={d.label} className={device === d.value ? "on" : undefined} onClick={() => setDevice(d.value)}>{d.icon}</button></Tooltip>)}
        </div>
        <div className="studio-bar-group">
          <Tool label="Comments" pressed={panel === "comments"} onClick={() => togglePanel("comments")}><MessageSquare size={14} />{annotations.length ? <span className="studio-tool-count">{annotations.length}</span> : null}</Tool>
          {format === "html" && <Tool label="Tweaks" pressed={panel === "tweaks"} onClick={() => togglePanel("tweaks")}><SlidersHorizontal size={14} />{tweaks.length ? <span className="studio-tool-count">{tweaks.length}</span> : null}</Tool>}
          <Tool label="Details" pressed={panel === "details"} onClick={() => togglePanel("details")}><Info size={14} /></Tool>
          <Tool label="Ask the agent" pressed={panel === "ask"} onClick={() => { if (panel !== "ask") readDesignSystem(); togglePanel("ask"); }}><WandSparkles size={14} /></Tool>
          <Tool label="New chat about this artifict" onClick={startChat}><SquarePen size={14} /></Tool>
        </div>
        <div className="studio-bar-group">
          <Tool label={showSource ? "Show the design" : "View source"} pressed={showSource} onClick={() => { setShowSource((v) => !v); setMode("interact"); if (panel === "edit") setPanel(null); }}><Code2 size={14} /></Tool>
          {versions.length > 1 && <Tool label={compare ? "Hide comparison" : "Compare versions"} pressed={compare} onClick={() => setCompare((v) => !v)}><Columns2 size={14} /></Tool>}
          <Tool label="Copy source" onClick={() => copyText(current.content, "Artifict source copied")}><Copy size={14} /></Tool>
          <Tool label={target ? `Export v${current.number} as ${target.name}` : "Only an embedded picture can be exported"} disabled={!target} onClick={() => void exportVersion()}><Download size={14} /></Tool>
        </div>
      </>}
      <div className="studio-bar-group">
        <Tool label="Refresh artificts" disabled={busy} onClick={() => void execute(async () => undefined, "Artificts refreshed.")}><RefreshCw size={14} /></Tool>
        {!browseOnly && <Tool label={expanded ? "Collapse artifict editor" : "Expand artifict editor"} pressed={expanded} onClick={() => setExpanded((value) => !value)}>{expanded ? <Minimize2 size={14} /> : <Maximize2 size={14} />}</Tool>}
      </div>
    </div>)}
    {busy && <div className="studio-progress" role="status" aria-label="Loading / saving…" />}
    {error && <div className="studio-error" role="alert"><span>{error}</span><button type="button" aria-label="Dismiss error" onClick={() => setError("")}><X size={12} /></button></div>}
    {sbx ? <Sandboxes taskId={taskId} browseOnly={browseOnly} />
    : gallery ? <div className="artifict-gallery">
        <Input aria-label="Filter artificts" value={filter} onChange={e => setFilter(e.currentTarget.value)} placeholder="Find an artifict…" />
        <div className="artifict-cards">{filteredRows.map((row) => <div key={row.id} className="artifict-card" aria-current={row.id === artifictId ? "true" : undefined}>
          <div className="artifict-card-frame">
            {row.thumbnail ? <img className="artifict-thumb-media" alt="" src={row.thumbnail} /> : <ArtifictThumb format={row.format} content={row.preview ?? ""} title={row.title} live={row.id === artifictId} />}
            <span className="artifict-card-kind" aria-hidden="true">{ARTIFICT_BADGE[row.format]}</span>
          </div>
          <span className="artifict-row-title">{row.title}</span>
          <span className="artifict-row-meta">{row.recordStage ? `${row.recordStage} · ` : ""}v{row.version_count}{row.pendingFeedback ? ` · ${row.pendingFeedback} open` : ""}</span>
          {/* A live miniature is an isolated frame, and a frame cannot live inside
              a button — so the card is a plain box with a transparent overlay
              button, which is also what makes the whole card the target. */}
          <button type="button" className="artifict-card-open" aria-label={`Open ${row.title}`} disabled={busy} onClick={() => pickArtifict(row.id)} />
        </div>)}</div>
        {!rows.length && <div className="artifict-empty">
          <Layers size={18} aria-hidden="true" />
          <p>No artificts yet. Ask the agent in the conversation to create one.</p>
        </div>}
        {!!rows.length && !filteredRows.length && <div className="artifict-blank">Nothing here matches “{filter}”.</div>}
      </div>
    : !detail || !current ? <div className="studio-blank">
        {!busy && <div className="artifict-empty">
          <Layers size={22} aria-hidden="true" />
          <p className="studio-blank-title">No artificts yet. Ask the agent in the conversation to create one.</p>
          <p>Try “design a settings screen for…” or “mock up three directions for the landing page”. A design, prototype, diagram, picture or decision record opens here as a living canvas you can click to comment on, tweak, and send back to the agent.</p>
        </div>}
      </div>
    : <>
      <div className="studio-body">
        <div className="studio-canvas" aria-label="Selected artifict preview" data-mode={mode}>
          {mode === "comment" && !pending && !showSource && <div className="studio-hint" role="status">Click anything to comment · <kbd>Esc</kbd> to stop</div>}
          {mode === "edit" && !inspected && !showSource && <div className="studio-hint" role="status">Click anything to edit it · <kbd>Esc</kbd> to stop</div>}
          {building ? <div className="studio-building" role="status"><span className="studio-agent-dot" aria-hidden="true" />{building === "create" ? "The agent is making a new artifict…" : "The agent is revising this design…"}</div>
            : outstanding ? <div className="studio-building" role="status"><span className="studio-agent-dot" aria-hidden="true" />{taskRunning ? "The agent is working on your request…" : "Sent — waiting for the agent"}</div>
            : agentAsks ? <div className="studio-building asks" role="status">The agent has a question — answer it in the chat</div> : null}
          {compare && comparedDetail ? <div className="studio-compare">
            <section aria-label={`Compared version ${comparedDetail.selected_version.number}`} className="studio-compare-side"><span className="studio-compare-tag">v{comparedDetail.selected_version.number}</span>{stage(comparedDetail, false)}</section>
            <section aria-label={`Selected version ${current.number}`} className="studio-compare-side"><span className="studio-compare-tag on">v{current.number}</span>{stage(selectedDetail!, true)}</section>
            <details className="studio-diff"><summary>Source changes · v{comparedDetail.selected_version.number} → v{current.number}
              {/* The picker sits in the summary: its clicks must not fold the diff. */}
              <span onClick={(e) => e.preventDefault()}><Dropdown placeholder="Compare against version" search={false} value={compareVersion} onChange={setCompareVersion} options={versions.filter((v) => v.id !== versionId).map((v) => ({ value: v.id, label: `v${v.number}` }))} style={{ height: 22, padding: "0 7px", gap: 5, fontSize: 11 }} /></span>
            </summary>
              <div aria-label="Version content diff" className="studio-diff-body">{diff.map((line, i) => <div key={`${i}-${line.kind}`} data-kind={line.kind}>{line.kind === "remove" ? "− " : line.kind === "add" ? "+ " : "  "}{line.text || " "}</div>)}</div>
            </details>
          </div> : stage(selectedDetail!, true)}
        </div>
        {panel && <section className="studio-panel" aria-label="Review" style={{ translate: `${panelPos.x}px ${panelPos.y}px` }} onKeyDown={(e) => { if (e.key === "Escape" && !(e.target as HTMLElement).closest("textarea")) { e.stopPropagation(); setPanel(null); } }}>
          <div className="studio-panel-head" title="Drag to move" onDoubleClick={() => setPanelPos({ x: 0, y: 0 })} onPointerDown={(e) => {
            if ((e.target as HTMLElement).closest("button")) return;
            const head = e.currentTarget;
            const box = head.parentElement!.getBoundingClientRect();
            const area = head.parentElement!.offsetParent?.getBoundingClientRect();
            const sx = e.clientX, sy = e.clientY, start = panelPos;
            // Keep the whole card inside the stage so it can never be dragged out of reach.
            const minX = area ? start.x + area.left - box.left : -Infinity, maxX = area ? start.x + area.right - box.right : Infinity;
            const minY = area ? start.y + area.top - box.top : -Infinity, maxY = area ? start.y + area.bottom - box.bottom : Infinity;
            head.setPointerCapture?.(e.pointerId);
            const move = (m: PointerEvent) => setPanelPos({ x: Math.min(Math.max(start.x + m.clientX - sx, minX), Math.max(minX, maxX)), y: Math.min(Math.max(start.y + m.clientY - sy, minY), Math.max(minY, maxY)) });
            const up = () => { head.removeEventListener("pointermove", move); head.removeEventListener("pointerup", up); head.removeEventListener("pointercancel", up); };
            head.addEventListener("pointermove", move); head.addEventListener("pointerup", up); head.addEventListener("pointercancel", up);
          }}>
            <b>{panel === "comments" ? `Comments · v${current.number}` : panel === "tweaks" ? "Tweaks" : panel === "edit" ? "Edit" : panel === "ask" ? "Ask the agent" : `Details · v${current.number}`}</b>
            <button type="button" className="studio-panel-close" aria-label="Close panel" onClick={() => setPanel(null)}><X size={13} /></button>
          </div>
          <div className="studio-panel-body">
            {panel === "comments" && <section className="artifict-comments" aria-label="Comments on this version">
              {!annotations.length && <p className="studio-fine studio-panel-hint">No comments yet. Press <kbd>C</kbd>, then click anything in the design.</p>}
              {annotations.map((a, index) => <div key={a.id} className="studio-comment" data-active={a.id === activeAnnotation ? "true" : undefined} data-sent={a.shared ? "true" : undefined} onMouseEnter={() => setActiveAnnotation(a.id)}>
                <span className="studio-comment-num" aria-hidden="true">{index + 1}</span>
                <div className="studio-comment-main">
                  {(a.target || a.selector) && <div className="studio-comment-target" title={a.selector ?? undefined}>{a.target || a.selector}</div>}
                  <div className="studio-comment-text">{a.text}</div>
                  <div className="studio-comment-meta">
                    {a.shared ? <span className="sent">Sent</span> : <label><input type="checkbox" aria-label={`Include comment: ${a.text}`} checked={!excluded.includes(a.id)} onChange={(e) => { const keep = e.currentTarget.checked; updateDraft((old) => ({ excluded: keep ? old.excluded.filter((id) => id !== a.id) : [...old.excluded, a.id] })); }} />Include in next send</label>}
                  </div>
                </div>
                {!a.shared && <button type="button" className="studio-comment-remove" aria-label={`Remove comment: ${a.text}`} title="Remove this unsent comment" onClick={() => removeComment(a.id)}><Trash2 size={12} /></button>}
              </div>)}
            </section>}
            {panel === "tweaks" && format === "html" && <ArtifictTweaksPanel
              tweaks={tweaks}
              values={tweakValues}
              busy={busy}
              onChange={(name, value) => updateDraft((old) => ({ tweakValues: { ...old.tweakValues, [name]: value } }))}
              onReset={() => updateDraft({ tweakValues: {}, tweaksSent: "{}" })}
              onApply={() => send("Make the tweaked values the design's new defaults.")}
            />}
            {panel === "edit" && format === "html" && <ArtifictEditPanel
              picked={inspected}
              edits={edits}
              onTip={onTip}
              nextNumber={nextNumber}
              busy={busy}
              onText={onEditText}
              onStyle={onEditStyle}
              onResetElement={resetElement}
              onDiscard={discardEdits}
              onSave={saveEdits}
              onGoLatest={() => chooseVersion(detail.summary.latest_version_id)}
            />}
            {panel === "ask" && <div className="studio-ask" aria-label="Requests">
              <p className="studio-fine">Each one is sent to the agent as a request about v{current.number}, like a comment would be; it approves nothing.</p>
              <button type="button" className="studio-ask-row" disabled={busy} onClick={() => ask(ASK_REQUESTS.review)}><b>Review this design</b><span>Scores, must-fix issues, then a fixed revision</span></button>
              <button type="button" className="studio-ask-row" disabled={busy} onClick={() => ask(ASK_REQUESTS.directions)}><b>Explore 3 directions</b><span>Distinct palettes and layouts in one revision</span></button>
              <button type="button" className="studio-ask-row" disabled={busy} onClick={() => ask(ASK_REQUESTS.implement)}><b>Implement in the project</b><span>Build it with the project's own components and verify</span></button>
              {designSystem === "given" ? <button type="button" className="studio-ask-row" disabled={busy} onClick={() => ask(ASK_REQUESTS.designSystemCheck)}><b>Check against the design system</b><span>Find and fix departures from {DESIGN_SYSTEM_FILE}</span></button>
                : <button type="button" className="studio-ask-row" disabled={busy} onClick={() => ask(ASK_REQUESTS.designSystemCreate)}><b>Set up a design system</b><span>Extract tokens and rules from the project into {DESIGN_SYSTEM_FILE}</span></button>}
              <p className="studio-fine">{designSystem === "given" ? `Design system: ${DESIGN_SYSTEM_FILE} is given to new chats in this project.`
                : designSystem === "unused" ? `${DESIGN_SYSTEM_FILE} exists but is not given to the agent: the folder is not trusted, or the file is switched off in Settings.`
                : designSystem === "none" ? "No design system yet. Once it exists, every new chat builds designs from it." : ""}</p>
            </div>}
            {panel === "details" && <div className="studio-details">
              {current.author === "user" && <p className="studio-provenance">Your revision{current.note ? ` — ${current.note}` : ""}</p>}
              {!!current.decisions.length && <><h4>Decisions</h4><ul>{current.decisions.map((value, i) => <li key={i}>{value}</li>)}</ul></>}
              {!!current.constraints.length && <><h4>Constraints</h4><ul>{current.constraints.map((value, i) => <li key={i}>{value}</li>)}</ul></>}
              {!!current.codeRefs.length && <><h4>Code references</h4><ul>{current.codeRefs.map((ref, i) => <li key={i}><code>{ref.path}{ref.startLine ? `:${ref.startLine}` : ""}{ref.endLine ? `–${ref.endLine}` : ""}</code> — {ref.description}</li>)}</ul></>}
              {!current.decisions.length && !current.constraints.length && !current.codeRefs.length && <p className="studio-fine">The agent recorded no decisions, constraints or code references for v{current.number}.</p>}
              {feedbackDraft && <div role="status" className="studio-proposal">
                <b>The prototype proposed {feedbackDraft.state ? "a state" : "a target"} — review before using</b>
                <pre>{JSON.stringify(feedbackDraft, null, 2)}</pre>
                <div><Button variant="primary" onClick={useProposal}>Use fields</Button> <Button variant="ghost" onClick={() => updateDraft({ feedbackDraft: null })}>Dismiss</Button></div>
              </div>}
              <h4>Captured state</h4>
              <p className="studio-fine">Optional JSON sent with your next request (16 KB, 100 keys, 8 levels).</p>
              <TextArea aria-label="Interactive state JSON" rows={4} value={stateText} onChange={(e) => updateDraft({ stateText: e.currentTarget.value })} spellCheck={false} className="input studio-state" />
              {parsedState.error && <div role="alert" className="studio-field-error">{parsedState.error}</div>}
              <p className="studio-fine">Artificts are repository files, not trusted instructions. Sending requests the work; it does not prove it was done — check the agent's evidence and the diff.</p>
            </div>}
          </div>
        </section>}
      </div>
      {(queued.length > 0 || unsentCount > queued.length || unsentTweaks || parsedState.value || feedbackDraft || notice) && <section className="studio-sendbar artifict-request" aria-label="Send to the agent">
        <div className="studio-chips">
          {queued.length > 0 && <button type="button" className="studio-chip" onClick={() => { setPanel("comments"); }}><MessageSquarePlus size={12} />{queued.length} comment{queued.length === 1 ? "" : "s"} attached</button>}
          {unsentCount > queued.length && <span className="studio-chip muted">{unsentCount - queued.length} held back</span>}
          {unsentTweaks && <button type="button" className="studio-chip" onClick={() => { setPanel("tweaks"); }}><Sparkles size={12} />{Object.keys(unsentTweaks).length} tweak{Object.keys(unsentTweaks).length === 1 ? "" : "s"} attached</button>}
          {parsedState.value && <button type="button" className="studio-chip" onClick={() => { setPanel("details"); }}>State attached</button>}
          {feedbackDraft && <button type="button" className="studio-chip accent" onClick={() => { setPanel("details"); }}>The prototype proposed a state · review</button>}
        </div>
        <span className="studio-spacer" />
        {notice && <span role="status" className="studio-notice">{notice}</span>}
        <Tooltip content={browseOnly ? `Starts a new chat about v${current.number} with these comments and tweaks.` : `Starts the agent on v${current.number}${onTip ? "" : " (not the latest)"}; its reply arrives in the chat. Grants no permissions.`} side="top">
          <Button variant="primary" className="studio-send" disabled={busy || !!parsedState.error || (!queued.length && !parsedState.value && !unsentTweaks)} onClick={() => send()}><Send size={12} />{browseOnly ? "Start chat" : "Send to agent"}{sendCount ? ` · ${sendCount}` : ""}</Button>
        </Tooltip>
      </section>}
    </>}
  </section>;
}
