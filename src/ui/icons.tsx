import {
  Archive, ArrowLeft, ArrowRight, ArrowUp, BarChart3, Bot, Check, ChevronDown, ChevronRight, Circle,
  CircleAlert, CircleX, Clock3, Copy, Download, Folder, GitBranch, Grid2X2, KeyRound, LayoutPanelLeft, ListChecks, Bookmark,
  Eye, EyeOff, Monitor, Network, PanelRight, Pause, PencilLine, Pin, Play, Plus, RotateCcw,
  Search, Settings, Shield, Sparkles, Square, SquarePen, Trash2, X, Paperclip,
} from "lucide-react";
import { Loader } from "./primitives";

const icon = (C: typeof Search, size = 14, className?: string) => <C aria-hidden="true" className={className} size={size} strokeWidth={1.7} />;

export const I = {
  sidebar: (w = 15) => icon(LayoutPanelLeft, w),
  panel: (w = 14) => icon(PanelRight, w),
  back: () => icon(ArrowLeft),
  fwd: () => icon(ArrowRight),
  search: (w = 12) => icon(Search, w),
  newTask: () => icon(SquarePen),
  grid: () => icon(Grid2X2),
  gear: (w = 14) => icon(Settings, w),
  plus: (w = 14) => icon(Plus, w),
  paperclip: (w = 14) => icon(Paperclip, w),
  up: (w = 13) => icon(ArrowUp, w),
  chev: (c = "var(--mut3)") => <ChevronDown aria-hidden="true" color={c} size={10} strokeWidth={2} />,
  chevR: () => icon(ChevronRight, 10),
  folder: () => icon(Folder, 13),
  monitor: () => icon(Monitor, 13),
  branch: () => icon(GitBranch, 13),
  clock: () => icon(Clock3),
  rewind: () => icon(RotateCcw, 12),
  agents: (w = 13) => icon(Bot, w),
  pause: (w = 12) => icon(Pause, w),
  play: (w = 12) => icon(Play, w),
  key: (w = 13) => icon(KeyRound, w),
  pin: (w = 12) => icon(Pin, w),
  archive: (w = 12) => icon(Archive, w),
  bookmark: (w = 13) => icon(Bookmark, w),
  edit: (w = 12) => icon(PencilLine, w),
  copy: (w = 12) => icon(Copy, w),
  download: (w = 12) => icon(Download, w),
  // Bar chart, not a gauge or a pie: the stats surfaces are all token/cost bars,
  // and an icon that matches its own screen is one fewer thing to decode.
  chart: (w = 12) => icon(BarChart3, w),
  // Reveal/hide pair for values shown on demand (the account email). Two
  // distinct glyphs rather than one rotated: the state has to be readable
  // without reading the label next to it.
  eye: (w = 12) => icon(Eye, w),
  eyeOff: (w = 12) => icon(EyeOff, w),
  trash: (w = 12) => icon(Trash2, w),
  close: (w = 12) => icon(X, w),
  shield: (w = 12) => icon(Shield, w),
  sparkles: (w = 12) => icon(Sparkles, w),
  check: (w = 12) => icon(ListChecks, w),
  route: (w = 12) => icon(Network, w),
  logo: () => (
    <svg viewBox="120 80 460 460" fill="none" strokeLinecap="round" strokeLinejoin="round" className="bglogo">
      <defs>
        <linearGradient id="olLogoFade" x1="0" y1="0" x2="0" y2="1"><stop offset="0%" stopColor="#fff" stopOpacity="1" /><stop offset="100%" stopColor="#fff" stopOpacity="0" /></linearGradient>
        <mask id="olLogoMask"><rect x="120" y="80" width="460" height="460" fill="url(#olLogoFade)" /></mask>
      </defs>
      <g stroke="#2a2a2e" strokeWidth="5" mask="url(#olLogoMask)">
        <path vectorEffect="non-scaling-stroke" d="M372 110 L192 476 Q180 502 206 490 C290 440 362 430 440 486 Q470 506 458 476 L418 400" />
        <path vectorEffect="non-scaling-stroke" d="M162 286 L498 410 Q532 422 542 392 Q548 372 534 350 L372 110" />
        <path vectorEffect="non-scaling-stroke" d="M286 126 L320 190" />
      </g>
    </svg>
  ),
};

export const ST: Record<string, { dot: string; text: string }> = {
  running: { dot: "var(--st-run)", text: "Running" },
  waiting: { dot: "var(--st-wait)", text: "Needs you" },
  idle: { dot: "var(--st-idle)", text: "Idle" },
  done: { dot: "var(--st-done)", text: "Done" },
  stopped: { dot: "var(--st-stop)", text: "Stopped" },
  failed: { dot: "var(--st-fail)", text: "Failed" },
  paused: { dot: "var(--st-pause)", text: "Paused" },
};

/** The status a row shows, with `paused` winning over the task's own status:
 *  a chat you froze is paused however the backend happens to label it.
 *
 *  The two returns come from named constants rather than `ST.paused` / `ST.idle`:
 *  `ST` is a `Record<string, …>`, so the type-checker reads every element access
 *  on it — including the literal keys — as possibly missing, and the result
 *  would stay `T | undefined` however the fallback is spelled. */
const IDLE = { dot: "var(--st-idle)", text: "Idle" };
const PAUSED = { dot: "var(--st-pause)", text: "Paused" };
export const statusOf = (status: string, paused: boolean) => (paused ? PAUSED : ST[status] ?? IDLE);

/**
 * The glyph for work that is not moving because the chat is frozen: a solid
 * squircle in the pause colour, still, where the other statuses animate.
 *
 * A spinner next to a frozen agent is a promise the app cannot keep — the trace
 * is drawn by an animation and nothing is running to drive it, so it reads as
 * "working" to anyone who opens the chat and looks for a second. A still shape
 * in the one colour the app uses for a freeze says the same thing without
 * animating, and it lands in the same slot and at the same size as the spinner
 * it replaces, so nothing shifts.
 */
export function PausedMark({ size = 13 }: { size?: number }) {
  return (
    <svg aria-hidden="true" width={size} height={size} viewBox="0 0 14 14" style={{ display: "block", flex: "none" }}>
      {/* A superellipse |x/a|^n + |y/a|^n = 1 sampled at n=4, filling the 1..13
          box exactly — the continuous corners are the whole point of the shape, and
          a plain rounded rect reads as a circle at this size. */}
      <path
        fill="var(--st-pause)"
        d="M13.00,7.00 L13.00,7.59 L13.00,8.19 L12.99,8.82 L12.96,9.47 L12.88,10.14 L12.73,10.83 L12.46,11.48 L12.05,12.05 L11.48,12.46 L10.83,12.73 L10.14,12.88 L9.47,12.96 L8.82,12.99 L8.19,13.00 L7.59,13.00 L7.00,13.00 L6.41,13.00 L5.77,12.99 L5.18,12.96 L4.53,12.88 L3.86,12.73 L3.27,12.46 L2.95,12.05 L2.52,11.48 L2.27,10.83 L2.12,10.14 L2.04,9.47 L2.01,8.82 L2.00,8.19 L2.00,7.59 L2.00,7.00 L2.00,6.41 L2.00,5.77 L2.01,5.18 L2.04,4.53 L2.12,3.86 L2.27,3.27 L2.52,2.52 L2.95,1.95 L3.27,1.54 L3.86,1.27 L4.53,1.12 L5.18,1.04 L5.77,1.01 L6.41,1.00 L7.00,1.00 L7.59,1.00 L8.19,1.00 L8.82,1.01 L9.47,1.04 L10.14,1.12 L10.83,1.27 L11.48,1.54 L12.05,1.95 L12.46,2.27 L12.73,2.95 L12.88,3.86 L12.96,4.53 L12.99,5.18 L13.00,5.77 L13.00,6.41 Z"
      />
    </svg>
  );
}

export function StatusIcon({ status, size = 13, color }: { status: string; size?: number; color?: string }) {
  const st = statusOf(status, false);
  if (status === "running") return <Loader variant="agent" size={size} color={color} />;
  if (status === "paused") return <PausedMark size={size} />;
  const C = status === "waiting" ? CircleAlert : status === "done" ? Check : status === "stopped" ? Square : status === "failed" ? CircleX : Circle;
  return <C aria-hidden="true" style={{ color: st.dot }} size={size} strokeWidth={1.8} />;
}
