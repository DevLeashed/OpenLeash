import { useEffect, useRef, useState } from "react";
import { ArrowUpRight, CircleAlert, CircleQuestionMark, Hand, type LucideIcon } from "lucide-react";
import { ago, api, oneLine, type Item, type TaskSummary } from "../api";
import { get, go, useNow, useStore } from "../store";
import { IconButton, Modal, Pressable } from "./primitives";
import { StatusIcon, statusOf } from "./icons";

const needsYou = (task: TaskSummary) => task.waiting_kind === "approval" || task.waiting_kind === "question";

function pendingItem(items: Item[], kind: "approval" | "question"): Item | undefined {
  return [...items].reverse().find((item) => item.kind === kind && (kind === "approval"
    ? !item.data?.resolved
    : !item.data?.answers && !item.data?.dismissed));
}

function preview(item: Item, kind: "approval" | "question"): string {
  const data = item.data ?? {};
  if (kind === "question") {
    const first = Array.isArray(data.questions) ? data.questions[0] : undefined;
    return oneLine(first?.question || first?.header || data.intro || data.title || item.text || "A question is waiting for your answer", 150);
  }
  const text = data.kind === "command"
    ? data.detail || data.title || data.reason || item.text
    : data.title || data.reason || data.detail || item.text;
  return oneLine(text || "An approval is waiting for your review", 150);
}

interface PendingPreview { itemId?: string; text: string; ts?: string; loaded: boolean }

function previewFrom(items: Item[], kind: "approval" | "question"): PendingPreview | null {
  const item = pendingItem(items, kind);
  return item ? { itemId: item.id, text: preview(item, kind), ts: item.ts, loaded: true } : null;
}

/** Focus and bring the still-pending decision into view after `go` has loaded its chat. */
function focusPending(taskId: string, kind: "approval" | "question", itemId?: string) {
  const attr = kind === "approval" ? "data-pending-approval" : "data-pending-question";
  const started = Date.now();
  const seek = () => {
    const state = get();
    if (state.view !== "session" || state.task !== taskId) return;
    const candidates = document.querySelectorAll<HTMLElement>(`[${attr}]`);
    const target = [...candidates].find((el) => itemId ? el.getAttribute(attr) === itemId : true);
    if (target) {
      // Question cards already accept focus; approval cards are informational
      // until the user chooses one of the controls in the existing decision UI.
      if (!target.hasAttribute("tabindex")) target.tabIndex = -1;
      target.focus({ preventScroll: true });
      target.scrollIntoView?.({ behavior: "smooth", block: "center" });
      return;
    }
    if (Date.now() - started < 8000) window.setTimeout(seek, 60);
  };
  window.setTimeout(seek, 0);
}

/** Cross-chat inbox for the existing, blocking decisions. It never answers for the user. */
export function NeedsYou() {
  const tasks = useStore((state) => state.tasks);
  const waiting = Object.values(tasks).filter(needsYou); // Filtering is stable: keep the existing chat/API order.
  const waitingKey = waiting.map((task) => `${task.id}:${task.waiting_kind}`).join("|");
  const [open, setOpen] = useState(false);
  const [previews, setPreviews] = useState<Record<string, PendingPreview>>({});
  const generation = useRef(0);

  useEffect(() => {
    if (!open) return;
    const run = ++generation.current;
    const current = Object.values(get().tasks).filter(needsYou);
    setPreviews((old) => {
      const keep = new Set(current.map((task) => task.id));
      return Object.fromEntries(Object.entries(old).filter(([id]) => keep.has(id)));
    });
    for (const task of current) {
      const kind = task.waiting_kind;
      if (kind !== "approval" && kind !== "question") continue;
      // The pending preview is transcript data; fetch it only while the human has
      // opened this inbox, not for every chat at app startup.
      setPreviews((old) => ({ ...old, [task.id]: { text: "Loading pending decision…", loaded: false } }));
      void api.task(task.id).then(({ items }) => {
        if (generation.current !== run) return;
        const result = previewFrom(items, kind) ?? { text: "Pending card will be visible in the chat", loaded: true };
        setPreviews((old) => ({ ...old, [task.id]: result }));
      }).catch(() => {
        if (generation.current !== run) return;
        setPreviews((old) => ({ ...old, [task.id]: { text: "Open the chat to inspect its pending card", loaded: true } }));
      });
    }
    return () => { if (generation.current === run) generation.current++; };
  }, [open, waitingKey]);

  const close = () => { generation.current++; setOpen(false); };
  const show = () => { setPreviews({}); setOpen(true); };

  return (
    <>
      <Pressable
        className={`srow needs-you-entry${open ? " on" : ""}${waiting.length > 0 ? " has-pending" : ""}`}
        aria-label={`Needs you inbox · ${waiting.length} pending`}
        aria-expanded={open}
        aria-controls="needs-you-list"
        onClick={show}
      >
        <Hand size={14} aria-hidden="true" strokeWidth={1.8} />
        <span className="t">Needs you</span>
        <span className="needs-you-count" aria-label={`${waiting.length} pending`}>{waiting.length}</span>
      </Pressable>
      {open && (
        <NeedsYouDialog waiting={waiting} previews={previews} onClose={close} onOpen={(task, item) => {
          close();
          go("session", { task: task.id });
          focusPending(task.id, task.waiting_kind as "approval" | "question", item?.itemId);
        }} />
      )}
    </>
  );
}

function NeedsYouDialog({
  waiting, previews, onClose, onOpen,
}: {
  waiting: TaskSummary[];
  previews: Record<string, PendingPreview>;
  onClose: () => void;
  onOpen: (task: TaskSummary, item?: PendingPreview) => void;
}) {
  useNow();
  return (
    <Modal className="modal-needs-you" ariaLabel="Needs you inbox" onClose={onClose}>
      <div className="mh">
        <Hand aria-hidden="true" size={17} color="var(--st-wait)" />
        <span style={{ flex: 1 }}>Needs you <span style={{ color: "var(--mut3)", fontWeight: 400 }}>· {waiting.length}</span></span>
        <IconButton label="Close Needs you inbox" onClick={onClose}><span aria-hidden="true">×</span></IconButton>
      </div>
      <div className="needs-you-note">Nothing is approved or answered here. Open a chat to review its existing decision card and choose what happens next.</div>
      <div className="mb needs-you-body" id="needs-you-list" aria-label="Chats waiting for your decision">
        {waiting.length ? (
          <div className="needs-you-list" role="list">
            {waiting.map((task) => {
              const kind = task.waiting_kind as "approval" | "question";
              const info = previews[task.id];
              const KindIcon: LucideIcon = kind === "question" ? CircleQuestionMark : CircleAlert;
              const status = statusOf(task.status, !!task.paused);
              const validTs = info?.ts && Number.isFinite(Date.parse(info.ts));
              return (
                <div key={task.id} className="needs-you-item" role="listitem">
                <button
                  type="button"
                  className="needs-you-row"
                  data-needs-you-task={task.id}
                  aria-label={`Open ${task.title || "Untitled chat"} · ${kind}`}
                  onClick={() => onOpen(task, info)}
                >
                  <span className="needs-you-row-icon"><KindIcon size={15} aria-hidden="true" /></span>
                  <span className="needs-you-row-main">
                    <span className="needs-you-title">{task.title || "Untitled chat"}</span>
                    <span className="needs-you-preview">{info?.text ?? (kind === "question" ? "Waiting for your answer" : "Waiting for your approval")}</span>
                    <span className="needs-you-meta">
                      <span className="needs-you-status"><StatusIcon status={task.status} size={11} color={status.dot} />{status.text}</span>
                      <span className={`needs-you-kind ${kind}`}>{kind === "question" ? "Question" : "Approval"}</span>
                      {validTs && <span className="needs-you-duration">Waiting {ago(info.ts!)}</span>}
                    </span>
                  </span>
                  <ArrowUpRight className="needs-you-open" aria-hidden="true" size={14} />
                </button>
                </div>
              );
            })}
          </div>
        ) : (
          <div className="needs-you-empty"><Hand size={20} aria-hidden="true" /><strong>No decisions pending</strong><span>Chats waiting for a question or approval will appear here.</span></div>
        )}
      </div>
    </Modal>
  );
}
