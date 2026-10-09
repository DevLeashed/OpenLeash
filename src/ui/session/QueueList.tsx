// The list of Alt-Enter messages: the ones the user asked to send when this turn
// ends. A plain Enter steers the agent straight away, the way it always has, so
// it never appears here — only the deliberately-deferred messages do.
//
// Shown above the composer rather than in the transcript, because a queued
// message hasn't been sent: it is editable, removable and reorderable until the
// agent reads it, which is the whole point of giving it a home of its own.
//
// The rows are a view of the backend's queue, not a second copy of it: each one
// is the timeline item that stands for a queued message, carrying `data.seq` for
// the order. Every action goes back through a command, and the events that come
// back are what redraw this.

import { useCallback, useState } from "react";
import { api, Item, TaskSummary } from "../../api";
import { flash, useStore } from "../../store";
import { I } from "../icons";
import { useRowDrag } from "../drag";
import { AnchoredPanel, Button, IconButton, MenuRow, TextArea, Thumb, useAnchoredPanel } from "../primitives";
import { Ellipsis, GripVertical, ListOrdered, Pencil, Send, Trash2 } from "lucide-react";

const li = (C: typeof Send, size = 14) => <C aria-hidden="true" size={size} strokeWidth={1.7} />;

/** One message waiting to go out, in the order it will be sent. */
export interface QueueRow {
  /** The timeline item id — what every command addresses the message by. */
  id: string;
  text: string;
  seq: number;
  images: string[];
}

/** The queued messages of a task, oldest first — the same order the agent reads them in. */
export function queueOf(items: Item[] | undefined): QueueRow[] {
  return (items ?? [])
    .filter((i) => i.kind === "user" && i.data?.queued)
    .map((i) => ({
      id: i.id,
      text: i.text,
      seq: typeof i.data.seq === "number" ? i.data.seq : 0,
      images: Array.isArray(i.data.images) ? (i.data.images as string[]) : [],
    }))
    .sort((a, b) => a.seq - b.seq);
}

export function QueueList({ task }: { task: TaskSummary }) {
  const items = useStore((s) => s.items[task.id]);
  const rows = queueOf(items);
  // Pointer-based, not HTML5 drag and drop: the window's native file-drop
  // handler has to stay installed (it is the only source of real paths for a
  // dropped file) and on Windows that OLE handler eats every drag before the
  // page sees it. See `ui/drag.ts`.
  const onDrop = useCallback((dragId: string, overId: string) => {
    // `before` is the row the message should land in front of; null sends it last.
    // Dropping on a row means "in front of it", which is where that row's top
    // edge is drawn, so the last row is the only one that means "to the end".
    const before = overId === rows[rows.length - 1]?.id ? null : overId;
    api.queueMove(task.id, dragId, before).catch((e) => flash(String(e)));
  }, [rows, task.id]);
  const drag = useRowDrag(onDrop);
  const remove = (id: string) => api.queueRemove(task.id, id).catch((e) => flash(String(e)));
  if (!rows.length) return null;

  return (
    <div className="queue">
      <div className="qh">
        <span className="qlab">{li(ListOrdered, 12)} {rows.length} after this turn</span>
        <div style={{ flex: 1 }} />
        <Button variant="ghost" className="qbtn" onClick={() => api.queueSendAll(task.id).catch((e) => flash(String(e)))}>Send all</Button>
        <Button variant="ghost" className="qbtn" onClick={() => rows.forEach((r) => remove(r.id))}>Discard</Button>
      </div>
      <div className="qlist" ref={drag.listRef}>
        {rows.map((r, i) => (
          <QueueItem
            key={r.id}
            task={task}
            row={r}
            first={i === 0}
            last={i === rows.length - 1}
            dragging={drag.dragging === r.id}
            over={drag.over === r.id && drag.dragging !== r.id}
            overEnd={drag.over === r.id && drag.dragging !== r.id && i === rows.length - 1}
            onPointerDown={drag.press(r.id)}
            onMove={(dir) => {
              // A row steps one place at a time: up is "in front of the row
              // above", down is "behind the row below".
              const j = i + dir;
              if (j < 0 || j >= rows.length) return;
              // `j` is in range by the check above, so there is a row to name.
              const target = rows[j];
              if (!target) return;
              api.queueMove(task.id, r.id, dir < 0 ? (j > 0 ? rows[j - 1]!.id : null) : target.id).catch((e) => flash(String(e)));
            }}
            onRemove={() => remove(r.id)}
          />
        ))}
      </div>
    </div>
  );
}

function QueueItem({ task, row, first, last, dragging, over, overEnd, onPointerDown, onMove, onRemove }: {
  task: TaskSummary;
  row: QueueRow;
  first: boolean;
  last: boolean;
  dragging: boolean;
  over: boolean;
  /** The drop lands at the end of the list, so the marker belongs underneath. */
  overEnd: boolean;
  onPointerDown: (e: React.PointerEvent) => void;
  onMove: (dir: -1 | 1) => void;
  onRemove: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(row.text);
  const menu = useAnchoredPanel();
  const save = () => {
    const next = text.trim();
    if (next && next !== row.text) api.queueEdit(task.id, row.id, next).catch((e) => flash(String(e)));
    setEditing(false);
  };
  // "Send now" takes this one out of the list and hands it over at once,
  // leaving everything else queued for the end of the turn.
  const sendNow = () => api.queueSendNow(task.id, row.id).catch((e) => flash(String(e)));

  return (
    <div
      className={"qrow" + (dragging ? " dragging" : "") + (over ? (overEnd ? " over end" : " over") : "")}
      data-dragrow={row.id}
      onPointerDown={editing ? undefined : onPointerDown}
    >
      <span className="qgrip" aria-hidden="true">{li(GripVertical, 12)}</span>
      {editing ? (
        <div className="qedit">
          <TextArea value={text} onChange={(e) => setText(e.currentTarget.value)} autoFocus
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); save(); }
              if (e.key === "Escape") { e.preventDefault(); setText(row.text); setEditing(false); }
            }} />
          <div className="qeditbar">
            <Button className="qbtn" onClick={save}>Save</Button>
            <Button variant="ghost" className="qbtn" onClick={() => { setText(row.text); setEditing(false); }}>Cancel</Button>
          </div>
        </div>
      ) : (
        <>
          <div className="qtext" title={row.text}>{row.text}</div>
          {row.images.length > 0 && (
            <div className="qimgs">
              {row.images.map((src, i) => <Thumb key={i} src={src} items={row.images.map((s, j) => ({ src: s, alt: `Attached image ${j + 1}` }))} index={i} alt={`Attached image ${i + 1}`} />)}
            </div>
          )}
          <Button variant="ghost" className="qbtn qsendnow" onClick={sendNow}>{li(Send, 11)} Send now</Button>
          <IconButton label="Remove from the queue" className="qico" onClick={onRemove}>{I.trash(12)}</IconButton>
          <IconButton label="More" className="qico" onClick={menu.toggle}>{li(Ellipsis, 14)}</IconButton>
        </>
      )}
      <AnchoredPanel anchor={menu.anchor} onClose={menu.close} width={190}>
        <MenuRow onClick={() => { setEditing(true); menu.close(); }}>{li(Pencil, 13)} Edit message</MenuRow>
        <MenuRow disabled={first} onClick={() => { onMove(-1); menu.close(); }}>Move up</MenuRow>
        <MenuRow disabled={last} onClick={() => { onMove(1); menu.close(); }}>Move down</MenuRow>
        <MenuRow onClick={() => { onRemove(); menu.close(); }}>{li(Trash2, 13)} Remove</MenuRow>
      </AnchoredPanel>
    </div>
  );
}
