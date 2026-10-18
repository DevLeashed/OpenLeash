// Long choice lists on the question cards (`ask_user` and `ask_nonblocking`).
//
// A question can carry up to 30 options. That is fine to read on a desktop
// card when the list is short, but past a handful it needs three things the
// short list does not: a way to narrow it, a bound on its height so the rest of
// the card (and on the non-blocking card, the composer) stays on screen, and
// number keys that keep meaning something once there are more rows than digits.
//
// The digits name the rows *as shown*: 1 is the first visible row, whatever the
// filter left. With no filter and nine or fewer options that is exactly the
// agent's numbering, so a short question behaves as it always has. The filter
// never reorders: the agent's order is part of the question, and a recommended
// option stays marked where it is rather than jumping about as you type.
import { forwardRef } from "react";
import type { KeyboardEvent } from "react";
import { Search, X } from "lucide-react";
import { IconButton, Input } from "./primitives";
import type { Opt } from "./Question";

/** More options than this get a filter box and a height-bounded, scrolling list. */
export const FILTER_AT = 8;

/** Whether a question's option list is long enough to need the filter. */
export const isLong = (opts: readonly Opt[]) => opts.length > FILTER_AT;

/**
 * Indexes (into `opts`) of the options matching `query`, in the agent's order.
 * Every whitespace-separated term has to appear in the label or description,
 * case-insensitively, so "pg 16" finds "Postgres 16" without being a regex.
 */
export function filterOptions(opts: readonly Opt[], query: string): number[] {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  const all = opts.map((_, i) => i);
  if (!terms.length) return all;
  return all.filter((i) => {
    const o = opts[i]!;
    const hay = `${o.label} ${o.description ?? ""}`.toLowerCase();
    return terms.every((t) => hay.includes(t));
  });
}

/**
 * What a digit key does on a choice question: pick the n-th visible row, or the
 * free-text row when it directly follows the visible ones and is still within
 * reach of a single digit. `null` when the digit names nothing.
 */
export function digitTarget(shown: readonly number[], n: number, hasOther: boolean): { option: number } | { other: true } | null {
  if (!Number.isInteger(n) || n < 1 || n > 9) return null;
  if (n <= shown.length) return { option: shown[n - 1]! };
  if (hasOther && n === shown.length + 1) return { other: true };
  return null;
}

/** How many rows the digits reach: the visible options, plus the free-text row if it is within reach. */
export const digitCount = (shown: readonly number[], hasOther: boolean) => Math.min(9, shown.length + (hasOther ? 1 : 0));

/** Scroll a row into the bounded list's view. jsdom has no `scrollIntoView`. */
export function revealRow(list: HTMLElement | null, index: number) {
  const row = list?.querySelector<HTMLElement>(`[data-opt="${index}"]`);
  row?.scrollIntoView?.({ block: "nearest" });
}

/** Move keyboard focus onto a row of the list (the row itself is the focusable radio/checkbox). */
export function focusRow(list: HTMLElement | null, index: number) {
  list?.querySelector(`[data-opt="${index}"]`)?.closest<HTMLElement>('[role="radio"],[role="checkbox"]')?.focus();
}

/** The line a filter that matched nothing leaves in place of the list. */
export function NoMatch({ query }: { query: string }) {
  return <div className="qfilter-empty">No options match. Enter uses “{query.trim()}” as your own answer.</div>;
}

export const OptionFilter = forwardRef<HTMLInputElement, {
  query: string;
  onChange: (q: string) => void;
  /** Enter: pick the first match, or carry on when there is nothing new to pick. */
  onEnter: () => void;
  /** ArrowDown: move focus into the list. */
  onDown: () => void;
  shown: number;
  total: number;
  /** Selected options the filter is currently hiding, so a pick never seems to vanish. */
  hiddenPicks: number;
}>(function OptionFilter({ query, onChange, onEnter, onDown, shown, total, hiddenPicks }, ref) {
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") { e.preventDefault(); onEnter(); }
    else if (e.key === "ArrowDown") { e.preventDefault(); onDown(); }
    // Escape clears first; only an empty box lets it through to whatever else
    // listens for it (closing a panel, say).
    else if (e.key === "Escape" && query) { e.preventDefault(); e.stopPropagation(); onChange(""); }
  };
  return (
    <div className="qfilter">
      <div className="qfilter-box">
        <Search size={12} aria-hidden="true" className="qfilter-ico" />
        <Input ref={ref} className="input qfilter-input" type="search" value={query} placeholder={`Filter ${total} options…`} aria-label="Filter options"
          onChange={(e) => onChange(e.currentTarget.value)} onKeyDown={onKeyDown} />
        {query && <IconButton className="qfilter-clear" label="Clear filter" onClick={() => onChange("")}><X size={11} /></IconButton>}
      </div>
      <span className="qfilter-count" aria-live="polite">
        {query ? `${shown} of ${total}` : `${total} options`}
        {hiddenPicks > 0 && ` · ${hiddenPicks} picked ${hiddenPicks === 1 ? "option" : "options"} hidden`}
      </span>
    </div>
  );
});

/** What the filter's Enter does, shared by both cards so they cannot disagree. */
export function filterEnter({ query, shown, selected, pick, ownAnswer, next }: {
  query: string; shown: readonly number[]; selected: readonly number[];
  pick: (i: number) => void; ownAnswer: (text: string) => void; next: () => void;
}) {
  const q = query.trim();
  if (!q) return next();
  const first = shown[0];
  // Nothing matches: what was typed is the answer the list did not have.
  if (first === undefined) return ownAnswer(q);
  if (!selected.includes(first)) return pick(first);
  next();
}
