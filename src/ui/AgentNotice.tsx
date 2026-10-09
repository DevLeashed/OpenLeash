// Durable informational cards, distinct from questions and transient attention toasts.
import { useState } from "react";
import { CircleAlert, Info, X } from "lucide-react";
import { api, type Item } from "../api";
import { flash, go, useStore } from "../store";
import { Button } from "./primitives";

export function pendingNotices(items: Item[] | undefined): Item[] {
  return (items ?? []).filter((it) => it.kind === "user_notice" && !it.data?.dismissed);
}

export function AgentNotice({ it, taskId, chatTitle }: { it: Item; taskId: string; chatTitle?: string }) {
  const [dismissing, setDismissing] = useState(false);
  const warning = it.data?.level === "warning";
  const Icon = warning ? CircleAlert : Info;
  const dismiss = async () => {
    if (dismissing) return;
    setDismissing(true);
    try {
      await api.dismissAgentNotice(taskId, it.id);
      // The backend publishes the updated item; only that persisted state removes the card.
    } catch (e) {
      setDismissing(false);
      flash(`Couldn't dismiss notice · ${String(e)}`);
    }
  };
  return (
    <section className={"agent-notice" + (warning ? " warning" : "")} role="status" aria-label={warning ? "Agent warning" : "Agent notice"}>
      <div className="agent-notice-head">
        <Icon size={16} strokeWidth={1.8} aria-hidden="true" />
        <div className="agent-notice-heading">
          <div className="agent-notice-title">{it.data?.title || "Agent notice"}</div>
          <div className="agent-notice-hint">{warning ? "Warning · " : ""}No reply needed · the agent isn't waiting</div>
        </div>
        <Button variant="ghost" disabled={dismissing} onClick={() => void dismiss()} aria-label="Dismiss notice"><X size={13} /> Dismiss</Button>
      </div>
      <div className="agent-notice-message">{it.text}</div>
      {chatTitle && <Button variant="ghost" className="agent-notice-chat" onClick={() => go("session", { task: taskId })}>Open chat: {chatTitle}</Button>}
    </section>
  );
}

export function AgentNoticeList({ taskId }: { taskId: string }) {
  const items = useStore((s) => s.items[taskId]);
  const excluded = useStore((s) => !!s.tasks[taskId]?.hidden || !!s.tasks[taskId]?.archived);
  const pending = pendingNotices(items);
  if (excluded || !pending.length) return null;
  return <div className="agent-notice-stack">{pending.map((it) => <AgentNotice key={it.id} it={it} taskId={taskId} />)}</div>;
}
