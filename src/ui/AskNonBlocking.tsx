// A question the agent asked while it kept working (`ask_nonblocking`).
//
// Same form as the blocking question card and the same answers, but it does not
// belong in the transcript: the run never stopped on it, and the whole point is
// that it stays out of the way while the agent gets on with the work. So it lives
// above the send bar, tied to it, the way the queued-message list does — visible
// without being buried in a feed that is still scrolling.
//
// Three things it has to get right, and all three are easy to get wrong:
//
// 1. It does not block. The card says so on every state, because a card that
//    looks like the blocking one reads as "the agent is stopped until I answer
//    this" — which is a lie, and the user ends up answering a question whose
//    work finished ten minutes ago.
// 2. Open by default. A question that has to be expanded before it can be read
//    is a question that gets skipped, and this card is already competing with
//    the agent's own output for the space above the composer. It arrives
//    expanded; folding is available, un-folding is never required.
// 3. One question per page, like the blocking card. Stacked into a single form
//    it grows without bound — a batch of ten is a wall, and it pushes the
//    composer off the screen.

import { useEffect, useMemo, useRef, useState } from "react";
import { api, Item, TaskSummary } from "../api";
import { inTextField } from "../keys";
import { flash, useStore } from "../store";
import { Check, CircleQuestionMark, X } from "lucide-react";
import { Button, ChoiceRow, Input, Kbd, Pressable, TextArea, Tooltip } from "./primitives";
import { entry, initial, Q, valid, Ans, clean, isRec, draftOf, restore, value, wasSkipped, hasLimit, pickHint, LimitToggle } from "./Question";
import { MdInline } from "./mdx";
import { digitCount, digitTarget, filterEnter, filterOptions, focusRow, isLong, NoMatch, OptionFilter, revealRow } from "./QuestionFilter";

/** The `ask_nonblocking` items of a task, oldest first — the order they were asked. */
export function pendingOf(items: Item[] | undefined): Item[] {
  return (items ?? []).filter((i) => i.kind === "asklater" && !i.data?.answers && !i.data?.dismissed);
}

export function AskNonBlocking({ it, task }: { it: Item; task: TaskSummary }) {
  // Memoised so the array is the same one every render: it is the dependency of
  // the memo below, and a fresh `[]` each render would recompute on every keystroke.
  const qs: Q[] = useMemo(() => it.data?.questions ?? [], [it.data?.questions]);
  const [ans, setAns] = useState<Ans[]>(() => qs.map(initial));
  // Open by default. The card is a thing to deal with whenever, not a thing you
  // have to open first, and the notification that came with it pointed here.
  const [open, setOpen] = useState(true);
  const [page, setPage] = useState(0);
  const [sent, setSent] = useState(false);
  const otherRef = useRef<HTMLInputElement>(null);
  // The filter over a long option list, cleared on every page turn.
  const [query, setQuery] = useState("");
  const filterRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => { setQuery(""); }, [page]);
  const saved = useRef<string | null>(null);
  const done = !!(it.data?.answers || it.data?.dismissed);

  // The same autosave the blocking form does, so a half-typed answer survives a
  // close or a switch to another chat — the run is still going, and so is the
  // user, most likely in another window.
  useEffect(() => {
    if (done) {
      void api.draftSet(it.id, "").catch(() => {});
      return;
    }
    const body = draftOf({ page, ans, dismissing: false, note: "" });
    if (body === saved.current) return;
    const t = setTimeout(() => {
      saved.current = body;
      void api.draftSet(it.id, body ?? "").catch(() => {});
    }, 250);
    return () => clearTimeout(t);
  }, [ans, page, done, it.id]);

  useEffect(() => {
    if (done) return;
    let live = true;
    api.draftGet(it.id).then((raw) => {
      const d = raw && restore(raw, qs);
      if (!live || !d) return;
      setAns((x) => d.ans.map((a, i) => (a.skipped || a.sel.length || a.otherOn || a.multi || a.unlimited || a.text || a.bool !== null ? a : x[i] ?? a)));
      setPage(d.page);
    }).catch(() => {});
    return () => { live = false; };
  }, [it.id]); // eslint-disable-line react-hooks/exhaustive-deps

  // `list` is `ans` with at most one page patched, so it stays the length of
  // `qs` and the two are read positionally.
  const answer = (list: Ans[], dismissed = false) => {
    setSent(true);
    const response = dismissed ? { dismissed: true } : {
      answers: qs.map((q, i) => entry(q.type === "single" && list[i]!.multi ? { ...q, type: "multi" } : q, list[i]!).value),
      notes: qs.map((q, i) => entry(q, list[i]!).note ?? ""),
      skipped: list.flatMap((a, i) => (a.skipped ? [i] : [])),
    };
    api.answerNonBlocking(task.id, it.id, response).catch((e: unknown) => { setSent(false); flash(String(e)); });
  };

  // `ans` is built from `qs` (`qs.map(initial)`, and `restore` maps over the
  // same list), so the two are the same length and read positionally.
  const answered = useMemo(() => ans.map((a, i) => !valid(qs[i]!, a) && (a.skipped || value(qs[i]!, a) !== null)), [ans, qs]);

  if (!qs.length) return null;

  // The folded card shows one line. It exists so the card can get out of the
  // way of a long run, not as a gate you pass through to read the question.
  if (!open) {
    const ready = answered.filter(Boolean).length;
    return (
      <div className="asklater">
        <Pressable className="al-head" onClick={() => setOpen(true)} aria-expanded={false} aria-label="Expand question">
          <span className="al-icon"><CircleQuestionMark size={13} strokeWidth={1.8} /></span>
          <span className="al-title">
            <span className="al-line">
              {it.data?.title ? <span className="md al-folded"><MdInline text={it.data.title} /></span>
                : <span className="md al-folded"><MdInline text={qs[0]?.question ?? "A question"} /></span>}
              {qs.length > 1 && <span className="al-n">{ready}/{qs.length} answered</span>}
            </span>
            <span className="al-sub">
              <span className="al-noblock"><CircleQuestionMark size={10.5} strokeWidth={1.8} /> not waiting on you</span>
              {ready > 0 ? ` · ${ready} answered` : " · answer now or leave it"}
            </span>
          </span>
          <Button variant="ghost" className="qbtn" onClick={() => setOpen(true)}>Open</Button>
        </Pressable>
      </div>
    );
  }

  const sourceQ = qs[page];
  if (!sourceQ) return null;
  const a = ans[page];
  // The answer is parallel to the question, so this page has one whenever the
  // question above was found. Mirrors that guard: nothing to draw beats a throw.
  if (!a) return null;
  // Widen only this answer; the agent's question remains single-choice.
  const q: Q = sourceQ.type === "single" && a.multi ? { ...sourceQ, type: "multi" } : sourceQ;
  const opts = q.options ?? [];
  // The bounds in force for this answer: none once the limits are removed.
  const lo = a.unlimited ? undefined : q.min;
  const hi = a.unlimited ? undefined : q.max;
  // A choice question always offers a free-text box: someone with a fourth
  // option shouldn't need a second form to say so.
  const other = q.type === "single" || q.type === "multi";
  const long = other && isLong(opts);
  const shown = long ? filterOptions(opts, query) : opts.map((_, i) => i);
  const hiddenPicks = a.sel.filter((i) => !shown.includes(i)).length;
  const upd = (patch: Partial<Ans>) => setAns((x) => x.map((v, j) => (j === page ? { ...v, ...patch, skipped: false } : v)));
  const pick = (i: number) => {
    if (q.type === "single") upd({ sel: [i], otherOn: false });
    else upd({ sel: a.sel.includes(i) ? a.sel.filter((x) => x !== i) : [...a.sel, i] });
  };
  const toggleOther = () => {
    upd(q.type === "single" ? { otherOn: true, sel: [] } : { otherOn: !a.otherOn });
    setTimeout(() => otherRef.current?.focus(), 20);
  };
  const ownAnswer = (text: string) => upd({ other: text, otherOn: true, ...(q.type === "single" ? { sel: [] } : {}) });
  // The digits reach the rows as shown, so a filtered list renumbers from 1.
  const choiceKeys = other ? digitCount(shown, true) : q.type === "confirm" ? 2 : 0;
  const many = qs.length > 8;

  // Nothing here is a gate, and that is the whole point of this card: the run
  // carried on without the answer, so the form cannot then refuse to hand over
  // what it does have. An empty question is recorded as unanswered and the agent
  // is told exactly that, which is a truthful report; blocking the send would
  // mean the card contradicted its own hint ("answer what you have, the rest is
  // optional") and taught the user that ignoring it is the only way out.
  //
  // `valid` is therefore only used for the paging hint, never to veto a send.
  const send = () => answer(ans);
  const next = () => {
    if (page < qs.length - 1) return setPage(page + 1);
    send();
  };
  const skip = () => {
    const list = ans.map((v, j) => (j === page ? { ...v, skipped: true } : v));
    setAns(list);
    if (page < qs.length - 1) setPage(page + 1);
    else answer(list);
  };
  // What is worth pointing at on this page: something is filled in, or the
  // question is optional and nobody had an opinion, or the user said to skip it.
  const pageDone = a.skipped || !valid(q, a) || q.required === false;
  const footHint = answered.every(Boolean)
    ? "ready to send"
    : pageDone
      ? `${answered.filter(Boolean).length} of ${qs.length} answered — the rest you can leave to the agent`
      : `this one is empty · ${answered.filter(Boolean).length} of ${qs.length} answered`;
  const keysHint = [choiceKeys ? `1-${Math.min(9, choiceKeys)} to choose` : null, long ? "/ to filter" : null, "Enter to continue", "S to skip"].filter(Boolean).join(" · ");

  return (
    <div className="asklater open" role="group" aria-label="Question the agent asked without waiting">
      <div className="al-head static">
        <span className="al-icon"><CircleQuestionMark size={13} strokeWidth={1.8} /></span>
        <span className="al-title">
          <span className="al-line">
            {it.data?.title ? <span className="md al-folded"><MdInline text={it.data.title} /></span>
              : <span className="md al-folded"><MdInline text={qs[0]?.question ?? "A question"} /></span>}
          </span>
          <span className="al-sub"><span className="al-noblock"><CircleQuestionMark size={10.5} strokeWidth={1.8} /> not waiting on you</span> · answer now or leave it</span>
        </span>
        <div style={{ flex: 1 }} />
        {qs.length > 1 && <span className="al-n">{page + 1} of {qs.length}</span>}
        {!many && qs.length > 1 && (
          <span className="al-dots">
            {qs.map((_, j) => (
              <Pressable key={j} className={"al-dot" + (j === page ? " on" : "")} aria-label={`Question ${j + 1}`} aria-current={j === page ? "step" : undefined} onClick={() => setPage(j)} />
            ))}
          </span>
        )}
        {many && <span className="al-n">{answered.filter(Boolean).length}/{qs.length}</span>}
        <Tooltip content="Fold this back into one line without answering — the agent decides for itself">
          <Button variant="ghost" className="qbtn al-fold" onClick={() => setOpen(false)}>Fold</Button>
        </Tooltip>
      </div>
      {page === 0 && it.data?.intro && <div className="md al-intro"><MdInline text={it.data.intro} /></div>}

      <div className="al-body" key={page} tabIndex={0}
        onKeyDown={(e) => {
          if (a.skipped || inTextField(e.target)) return;
          const n = parseInt(e.key);
          if (n >= 1 && n <= Math.min(9, choiceKeys)) {
            e.preventDefault();
            if (q.type === "confirm") upd({ bool: n === 1 });
            else {
              const t = digitTarget(shown, n, other);
              if (t && "other" in t) toggleOther();
              else if (t) { pick(t.option); revealRow(listRef.current, t.option); }
            }
          }
          if (e.key === "/" && long) { e.preventDefault(); filterRef.current?.focus(); }
          if (e.key === "Enter") { e.preventDefault(); next(); }
          if ((e.key === "s" || e.key === "S") && !a.skipped) { e.preventDefault(); skip(); }
          if (e.key === "ArrowLeft" && page > 0) setPage(page - 1);
          if (e.key === "ArrowRight" && page < qs.length - 1) next();
        }}>
        <div className="al-qhead">
          {q.header ? <span className="qchip">{q.header}</span> : <span className="al-qkind">Question</span>}
          <span className="md al-qtext"><MdInline text={q.question} /></span>
        </div>
        {q.description && <div className="md al-qdesc"><MdInline text={q.description} /></div>}

        {q.type === "multi" && (
          <div className="al-row">
            <div className="al-pick">{pickHint(q, a)}</div>
            {hasLimit(q) && <LimitToggle a={a} onToggle={() => upd({ unlimited: !a.unlimited })} className="qbtn" />}
          </div>
        )}

        {(q.type === "single" || q.type === "multi") && (
          <div className="al-choices">
            {sourceQ.type === "single" && <Button variant="ghost" className="qbtn" aria-pressed={!!a.multi} onClick={() => upd({ multi: !a.multi, ...(a.multi ? { sel: a.sel.slice(0, 1) } : {}) })}>{a.multi ? "Use single choice" : "Allow multiple"}</Button>}
            {long && (
              <OptionFilter ref={filterRef} query={query} onChange={setQuery} shown={shown.length} total={opts.length} hiddenPicks={hiddenPicks}
                onDown={() => { const first = shown[0]; if (first !== undefined) focusRow(listRef.current, first); }}
                onEnter={() => filterEnter({ query, shown, selected: a.sel, pick, ownAnswer, next })} />
            )}
            {/* Bounded only when long: this card sits above the composer, and
                thirty rows would push the send bar off the screen. */}
            <div ref={listRef} className={long ? "al-choices opt-scroll" : "al-choices"}>
            {long && !shown.length && <NoMatch query={query} />}
            {shown.map((j, p) => {
              const o = opts[j]!;
              const on = a.sel.includes(j);
              return (
                <ChoiceRow key={j} variant={q.type === "multi" ? "check" : "radio"} selected={on} onClick={() => pick(j)}>
                  <div className="al-choice" data-opt={j}>
                    <div className="al-choicelab">
                      <MdInline text={clean(o.label)} />
                      {isRec(o) && <span className="al-rec">Recommended</span>}
                    </div>
                    {o.description && <div className="md al-qdesc"><MdInline text={o.description} /></div>}
                  </div>
                  {p < 9 && <Kbd className="dim">{`${p + 1}`}</Kbd>}
                </ChoiceRow>
              );
            })}
            </div>
            {other && (
              <ChoiceRow variant={q.type === "multi" ? "check" : "radio"} selected={a.otherOn} style={{ alignItems: "center", padding: "0 10px", height: 34 }} onClick={toggleOther}>
                <Input ref={otherRef} className="" value={a.other} onClick={(e) => e.stopPropagation()} onFocus={() => !a.otherOn && upd(q.type === "single" ? { otherOn: true, sel: [] } : { otherOn: true })} onChange={(e) => upd({ other: e.currentTarget.value, otherOn: true, ...(q.type === "single" ? { sel: [] } : {}) })} onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); next(); } }} placeholder={q.type === "multi" ? "Your own choices…" : "Your own choice…"} style={{ flex: 1, minWidth: 0, border: 0, outline: 0, background: "transparent", color: "var(--fg)", font: "inherit" }} />
                {shown.length < 9 && <Kbd className="dim">{`${shown.length + 1}`}</Kbd>}
              </ChoiceRow>
            )}
          </div>
        )}

        {q.type === "confirm" && (
          <div className="al-choices">
            {[true, false].map((b, j) => (
              <ChoiceRow key={j} variant="radio" selected={a.bool === b} style={{ alignItems: "center" }} onClick={() => upd({ bool: b })}>
                <span style={{ fontWeight: 500, flex: 1 }}>{b ? "Yes" : "No"}</span>
                <Kbd className="dim">{`${j + 1}`}</Kbd>
              </ChoiceRow>
            ))}
          </div>
        )}

        {q.type === "text" && (
          <TextArea autoFocus rows={3} value={a.text} placeholder={q.placeholder ?? "Type your answer"} onChange={(e) => upd({ text: e.currentTarget.value })}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); next(); } }} />
        )}

        {q.type === "number" && (
          <div className="al-row">
            <Input autoFocus type="number" min={lo} max={hi} value={a.text} placeholder={q.placeholder ?? (lo !== undefined && hi !== undefined ? `${lo}-${hi}` : "0")} onChange={(e) => upd({ text: e.currentTarget.value })}
              onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); next(); } }} style={{ width: 180 }} />
            {hasLimit(q) && <LimitToggle a={a} onToggle={() => upd({ unlimited: !a.unlimited })} className="qbtn" />}
          </div>
        )}

        {/* Every question can take a note, asked for or not: on a card nobody is
            blocked on, a note is the only way to say more than the answer. */}
        <Input value={a.note.text} placeholder={q.note_placeholder ?? "Add a note (optional)"} onChange={(e) => upd({ note: { on: true, text: e.currentTarget.value } })} onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); next(); } }} style={{ height: 28, fontSize: 12 }} />
      </div>

      <div className="al-foot">
        {/* Count first, keys second: what you have done with the form is the
            thing worth reading, and the shortcuts are a footnote on that. */}
        <span className="al-hint">{footHint}</span>
        {keysHint && <span className="al-hint dim al-keyhint">{keysHint}</span>}
        <div style={{ flex: 1 }} />
        <Tooltip content="The agent decides this itself and you won't be asked again">
          <Button variant="ghost" className="qbtn" disabled={sent} onClick={() => answer(ans, true)}>
            <X size={11} /> Skip
          </Button>
        </Tooltip>
        {page > 0 && <Button variant="ghost" className="qbtn" onClick={() => setPage(page - 1)}>Back</Button>}
        {qs.length > 1 && page < qs.length - 1 && <Button variant="ghost" className="qbtn" onClick={skip}>Skip this</Button>}
        {/* Never disabled for an empty page. An all-empty form is a legitimate
            answer — "decide it yourself" — and the Skip button is the explicit
            way to say exactly that, so the send has to be available too. */}
        <Button variant="primary" className="qbtn" disabled={sent} onClick={page < qs.length - 1 ? next : send}>
          <Check size={11} /> {page < qs.length - 1 ? "Next" : "Send answer"}
        </Button>
      </div>
    </div>
  );
}

/**
 * Every unanswered `ask_nonblocking` in this chat, above the composer. Answers
 * land as a note the agent reads on its next request, so a card here outlives
 * the run that asked it — a late answer is still worth sending, and still says
 * so.
 */
export function AskNonBlockingList({ task }: { task: TaskSummary }) {
  // Derived inside the selector, and the previous list handed back while the
  // cards are the same objects: the transcript array is replaced on every
  // streamed frame, and subscribing to it whole re-rendered (and rescanned)
  // this list sixty times a second for cards that had not changed.
  const cache = useRef<{ items: Item[] | undefined; out: Item[] } | null>(null);
  const pending = useStore((s) => {
    const items = s.items[task.id];
    const c = cache.current;
    if (c && c.items === items) return c.out;
    const next = pendingOf(items);
    const out = c && c.out.length === next.length && c.out.every((x, i) => x === next[i]) ? c.out : next;
    cache.current = { items, out };
    return out;
  });
  if (!pending.length) return null;
  return (
    <div className="asklater-stack">
      {pending.map((it) => <AskNonBlocking key={it.id} it={it} task={task} />)}
    </div>
  );
}

/** Re-exported for the tests that pin what the list shows. */
export { wasSkipped };
