import { lazy, Suspense, useEffect } from "react";
import "./App.css";
import { api } from "./api";
import { boot, disposeBoot, flash, get, go, requestPause, set, stepZoom, useStore, zoom, zoomFactor } from "./store";
import { isRemoteClient } from "./remote";
import { applyMode } from "./theme";
import { AppUpdateBanner } from "./ui/AppUpdate";
import { Sidebar, Titlebar } from "./ui/Chrome";
import { togglePlan } from "./ui/Composer";
import { Home, pickFolder } from "./ui/Home";
import { Overlays } from "./ui/Overlays";
import { Paused } from "./ui/Paused";
import { Session } from "./ui/Session";
import { RemotePicker } from "./ui/RemotePicker";
import { SelectionNoteLayer } from "./ui/SelectionNote";
import { Loader, LightboxProvider } from "./ui/primitives";

// Views that are never on screen at launch load on first visit, so their code
// (Settings pulls in every settings panel) stays out of the bundle the window
// has to parse before its first paint.
const loadReview = () => import("./ui/Review");
const loadSaved = () => import("./ui/Saved");
const loadSettings = () => import("./ui/Settings");
const Review = lazy(() => loadReview().then((m) => ({ default: m.Review })));
const Saved = lazy(() => loadSaved().then((m) => ({ default: m.Saved })));
const SettingsView = lazy(() => loadSettings().then((m) => ({ default: m.SettingsView })));

function pendingApproval(): string | null {
  const s = get();
  if (s.view !== "session" || !s.task) return null;
  const items = s.items[s.task] ?? [];
  const it = [...items].reverse().find((i) => i.kind === "approval" && !i.data?.resolved);
  return it?.id ?? null;
}

function onKey(e: KeyboardEvent) {
  const s = get();
  const mod = e.ctrlKey || e.metaKey;
  const k = e.key.toLowerCase();
  const inField = /TEXTAREA|INPUT/.test((document.activeElement as HTMLElement)?.tagName ?? "");
  if (mod && k === "k") { e.preventDefault(); set({ pal: !s.pal, menu: null }); return; }
  // Ctrl-F is find-in-this-chat. The browser's own find isn't in play: this is a
  // webview, and its dialog can't reach the transcript, which is virtualised and
  // scrolls under a sticky composer.
  if (mod && k === "f") { e.preventDefault(); if (s.view === "session" && s.task) set({ find: true, findSeed: "" }); return; }
  if (e.key === "Escape") {
    if (s.selecting) { set({ selecting: false, picked: {} }); return; }
    if (s.find) { set({ find: false }); return; }
    if (s.pal || s.menu || s.swap || s.mass || s.chatStats) { set({ pal: false, menu: null, swap: false, mass: false, chatStats: null }); return; }
    if (s.view === "session" && s.task) {
      const ap = pendingApproval();
      if (ap) { api.respond(s.task, ap, { decision: "deny" }).catch(() => {}); return; }
      const t = s.tasks[s.task];
      // First Esc pauses (commands finish, nothing is lost); Esc again while paused stops.
      const paused = !!t?.paused || (!!s.settings?.paused_all && !t?.unpaused);
      if (t && (t.status === "running" || t.status === "waiting")) {
        if (paused) api.dismissPause(t.id).then(() => flash("Stopped")).catch(() => {});
        else {
          const request = requestPause(t.id);
          if (request) {
            // The task summary and Esc feedback should not wait for checkpoint
            // snapshots or persistence to finish inside the pause command.
            flash("Pausing · Esc again to stop");
            void request.catch((error) => flash(String(error)));
          }
        }
      }
    }
    return;
  }
  if (s.pal || s.menu) return;
  if (mod && k === "n") { e.preventDefault(); go("home"); }
  if (mod && k === "j") { e.preventDefault(); set({ details: !s.details }); }
  if (mod && k === "b") { e.preventDefault(); set({ sidebar: !s.sidebar }); }
  if (mod && k === "o") { e.preventDefault(); pickFolder(); }
  if (mod && e.key === ",") { e.preventDefault(); go("settings"); }
  if (mod && (e.key === "=" || e.key === "+")) { e.preventDefault(); stepZoom(1); }
  if (mod && e.key === "-") { e.preventDefault(); stepZoom(-1); }
  if (mod && e.key === "0") { e.preventDefault(); stepZoom(0); }
  if (e.shiftKey && e.key === "Tab" && (s.view === "home" || s.view === "session")) { e.preventDefault(); togglePlan(); }
  if (e.key === "Enter" && !inField && s.view === "session" && s.task) {
    const ap = pendingApproval();
    // A card can name its own Enter choice (e.g. the ultrathread card's suggested option).
    // The id goes into an attribute selector, so it needs escaping like Find.tsx does —
    // an id that isn't a plain token would otherwise make this a syntax error.
    const decision = ap ? document.querySelector<HTMLElement>(`[data-pending-approval="${CSS.escape(ap)}"]`)?.dataset.enter || "once" : "once";
    if (ap) { e.preventDefault(); api.respond(s.task, ap, { decision }).catch(() => {}); }
  }
}

export default function App() {
  const ready = useStore((s) => s.ready);
  const err = useStore((s) => s.bootError);
  const view = useStore((s) => s.view);
  const z = useStore((s) => zoomFactor(s.settings?.ui_zoom));
  const theme = useStore((s) => s.settings?.theme ?? "raycast");
  useEffect(() => { document.documentElement.dataset.theme = theme; }, [theme]);
  // Light/dark comes from the setting, including "follow the OS". This used to
  // be a hardcoded "dark", which meant the `mode` setting and the whole
  // `[data-mode="light"]` half of the stylesheet were unreachable.
  const mode = useStore((s) => s.settings?.mode);
  useEffect(() => applyMode(mode), [mode]);
  // Browser-only styling (safe areas, no pull-to-refresh) hangs off this class.
  useEffect(() => { document.documentElement.classList.toggle("remote", isRemoteClient()); }, []);
  // Warm the lazy views once startup has settled, so the first visit to one
  // doesn't wait on a chunk load. Module imports are cached, so this is free
  // if the user already got there.
  useEffect(() => {
    if (!ready) return;
    const t = window.setTimeout(() => { for (const load of [loadSettings, loadReview, loadSaved]) load().catch(() => {}); }, 1500);
    return () => window.clearTimeout(t);
  }, [ready]);

  useEffect(() => {
    void boot();
    window.addEventListener("keydown", onKey);
    const rs = () => { if (window.innerWidth / zoom() < 1100 && get().details) set({ details: false }); };
    window.addEventListener("resize", rs);
    // No global clock tick: it replaced the root state object every 30s, so
    // every mounted selector re-ran and the sidebar and home screen re-sorted
    // the whole chat list. Components that show relative times use `useNow`.
    return () => { window.removeEventListener("keydown", onKey); window.removeEventListener("resize", rs); disposeBoot(); };
  }, []);

  if (err) return <div className="app" style={{ display: "grid", placeItems: "center", color: "#ff8a8a" }}>Failed to start the harness: {err}</div>;

  return (
    <LightboxProvider>
    // A phone browser's `vh` is the height with its toolbars hidden, so the
    // bottom of the app (the composer) would sit under them; `dvh` follows the
    // toolbars and, with `interactive-widget` in index.html, the keyboard. Only
    // in a remote browser: the desktop webview has neither, and an engine without
    // `dvh` drops the value and falls back to `.app`'s own `inset: 0`.
    <div className="app" style={{ zoom: z, width: `${100 / z}vw`, height: `${100 / z}${isRemoteClient() ? "dvh" : "vh"}` }}>
      <Titlebar />
      <AppUpdateBanner />
      <div className="body">
        <Sidebar />
        <main className="main">
          {!ready ? <Loader className="app-loader" size={18} label="Starting…" /> : (
            <div className="viewwrap" key={view === "session" || view === "diff" ? view + get().task : view}>
              <Suspense fallback={<Loader className="app-loader" size={18} />}>
              {view === "home" && <Home />}
              {view === "session" && <Session />}
              {view === "diff" && <Review />}
              {view === "saved" && <Saved />}
              {view === "paused" && <Paused />}
              {view === "settings" && <SettingsView />}
              </Suspense>
            </div>
          )}
        </main>
      </div>
      <Overlays z={z} />
      <RemotePicker />
      {/* Outside the zoomed view tree but inside the app root: the note pill is
          placed from a selection rect in unzoomed window pixels, and it has to
          land over the transcript without scrolling with it. */}
      <SelectionNoteLayer />
    </div>
    </LightboxProvider>
  );
}
