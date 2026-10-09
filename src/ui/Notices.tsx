// In-app attention notices. The window is focused, so an OS toast would either
// be ignored or cover the chat you're reading — these slide in instead.
import { useEffect } from "react";
import { createPortal } from "react-dom";
import { Check, CircleAlert, CircleQuestionMark, CircleX, MessageCircleQuestionMark, Pause, type LucideIcon } from "lucide-react";
import type { Needs } from "../notify";
import { dismissNotice, type Notice, useStore } from "../store";
import { AgentNotice, pendingNotices } from "./AgentNotice";

/** Pure: how a notice looks and how long it lingers. Exported for tests. */
export function noticeMeta(needs: Needs): { icon: LucideIcon; color: string; cta: string; linger: number } {
  switch (needs) {
    // A blocking notice never times out: the agent is frozen until answered.
    case "question": return { icon: CircleQuestionMark, color: "#c4b5fd", cta: "Answer", linger: 0 };
    // This one does not: the run carried on, so the card is an offer with an
    // expiry, not a demand. It is placed above the blocking cards when both are
    // up, because a question the agent is waiting on outranks one it isn't.
    case "nonblocking": return { icon: MessageCircleQuestionMark, color: "#a8a29e", cta: "Answer", linger: 12000 };
    case "notice": return { icon: Check, color: "#92aa99", cta: "Open", linger: 0 };
    case "approval": return { icon: CircleAlert, color: "#e98585", cta: "Review", linger: 0 };
    case "failed": return { icon: CircleX, color: "#e98585", cta: "Open", linger: 9000 };
    case "paused": return { icon: Pause, color: "#b9a779", cta: "Open", linger: 7000 };
    case "done": return { icon: Check, color: "#92aa99", cta: "Open", linger: 6000 };
  }
}

export function NoticeCard({ n }: { n: Notice }) {
  const { icon: Icon, color, cta, linger } = noticeMeta(n.needs);
  const blocking = linger === 0;
  useEffect(() => {
    if (!linger) return;
    const t = window.setTimeout(() => dismissNotice(n.key), linger);
    return () => window.clearTimeout(t);
  }, [n.key, linger]);
  return (
    <div className={"notice" + (blocking ? " blocking" : "")} role={blocking ? "alert" : "status"} style={{ borderColor: `color-mix(in srgb, ${color} 34%, transparent)` }}>
      <span className="nico" style={{ color }}><Icon size={16} strokeWidth={1.8} aria-hidden="true" /></span>
      <div className="nbody">
        {/* The glyph is already on the left; don't repeat it in the title. */}
        <div className="ntitle">{n.title.replace(/^[✓✗⏸]\s*/, "")}</div>
        {n.body && <div className="nsub">{n.body}</div>}
      </div>
      <button className="ncta" type="button" style={{ color }} onClick={() => dismissNotice(n.key, true)}>{cta}</button>
      <button className="nx" type="button" aria-label="Dismiss" onClick={() => dismissNotice(n.key)}>×</button>
    </div>
  );
}

export function Notices() {
  const notices = useStore((s) => s.notices);
  const items = useStore((s) => s.items);
  const tasks = useStore((s) => s.tasks);
  const activeTask = useStore((s) => s.view === "session" ? s.task : null);
  // These cards are persisted items, not transient toasts: navigation, new
  // attention events, and timeouts must never acknowledge them for the user.
  const persistent = Object.entries(items).flatMap(([taskId, list]) => {
    const task = tasks[taskId];
    return !task || task.hidden || task.archived || taskId === activeTask ? []
      : pendingNotices(list).map((it) => ({ taskId, chatTitle: task.title, it }));
  });
  if (!notices.length && !persistent.length) return null;
  return createPortal(
    <div className="notices">
      {notices.map((n) => <NoticeCard key={n.key} n={n} />)}
      {persistent.map(({ taskId, chatTitle, it }) => <AgentNotice key={`${taskId}:${it.id}`} it={it} taskId={taskId} chatTitle={chatTitle} />)}
    </div>,
    document.body,
  );
}
