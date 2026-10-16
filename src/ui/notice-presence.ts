import { useEffect, useRef, useState } from "react";

// Keep removed cards mounted only for their visual exit; store/backend dismissal
// still happens immediately. Match the CSS duration, with no delay for reduced motion.
export const NOTICE_EXIT_MS = 180;
const exitDelay = () => window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ? 0 : NOTICE_EXIT_MS;

type Entry<T> = { key: string; value: T; exiting: boolean };

export function useNoticePresence<T>(items: T[], keyOf: (item: T) => string): Entry<T>[] {
  const [snapshot, setSnapshot] = useState(items);
  const [entries, setEntries] = useState<Entry<T>[]>(() => items.map((value) => ({ key: keyOf(value), value, exiting: false })));
  if (snapshot !== items) {
    const current = new Map(items.map((value) => [keyOf(value), value]));
    const next = entries.map((entry) => current.has(entry.key)
      ? { key: entry.key, value: current.get(entry.key)!, exiting: false }
      : { ...entry, exiting: true });
    const known = new Set(entries.map((entry) => entry.key));
    next.push(...items.filter((value) => !known.has(keyOf(value))).map((value) => ({ key: keyOf(value), value, exiting: false })));
    setSnapshot(items);
    setEntries(next);
  }
  // Track each exit independently: new arrivals or simultaneous dismissals must
  // not restart another card's timer. Reappearing keys cancel their pending exit.
  const timers = useRef(new Map<string, number>());
  const exitingKeys = JSON.stringify(entries.filter((entry) => entry.exiting).map((entry) => entry.key));
  useEffect(() => {
    const keys = new Set<string>(JSON.parse(exitingKeys));
    for (const [key, timer] of timers.current) {
      if (!keys.has(key)) { window.clearTimeout(timer); timers.current.delete(key); }
    }
    for (const key of keys) {
      if (timers.current.has(key)) continue;
      timers.current.set(key, window.setTimeout(() => {
        timers.current.delete(key);
        setEntries((current) => current.filter((entry) => !entry.exiting || entry.key !== key));
      }, exitDelay()));
    }
  }, [exitingKeys]);
  useEffect(() => {
    const pending = timers.current;
    return () => { pending.forEach((timer) => window.clearTimeout(timer)); pending.clear(); };
  }, []);
  return entries;
}

export function useNoticeHidden(exiting: boolean): boolean {
  const [hidden, setHidden] = useState(false);
  if (!exiting && hidden) setHidden(false);
  useEffect(() => {
    if (!exiting) return;
    const timer = window.setTimeout(() => setHidden(true), exitDelay());
    return () => window.clearTimeout(timer);
  }, [exiting]);
  return exiting && hidden;
}
