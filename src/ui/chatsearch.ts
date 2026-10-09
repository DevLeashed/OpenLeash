/** Finding things inside one chat.
 *
 *  The Ctrl-K palette searches chats and commands; this searches the transcript
 *  of the chat you're reading — the thing you want once a conversation is long
 *  enough that "what did the agent say three turns ago" is a real question.
 *
 *  Kept as pure functions over `Item[]` so it can be tested without a DOM, and
 *  so the same matching can drive both the result list and the row highlight. */
import type { Item } from "../api";

/** One hit: which item, a line to show, and the char offsets of the match
 *  inside that line so the view can mark it without re-searching. */
export interface Hit {
  id: string;
  /** The line the match is on, trimmed for display. */
  line: string;
  /** 0-based index into `line` where the match starts, -1 if not on this line. */
  at: number;
  /** How many results this item contributed, so "12 matches" can be honest
   *  about repeated words on one line. */
  count: number;
}

/** Everything a row of this item could be searched on: what it says, and what
 *  it did. A tool call's command or file path is as findable as prose. */
export function haystack(it: Item): string {
  const parts: string[] = [it.text ?? ""];
  const d = it.data ?? {};
  // Tool inputs are short and specific: a path, a command, a query. Walking the
  // whole object blindly would index schema boilerplate nobody searches for.
  if (it.kind === "tool") {
    const i = d.input ?? {};
    if (typeof i === "string") parts.push(i);
    else for (const v of Object.values(i)) if (typeof v === "string") parts.push(v);
    if (typeof d.output === "string") parts.push(d.output);
  }
  if (it.kind === "sub" && typeof d.label === "string") parts.push(d.label);
  return parts.filter(Boolean).join("\n");
}

/** How many lines of one item are indexed. A tool's output can be a 30 KB build
 *  log; nobody means to find a word in it, and letting it match would bury the
 *  handful of results that do matter. */
const MAX_LINES = 40;

/** The lines of an item worth showing a hit on: prose and tool input, minus the
 *  giant blobs. */
function lines(it: Item): string[] {
  const out: string[] = [];
  const text = haystack(it);
  // Scan only the indexed prefix instead of splitting a potentially huge log.
  let start = 0;
  while (start < text.length && out.length < MAX_LINES) {
    const end = text.indexOf("\n", start);
    const s = text.slice(start, end < 0 ? text.length : end).trim();
    if (s) out.push(s);
    if (end < 0) break;
    start = end + 1;
  }
  return out;
}

/** All hits for `q` across `items`, in transcript order.
 *
 *  Matching is case-insensitive substring, which is what people expect from a
 *  chat search: they remember "permissions" not a regex. An empty or
 *  whitespace-only query returns nothing rather than everything. */
export function searchItems(items: Item[], q: string, limit = Infinity): Hit[] {
  const needle = q.trim().toLowerCase();
  if (!needle || limit <= 0) return [];
  const hits: Hit[] = [];
  for (const it of items) {
    // A queued message the user hasn't sent yet isn't part of the transcript.
    if (it.kind === "text" && !it.text.trim()) continue;
    let count = 0;
    let first: Hit | null = null;
    for (const line of lines(it)) {
      const from = line.toLowerCase().indexOf(needle);
      if (from < 0) continue;
      count++;
      if (!first) first = { id: it.id, line: line.length > 300 ? line.slice(0, 300) + "…" : line, at: from, count: 0 };
    }
    if (first) {
      hits.push({ ...first, count });
      // Finish counting this item's lines, but don't scan unseen result rows.
      if (hits.length >= limit) break;
    }
  }
  return hits;
}

/** Wrap the matched span of a result line in <mark>, for the preview row.
 *  Returns React-safe segments so the caller doesn't build raw HTML. */
export function segments(line: string, at: number, needle: string): [string, string, string][] {
  const n = needle.trim().toLowerCase();
  if (!n || at < 0) return [[line, "", ""]];
  // `at` came from the untrimmed search line, so re-find it on the trimmed one.
  const start = line.toLowerCase().indexOf(n);
  if (start < 0) return [[line, "", ""]];
  return [[line.slice(0, start), line.slice(start, start + n.length), line.slice(start + n.length)]];
}
