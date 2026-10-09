/** Find inside the chat you're reading.
 *
 *  Ctrl-K finds chats, commands and models. This finds the thing you already
 *  half-remember: the line the agent wrote three turns ago, the command it ran,
 *  the path it edited. It lives over the transcript rather than in the palette
 *  because it's a view action, not a navigation one — you stay where you are and
 *  step through matches in place. */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useStore } from "../store";
import { searchItems, segments } from "./chatsearch";
import { IconButton, Input } from "./primitives";
import { I } from "./icons";
import { ChevronUp, ChevronDown, X } from "lucide-react";

/** Cap on how many hits are listed. A chat where every turn matches "the" has
 *  thousands, and stepping through them one at a time is the point — but there
 *  is no value in holding every one to render a count. */
const MAX_HITS = 500;

/** Scroll a transcript row into view, centring it so the lines around it stay
 *  readable. A no-op when the row isn't mounted: a match folded inside a closed
 *  tool group has no DOM to scroll to, and hunting for it would be worse than
 *  letting the user open the group themselves.
 *
 *  Everything is guarded because a throw here would take the find bar down with
 *  it — the bar is the one thing the user cannot dismiss without. */
function reveal(id: string) {
  try {
    // Item ids are harness-generated, but escaping keeps a stray quote in an id
    // from turning this into an invalid selector that throws.
    document.querySelector<HTMLElement>(`[data-item="${CSS.escape(id)}"]`)?.scrollIntoView?.({ block: "center", behavior: "smooth" });
  } catch {
    /* no row to reveal; the match still shows in the count and the preview */
  }
}

export function FindBar({ onClose }: { onClose: () => void }) {
  const task = useStore((s) => s.task);
  const items = useStore((s) => (s.task ? s.items[s.task] : undefined));
  const seed = useStore((s) => s.findSeed);
  const [q, setQ] = useState(seed);
  const [cursor, setCursor] = useState(0);
  const inp = useRef<HTMLInputElement>(null);

  const hits = useMemo(() => searchItems(items ?? [], q, MAX_HITS), [items, q]);
  // Keep the cursor on a real hit: narrowing the query can leave it past the end.
  const active = Math.min(cursor, Math.max(0, hits.length - 1));

  useEffect(() => { inp.current?.focus(); inp.current?.select(); }, []);
  useEffect(() => { setCursor(0); }, [q, task]);
  // Jump to the first hit as soon as there is one, so typing lands you on it.
  useEffect(() => { if (hits.length) reveal(hits[0]!.id); }, [q, task]);

  const step = useCallback((dir: 1 | -1) => {
    if (!hits.length) return;
    setCursor((c) => {
      // The modulo lands inside `hits`, and we returned above when it is empty,
      // so there is always a hit here to reveal.
      const next = (c + dir + hits.length) % hits.length;
      reveal(hits[next]!.id);
      return next;
    });
  }, [hits]);

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") { e.preventDefault(); step(e.shiftKey ? -1 : 1); }
    else if (e.key === "Escape") { e.preventDefault(); onClose(); }
  };

  const hit = hits[active];
  return (
    <div className="findbar" role="search" onKeyDown={onKey}>
      <span className="findbar-ico" aria-hidden="true">{I.search()}</span>
      <Input
        ref={inp}
        className="findbar-in"
        value={q}
        placeholder="Find in this chat"
        aria-label="Find in this chat"
        onChange={(e) => setQ(e.currentTarget.value)}
      />
      <span className="findbar-count" aria-live="polite">
        {!q.trim() ? "" : hits.length ? `${active + 1} of ${hits.length}${hits.length === MAX_HITS ? "+" : ""}` : "No results"}
      </span>
      <IconButton label="Previous match" disabled={!hits.length} onClick={() => step(-1)}><ChevronUp size={13} /></IconButton>
      <IconButton label="Next match" disabled={!hits.length} onClick={() => step(1)}><ChevronDown size={13} /></IconButton>
      <IconButton label="Close find" onClick={onClose}><X size={13} /></IconButton>
      {hit && (
        <div className="findbar-hit" title={hit.line}>
          {segments(hit.line, hit.at, q).map(([before, match, after], k) => (
            <span key={k}>{before}{match && <mark>{match}</mark>}{after}</span>
          ))}
        </div>
      )}
    </div>
  );
}
