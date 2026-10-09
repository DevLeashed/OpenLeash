// A styled, searchable dropdown (the native <select> popup can't be themed on Windows).
import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { zoom } from "../../store";
import { I } from "../icons";
import { Check } from "lucide-react";
import { Input, MenuRow } from "./Controls";
import { useEscapeClose } from "./escape";

export interface Opt { value: string; label: string; hint?: string; group?: string; icon?: React.ReactNode }

/** Options a long list mounts at once. A search lifts the cap, so a cap can never
 *  hide the row someone is typing to reach. */
const ROW_CAP = 300;

export function Dropdown({ value, options, onChange, placeholder = "Select…", search = true, showHint = true, style }: { value: string; options: Opt[]; onChange: (v: string) => void; placeholder?: string; search?: boolean; showHint?: boolean; style?: React.CSSProperties }) {
  const listId = useId();
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const [hi, setHi] = useState(0);
  const btn = useRef<HTMLDivElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: 0, top: 0, width: 0, up: false, max: 300 });
  const cur = options.find((o) => o.value === value);
  const ql = q.trim().toLowerCase();
  // Shown and selectable are the same list: keyboard and mouse agree, and typing
  // reaches every option rather than just the head of an unfiltered list.
  const shown = useMemo(() => (ql ? options.filter((o) => (o.label + " " + (o.hint ?? "") + " " + (o.group ?? "")).toLowerCase().includes(ql)) : options.slice(0, ROW_CAP)), [ql, options]);

  useLayoutEffect(() => {
    if (!open || !btn.current) return;
    // Coordinates are in unzoomed CSS pixels; the app root is zoomed.
    const z = zoom();
    const r = btn.current.getBoundingClientRect();
    const vw = window.innerWidth / z, vh = window.innerHeight / z;
    const below = vh - r.bottom / z - 12;
    const up = below < 220 && r.top / z > below;
    const width = Math.min(Math.max(r.width / z, 240), vw - 20);
    setPos({ left: Math.max(10, Math.min(r.left / z, vw - width - 10)), top: up ? r.top / z - 6 : r.bottom / z + 6, width, up, max: Math.max(96, Math.min(320, up ? r.top / z - 20 : below)) });
  }, [open]);
  useEffect(() => { if (open) { setQ(""); setHi(Math.max(0, options.findIndex((o) => o.value === value))); } }, [open]);
  // The list starts with scrolling off for a frame: enabling overflow at first paint flashes a scrollbar gutter.
  const [scrolled, setScrolled] = useState(false);
  useEffect(() => {
    if (!open) { setScrolled(false); return; }
    const id = window.setTimeout(() => setScrolled(true), 50);
    return () => window.clearTimeout(id);
  }, [open]);
  useEffect(() => { if (scrolled) list.current?.querySelector(".hi")?.scrollIntoView({ block: "nearest" }); }, [hi, open, scrolled]);
  useEscapeClose(open, () => setOpen(false));

  const pick = (o: Opt | undefined) => { if (!o) return; onChange(o.value); setOpen(false); };
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") { e.preventDefault(); setHi(Math.min(hi + 1, shown.length - 1)); }
    else if (e.key === "ArrowUp") { e.preventDefault(); setHi(Math.max(hi - 1, 0)); }
    else if (e.key === "Enter") { e.preventDefault(); pick(shown[hi]); }
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); setOpen(false); }
  };

  let lastGroup: string | undefined;
  const row = (o: Opt, i: number) => {
    const head = o.group && o.group !== lastGroup ? o.group : null;
    lastGroup = o.group;
    return (
      <div key={o.value + i}>
        {head && <div className="mhead">{head}</div>}
        <MenuRow role="option" aria-selected={o.value === value} className={i === hi ? "hi" : ""} style={{ animationDelay: Math.min(i, 16) * 5 + "ms" }} onMouseMove={() => setHi(i)} onClick={() => pick(o)}>
          {o.icon}
          <span className="dd-option-copy">
            <span className="dd-option-label">{o.label}</span>
            {o.hint && <span className="dd-option-hint">{o.hint}</span>}
          </span>
          <span style={{ width: 12, flex: "none", color: "var(--violet)", display: "flex" }}>{o.value === value ? <Check size={12} strokeWidth={2} /> : null}</span>
        </MenuRow>
      </div>
    );
  };
  return (
    <>
      <div ref={btn} className={"dd" + (open ? " open" : "")} style={style} role="combobox" aria-label={placeholder} aria-expanded={open} aria-haspopup="listbox" aria-controls={open ? listId : undefined} tabIndex={0} onClick={() => setOpen(!open)} onKeyDown={(e) => { if (!open && (e.key === "Enter" || e.key === "ArrowDown")) { e.preventDefault(); setOpen(true); } }}>
        {cur?.icon}
        <span className="ddv">{cur ? cur.label : <span style={{ color: "var(--dim)" }}>{value || placeholder}</span>}</span>
        {showHint && cur?.hint && <span className="ddh">{cur.hint}</span>}
        <span className="ddc">{I.chev()}</span>
      </div>
      {open && createPortal(
        <div style={{ position: "fixed", inset: 0, zoom: zoom(), zIndex: btn.current?.closest(".modal") ? 950 : 80 }}>
          <div style={{ position: "absolute", inset: 0 }} onMouseDown={() => setOpen(false)} />
          <div className={"ddpop" + (pos.up ? " up" : "")} style={{ left: pos.left, width: pos.width, ...(pos.up ? { bottom: `calc(100% - ${pos.top}px)` } : { top: pos.top }) }} onKeyDown={onKey}>
            {search && <Input autoFocus placeholder="Search…" value={q} onChange={(e) => { setQ(e.currentTarget.value); setHi(0); }} style={{ width: "100%", height: 28, marginBottom: 4 }} />}
            <div ref={list} id={listId} role="listbox" aria-label={placeholder} className="ddlist" style={{ maxHeight: pos.max, overflowY: scrolled ? "auto" : "hidden" }} tabIndex={search ? -1 : 0} autoFocus={!search}>
              {shown.map((o, i) => row(o, i))}
              {!ql && options.length > ROW_CAP && <div className="empty">Showing the first {ROW_CAP} of {options.length} · search to narrow it down</div>}
              {!shown.length && <div className="empty">No matches</div>}
            </div>
          </div>
        </div>,
        document.body,
      )}
    </>
  );
}
