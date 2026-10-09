// Attention events → desktop notification (window in the background) or in-app
// notice (window focused). Both paths read the same `describe()` so the copy
// never drifts between them.
//
// The desktop toast is raised through a `ToastRaiser` the caller passes in
// rather than by calling `sendNotification` here: a clickable toast needs the
// `launch` argument naming its chat, which the plugin's desktop path has
// nowhere to put. See `notify`.
import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";

let allowed: boolean | null = null;

async function permitted(): Promise<boolean> {
  if (allowed !== null) return allowed;
  try {
    allowed = (await isPermissionGranted()) || (await requestPermission()) === "granted";
  } catch {
    allowed = false;
  }
  return allowed;
}

export type Attention = { task_id: string; kind: string; pause?: string; reason?: string; item_id?: string };

/** The states a chat can demand the user for. */
export type Needs = "question" | "approval" | "failed" | "done" | "paused" | "nonblocking" | "notice";

/** The app-owned counters a toast body is built from. Shapes are copied rather
 *  than imported so `notify` stays free of the API module's runtime deps, but
 *  only these two fields are ever read. */
export type ToastContext = { todos?: readonly { status: string }[]; subs?: readonly { status: string }[] };

/** Raises a desktop toast, told by the caller rather than asked for.
 *
 *  Passed in rather than imported from `api` for the same reason the shapes above
 *  are copied: this module stays free of the API module's runtime deps, which is
 *  what lets its decision functions be tested without a Tauri IPC bridge.
 */
export type ToastRaiser = (title: string, body: string, taskId: string) => void;

/** Fixed OS-toast bodies, keyed by attention kind. Deliberately not derived from
 *  `step`: see `notify`. */
const FIXED_BODY: Record<string, string> = {
  done: "Finished",
  failed: "The run failed",
  approval: "An action is waiting for you",
  question: "The agent is waiting for your answer",
  nonblocking: "Answer it now or leave it — the agent isn't waiting",
  notice: "The agent left a notice — no reply needed",
  paused: "Paused",
};

/** Pure: what (if anything) to say for an attention event. Exported for tests. */
export function describe(p: Attention, title: string, step: string): { title: string; body: string; needs: Needs } | null {
  const t = title.length > 60 ? title.slice(0, 59) + "…" : title;
  switch (p.kind) {
    case "done": return { title: `✓ ${t}`, body: step || "Finished", needs: "done" };
    case "failed": return { title: `✗ ${t} failed`, body: step || "The run failed", needs: "failed" };
    case "approval": return { title: `${t} needs your approval`, body: step || "An action is waiting for you", needs: "approval" };
    case "question": return { title: `${t} has a question`, body: step || "The agent is waiting for your answer", needs: "question" };
    // A question the agent kept working through. It says so in the body, because
    // a toast worded like the blocking one makes an optional question look like
    // an emergency — and the whole point of asking this way is that it isn't one.
    case "nonblocking": return { title: `${t} asked a question`, body: step || "The agent kept going — answer now or leave it", needs: "nonblocking" };
    case "notice": return { title: `${t} has an update`, body: FIXED_BODY.notice!, needs: "notice" };
    // Manual pauses/stops are yours: nothing to tell you.
    case "paused": return p.pause && p.pause !== "manual" ? { title: `⏸ ${t} paused`, body: p.reason || "Paused", needs: "paused" } : null;
    default: return null;
  }
}

/**
 * Pure: which surfaces an attention event should reach. Returns a set, because
 * the unfocused case is genuinely two surfaces: the user is elsewhere, so the
 * window's own notice is out of sight, and the OS toast alone lands in the
 * Action Center where a run that wants an answer can go unnoticed. Both is the
 * point — the toast interrupts, the card is there when you get back.
 *
 * `viewing` = this chat is the one on screen, so a notice would be redundant
 * with a card you can already see. `toasts` = the Desktop notifications switch
 * (defaults on; `false` only when settings have loaded and said so): it owns
 * the OS toast only. In-app notices are how the app reports itself to a window
 * that has lost focus, so the switch must not silence those — that was what
 * left an unfocused window with no signal at all.
 */
export function surfaces(p: Attention, title: string, step: string, focused: boolean, viewing: boolean, toasts = true): ("desktop" | "inapp")[] {
  const n = describe(p, title, step);
  if (!n) return [];
  const both: ("desktop" | "inapp")[] = [];
  // Focused: a toast would cover the chat you are reading, so in-app only, and
  // not at all if this very chat is already on screen showing a card.
  if (focused) return viewing && p.kind !== "nonblocking" ? [] : ["inapp"];
  // A nonblocking question is the one exception to `viewing`, and it is
  // deliberately *not* an exception to the toast: the run did not stop, so
  // there is no card in the transcript to come back to, which is exactly the
  // situation a toast exists for.
  if (toasts) both.push("desktop");
  if (!viewing || p.kind === "nonblocking") both.push("inapp");
  return both;
}

/** Pure: the OS-toast body. Exported for tests.
 *
 *  Deliberately built from counts, never from `step`. A toast is rendered by
 *  the OS, not the app: on Windows it lands in Action Center history and is
 *  visible from the lock screen. `step` is built from tool arguments the model
 *  chose — a file path from `read`, a shell command from `bash` — so a
 *  prompt-injected file could steer text that outlives the session and is shown
 *  outside the app entirely. A count is arithmetic on state this app owns, so
 *  there is nothing in it for a model to author.
 *
 *  `step` still reaches the in-app notice, where the user is already looking at
 *  the thing it describes and can judge where the text came from.
 */
export function toastBody(p: Attention, t: ToastContext): string {
  const fixed = FIXED_BODY[p.kind] ?? "";
  const bits: string[] = [];
  if (p.kind === "failed") {
    // Failures are the one case where the bare cause is the whole message.
    bits.push(p.reason ? p.reason : "");
  }
  const todos = t.todos ?? [];
  if (p.kind === "done" && todos.length) {
    // "5/7 done" says what the run achieved; "Finished" alone says nothing you
    // could not have inferred from the absence of further noise.
    bits.push(`${todos.filter((x) => x.status === "completed").length}/${todos.length} tasks done`);
  }
  const subs = t.subs ?? [];
  if (subs.length) {
    const running = subs.filter((s) => s.status === "running").length;
    const failed = subs.filter((s) => s.status === "failed").length;
    const parts: string[] = [];
    if (running) parts.push(`${running} subagent${running === 1 ? "" : "s"} running`);
    if (failed) parts.push(`${failed} failed`);
    if (parts.length) bits.push(parts.join(", "));
  }
  const tail = bits.filter(Boolean).join(" · ");
  return tail ? `${fixed} — ${tail}` : fixed;
}

/** Desktop toast, for when the window isn't focused (hidden to tray, minimised, another app on top).
 *
 *  The toast is raised by `raise` rather than by `sendNotification` because the
 *  plugin's toast cannot be clicked: it builds its XML from fixed `text1`/`text2`
 *  fields with nowhere to put the `launch` argument naming the chat, and the
 *  backend that can carry that argument also owns the COM activator that turns
 *  the click back into a navigation — see `win::install`. */
export async function notify(p: Attention, title: string, step: string, t: ToastContext = {}, raise?: ToastRaiser) {
  const n = describe(p, title, step);
  if (!n || !raise || !(await permitted())) return;
  try {
    raise(n.title, toastBody(p, t), p.task_id);
  } catch {
    /* notifications unavailable: the in-app badge still shows it */
  }
}
