/** Shared by the Review view and the bisect panel, which both show one file's
 *  diff and both name files a repository supplied. In its own module so the two
 *  screens do not import each other. */

/** Unicode bidi overrides let a filename lie about its own name: `evil\u202Efdp.exe`
 *  renders as `evilepx.d` — a trojan renaming itself. A repo that ships one gets
 *  a user to revert the wrong file on the one screen meant to catch that, so the
 *  control characters are dropped and the override shown as the escape it is.
 *  The character classes are written as `\u` escapes, which is why this needs no
 *  `no-control-regex` disable — the rule does not fire on an escaped range. */
export function safeName(s: string): string {
  return s.replace(/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g, (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, "0")}`);
}

export interface DiffRow { a: string; b: string; sign: string; signColor: string; t: string; color: string; bg: string; bar: string }

/** A diff's lines as `.dline` rows. Line numbers come from the diff's own hunk
 *  headers, so they have to be counted in order — a skip that started at zero
 *  would renumber every line. Same shape and same tokens as the transcript's
 *  diff rows: the sign and the bar carry the hue, the code text stays in a
 *  readable body colour, and the band is a 10% tint rather than a slab the text
 *  has to fight. */
export function diffRows(lines: readonly { k: "a" | "d" | "c" | "h"; t: string }[]): DiffRow[] {
  let a = 0, b = 0;
  return lines.map((l) => {
    if (l.k === "h") {
      const m = l.t.match(/-(\d+)(?:,\d+)? \+(\d+)/);
      if (m) { a = +m[1]! - 1; b = +m[2]! - 1; }
      return { a: "", b: "", sign: "", signColor: "var(--diff-hunk)", t: l.t, color: "var(--diff-hunk)", bg: "var(--diff-hunk-bg)", bar: "transparent" };
    }
    if (l.k === "a") { b++; return { a: "", b: String(b), sign: "+", signColor: "var(--diff-add)", t: l.t, color: "var(--fg2)", bg: "var(--diff-add-bg)", bar: "var(--diff-add)" }; }
    if (l.k === "d") { a++; return { a: String(a), b: "", sign: "−", signColor: "var(--diff-del)", t: l.t, color: "var(--fg2)", bg: "var(--diff-del-bg)", bar: "var(--diff-del)" }; }
    a++; b++;
    return { a: String(a), b: String(b), sign: "", signColor: "", t: l.t, color: "var(--mut)", bg: "transparent", bar: "transparent" };
  });
}
