import { useEffect, useRef, useState, type CSSProperties, type PointerEvent, type ReactNode } from "react";
import "./Welcome.css";
import { get, go, isConnected, saveSettings, set, useStore } from "../store";
import { pickFolder } from "./Home";
import { Kbd, Modal } from "./primitives";

/** The welcome tour: a paged walkthrough of how to actually drive the app.
 *
 *  It teaches the parts nobody finds on their own (plan mode, steering, Esc vs
 *  Esc Esc, worktrees, ultrathread, evidence, reusable workflows) and keeps the parts
 *  every agent tool shares (connect a model, type a task) to a single page.
 *  Each page pairs a few lines of copy with a looping scene of the real UI, so
 *  the user sees the gesture rather than reads about it. The motion follows
 *  openleash.net: words rising out of a blur, a light running round the frame,
 *  a grid that lights up under the pointer.
 *
 *  Every scene's resting CSS is its finished state and the keyframes only
 *  describe how it gets there, so with reduced motion (which switches the
 *  animations off) each page is a complete still picture, not a blank one. */

type Page = { id: string; eyebrow: string; title: string; em: string; body: ReactNode; scene: ReactNode };

/** A headline whose words rise in one after another; `em` gets the sheen. */
function Words({ text, em, from = 0 }: { text: string; em: string; from?: number }) {
  const plain = text.split(" ");
  const shine = em ? em.split(" ") : [];
  return (
    <h2 className="wt-title">
      {plain.map((w, i) => <span key={`p${i}`} className="wt-w" style={{ "--i": from + i } as CSSProperties}>{w} </span>)}
      {shine.length > 0 && (
        <em>{shine.map((w, i) => <span key={`e${i}`} className="wt-w" style={{ "--i": from + plain.length + i } as CSSProperties}>{w}{i < shine.length - 1 ? " " : ""}</span>)}</em>
      )}
    </h2>
  );
}

/** One bullet of copy; they rise in after the headline. */
function Point({ i, k, children }: { i: number; k?: string[]; children: ReactNode }) {
  return (
    <li className="wt-pt" style={{ "--i": i } as CSSProperties}>
      <span className="wt-pt-dot" aria-hidden="true" />
      <span className="wt-pt-text">{children}</span>
      {k && <span className="wt-pt-keys">{k.map((c) => <Kbd key={c}>{c}</Kbd>)}</span>}
    </li>
  );
}

const Logo = ({ className }: { className?: string }) => (
  <svg className={className} viewBox="120 80 460 460" fill="none" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <g stroke="currentColor" strokeWidth="30">
      <path pathLength={1} d="M372 110 L192 476 Q180 502 206 490 C290 440 362 430 440 486 Q470 506 458 476 L418 400" />
      <path pathLength={1} d="M162 286 L498 410 Q532 422 542 392 Q548 372 534 350 L372 110" />
      <path pathLength={1} d="M286 126 L320 190" />
    </g>
  </svg>
);

// ---------- Scenes ----------
// Each is a small, stylised mock of the real control it teaches. Class names
// are `ws-*` so they can never collide with the app's own components.

function SceneHello() {
  return (
    <div className="ws ws-hello">
      <div className="ws-hello-rings" aria-hidden="true"><i /><i /><i /></div>
      <Logo className="ws-hello-mark" />
    </div>
  );
}

function SceneBasics() {
  return (
    <div className="ws ws-basics">
      <div className="ws-composer">
        <div className="ws-type"><span className="ws-type-text">Add a dark mode toggle to settings</span><span className="ws-caret" /></div>
        <div className="ws-bar">
          <span className="ws-chip ws-perm"><i />Auto-edit</span>
          <span className="ws-chip ws-model">claude-sonnet</span>
          <span className="ws-send">↑</span>
        </div>
      </div>
      <div className="ws-row ws-r1"><span className="ws-tool">read</span>src/ui/Settings.tsx</div>
      <div className="ws-row ws-r2"><span className="ws-tool">edit</span>src/ui/Settings.tsx <b className="ws-plus">+24</b><b className="ws-minus">−3</b></div>
      <div className="ws-approve">
        <div className="ws-approve-h"><span className="ws-tool">bash</span>npm test</div>
        <div className="ws-approve-b"><span className="ws-btn ws-btn-go">Allow <Kbd>Enter</Kbd></span><span className="ws-btn">Always</span><span className="ws-btn">Deny <Kbd>Esc</Kbd></span></div>
      </div>
    </div>
  );
}

function ScenePlan() {
  return (
    <div className="ws ws-plan">
      <div className="ws-keypress"><Kbd>Shift Tab</Kbd></div>
      <div className="ws-composer ws-composer-sm">
        <div className="ws-bar"><span className="ws-planpill">Plan ×</span><span className="ws-chip ws-perm"><i />Auto-edit</span><span className="ws-send">↑</span></div>
      </div>
      <div className="ws-doc">
        <div className="ws-doc-h">Plan · dark mode</div>
        <div className="ws-doc-l" style={{ "--i": 0 } as CSSProperties}><span className="ws-check" />Add <code>mode</code> to Settings and its default</div>
        <div className="ws-doc-l" style={{ "--i": 1 } as CSSProperties}><span className="ws-check" />Toggle in Settings → Appearance</div>
        <div className="ws-doc-l" style={{ "--i": 2 } as CSSProperties}><span className="ws-check" />Apply <code>data-mode</code> at boot</div>
        <div className="ws-doc-f"><span className="ws-btn ws-btn-go ws-approve-plan">Approve plan</span><span className="ws-btn">Refine</span></div>
      </div>
      <div className="ws-lock" aria-hidden="true">no edits until you approve</div>
    </div>
  );
}

function SceneSteer() {
  return (
    <div className="ws ws-steer">
      <div className="ws-row ws-s1"><span className="ws-tool">bash</span>npm install date-fns</div>
      <div className="ws-bubble">use dayjs, we already ship it</div>
      <div className="ws-row ws-s2"><span className="ws-tool">think</span>Switching to dayjs — already a dependency</div>
      <div className="ws-row ws-s3"><span className="ws-tool">edit</span>src/time.ts <b className="ws-plus">+12</b></div>
      <div className="ws-pausebar"><span className="ws-pz">❚❚</span>Paused — command finished, nothing lost<span className="ws-btn ws-btn-amber">Resume</span></div>
      <div className="ws-keys">
        <span className="ws-key ws-k1"><Kbd>Enter</Kbd><small>steer</small></span>
        <span className="ws-key ws-k2"><Kbd>Esc</Kbd><small>pause</small></span>
        <span className="ws-key ws-k3"><Kbd>Esc</Kbd><Kbd>Esc</Kbd><small>stop</small></span>
      </div>
    </div>
  );
}

function SceneParallel() {
  const chats = [
    { t: "Dark mode toggle", b: "ol/dark-mode", s: "run" },
    { t: "Fix flaky login test", b: "ol/fix-login", s: "run" },
    { t: "Write API docs", b: "ol/api-docs", s: "ask" },
    { t: "Bump dependencies", b: "ol/deps", s: "done" },
  ];
  return (
    <div className="ws ws-par">
      <svg className="ws-branches" viewBox="0 0 120 200" aria-hidden="true">
        <path pathLength={1} className="ws-trunk" d="M14 0 V200" />
        {chats.map((_, i) => <path key={i} pathLength={1} className="ws-br" style={{ "--i": i } as CSSProperties} d={`M14 ${22 + i * 44} C 14 ${36 + i * 44}, 60 ${30 + i * 44}, 110 ${36 + i * 44}`} />)}
      </svg>
      <div className="ws-side">
        {chats.map((c, i) => (
          <div key={c.b} className="ws-chat" style={{ "--i": i } as CSSProperties}>
            <span className={`ws-st ws-st-${c.s}`} />
            <span className="ws-chat-t">{c.t}</span>
            <span className="ws-chat-b">{c.b}</span>
          </div>
        ))}
      </div>
      <div className="ws-seg"><span>Local</span><span className="on">Worktree</span><span>Docker</span></div>
    </div>
  );
}

function SceneDelegate() {
  const subs = ["explore · auth flow", "general · API client", "general · tests"];
  return (
    <div className="ws ws-del">
      <div className="ws-node ws-lead"><span className="ws-ultra" />Lead agent</div>
      <svg className="ws-wires" viewBox="0 0 300 70" preserveAspectRatio="none" aria-hidden="true">
        {[50, 150, 250].map((x, i) => <path key={x} pathLength={1} style={{ "--i": i } as CSSProperties} d={`M150 0 C150 35, ${x} 35, ${x} 70`} />)}
      </svg>
      <div className="ws-subs">
        {subs.map((s, i) => (
          <div key={s} className="ws-node ws-sub" style={{ "--i": i } as CSSProperties}>
            <span className="ws-sub-t">{s}</span>
            <span className="ws-meter"><i /></span>
          </div>
        ))}
      </div>
      <div className="ws-goal"><span className="ws-goal-ic">✓</span>Goal verified · <span className="mono">npm test</span> 148 passed</div>
    </div>
  );
}

function SceneArtificts() {
  return (
    <div className="ws ws-art">
      <div className="ws-canvas">
        <div className="ws-card">
          <div className="ws-card-hero" />
          <div className="ws-card-l ws-w70" />
          <div className="ws-card-l ws-w45" />
          <div className="ws-card-cta">Get started</div>
        </div>
        <div className="ws-pin"><span>1</span></div>
        <div className="ws-note">Make the button bigger</div>
      </div>
      <div className="ws-tweak"><span>Accent</span><span className="ws-slider"><i /></span></div>
      <div className="ws-vers"><span>v1</span><span>v2</span><span className="ws-v3">v3 · yours</span></div>
    </div>
  );
}

function SceneKeys() {
  return (
    <div className="ws ws-pal">
      <div className="ws-pal-box">
        <div className="ws-pal-in"><span className="ws-pal-q">plan</span><span className="ws-caret" /><Kbd>Ctrl K</Kbd></div>
        <div className="ws-pal-r on"><span>Turn plan mode on</span><Kbd>Shift Tab</Kbd></div>
        <div className="ws-pal-r"><span>Paused chats</span></div>
        <div className="ws-pal-r"><span>Permission: Auto-edit</span></div>
      </div>
      <div className="ws-cheats">
        {[["Ctrl N", "new chat"], ["Ctrl J", "details"], ["Ctrl B", "sidebar"], ["/", "commands"]].map(([k, w], i) => (
          <span key={k} className="ws-cheat" style={{ "--i": i } as CSSProperties}><Kbd>{k!}</Kbd>{w}</span>
        ))}
      </div>
    </div>
  );
}

// ---------- Setup (the one page about the basics) ----------

function SetupRows() {
  const project = useStore((s) => s.settings?.project ?? "");
  const modelReady = useStore((s) => s.providers.some((p) => p.enabled && isConnected(p)));
  const rows = [
    { done: modelReady, label: "Connect a model", hint: "API key or a subscription sign-in", cta: "Models", run: () => { close(); go("settings", { settingsTab: "models", modelSetupOpen: true }); } },
    { done: !!project, label: "Open a project folder", hint: "Agents edit real files there", cta: "Open", run: () => void pickFolder() },
  ];
  return (
    <div className="wt-setup">
      {rows.map((r, i) => (
        <div key={r.label} className={`wt-setup-row${r.done ? " done" : ""}`} style={{ "--i": i } as CSSProperties}>
          <span className="wt-setup-mark" aria-hidden="true">{r.done ? "✓" : i + 1}</span>
          <span className="wt-setup-text"><b>{r.label}</b><small>{r.done ? "Done" : r.hint}</small></span>
          {!r.done && <button type="button" className="wt-setup-btn" onClick={r.run}>{r.cta}</button>}
        </div>
      ))}
    </div>
  );
}

export const PAGES: Page[] = [
  {
    id: "hello", eyebrow: "Welcome", title: "Your coding agents,", em: "in one place.",
    scene: <SceneHello />,
    body: <>
      <p className="wt-lede">Agents here edit real files and run real commands in your project. A quick tour of how to steer them — and the useful controls you might otherwise miss.</p>
      <SetupRows />
    </>,
  },
  {
    id: "basics", eyebrow: "The basics", title: "Say what you want.", em: "Approve what matters.",
    scene: <SceneBasics />,
    body: <ul className="wt-pts">
      <Point i={0}>Describe the outcome, not the steps. Drop files onto the box to attach them.</Point>
      <Point i={1} k={["/"]}>Type <code>/</code> for every command.</Point>
      <Point i={2}>The coloured chip is the permission mode — <b>Allowlist only</b> asks before commands, <b>Auto-edit</b> edits freely, <b>Full Access</b> asks nothing.</Point>
      <Point i={3} k={["Enter", "Esc"]}>Approval cards: allow once, always, or deny.</Point>
    </ul>,
  },
  {
    id: "plan", eyebrow: "Plan mode", title: "Look before", em: "it leaps.",
    scene: <ScenePlan />,
    body: <ul className="wt-pts">
      <Point i={0} k={["Shift Tab"]}>Turn on plan mode before anything bigger than a one-liner.</Point>
      <Point i={1}>The agent reads the code and proposes a plan. It <b>cannot edit</b> until you approve.</Point>
      <Point i={2}>Refine it in plain words, then approve — the plan is saved as an artifict you can reopen from any chat.</Point>
    </ul>,
  },
  {
    id: "steer", eyebrow: "Steering", title: "Talk to it", em: "while it works.",
    scene: <SceneSteer />,
    body: <ul className="wt-pts">
      <Point i={0} k={["Enter"]}>Typing while it runs <b>steers</b>: your message lands in its very next step, no restart.</Point>
      <Point i={1} k={["Alt Enter"]}>Queue a message for after the current turn instead.</Point>
      <Point i={2} k={["Esc"]}><b>Pause</b>: the current command finishes, then it freezes. Resume carries on like nothing happened.</Point>
      <Point i={3} k={["Esc", "Esc"]}><b>Stop</b>: kills everything, background servers too. Resume offers continue, summarise, or wrap up.</Point>
    </ul>,
  },
  {
    id: "parallel", eyebrow: "Parallel chats", title: "Run five tasks", em: "at once.",
    scene: <SceneParallel />,
    body: <ul className="wt-pts">
      <Point i={0} k={["Ctrl N"]}>Every chat is its own agent. Start another while the first one works.</Point>
      <Point i={1}>Pick <b>Worktree</b> under the composer and each chat gets its own git branch and folder, so they never step on each other.</Point>
      <Point i={2}>The sidebar shows who is running, who is done and who needs you. <b>Pause all</b> freezes every agent at once.</Point>
    </ul>,
  },
  {
    id: "delegate", eyebrow: "Subagents, ultrathread, goals", title: "Hand it the", em: "whole job.",
    scene: <SceneDelegate />,
    body: <ul className="wt-pts">
      <Point i={0}>The agent sends big searches to <b>subagents</b> with fresh context. The people icon in the composer picks which ones it may use.</Point>
      <Point i={1} k={["/ultra"]}><b>Ultrathread</b> makes it the lead of a team that keeps going until the list is done. <code>/ultrawt</code> gives every worker its own worktree.</Point>
      <Point i={2} k={["/goal"]}><code>/goal tests pass on CI</code> — it can't call it done until it proves it with evidence.</Point>
    </ul>,
  },
  {
    id: "choices", eyebrow: "Races, routers, quota", title: "Try more paths.", em: "Spend deliberately.",
    scene: <SceneParallel />,
    body: <ul className="wt-pts">
      <Point i={0} k={["/race"]}><b>Best-of-N</b>: turn on Race in a new chat for 2–6 attempts in separate worktrees. Compare the judge's ranking, then pick a winner. Merging asks first.</Point>
      <Point i={1}><b>Settings → Routers</b> chooses models by rules and a fallback ladder. Use it to route work without changing models by hand.</Point>
      <Point i={2}><b>Settings → Accounts → Quota burner</b> can run your backlog before subscription quota resets. It's off by default; choose a spend cap. Usage caps still win.</Point>
    </ul>,
  },
  {
    id: "artificts", eyebrow: "Artificts", title: "Design it,", em: "then build it.",
    scene: <SceneArtificts />,
    body: <ul className="wt-pts">
      <Point i={0}>Ask for a screen, page or diagram and it builds a live <b>artifict</b> beside the chat, saved with the project.</Point>
      <Point i={1}>Click anywhere to pin a comment, drag the tweaks, or press <Kbd>E</Kbd> to edit it yourself.</Point>
      <Point i={2}>Every change is a version. When it looks right, <b>Implement this revision</b> turns it into code.</Point>
      <Point i={3}>Start one from <b>+ → Artificts</b> on a new chat.</Point>
    </ul>,
  },
  {
    id: "evidence", eyebrow: "Evidence and debugging", title: "See what changed.", em: "Find what broke.",
    scene: <SceneDelegate />,
    body: <ul className="wt-pts">
      <Point i={0}>Ask for a <b>proof clip</b> with browser <code>record_start</code> / <code>record_stop</code>. It records tool-captured frames, not continuous video; the browser plugin must be on.</Point>
      <Point i={1}>For a <b>visual diff</b>, ask for browser <code>baseline</code> before UI edits and <code>compare</code> after. Review → <b>Visual changes</b> shows the before/after slider.</Point>
      <Point i={2}>A test started failing? Review → <b>Find breaking edit…</b> bisects recorded edits in scratch copies with your test command. Requires checkpoints; not SSH or Docker.</Point>
      <Point i={3}>Right-click a chat → <b>Run timeline…</b> to see model calls, subagents and commands by time and cost.</Point>
    </ul>,
  },
  {
    id: "preferences", eyebrow: "Questions and reusable workflows", title: "Make the calls.", em: "Keep what works.",
    scene: <SceneBasics />,
    body: <ul className="wt-pts">
      <Point i={0}>Question cards can offer up to <b>30 options</b>, with a filter for long lists. Pick several when offered, write your own answer, or add a note to each pick.</Point>
      <Point i={1}>You can <b>skip any question</b> and let the agent decide. Non-blocking questions let it carry on while you consider an answer.</Point>
      <Point i={2}>Enable remote control, then set up <b>ntfy</b> in its settings for phone approvals and questions. Notifications open a confirmation page; they don't approve anything automatically.</Point>
      <Point i={3}>Right-click a useful chat → <b>Save as skill…</b>. You start the draft, review and edit it, then choose where to save the reusable <code>SKILL.md</code>. Nothing is saved automatically.</Point>
    </ul>,
  },
  {
    id: "keys", eyebrow: "One last thing", title: "Everything is", em: "one keystroke away.",
    scene: <SceneKeys />,
    body: <ul className="wt-pts">
      <Point i={0} k={["Ctrl K"]}>The palette reaches every action, chat and setting. Start typing.</Point>
      <Point i={1}>Search it for <b>Keyboard shortcuts</b> for the full list, or <b>Welcome tour</b> to see this again.</Point>
    </ul>,
  },
];

/** Mark the tour seen and close it. Exported for the palette and the tests. */
export function close() {
  set({ welcome: false });
  if (!get().settings?.welcome_seen) void saveSettings({ welcome_seen: true });
}

export const openWelcome = () => set({ welcome: true, pal: false, menu: null });

/** Opens the tour by itself on a genuine first run: never closed before, and no
 *  chats at all. An existing install upgrading into this build loads the flag as
 *  false, and the empty-chat-list condition is what keeps it from being shown
 *  a tour it doesn't need. */
export function useWelcomeAutoOpen() {
  const ready = useStore((s) => s.ready);
  const seen = useStore((s) => s.settings?.welcome_seen ?? true);
  const empty = useStore((s) => Object.keys(s.tasks).length === 0);
  const done = useRef(false);
  useEffect(() => {
    if (done.current || !ready) return;
    done.current = true;
    if (!seen && empty) set({ welcome: true });
  }, [ready, seen, empty]);
}

export function Welcome() {
  useWelcomeAutoOpen();
  const open = useStore((s) => s.welcome);
  return open ? <Tour /> : null;
}

/** Embers drifting up through the stage. Fixed, not random, so renders and
 *  tests are stable; the spread is what makes them read as random. */
const SPARKS: CSSProperties[] = [8, 19, 27, 38, 46, 57, 64, 73, 81, 92].map((x, i) => ({
  left: `${x}%`, animationDelay: `${(i * 0.73) % 5}s`, animationDuration: `${5 + (i * 1.7) % 4}s`, "--s": `${2 + (i % 3)}px`,
} as CSSProperties));

function Tour() {
  const [at, setAt] = useState(0);
  const [dir, setDir] = useState<1 | -1>(1);
  const stage = useRef<HTMLDivElement>(null);
  const next = useRef<HTMLButtonElement>(null);
  // Focus starts on Next, so Enter walks the tour. Set before Modal's own
  // first-focus pass, which leaves focus alone once it is already inside.
  useEffect(() => { next.current?.focus(); }, []);
  const page = PAGES[at]!;
  const last = at === PAGES.length - 1;
  const step = (d: 1 | -1) => {
    const n = at + d;
    if (n < 0) return;
    if (n >= PAGES.length) { close(); return; }
    setDir(d);
    setAt(n);
  };
  const jump = (n: number) => { if (n === at) return; setDir(n > at ? 1 : -1); setAt(n); };
  // Arrow keys page, like the question cards. Not while typing in a field.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (/TEXTAREA|INPUT/.test((document.activeElement as HTMLElement)?.tagName ?? "")) return;
      if (e.key === "ArrowRight") { e.preventDefault(); e.stopPropagation(); step(1); }
      if (e.key === "ArrowLeft") { e.preventDefault(); e.stopPropagation(); step(-1); }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });
  // The grid under the scene lights up around the pointer, as on the website.
  const glow = (e: PointerEvent) => {
    const el = stage.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    el.style.setProperty("--gx", `${e.clientX - r.left}px`);
    el.style.setProperty("--gy", `${e.clientY - r.top}px`);
  };
  return (
    <Modal onClose={close} className="wt" ariaLabel="Welcome tour">
      <div className="wt-ring" aria-hidden="true" />
      <div className={`wt-body wt-${page.id}`}>
        <div className="wt-stage" ref={stage} onPointerMove={glow} onPointerLeave={() => stage.current?.style.removeProperty("--gy")}>
          <div className="wt-grid" aria-hidden="true" />
          <div className="wt-aura" aria-hidden="true" />
          <div className="wt-sparks" aria-hidden="true">{SPARKS.map((s, i) => <i key={i} style={s} />)}</div>
          {/* Keyed by page, so every scene replays from its start on arrival. */}
          <div key={page.id} className={`wt-scene ${dir > 0 ? "fwd" : "back"}`} aria-hidden="true">{page.scene}</div>
        </div>
        <div className="wt-copy">
          <div key={page.id} className={`wt-copy-in ${dir > 0 ? "fwd" : "back"}`}>
            <div className="wt-eyebrow"><i aria-hidden="true" />{page.eyebrow}<span className="wt-count">{String(at + 1).padStart(2, "0")} / {String(PAGES.length).padStart(2, "0")}</span></div>
            <Words text={page.title} em={page.em} />
            <div className="wt-text">{page.body}</div>
          </div>
        </div>
      </div>
      <div className="wt-foot">
        <div className="wt-dots" role="tablist" aria-label="Tour pages">
          {PAGES.map((p, i) => (
            <button key={p.id} type="button" role="tab" aria-selected={i === at} aria-label={`${i + 1}: ${p.eyebrow}`}
              className={`wt-dot${i < at ? " past" : ""}${i === at ? " now" : ""}`} onClick={() => jump(i)}><i /></button>
          ))}
        </div>
        <div style={{ flex: 1 }} />
        {!last && <button type="button" className="wt-skip" onClick={close}>Skip tour</button>}
        {at > 0 && <button type="button" className="wt-back" onClick={() => step(-1)} aria-label="Previous page">←</button>}
        <button ref={next} type="button" className="wt-next" onClick={() => step(1)}>
          <span>{last ? "Start building" : at === 0 ? "Show me" : "Next"}</span><span className="wt-next-arrow" aria-hidden="true">→</span>
        </button>
      </div>
    </Modal>
  );
}
