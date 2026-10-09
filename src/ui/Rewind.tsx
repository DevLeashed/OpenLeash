import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, CheckpointInfo, RewindInfo, RewindResult } from "../api";
import { flash, persistDraft, set } from "../store";
import { Button, ChoiceRow, IconButton, Kbd, Loader, Modal, Pressable, Tooltip } from "./primitives";
import { I } from "./icons";
import { CircleAlert, GitFork } from "lucide-react";

/** What a rewind puts back. These three strings are the whole `mode` vocabulary
 *  of `task_rewind_to`, so the picker never invents a fourth. */
type Mode = "both" | "conversation" | "files";

/** In the order the dialog offers them, default first. */
const MODES: { id: Mode; name: string; hint: string }[] = [
  { id: "both", name: "Restore files & conversation", hint: "The code goes back to how it was here, and this prompt goes back in the box" },
  { id: "conversation", name: "Conversation only", hint: "The chat is cut back, and the files are left exactly as they are now" },
  { id: "files", name: "Files only", hint: "The code goes back, and the chat is left as it is now" },
];

/** Rows of one checkpoint's file list mounted at once. Bounded for the same
 *  reason the review view bounds its own: a big refactor can touch hundreds of
 *  files, and only a handful are on screen. */
const FILE_CAP = 100;

/** Unicode bidi overrides let a filename lie about its own name: `evil\u202Efdp.exe`
 *  renders as `evilepx.d` — a trojan renaming itself. This dialog is where the
 *  user decides *not* to restore a file, so a path that renders as someone else's
 *  is the mistake worth guarding. Copied from Review.tsx rather than imported:
 *  that file is not this one's to depend on, and the helper is five lines. The
 *  character classes are `\u` escapes, so `no-control-regex` has nothing to fire on. */
function safeName(s: string): string {
  return s.replace(/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g, (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, "0")}`);
}

/**
 * The rewind picker: every checkpoint the backend kept for this chat, with the
 * three-way choice of what to put back.
 *
 * Opened either from a message's own rewind button (`itemId` selects that
 * message's checkpoint) or without one, in which case the newest is selected.
 * A rewind is destructive — it drops everything after the checkpoint and can
 * overwrite files — so the two things that would otherwise be silent are said
 * out loud here: which file snapshots do not exist, and which files the backend
 * refused to touch because the user had edited them since.
 */
export function Rewind({ taskId, itemId, onClose }: { taskId: string; itemId?: string; onClose: () => void }) {
  const [info, setInfo] = useState<RewindInfo | null>(null);
  const [err, setErr] = useState("");
  const [sel, setSel] = useState<string | null>(itemId ?? null);
  const [open, setOpen] = useState<string | null>(null);
  const [mode, setMode] = useState<Mode>("both");
  const [busy, setBusy] = useState<"" | "rewind" | "fork">("");
  const [help, setHelp] = useState(false);
  const [result, setResult] = useState<RewindResult | null>(null);
  const listBox = useRef<HTMLDivElement>(null);

  const load = useCallback(() => {
    setErr("");
    api.rewindInfo(taskId)
      .then((r) => {
        setInfo(r);
        // Keep whatever was asked for if it is still a real checkpoint: opening
        // this from a message button means that message, and falling back to
        // "whatever is newest" would silently rewind the wrong place.
        setSel((cur) => {
          if (cur && r.checkpoints.some((c) => c.item_id === cur)) return cur;
          return r.checkpoints.find((c) => c.current)?.item_id ?? r.checkpoints[r.checkpoints.length - 1]?.item_id ?? null;
        });
      })
      .catch((e) => setErr(String(e)));
  }, [taskId]);
  useEffect(load, [load]);

  // Newest first: the thing you most likely want to undo is the last thing that
  // happened, and the newest checkpoint is also where the current one is marked.
  const list = useMemo(() => (info ? [...info.checkpoints].reverse() : []), [info]);
  const idx = Math.max(0, list.findIndex((c) => c.item_id === sel));
  const cur = list[idx] ?? null;
  const filesOk = !!cur?.has_files;
  // A checkpoint with no snapshot cannot have its files restored whatever the
  // radio says, so the mode is read through this rather than trusted: the choice
  // is not even offered, and a stale "both" from another row can't leak in.
  const effMode: Mode = filesOk ? mode : "conversation";
  const label = effMode === "both" ? "Rewind" : effMode === "files" ? "Restore files" : "Rewind conversation";

  // Land focus on the list, not on the close button. Modal focuses the first
  // focusable thing it finds, which here is the X — and Enter on a focused
  // button activates it, so the dialog would open with Enter wired to "dismiss".
  // The list's own Enter means the primary action, which is the binding the
  // footer advertises. It is a real Tab stop (not `tabIndex={-1}`) on purpose:
  // Modal's trap only wraps Tab at the first and last stop of the panel, so a
  // focus target outside that set is one Shift+Tab away from walking behind the
  // scrim. Once only: re-focusing on every selection would yank focus back off a
  // radio the user just clicked, mid-Tab.
  const landed = useRef(false);
  useEffect(() => {
    if (!landed.current && info) { landed.current = true; listBox.current?.focus(); }
  }, [info]);

  const pick = (c: CheckpointInfo) => {
    setSel(c.item_id);
    setMode("both");
  };

  const confirm = async () => {
    if (!cur || busy) return;
    setBusy("rewind");
    setErr("");
    try {
      const r = await api.rewindTo(taskId, cur.item_id, effMode);
      const t = await api.task(taskId);
      // The same store write the old instant rewind did. The prompt goes back in
      // the box only when the conversation was actually rewound — a files-only
      // rewind left the chat where it was, and writing the (empty) returned text
      // into the draft would clear what the user had typed.
      const back = effMode !== "files" ? r.text : null;
      set((s) => ({
        items: { ...s.items, [taskId]: t.items },
        tasks: { ...s.tasks, [taskId]: t.summary },
        ...(back !== null ? { sessionDrafts: { ...s.sessionDrafts, [taskId]: back } } : {}),
      }));
      if (back !== null) persistDraft(taskId, back);
      if (r.skipped.length) {
        // The full report stays in the dialog, where the list belongs. But this
        // dialog is usually opened from a message row, and a conversation rewind
        // drops that very message — unmounting the row and the report with it
        // before anyone could read it. So the count also goes to a toast, which
        // is owned by the app rather than by the message being rewound away.
        setResult(r);
        flash(`Rewind skipped ${r.skipped.length} file${r.skipped.length === 1 ? "" : "s"} you had edited — ${r.skipped.length === 1 ? "it is" : "they are"} left as they are`);
        return;
      }
      const files = effMode === "conversation" ? "" : `Restored ${r.restored} file${r.restored === 1 ? "" : "s"}`;
      const conv = effMode === "files" ? "" : "the prompt is back in the box";
      flash([files, conv].filter(Boolean).join(" · ") || "Rewound");
      onClose();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy("");
    }
  };

  const fork = async () => {
    if (!cur || busy) return;
    setBusy("fork");
    setErr("");
    try {
      const f = await api.forkAt(taskId, cur.item_id);
      // Nothing else will put the new chat in the sidebar until its first event
      // arrives, and a fork made here is one the user expects to see.
      set((s) => ({ tasks: { ...s.tasks, [f.id]: f } }));
      flash(`Forked into a new worktree${f.title ? ` · ${f.title}` : ""}`);
      onClose();
    } catch (e) {
      setErr(String(e));
      setBusy("");
    }
  };

  // Arrows walk the checkpoints; Enter takes the primary action. Modal owns Esc
  // (and the focus trap), so cancel is not repeated here.
  const keys = (e: React.KeyboardEvent) => {
    // The skip report is a different screen: there is no list to walk and
    // nothing to confirm, and leaving Enter wired up would re-run the rewind the
    // report is describing.
    if (result) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      if (!list.length) return;
      e.preventDefault();
      const step = e.key === "ArrowDown" ? 1 : -1;
      const next = list[(idx + step + list.length) % list.length];
      if (next) pick(next);
      return;
    }
    if (e.key !== "Enter" || e.defaultPrevented) return;
    // A focused button and a `Pressable` row both mean their own Enter first —
    // either would otherwise rewind while the user was doing something else.
    if ((e.target as HTMLElement).closest("button")) return;
    e.preventDefault();
    void confirm();
  };

  return (
    <Modal onClose={onClose} style={{ width: "min(600px, 100%)" }}>
      {/* The dialog catches the arrows/Enter it binds from one wrapper, so a key
          on a row and a key on the panel are handled in the same place. */}
      <div onKeyDown={keys} style={{ display: "contents" }}>
      <div className="mh">
        <span style={{ display: "flex", color: "var(--mut2)" }}>{I.rewind()}</span>
        <span style={{ flex: 1, minWidth: 0 }}>Rewind</span>
        <IconButton label="Close" onClick={onClose}>{I.close()}</IconButton>
      </div>
      {result ? (
        <div className="mb" style={{ gap: 10 }} role="status">
          <div style={{ display: "flex", gap: 8, alignItems: "flex-start", color: "#fde68a", fontWeight: 600 }}>
            <CircleAlert aria-hidden="true" size={14} style={{ marginTop: 2, flex: "none" }} />
            <span>Restored the code, but skipped {result.skipped.length} file{result.skipped.length === 1 ? "" : "s"} — you had edited them yourself.</span>
          </div>
          <div style={{ fontSize: 11.5, color: "var(--hint)", lineHeight: 1.5 }}>
            Rewinding stops at a file you have changed since the checkpoint: putting it back would throw that edit away, so it is left exactly as it is. {result.restored > 0 ? `${result.restored} other file${result.restored === 1 ? "" : "s"} went back.` : "Nothing else needed changing."}
          </div>
          <div className="sgroup" style={{ maxHeight: 240, overflowY: "auto" }}>
            {result.skipped.map((p) => (
              <div key={p} className="srowx" style={{ minHeight: 34, padding: "6px 12px" }}>
                <span className="mono" style={{ fontSize: 11.5, color: "var(--fg2)", minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{safeName(p)}</span>
              </div>
            ))}
          </div>
          {result.text && <div style={{ fontSize: 11.5, color: "var(--hint)" }}>The prompt is back in the input box.</div>}
          <div style={{ display: "flex", gap: 8 }}>
            <Button variant="ghost" onClick={() => { setResult(null); load(); }}>Back to checkpoints</Button>
            <div style={{ flex: 1 }} />
            <Button variant="primary" onClick={onClose}>Done</Button>
          </div>
        </div>
      ) : err && !info ? (
        <div className="mb" role="alert">
          <div style={{ color: "#ff8a8a" }}>Couldn't read the checkpoints · {err}</div>
          <div style={{ display: "flex", gap: 8 }}>
            <Button onClick={load}>Try again</Button>
            <Button variant="ghost" onClick={onClose}>Cancel</Button>
          </div>
        </div>
      ) : !info ? (
        <div className="mb"><Loader className="loading-state" size={16} label="Reading checkpoints…" /></div>
      ) : !list.length ? (
        <div className="mb">
          <div className="empty">No checkpoints for this chat yet. One is taken per message you send.</div>
          <div style={{ display: "flex" }}><div style={{ flex: 1 }} /><Button variant="ghost" onClick={onClose}>Cancel</Button></div>
        </div>
      ) : (
        <>
          <div ref={listBox} tabIndex={0} className="mb" style={{ gap: 8, outline: "none" }}>
            <div style={{ fontSize: 11.5, color: "var(--hint)", lineHeight: 1.5 }}>
              Go back to the state just before a message: its prompt returns to the box and everything after it is dropped from the chat. The file options put the code back to how it was at that point.
            </div>
            {list.map((c) => {
              const on = c.item_id === sel;
              const add = c.files.reduce((n, f) => n + f.add, 0);
              const del = c.files.reduce((n, f) => n + f.del, 0);
              const shown = open === c.item_id;
              return (
                <Pressable key={c.item_id} className={"opt" + (on ? " on" : "")} onClick={() => pick(c)}>
                  <span aria-hidden="true" style={{ marginTop: 3, width: 9, height: 9, flex: "none", borderRadius: "50%", border: `1.5px solid ${on ? "var(--violet)" : "var(--ov-140)"}`, background: on ? "var(--violet)" : "transparent" }} />
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ display: "flex", alignItems: "baseline", gap: 8, minWidth: 0 }}>
                      <span className="mono" style={{ fontSize: 11, color: "var(--mut3)", minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{c.label}</span>
                      {c.current && <span className="bgtag">current</span>}
                    </div>
                    <div title={c.text} style={{ marginTop: 2, color: "var(--fg2)", fontWeight: 500, display: "-webkit-box", WebkitLineClamp: 2, WebkitBoxOrient: "vertical", overflow: "hidden" }}>
                      {c.text.trim() || "(empty prompt)"}
                    </div>
                    <div style={{ marginTop: 4, fontSize: 11, color: "var(--mut3)", display: "flex", gap: 8 }}>
                      {c.files.length > 0 ? (
                        <>
                          <span>{c.files.length} file{c.files.length === 1 ? "" : "s"}</span>
                          <span className="mono" style={{ color: "var(--diff-add)" }}>+{add}</span>
                          <span className="mono" style={{ color: "var(--diff-del)" }}>−{del}</span>
                        </>
                      ) : <span>{c.has_files ? "no changes" : "no file snapshot"}</span>}
                    </div>
                    {shown && (
                      <div style={{ marginTop: 8, borderTop: "1px solid var(--ov-50)", paddingTop: 6, display: "flex", flexDirection: "column", gap: 3 }}>
                        {c.files.slice(0, FILE_CAP).map((f) => (
                          <div key={f.path} style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 11.5 }}>
                            <span className="mono" style={{ width: 12, flex: "none", fontSize: 10.5, fontWeight: 600, color: f.status === "A" ? "var(--diff-add)" : f.status === "D" ? "var(--diff-del)" : "var(--st-pause)" }}>{f.status}</span>
                            <Tooltip content={safeName(f.path)}>
                              <span className="mono" style={{ flex: 1, minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", direction: "rtl", textAlign: "left", color: "var(--fg2)" }}>{safeName(f.path)}</span>
                            </Tooltip>
                            <span className="mono" style={{ fontSize: 10, color: "var(--diff-add)" }}>+{f.add}</span>
                            <span className="mono" style={{ fontSize: 10, color: "var(--diff-del)" }}>−{f.del}</span>
                          </div>
                        ))}
                        {c.files.length > FILE_CAP && <div style={{ fontSize: 11, color: "var(--dim)" }}>Showing the first {FILE_CAP} of {c.files.length} changed files</div>}
                      </div>
                    )}
                  </div>
                  {/* The fold is a control of its own: expanding a row must not
                      also count as choosing it. */}
                  {c.files.length > 0 && (
                    <IconButton label={shown ? "Hide the files in this checkpoint" : "Show the files in this checkpoint"} style={{ width: 22, height: 22, transform: `rotate(${shown ? 90 : 0}deg)` }}
                      onClick={(e) => { e.stopPropagation(); setOpen(shown ? null : c.item_id); }}>{I.chevR()}</IconButton>
                  )}
                </Pressable>
              );
            })}
          </div>
          <div className="mf" style={{ flexDirection: "column", alignItems: "stretch", gap: 8 }}>
            {cur && (
              <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                {MODES.map((m) => {
                  const off = m.id !== "conversation" && !filesOk;
                  return (
                    <ChoiceRow key={m.id} variant="radio" selected={!off && effMode === m.id} style={off ? { opacity: 0.45, pointerEvents: "none" } : undefined}
                      onClick={() => { if (!off) setMode(m.id); }}>
                      <div style={{ flex: 1, minWidth: 0 }}>
                        <div style={{ fontWeight: 500 }}>{m.name}</div>
                        <div className="desc" style={{ fontSize: 11.5, marginTop: 2 }}>{m.hint}</div>
                      </div>
                    </ChoiceRow>
                  );
                })}
              </div>
            )}
            {cur && !filesOk && (
              <div style={{ fontSize: 11.5, color: "var(--hint)", lineHeight: 1.5 }}>
                This checkpoint has no file snapshot — nothing had been edited yet when it was taken, so there is no code to put back. Only the conversation can be rewound here.
              </div>
            )}
            {cur?.current && filesOk && <div style={{ fontSize: 11.5, color: "var(--hint)" }}>This is the newest checkpoint, so its files are already in that state — restoring them changes nothing.</div>}
            <Pressable className="tg-line" aria-expanded={help} onClick={() => setHelp(!help)} style={{ padding: "2px 0" }}>
              <span style={{ display: "flex", transform: `rotate(${help ? 90 : 0}deg)` }}>{I.chevR()}</span>
              <span className="tg-text" style={{ fontWeight: 500 }}>What can't be rewound</span>
            </Pressable>
            <div className="fold" style={{ gridTemplateRows: help ? "1fr" : "0fr" }}>
              <div>
                {help && (
                  <ul style={{ margin: "2px 0 4px", paddingLeft: 18, fontSize: 11.5, color: "var(--hint)", lineHeight: 1.6, display: "flex", flexDirection: "column", gap: 3 }}>
                    <li>Files changed by a command — <code className="mono">rm</code>, <code className="mono">mv</code>, <code className="mono">cp</code>, a build script — are not tracked. Only file edits and writes are.</li>
                    <li>Edits made by sub-agents are not restored.</li>
                    <li>Changes made outside OpenLeash while this task was running are not restored.</li>
                    <li>Messages queued mid-turn are not part of any checkpoint.</li>
                    {info?.in_worktree && <li>This chat runs in a worktree, so restoring files only affects that worktree. Your main checkout is left alone.</li>}
                  </ul>
                )}
              </div>
            </div>
            {err && <div role="alert" style={{ color: "#ff8a8a", fontSize: 11.5 }}>{err}</div>}
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <Tooltip content="Start a new chat from this checkpoint, in its own git worktree. This chat is left exactly as it is.">
                <Button variant="ghost" disabled={!cur || !!busy} onClick={() => void fork()}>
                  <GitFork aria-hidden="true" size={12} />Fork into a new worktree{busy === "fork" && <Loader size={12} />}
                </Button>
              </Tooltip>
              <div style={{ flex: 1 }} />
              <Button variant="ghost" onClick={onClose}>Cancel</Button>
              <Button variant="primary" disabled={!cur || !!busy} onClick={() => void confirm()}>
                {busy === "rewind" && <Loader size={12} />}{label}<Kbd>Enter</Kbd>
              </Button>
            </div>
          </div>
        </>
      )}
      </div>
    </Modal>
  );
}
