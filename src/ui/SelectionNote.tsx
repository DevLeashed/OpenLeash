// Selecting text anywhere in a chat puts a note on it: a pill lands by the
// selection, "Add to chat" turns that into a small composer where the quoted
// text carries an optional comment, and saving adds the note to the chat as an
// annotation — a "3 annotations" chip above the composer, expandable to the
// quotes themselves.
//
// The note is *not* written into the composer as a blockquote. The composer is
// where the user types the next message, and a note they did not type is not part
// of it: it could be edited by accident or left behind in the draft after it
// had been answered. Notes can be sent with or without a typed message, and
// travel to the model as the annotation block the composer prepends at send
// time (see `annotationBlock` and `api.send`).
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { Note } from "../store";
import { flash, get, set, useStore, zoom } from "../store";
import { copyText } from "./mdx";
import { Button, IconButton, MenuScrim, TextArea, Tooltip, useEscapeClose } from "./primitives";
import { I } from "./icons";

/** A selection that has become a note: where it was, what it says, and the chat
 *  it would go to. The comment lives in the pill, not here — it is only ever
 *  read by the Save button beside it. */
type Pending = { anchor: DOMRect; text: string; chat: string; item: string };

/** The pill's height, needed before it has been measured. A flat pill, so this
 *  is exact rather than a guess the way `ctxPlace` takes one. */
const PILL_H = 28;

/**
 * A selection is only a note if it was made on purpose, and `mouseup` cannot tell
 * that apart from the drag that happens to finish over a word. So the text is
 * read but the pill is withheld until the pointer has been still for a beat: a
 * drag's selection is still changing as it ends, while a double-click and a
 * shift-click never move at all.
 */
const SETTLE_MS = 220;

/** Past this the agent is quoting a chapter, not a passage. Kept in the note
 *  rather than clipped: a truncated quote reads as the whole sentence. */
const MAX_NOTE = 6000;

/** The chat a selection came from, and the item it was in. The item is what the
 *  annotation is drawn under, so a note on a tool call's output does not end up
 *  under the message above it. */
function where(node: Node | null) {
  const el = node instanceof Element ? node : node?.parentElement;
  if (!el) return { chat: "", item: "" };
  return {
    chat: el.closest<HTMLElement>(".session-pane")?.dataset.task ?? "",
    item: el.closest<HTMLElement>(".it")?.dataset.item ?? "",
  };
}

/** `getSelection` text keeps the newlines the reader saw as blank lines; a quote
 *  that keeps them is mostly gap. */
const cleanup = (s: string) => s.replace(/[ \t]+/g, " ").replace(/\n{3,}/g, "\n\n").trim();

/**
 * Where the pill goes for a selection, in app-root pixels: below it, or above it
 * when the transcript is at the bottom of the window, and clamped so it stays on
 * screen. Exported because a placement this fiddly needs a test that doesn't
 * depend on a layout engine.
 */
export function pillPlace(a: { top: number; bottom: number; left: number; width: number }, w: number, z: number) {
  const vw = window.innerWidth / z, vh = window.innerHeight / z;
  const up = a.bottom / z + PILL_H + 8 > vh && a.top / z - PILL_H - 8 > 0;
  return {
    top: up ? a.top / z - PILL_H - 8 : a.bottom / z + 8,
    left: Math.max(8, Math.min(a.left / z + a.width / z / 2 - w / 2, vw - w - 8)),
  };
}

/**
 * How the notes on a chat are handed to the agent. A block under its own
 * heading, preserving the selected text verbatim, including backticks and
 * asterisks. An end marker lets the transcript collapse this context without
 * hiding the user's accompanying message.
 *
 * Headings are numbered so a note can be referred to ("what's note 2 about?")
 * without the user having to re-quote it.
 */
export function annotationBlock(notes: Note[]): string {
  if (!notes.length) return "";
  const rows = notes.map((n, i) => {
    const head = `### Note ${i + 1}${n.item ? ` (on your message)` : ""}`;
    return n.body.trim()
      ? `${head}\n\n${n.text.slice(0, MAX_NOTE)}\n\nThe user says about it: ${n.body.trim()}`
      : `${head}\n\n${n.text.slice(0, MAX_NOTE)}`;
  });
  return `Annotations the user added to your answers:\n\n${rows.join("\n\n")}\n\n<!-- /openleash-annotations -->`;
}

/** Keep the model's annotation context out of the visible message body. The
 * delimiter makes the boundary unambiguous even when quotes contain headings. */
export function SentAnnotationMessage({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const prefix = "Annotations the user added to your answers:\n\n";
  const marker = "\n\n<!-- /openleash-annotations -->";
  const end = text.startsWith(prefix) ? text.indexOf(marker, prefix.length) : -1;
  if (end < 0) return <>{text}</>;
  const annotations = text.slice(prefix.length, end);
  const count = annotations.match(/^### Note \d+(?: \(on your message\))?$/gm)?.length ?? 1;
  const message = text.slice(end + marker.length).replace(/^\n\n/, "");
  return <>
    <div className="annots">
      <button type="button" className="anchip" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span className="an-lead" aria-hidden="true">{I.check(12)}</span>
        <span>{count} annotation{count === 1 ? "" : "s"}</span>
        <span className="an-chev" style={{ transform: `rotate(${open ? 90 : 0}deg)` }}>{I.chevR()}</span>
      </button>
      {open && <div className="anlist sel" style={{ whiteSpace: "pre-wrap" }}>{annotations}</div>}
    </div>
    {message}
  </>;
}

/**
 * The pill, and the composer it opens into. Portalled into the app root, which
 * carries the zoom: the root is `zoom`ed, so a layer placed in unzoomed window
 * pixels at a rect read from zoomed content lands in the wrong place and renders
 * at the wrong size.
 */
function Pill({ p, onSave, onClose }: { p: Pending; onSave: (body: string) => void; onClose: () => void }) {
  const root = document.querySelector<HTMLElement>(".app");
  const ref = useRef<HTMLDivElement>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  const [w, setW] = useState(150);
  const [editing, setEditing] = useState(false);
  const [body, setBody] = useState("");
  // The editor is a textarea that has to grow with what is typed into it, and it
  // sits below the selection where the last word was — so it grows downward and
  // clamps at 200px rather than pushing its own Save button off the screen.
  const grow = useCallback(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 200) + "px";
  }, []);
  useLayoutEffect(() => { grow(); }, [body, grow]);
  // Only the collapsed pill is measured, and only for its own width: the editor
  // is a fixed 360px in CSS, and measuring a layout that is already fixed is a
  // forced sync layout paid on every selection.
  useLayoutEffect(() => { if (!editing) setW(ref.current?.offsetWidth || 150); }, [editing]);
  useEscapeClose(true, onClose);
  if (!root) return null;
  const at = pillPlace(p.anchor, w, zoom());
  return createPortal(
    <>
      {editing && <MenuScrim layer={89} onClick={onClose} />}
      {/* `pillPlace` is handed the height of the *collapsed* pill, so an editor
          that has grown can reach past the bottom of the window. It flips above
          the selection for the same reason the pill itself does; it just happens
          later, on the second measurement. */}
      <div ref={ref} className={"notepill" + (editing ? " editing" : "")} style={{ ...at, width: editing ? 360 : undefined }} data-note="pill">
        {editing ? (
          <>
            <TextArea ref={box} rows={1} autoFocus value={body} placeholder="Add an optional comment…"
              onChange={(e) => setBody(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); onSave(body); }
                else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); onClose(); }
              }} />
            <div className="nobar">
              <Tooltip content="Deletes this note before it is added"><IconButton label="Discard this note" onClick={onClose}>{I.trash(13)}</IconButton></Tooltip>
              <span style={{ flex: 1 }} />
              <Button variant="ghost" onClick={onClose}>Cancel</Button>
              <Button onClick={() => onSave(body)}>Save</Button>
            </div>
          </>
        ) : (
          <>
            <button type="button" className="np-a" onClick={() => setEditing(true)}>Add to chat</button>
            <span className="np-sep" />
            <button type="button" className="np-a quiet" onClick={() => copyText(p.text, "Copied")}>Copy</button>
          </>
        )}
      </div>
    </>,
    root,
  );
}

/** One global selection watcher. It reads the selection rather than being handed
 *  it: a selection can cross message boundaries, and per-row handlers would each
 *  only ever see the part that started inside them. */
export function SelectionNoteLayer() {
  const [pill, setPill] = useState<Pending | null>(null);
  const ready = useStore((s) => s.ready);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);

  const onUp = useCallback((e: MouseEvent) => {
    if (e.button !== 0) return;
    // The note composer is a text field, so a drag inside it is its own business.
    if ((e.target as Element | null)?.closest?.(".notepill")) return;
    const sel = window.getSelection();
    const raw = sel?.toString() ?? "";
    if (!sel || !sel.rangeCount || !raw.trim()) return setPill(null);
    const r = sel.getRangeAt(0).getBoundingClientRect();
    // A collapsed range — a click that cleared a selection — has no box, and a
    // pill for it would point at nothing.
    if (!r.width && !r.height) return setPill(null);
    const w = where(e.target as Node | null);
    const anchor = w.chat ? w : where(sel.getRangeAt(0).commonAncestorContainer);
    // Only a chat has a composer to add a note to, so outside one there is
    // nothing the pill could offer.
    if (!anchor.chat) return setPill(null);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setPill({ anchor: r, text: cleanup(raw).slice(0, MAX_NOTE), chat: anchor.chat, item: anchor.item }), SETTLE_MS);
  }, []);

  useEffect(() => {
    window.addEventListener("mouseup", onUp);
    return () => { window.removeEventListener("mouseup", onUp); clearTimeout(timer.current); };
  }, [onUp]);

  const close = useCallback(() => {
    setPill(null);
    // A note left selected underneath a closed pill would swallow the next click,
    // and the reader's next drag would re-open the pill over the same words.
    window.getSelection()?.removeAllRanges();
  }, []);

  const save = useCallback((body: string) => {
    if (!pill) return;
    const note: Note = { id: `n${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`, item: pill.item, text: pill.text, body: body.trim() };
    set((s) => ({ notes: { ...s.notes, [pill.chat]: [...(s.notes[pill.chat] ?? []), note] } }));
    close();
    flash(note.body ? "Note added to the chat" : "Selection added to the chat");
  }, [pill, close]);

  // Mounted behind `ready` so the app root it portals into exists, and so a
  // selection made during boot isn't offered a note.
  if (!ready) return null;
  return pill ? <Pill p={pill} onSave={save} onClose={close} /> : null;
}

/**
 * A chat's annotations, sitting between the transcript and the composer.
 *
 * One line, collapsed, because the notes are a receipt rather than the message:
 * the user has already read the quotes and written the comments, and the box below
 * is what they are about to send. Expanded, each quote is shown against its
 * comment so a note can be checked — or dropped — before it travels.
 */
export function Annotations({ chat }: { chat: string }) {
  const notes = useStore((s) => s.notes[chat]);
  const [open, setOpen] = useState(false);
  const drop = useCallback((id: string) => set((s) => ({ notes: { ...s.notes, [chat]: (s.notes[chat] ?? []).filter((n) => n.id !== id) } })), [chat]);
  if (!notes?.length) return null;
  const drop_ = notes.length > 1 && notes.every((n) => !n.body);
  return (
    <div className="annots">
      <Pressableish onClick={() => setOpen(!open)}>
        <span className="an-lead" aria-hidden="true">{I.check(12)}</span>
        <span>{notes.length} annotation{notes.length === 1 ? "" : "s"}</span>
        <span className="an-chev" style={{ transform: `rotate(${open ? 90 : 0}deg)` }}>{I.chevR()}</span>
      </Pressableish>
      {open && (
        <div className="anlist">
          {notes.map((n) => (
            <div key={n.id} className="anrow">
              <div className="anquote sel">{n.text}</div>
              {n.body && <div className="anbody sel">{n.body}</div>}
              <Tooltip content="Remove this annotation before it is sent">
                <IconButton label="Remove annotation" className="an-x" onClick={() => drop(n.id)}>{I.close(11)}</IconButton>
              </Tooltip>
            </div>
          ))}
          {drop_ && <div className="anhint">No comment on any of them — these go as bare quotes.</div>}
        </div>
      )}
    </div>
  );
}

/** A plain button-row row, not a `Pressable` div: it sits in the composer above
 *  the box, where the surrounding controls are real buttons and a role="button"
 *  div would not match them. */
function Pressableish({ children, onClick }: { children: React.ReactNode; onClick: () => void }) {
  return <button type="button" className="anchip" onClick={onClick}>{children}</button>;
}

/** Read the notes a chat is about to send, for the composer's send path. */
export function notesFor(chat: string | null): Note[] {
  return (chat && get().notes[chat]) || [];
}
