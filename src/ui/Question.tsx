// The agent-designed question form: 1-20 questions, one per page, each with
// its own type (single / multi / text / number / confirm).
import { useEffect, useMemo, useRef, useState } from "react";
import { api, Item, TaskSummary } from "../api";
import { flash } from "../store";
import { Check, Minus, Pencil, X } from "lucide-react";
import { Button, ChoiceRow, IconButton, Input, Kbd, Pressable, TextArea } from "./primitives";
import { MdInline } from "./mdx";

type QType = "single" | "multi" | "text" | "number" | "confirm";
export interface Opt { label: string; description?: string; recommended?: boolean }
export interface Q {
  question: string; header?: string; type: QType; description?: string; options?: Opt[];
  allow_other?: boolean; // legacy: a choice question always offers a free-text box now
  required?: boolean; placeholder?: string; default?: any; min?: number; max?: number;
  note?: boolean; note_placeholder?: string;
}
/** An optional free-text remark that rides along with one question's answer. */
type AnsNote = { on: boolean; text: string };
/**
 * One remark per chosen option, to extend it: "Postgres — but on the existing
 * cluster". Keyed by option index, not label, because two options can read the
 * same and a label can be edited out from under a saved form.
 */
type OptNotes = Record<number, string>;
export type Ans = { sel: number[]; other: string; otherOn: boolean; text: string; bool: boolean | null; skipped: boolean; note: AnsNote; optNotes: OptNotes; multi?: boolean };
/** Everything about a form in flight, saved to disk so it survives a close or a chat switch. */
interface Draft { page: number; ans: Ans[]; dismissing: boolean; note: string }

/** The clean label for an option, the one thing the answer line shows. */
export const clean = (l: string) => l.replace(/\s*\n?\(recommended\)/i, "");
export const isRec = (o: Opt) => o.recommended || /\(recommended\)/i.test(o.label);
const blankNote = (): AnsNote => ({ on: false, text: "" });
/** Keep only notes that are whole option indexes, up to `max`. */
const optNotesOf = (raw: any, max: number): OptNotes => {
  if (!raw || typeof raw !== "object") return {};
  const out: OptNotes = {};
  for (const [k, v] of Object.entries(raw as Record<string, unknown>)) {
    const i = Number(k);
    if (Number.isInteger(i) && i >= 0 && i < max && typeof v === "string") out[i] = v;
  }
  return out;
};
/** An option note only counts once it has something in it. */
const hasOptNote = (a: Ans) => Object.values(a.optNotes ?? {}).some((t) => t.trim().length > 0);

export function initial(q: Q): Ans {
  const a: Ans = { sel: [], other: "", otherOn: false, text: "", bool: null, skipped: false, note: blankNote(), optNotes: {} };
  const d = q.default;
  if (d === undefined || d === null) return a;
  if (q.type === "confirm") a.bool = !!d;
  else if (q.type === "text" || q.type === "number") a.text = String(d);
  else {
    const labels = (Array.isArray(d) ? d : [d]).map(String);
    a.sel = (q.options ?? []).map((o, i) => (labels.includes(clean(o.label)) || labels.includes(o.label) ? i : -1)).filter((i) => i >= 0);
    if (q.type === "single") a.sel = a.sel.slice(0, 1);
  }
  return a;
}

export function valid(q: Q, a: Ans): string | null {
  if (a.skipped) return null;
  const req = q.required !== false;
  switch (q.type) {
    case "single":
      if (a.otherOn) return a.other.trim() ? null : "Type your answer";
      return a.sel.length || !req ? null : "Pick one";
    case "multi": {
      const n = a.sel.length + (a.otherOn && a.other.trim() ? 1 : 0);
      if (req && n === 0) return "Pick at least one";
      if (q.min && n < q.min) return `Pick at least ${q.min}`;
      if (q.max && n > q.max) return `Pick at most ${q.max}`;
      return null;
    }
    case "text":
      return a.text.trim() || !req ? null : "Required";
    case "number": {
      if (!a.text.trim()) return req ? "Required" : null;
      const n = Number(a.text);
      if (Number.isNaN(n)) return "Not a number";
      if (q.min !== undefined && n < q.min) return `Min ${q.min}`;
      if (q.max !== undefined && n > q.max) return `Max ${q.max}`;
      return null;
    }
    case "confirm":
      return a.bool !== null || !req ? null : "Yes or no";
  }
}

export function value(q: Q, a: Ans): any {
  if (a.skipped) return null;
  const opts = q.options ?? [];
  // Every index in `sel` names one of `opts` — `initial` and `restore` filter to
  // the option range and `pick` only ever hands over the index of a row that was
  // rendered from them — so the reads below are in range by construction, which
  // the type-checker cannot see.
  switch (q.type) {
    case "single": {
      // The user's own choice is never extended: they wrote it out, so there is
      // nothing for a note to add. The question's own note is where that goes.
      if (a.otherOn) return a.other.trim() ? a.other.trim() : null;
      return a.sel.length ? choice(clean(opts[a.sel[0]!]!.label), optNote(a, a.sel[0]!)) : null;
    }
    case "multi": {
      const out = [
        ...a.sel.map((i) => choice(clean(opts[i]!.label), optNote(a, i))),
        ...(a.otherOn && a.other.trim() ? [a.other.trim()] : []),
      ];
      return out;
    }
    case "text": return a.text.trim() || null;
    case "number": return a.text.trim() ? Number(a.text) : null;
    case "confirm": return a.bool;
  }
}

/** The extension on one chosen option, by its index in the question. */
const optNote = (a: Ans, i: number) => a.optNotes?.[i]?.trim() || undefined;
/**
 * One chosen option: the label alone unless it was extended, so an answer
 * without an extension is exactly the string it has always been.
 */
const choice = (label: string, note?: string) => (note ? { label, note } : label);

/** The answer, plus a note about it when the question asked for one. */
export function entry(q: Q, a: Ans): { value: any; note?: string } {
  const v = value(q, a);
  const n = a.note.on ? a.note.text.trim() : "";
  return n ? { value: v, note: n } : { value: v };
}

/** A question is skipped when nothing came back for it, whichever way it was left empty. */
export const wasSkipped = (it: Item, i: number) => {
  const v = readAnswer(it, i).value;
  return v === null || v === undefined || v === "";
};

/** `null` when the user set nothing to save. */
export function draftOf(d: Draft): string | null {
  if (d.dismissing || d.note.trim()) return JSON.stringify(d);
  if (d.ans.some((a) => a.sel.length || a.otherOn || a.text.trim() || a.bool !== null || a.skipped || a.note.text || hasOptNote(a) || a.multi)) return JSON.stringify(d);
  return null;
}

/** Restore a saved form, ignoring anything that no longer matches the questions. */
export function restore(raw: string, qs: Q[]): Draft | null {
  try {
    const d = JSON.parse(raw) as Partial<Draft>;
    if (!d || !Array.isArray(d.ans) || !d.ans.length) return null;
    const ans: Ans[] = qs.map((q, i) => {
      const base = initial(q);
      const s = d.ans![i];
      if (!s || typeof s !== "object") return base;
      return {
        sel: Array.isArray(s.sel) ? s.sel.filter((n) => Number.isInteger(n) && n >= 0 && n < (q.options?.length ?? 0)) : base.sel,
        other: typeof s.other === "string" ? s.other : base.other,
        otherOn: !!s.otherOn,
        text: typeof s.text === "string" ? s.text : base.text,
        bool: typeof s.bool === "boolean" ? s.bool : base.bool,
        skipped: !!s.skipped,
        note: { on: !!s.note?.on, text: typeof s.note?.text === "string" ? s.note.text : "" },
        // Indexes only: a note saved against an option that is no longer there
        // is dropped rather than pasted onto whichever one took its place.
        optNotes: optNotesOf(s.optNotes, q.options?.length ?? 0),
        multi: !!s.multi,
      };
    });
    return {
      page: Number.isInteger(d.page) && d.page! >= 0 && d.page! < qs.length ? d.page! : 0,
      ans,
      dismissing: !!d.dismissing,
      note: typeof d.note === "string" ? d.note : "",
    };
  } catch {
    return null;
  }
}

export const showAnswer = (v: any): string => {
  if (typeof v === "boolean") return v ? "Yes" : "No";
  const one = (x: any) => (x && typeof x === "object" && !Array.isArray(x) && "label" in x
    ? String(x.label) + (x.note ? ` — “${x.note}”` : "")
    : String(x));
  return v === null || v === undefined || v === "" ? "skipped"
    : Array.isArray(v) ? (v.length ? v.map(one).join(", ") : "none")
    : one(v);
};

/** Answers are positional with a parallel `notes` array, so old sessions still read. */
export function readAnswer(it: Item, i: number): { value: any; note?: string } {
  const a = it.data?.answers?.[i] ?? null;
  return { value: a && typeof a === "object" && !Array.isArray(a) && "value" in a ? a.value : a, note: it.data?.notes?.[i] || undefined };
}

const NOTE_HINT = "Add a note";

function Note({ q, a, onChange, onCommit, showAll, hideAll }: {
  q: Q; a: Ans; onChange: (text: string) => void; onCommit: () => void;
  showAll: boolean; hideAll: () => void;
}) {
  const ref = useRef<HTMLInputElement>(null);
  const [open, setOpen] = useState(showAll || a.note.text.length > 0);
  const armed = showAll || open;
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6, marginTop: 2 }}>
      {(armed || a.note.text) ? (
        <>
          <Input ref={ref} value={a.note.text} placeholder={q.note_placeholder ?? "Anything to add? (optional)"}
            onChange={(e) => onChange(e.currentTarget.value)}
            onFocus={() => { if (!armed) setOpen(true); }}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); onCommit(); } }}
            style={{ height: 28, fontSize: 12 }} />
          {showAll ? <span style={{ fontSize: 10.5, color: "var(--dim)", marginTop: -2 }}>Enter to continue</span>
            : <button type="button" className="text-button" style={{ fontSize: 11, alignSelf: "flex-start", color: "var(--mut3)" }} onClick={hideAll}>Hide note</button>}
        </>
      ) : (
        <button type="button" className="text-button" style={{ display: "inline-flex", alignItems: "center", fontSize: 11.5, color: "var(--mut3)", alignSelf: "flex-start" }} onClick={() => { setOpen(true); setTimeout(() => ref.current?.focus(), 20); }}>
          {NOTE_HINT}
        </button>
      )}
    </div>
  );
}

/**
 * The pencil that opens one chosen option's extension. It sits at the far end of
 * the row it belongs to, next to the number key, so which option it extends
 * stays obvious — on a multi question with two picks that is the only thing the
 * per-question note can't say. It stops the row's own click, which on a
 * single-choice question would otherwise re-pick the option behind the pencil.
 */
function OptNotePencil({ label, onOpen }: { label: string; onOpen: () => void }) {
  return (
    <IconButton className="opt-note" label={label} onClick={(e) => { e.stopPropagation(); onOpen(); }}>
      <Pencil size={12} />
    </IconButton>
  );
}

/** The box itself, under the row its pencil belongs to. */
function OptNoteBox({ i, a, onChange }: { i: number; a: Ans; onChange: (i: number, text: string) => void }) {
  const on = a.optNotes?.[i] ?? "";
  return (
    <div onClick={(e) => e.stopPropagation()}>
      {/* Only mounted once the pencil is pressed, so with nothing in it yet:
          the autofocus lands on the box that click asked for. */}
      <Input autoFocus={!on} value={on} placeholder="Extend this… (optional)"
        onChange={(e) => onChange(i, e.currentTarget.value)}
        onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }}
        style={{ height: 26, fontSize: 12 }} />
    </div>
  );
}

function Resolved({ it }: { it: Item }) {
  const qs: Q[] = it.data?.questions ?? [];
  const [open, setOpen] = useState(false);
  const one = qs.length === 1;
  const first = readAnswer(it, 0);
  const notes: string[] = (it.data?.notes ?? []).map((n: any) => (typeof n === "string" ? n : ""));
  if (it.data?.dismissed) {
    const why = typeof it.data.note === "string" ? it.data.note : "";
    return (
      <div className="resolved"><span style={{ color: "var(--mut)", display: "flex" }}><X size={12} /></span>Questions dismissed{why ? ` · ${why}` : " · the agent will use its judgment"}</div>
    );
  }
  // Every question skipped is not an answer, so it doesn't read as one.
  const skips = qs.map((_, i) => wasSkipped(it, i));
  if (skips.every(Boolean)) {
    return (
      <div className="resolved"><span style={{ color: "var(--mut)", display: "flex" }}><Minus size={12} /></span>Skipped · the agent will use its judgment</div>
    );
  }
  const nSkipped = skips.filter(Boolean).length;
  if (one) return (
    <div className="resolved">
      <span style={{ color: "var(--mut2)", display: "flex" }}><Check size={12} /></span>Answered · {showAnswer(first.value)}
      {first.note && <span style={{ color: "var(--mut3)" }}> — “{first.note}”</span>}
    </div>
  );
  const noted = notes.filter(Boolean).length;
  return (
    <div>
      <Pressable className="resolved" onClick={() => setOpen(!open)} style={{ cursor: "default" }}>
        <span style={{ color: "var(--mut2)", display: "flex" }}><Check size={12} /></span>Answered {qs.length} questions{noted ? ` · ${noted} with a note` : ""}{nSkipped ? ` · ${nSkipped} skipped` : ""}{it.data?.title ? ` · ${it.data.title}` : ""}
        <span style={{ color: "var(--dim)" }}>{open ? "hide" : "show"}</span>
      </Pressable>
      {open && (
        <div style={{ margin: "6px 0 2px 22px", display: "grid", gridTemplateColumns: "auto 1fr", gap: "4px 14px", fontSize: 12 }}>
          {qs.map((q, i) => {
            const r = readAnswer(it, i);
            return [
              <span key={"q" + i} style={{ color: "var(--mut3)" }}><MdInline text={q.header || q.question} /></span>,
              <span key={"a" + i} style={{ color: "#d4d4d8" }}>
                {showAnswer(r.value)}
                {r.note && <span style={{ color: "var(--mut3)" }}> — “{r.note}”</span>}
              </span>,
            ];
          })}
        </div>
      )}
    </div>
  );
}

export function Question({ it, task }: { it: Item; task: TaskSummary }) {
  const qs: Q[] = it.data?.questions ?? [];
  const [page, setPage] = useState(0);
  const [ans, setAns] = useState<Ans[]>(() => qs.map(initial));
  const [err, setErr] = useState<string | null>(null);
  const [dismissing, setDismissing] = useState(false);
  const [note, setNote] = useState("");
  const [saving, setSaving] = useState(false);
  // Which option's extension box is open, by option index. Only an empty box
  // needs remembering: a box with text in it stays open on its own, since the
  // note itself is what says so.
  const [optOpen, setOptOpen] = useState<number[]>([]);
  const otherRef = useRef<HTMLInputElement>(null);
  // The last body handed to the backend, so we only write real changes.
  const saved = useRef<string | null>(null);
  // `ans` is built from `qs` (`qs.map(initial)`, and `restore` maps over the
  // same list), so the two are the same length and read positionally.
  const answered = useMemo(() => ans.map((a, i) => !valid(qs[i]!, a) && (a.skipped || value(qs[i]!, a) !== null)), [ans]);
  const done = !!(it.data?.answers || it.data?.dismissed);

  // An item already on disk wins over the local state: re-answering replaces
  // the saved form, and a chat opened in two windows shares one draft.
  useEffect(() => {
    if (done) {
      setSaving(false);
      void api.draftSet(it.id, "").catch(() => {});
    }
  }, [done, it.id]);

  // Load the saved form once, then persist every change (debounced) so a close
  // or a switch to another chat never loses an answer in progress.
  useEffect(() => {
    if (done) return;
    let live = true;
    api.draftGet(it.id).then((raw) => {
      const d = raw && restore(raw, qs);
      if (!live || !d) return;
      setAns((x) => d.ans.map((a, i) => (a.sel.length || a.otherOn || a.multi || a.text || a.bool !== null || a.skipped || a.note.text || hasOptNote(a) ? a : x[i] ?? a)));
      setPage(d.page);
      setDismissing(d.dismissing);
      setNote(d.note);
    }).catch(() => {});
    return () => { live = false; };
  }, [it.id]); // eslint-disable-line react-hooks/exhaustive-deps

  // The form as of the latest render, for the unmount flush below. Kept in a
  // ref on purpose: the flush effect must run its cleanup ONLY when the card
  // goes away, but it has to see the newest value, not the value from whenever
  // it last ran. Reading it out of state would force the deps that reintroduce
  // the per-keystroke write this is meant to avoid.
  const latest = useRef({ page, ans, dismissing, note });
  latest.current = { page, ans, dismissing, note };

  useEffect(() => {
    if (done) return;
    const body = draftOf({ page, ans, dismissing, note });
    if (body === saved.current) return;
    setSaving(true);
    const t = setTimeout(() => {
      saved.current = body;
      setSaving(false);
      void api.draftSet(it.id, body ?? "").catch(() => {});
    }, 250);
    return () => clearTimeout(t);
  }, [page, ans, dismissing, note, done, it.id]);

  // Switching away unmounts this card, so flush the last change now: the
  // debounce above would otherwise drop whatever was typed in the last 250ms.
  // Empty deps, deliberately: with the form state in them this cleanup ran on
  // every keystroke and wrote to disk on every keystroke, which is what the
  // 250ms debounce exists to prevent.
  useEffect(() => () => {
    const body = draftOf(latest.current);
    if (body !== saved.current) void api.draftSet(it.id, body ?? "").catch(() => {});
  }, [it.id]);

  // Moving to another question drops the open extension boxes: they belonged to
  // the page just left, and their text is in the draft either way. Above the
  // early returns below, because it is a hook and they are not.
  useEffect(() => { setOptOpen([]); }, [page]);

  if (done) return <Resolved it={it} />;
  const sourceQ = qs[page];
  if (!sourceQ) return null;
  const a = ans[page];
  // The answer is parallel to the question, so this page has one whenever the
  // question above was found. Mirrors that guard: nothing to draw beats a throw.
  if (!a) return null;
  // A single-choice question can be widened for this answer without changing
  // the agent's stored question or affecting any other page.
  const q: Q = sourceQ.type === "single" && a.multi ? { ...sourceQ, type: "multi" } : sourceQ;
  const opts = q.options ?? [];
  // A choice question always offers a free-text box: someone with a fourth
  // option shouldn't need a second form to say so.
  const other = q.type === "single" || q.type === "multi";
  const upd = (patch: Partial<Ans>) => { setErr(null); setAns((x) => x.map((v, j) => (j === page ? { ...v, ...patch, skipped: false } : v))); };
  const pick = (i: number) => {
    if (q.type === "single") upd({ sel: [i], otherOn: false });
    else upd({ sel: a.sel.includes(i) ? a.sel.filter((x) => x !== i) : [...a.sel, i] });
  };
  const toggleOther = () => {
    upd(q.type === "single" ? { otherOn: true, sel: [] } : { otherOn: !a.otherOn });
    setTimeout(() => otherRef.current?.focus(), 20);
  };
  const setNoteText = (text: string) => upd({ note: { on: true, text } });
  const setOptNote = (i: number, text: string) => upd({ optNotes: { ...a.optNotes, [i]: text } });
  const clearNote = () => { setErr(null); setAns((x) => x.map((v, j) => (j === page ? { ...v, note: blankNote() } : v))); };
  const respond = (response: unknown) => api.respond(task.id, it.id, response).catch((e) => flash(String(e)));
  // The skipped list is sent explicitly: an answer alone can't tell a skip from
  // an optional field the user had no opinion on, and those mean different
  // things to the agent.
  // `list` is `ans` with at most one page patched, so it stays the length of
  // `qs` and the two are read positionally.
  const submit = (list: Ans[]) => respond({
    answers: qs.map((qq, i) => entry(qq.type === "single" && list[i]!.multi ? { ...qq, type: "multi" } : qq, list[i]!).value),
    notes: qs.map((qq, i) => entry(qq, list[i]!).note ?? ""),
    skipped: list.flatMap((a, i) => (a.skipped ? [i] : [])),
  });
  const next = (list = ans) => {
    const e = valid(q, list[page]!);
    if (e) return setErr(e);
    if (page < qs.length - 1) return setPage(page + 1);
    // `firstBad` came out of this very list, so it names a question and its answer.
    const firstBad = qs.findIndex((qq, i) => valid(qq, list[i]!));
    if (firstBad >= 0) { setPage(firstBad); return setErr(valid(qs[firstBad]!, list[firstBad]!)); }
    submit(list);
  };
  const skip = () => {
    const list = ans.map((v, j) => (j === page ? { ...v, skipped: true } : v));
    setAns(list);
    setErr(null);
    next(list);
  };
  // Any question can be skipped, required or not: "I don't want to answer this"
  // is a real answer. `S` leaves the page the same way the button does.
  const canSkip = !a.skipped;
  const many = qs.length > 8;
  const choiceKeys = q.type === "single" || q.type === "multi" ? opts.length + (other ? 1 : 0) : q.type === "confirm" ? 2 : 0;
  // Every question can take a note, even without asking: it saves a followup.
  const noteField = <Note key={page} q={q} a={a} showAll={!!q.note} onChange={setNoteText} onCommit={() => next()} hideAll={clearNote} />;
  const footHint = [choiceKeys ? `1-${Math.min(9, choiceKeys)} to choose` : null, "Enter to continue", "S to skip"].filter(Boolean).join(" · ");

  return (
    <div
      className="card-q" tabIndex={0} data-pending-question={it.id}
      onKeyDown={(e) => {
        const tag = (e.target as HTMLElement).tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || dismissing) return;
        const n = parseInt(e.key);
        if (n >= 1 && n <= Math.min(9, choiceKeys)) {
          e.preventDefault();
          if (q.type === "confirm") upd({ bool: n === 1 });
          else if (n === opts.length + 1 && other) toggleOther();
          else pick(n - 1);
        }
        if (e.key === "Enter") { e.preventDefault(); next(); }
        if ((e.key === "s" || e.key === "S") && canSkip) { e.preventDefault(); skip(); }
        if (e.key === "ArrowLeft" && page > 0) setPage(page - 1);
        if (e.key === "ArrowRight" && page < qs.length - 1) next();
      }}
    >
      {it.data?.from && <div className="fromsub">{it.data.from} subagent asks · the main agent couldn't answer this one</div>}
      {page === 0 && (it.data?.title || it.data?.intro) && (
        <div style={{ paddingBottom: 2 }}>
          {it.data.title && <div className="md qtitle" style={{ fontWeight: 600, fontSize: 14 }}><MdInline text={it.data.title} /></div>}
          {it.data.intro && <div className="md qintro" style={{ color: "var(--mut)", marginTop: 3, lineHeight: 1.5 }}><MdInline text={it.data.intro} /></div>}
        </div>
      )}
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <div style={{ width: 16, height: 16, borderRadius: "50%", display: "grid", placeItems: "center", background: "rgba(167,139,250,0.14)", color: "var(--violet)", fontSize: 10, fontWeight: 700 }}>?</div>
        {q.header ? <span className="qchip">{q.header}</span> : <span style={{ fontWeight: 600, color: "var(--fg3)" }}>Question</span>}
        {qs.length > 1 && <span style={{ color: "var(--mut3)" }}>{page + 1} of {qs.length}</span>}
        {q.required === false && <span style={{ fontSize: 10.5, color: "var(--dim)" }}>optional</span>}
        {a.skipped && <span style={{ fontSize: 10.5, color: "var(--mut3)" }}>skipped — the agent will decide</span>}
        {q.note && <span style={{ fontSize: 10.5, color: "var(--dim)" }}>note</span>}
        <div style={{ flex: 1 }} />
        {saving && !dismissing && <span style={{ fontSize: 10, color: "var(--dim)" }}>saving…</span>}
        {qs.length > 1 && !many && qs.map((_, j) => (
          <Pressable key={j} aria-label={`Question ${j + 1}`} aria-current={j === page ? "step" : undefined} onClick={() => setPage(j)} style={{ width: j === page ? 14 : 5, height: 5, borderRadius: 3, background: j === page ? "var(--violet)" : answered[j] ? "rgba(167,139,250,0.45)" : "rgba(255,255,255,0.15)", transition: "all .2s cubic-bezier(.32,.72,0,1)" }} />
        ))}
      </div>
      {many && <div className="bar3" style={{ marginTop: -4 }}><div style={{ width: `${(answered.filter(Boolean).length / qs.length) * 100}%`, background: "var(--violet)" }} /></div>}
      <div key={page} style={{ display: "flex", flexDirection: "column", gap: 10, animation: "olIn .2s cubic-bezier(.22,1,.36,1) both" }}>
        <div>
          <div className="md qtext" style={{ fontWeight: 500, fontSize: 13.5, padding: "0 2px", lineHeight: 1.45 }}><MdInline text={q.question} /></div>
          {q.description && <div className="md qtext" style={{ fontSize: 12, color: "var(--mut2)", padding: "3px 2px 0", lineHeight: 1.5 }}><MdInline text={q.description} /></div>}
        </div>

        {(q.type === "single" || q.type === "multi") && (
          <>
            {/* No cap of its own, and no inner scrolling: a choice is read before it
                is picked, and the descriptions are the half that says what the label
                cannot. Capping this list at a fixed height cut a description off
                mid-sentence with nothing to show it continued — it looked like the
                card was hiding text rather than offering more. The card grows to fit
                and the transcript scrolls, which it already does, with the floating
                composer's height kept clear of the end of it. */}
            <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
              {sourceQ.type === "single" && <Button variant="ghost" style={{ height: 24, alignSelf: "flex-start", fontSize: 11 }} aria-pressed={!!a.multi} onClick={() => upd({ multi: !a.multi, ...(a.multi ? { sel: a.sel.slice(0, 1) } : {}) })}>{a.multi ? "Use single choice" : "Allow multiple"}</Button>}
              {q.type === "multi" && <div style={{ fontSize: 11, color: "var(--dim)", padding: "0 2px" }}>Pick any{q.min ? ` · at least ${q.min}` : ""}{q.max ? ` · at most ${q.max}` : ""}</div>}
              {opts.map((o, j) => {
                const on = a.sel.includes(j);
                // The extension box is open when its pencil was pressed, or when
                // the draft came back with something in it.
                const ext = on && (optOpen.includes(j) || !!a.optNotes?.[j]?.trim());
                return (
                  <ChoiceRow key={j} variant={q.type === "multi" ? "check" : "radio"} selected={on} style={on ? { flexDirection: "column", alignItems: "stretch", gap: 6 } : undefined} onClick={() => pick(j)}>
                    <div style={{ display: "flex", alignItems: "center", gap: 7, width: "100%" }}>
                      <div style={{ flex: 1, minWidth: 0 }}>
                        <div style={{ display: "flex", alignItems: "center", gap: 7, fontWeight: 500 }}><MdInline text={clean(o.label)} />{isRec(o) && <span style={{ fontSize: 10.5, color: "var(--mut2)" }}>Recommended</span>}</div>
                        {o.description && <div className="md qtext" style={{ fontSize: 11.5, color: "var(--mut2)", marginTop: 2, lineHeight: 1.45 }}><MdInline text={o.description} /></div>}
                      </div>
                      {on && !ext && <OptNotePencil label={`Extend “${clean(o.label)}”`} onOpen={() => setOptOpen((x) => (x.includes(j) ? x : [...x, j]))} />}
                      {j < 9 && <Kbd className="dim">{`${j + 1}`}</Kbd>}
                    </div>
                    {ext && <OptNoteBox i={j} a={a} onChange={setOptNote} />}
                  </ChoiceRow>
                );
              })}
              {other && (
                <>
                  <ChoiceRow variant={q.type === "multi" ? "check" : "radio"} selected={a.otherOn} style={{ alignItems: "center", padding: "0 10px", height: 36 }} onClick={toggleOther}>
                    <Input ref={otherRef} className="" value={a.other} onClick={(e) => e.stopPropagation()} onFocus={() => !a.otherOn && upd(q.type === "single" ? { otherOn: true, sel: [] } : { otherOn: true })} onChange={(e) => upd({ other: e.currentTarget.value, otherOn: true, ...(q.type === "single" ? { sel: [] } : {}) })} onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); next(); } }} placeholder={q.type === "multi" ? "Your own choices…" : "Your own choice…"} style={{ flex: 1, minWidth: 0, border: 0, outline: 0, background: "transparent", color: "#f4f4f5", font: "inherit" }} />
                    {/* No pencil here: the user writes the choice themselves, so a
                        box beside it would only repeat what they already said. The
                        question's own note is where that goes. */}
                    {opts.length < 9 && <Kbd className="dim">{`${opts.length + 1}`}</Kbd>}
                  </ChoiceRow>
                </>
              )}
            </div>
            {noteField}
          </>
        )}

        {q.type === "text" && (
          <TextArea autoFocus rows={3} value={a.text} placeholder={q.placeholder ?? "Type your answer"} onChange={(e) => upd({ text: e.currentTarget.value })}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); next(); } }}
            style={{ height: "auto", padding: "8px 10px", resize: "vertical", lineHeight: 1.5 }} />
        )}

        {q.type === "number" && (
          <Input autoFocus type="number" min={q.min} max={q.max} value={a.text} placeholder={q.placeholder ?? (q.min !== undefined && q.max !== undefined ? `${q.min}-${q.max}` : "0")} onChange={(e) => upd({ text: e.currentTarget.value })}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); next(); } }} style={{ width: 180 }} />
        )}

        {q.type === "confirm" && (
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            <div style={{ display: "flex", gap: 6 }}>
              {[true, false].map((b, j) => (
                <ChoiceRow key={j} variant="radio" selected={a.bool === b} style={{ flex: 1, alignItems: "center" }} onClick={() => upd({ bool: b })}>
                  <span style={{ fontWeight: 500, flex: 1 }}>{b ? "Yes" : "No"}</span>
                  <Kbd className="dim">{`${j + 1}`}</Kbd>
                </ChoiceRow>
              ))}
            </div>
            {noteField}
          </div>
        )}
      </div>

      {dismissing && (
        <Input autoFocus value={note} onChange={(e) => setNote(e.currentTarget.value)} placeholder="Optional: tell the agent why, or what to do instead"
          onKeyDown={(e) => { if (e.key === "Enter") respond({ dismissed: true, note }); }} />
      )}

      <div style={{ display: "flex", alignItems: "center", gap: 6, paddingTop: 2 }}>
        <span style={{ fontSize: 11, color: err ? "#ff8a8a" : "var(--dim)" }}>
          {err ?? (dismissing ? "Confirm below to dismiss" : footHint)}
        </span>
        <div style={{ flex: 1 }} />
        {dismissing ? (
          <>
            <Button variant="ghost" style={{ height: 26 }} onClick={() => { setDismissing(false); setNote(""); }}>Cancel</Button>
            <Button variant="ghost" style={{ height: 26, color: "#ff8a8a" }} onClick={() => respond({ dismissed: true, note })}>Confirm dismiss</Button>
          </>
        ) : (
          <Button variant="ghost" style={{ height: 26 }} onClick={() => setDismissing(true)}>Dismiss…</Button>
        )}
        {canSkip && <Button variant="ghost" style={{ height: 26 }} onClick={skip}>Skip</Button>}
        {a.skipped && <Button variant="ghost" style={{ height: 26 }} onClick={() => upd({ skipped: false })}>Undo skip</Button>}
        {page > 0 && !dismissing && <Button variant="ghost" style={{ height: 26 }} onClick={() => setPage(page - 1)}>Back</Button>}
        {!dismissing && <Button variant={!valid(q, a) ? "primary" : "default"} style={{ height: 26, opacity: valid(q, a) ? 0.45 : 1 }} onClick={() => next()}>{page < qs.length - 1 ? "Next" : qs.length > 1 ? `Submit ${qs.length}` : "Submit"}</Button>}
      </div>
    </div>
  );
}
