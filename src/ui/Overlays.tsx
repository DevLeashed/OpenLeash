import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { chord } from "../keys";
import { api, ASSISTS, baseName, clampEffort, EFFORTS, effortLabel, effortSteps, isRequiredAgent, ModelInfo, PERMS, TaskSummary, withRequiredAgents, xOn } from "../api";
import { matchesTaskQuery, taskSearchDetail } from "./task-search";
import { chatModel, defaultRoute, flash, get, go, isConnected, modelColor, modelInfo, openProject, openTask, saveSettings, set, shownProviders, stepZoom, subModel, useStore, DEFAULT_ZOOM, ZOOMS } from "../store";
import { afterChange, openUltraXEditor, setOpts, togglePlan, toggleUltra, toggleUltraWt, ULTRA_X_SHOWN } from "./Composer";
import { messageable, chatProjectIsCurrent, MassScope, massTargets, pickAll, projectMenuEntries, ProjectCtxMenu, sortKey, togglePick } from "./Chrome";
import { pickFolder } from "./Home";
import { I, StatusIcon } from "./icons";
import { ChatStatsPanel } from "./ChatStats";
import { ModelSwap, openSwap } from "./ModelSwap";
import { Notices } from "./Notices";
import { isPaused } from "./Paused";
import { UltraXEditor } from "./UltraX";
import { TrustDialog } from "./Trust";
import { ProviderIcon } from "./ProviderIcon";
import { AnchoredPanel, ColorPicker, Button, Loader, Modal, TextArea, TextButton, Tooltip, IconButton, Input, MenuRow, MenuScrim, Segmented, Switch, Kbd, useAnchoredPanel } from "./primitives";
import { ArrowLeftRight, BrainCircuit, Check, CircleDollarSign, Copy, FolderOpen, GitCompare, GitFork, History, Layers, MessagesSquare, Network, PanelLeft, PanelRight, Route, ScrollText, ServerCog, Shield, Sparkles, Star, UserRoundCog, ZoomIn, ZoomOut } from "lucide-react";

const li = (C: typeof History, size = 14) => <C aria-hidden="true" size={size} strokeWidth={1.7} />;

/** Open the "Message all chats" box: every agent working right now, rather than
 *  the ticked chats the sidebar's own send reaches. */
export const openMassAll = () => set({ menu: null, pal: false, mass: true, massAll: true });

function place(anchor: DOMRect | null, W: number, H: number, right: boolean, z: number) {
  const r = anchor ?? new DOMRect(100, 100, 0, 0);
  const vw = window.innerWidth / z, vh = window.innerHeight / z;
  const L = r.left / z, R = r.right / z, T = r.top / z, B = r.bottom / z;
  let x = right ? R - W : L;
  x = Math.max(10, Math.min(x, vw - W - 10));
  const up = T > H + 16 || T > vh - B;
  const room = (up ? T : vh - B) - 16;
  return { style: { left: x, width: W, top: up ? "auto" : B + 6, bottom: up ? vh - T + 6 : "auto", transformOrigin: (up ? "bottom " : "top ") + (right ? "right" : "left") } as React.CSSProperties, room };
}

const NONE: string[] = [];

/** Model rows the picker mounts at once; a search lifts the cap. */
const MODEL_ROWS = 200;

type Row = { sep?: true; head?: string; label?: string; desc?: string; icon?: React.ReactNode; mono?: boolean; k?: string; check?: boolean; toggle?: boolean; keep?: boolean; ctx?: (x: number, y: number) => void; run?: (e: React.MouseEvent) => void };

function SmallMenu({ z }: { z: number }) {
  const menu = useStore((s) => s.menu);
  const anchor = useStore((s) => s.menuAnchor);
  const settings = useStore((s) => s.settings);
  const info = useStore((s) => s.projectInfo);
  const agents = useStore((s) => s.agents);
  const home = useStore((s) => s.home);
  const task = useStore((s) => (s.view !== "home" && s.task ? s.tasks[s.task] : null));
  const close = () => set({ menu: null });
  // Read from the subscriptions above, not through get(): these two mirror
  // currentOpts(), and were re-rendered by an unlabelled `useStore(s => s.home)`
  // rather than by the value actually being used.
  const o = task
    ? { plan: task.plan, ultra: !!task.ultra, ultra_wt: !!task.ultra_wt, ultra_x: task.ultra_x ?? null, perm: task.perm, model: task.model, effort: task.effort, assist: task.assist, agents: task.agents }
    : home;
  let rows: Row[] = [];
  if (menu === "plus") rows = [
    ...(task ? [{ icon: li(GitFork), label: "Fork this chat…", desc: "Branch off a new chat from here — the whole conversation, or just its context", run: () => openFork(task.id) }] : []),
    { icon: li(ArrowLeftRight), label: task ? "Swap Models…" : "Swap All Models…", k: "/models", run: openSwap },
    // Home only, and only because it needs nothing to point at: it works from
    // the roster of working chats, so there is no "the chat this was opened for"
    // to narrow it by. In a chat the sidebar's ticked send is the one that fits.
    ...(task ? [] : [{ icon: li(MessagesSquare), label: "Message all chats…", desc: "One message to the main agent of every chat working right now in this project", run: openMassAll }]),
    { sep: true },
    { icon: li(BrainCircuit), label: "Plan mode", desc: "Propose a plan before editing", k: "Shift Tab", toggle: o.plan, run: () => togglePlan() },
    { icon: li(Network), label: "Ultrathread", desc: "Fan out nested subagents and keep going until everything's done", k: "/ultra", toggle: o.ultra, run: () => toggleUltra() },
    { icon: li(GitFork), label: "Ultrathread worktrees", desc: "Each worker gets its own git worktree; a fuze agent merges them back", k: "/ultrawt", toggle: o.ultra_wt, run: () => toggleUltraWt() },
    ...(ULTRA_X_SHOWN ? [{ icon: li(Layers), label: "Ultrathread X…", desc: "A ladder of subagent layers, each with its own model, effort and fanout", k: "/ultrax", toggle: xOn(o.ultra_x), run: () => { close(); openUltraXEditor(); } }] : []),
    ...(task ? [task.paused
      ? { icon: I.play(12), label: "Resume this task", run: () => { close(); api.resume(task.id).catch((e) => flash(String(e))); } }
      : { icon: I.pause(12), label: "Pause this task", desc: "Freezes the agent and all its subagents", run: () => { close(); api.pause(task.id).catch((e) => flash(String(e))); } }] : []),
    { sep: true },
    { icon: li(ServerCog), label: "MCP servers", k: `${settings?.mcp.filter((m) => m.enabled).length ?? 0} on`, run: () => go("settings", { settingsTab: "mcp" }) },
  ];
  if (menu === "route") {
    const routes = settings?.routes ?? [];
    const cur = task ? task.route : home.route;
    const model = task ? task.pending.model ?? task.model : home.model;
    const pickRoute = (id: string) => { close(); void setOpts({ route: id }); };
    rows = [
      { head: "When the model runs out" },
      { icon: I.pause(12), label: "No routing", desc: "Pause and wait", check: !cur, run: () => pickRoute("") },
      ...routes.map((r) => ({ icon: li(Route), label: r.name, desc: r.steps.map((s) => modelInfo(s).name).concat(r.on_exhausted).join(" then "), k: r.heads?.includes(model) ? "for this model" : r.all ? "all models" : "", check: r.id === cur, run: () => pickRoute(r.id) })),
      { sep: true },
      { icon: I.plus(13), label: "Edit routes…", run: () => go("settings", { settingsTab: "routing" }) },
    ];
  }
  if (menu === "assist") rows = [
    { head: "Assist mode" },
    ...ASSISTS.map((a) => ({ label: a.name, desc: a.desc, check: a.id === (task?.pending.assist ?? o.assist), run: (e: React.MouseEvent) => { close(); void setOpts({ assist: a.id, now: e.shiftKey }); afterChange("Assist mode: " + a.name, e.shiftKey); } })),
  ];
  if (menu === "agents") {
    const cur = withRequiredAgents(task?.pending.agents ?? o.agents);
    const toggle = (id: string, e: React.MouseEvent) => {
      // `explore` + `general` are always on.
      if (isRequiredAgent(id)) {
        flash("Explore and General are always on");
        return;
      }
      const next = withRequiredAgents(cur.includes(id) ? cur.filter((x) => x !== id) : [...cur, id]);
      void setOpts({ agents: next, now: e.shiftKey });
      if (!task) void saveSettings({ default_agents: next });
    };
    rows = [
      { head: "Subagents this chat may use" },
      ...agents.filter((d) => !isRequiredAgent(d.id)).map((d) => ({ icon: li(UserRoundCog), label: d.name, desc: d.description, k: d.tools === "read_only" ? "read-only" : d.source === "project" ? "project" : "", toggle: cur.includes(d.id), keep: true, run: (e: React.MouseEvent) => toggle(d.id, e) })),
      { sep: true },
      { icon: I.plus(13), label: "Manage subagents…", run: () => go("settings", { settingsTab: "agents" }) },
    ];
  }
  if (menu === "perm") rows = [
    { head: "Permissions" },
    ...PERMS.map((p) => ({ label: p.name, desc: p.desc, check: p.id === o.perm, run: () => { setOpts({ perm: p.id }); close(); } })),
  ];
  if (menu === "branch") rows = [{ head: "Base branch" }, ...(info?.branches ?? []).slice(0, 12).map((b) => ({ label: b, mono: true, k: b === info?.branch ? "current" : "", check: b === (home.branch || info?.branch), run: () => { set((s) => ({ home: { ...s.home, branch: b }, menu: null })); } }))];
  if (menu === "folder") rows = [
    { head: "Projects" },
    ...(settings?.projects ?? []).slice(0, 8).map((p) => ({ label: baseName(p), desc: p, check: p === settings?.project, ctx: (x: number, y: number) => set({ menu: null, projCtx: { p, x, y } }), run: async () => { close(); await openProject(p); } })),
    { sep: true },
    { label: "Open folder…", run: () => { close(); pickFolder(); } },
  ];
  if (!menu || menu === "model" || !rows.length) return null;
  const W = { plus: 280, perm: 290, branch: 250, folder: 300, assist: 300, agents: 330, route: 340 }[menu] ?? 260;
  const { style } = place(anchor, W, 240, false, z);
  // Permissions is three short labels, so a fixed W just leaves a wide empty
  // band past the checkmark. W stays as the cap for a label that does run long.
  const fit = menu === "perm";
  return (
    <>
      <MenuScrim onClick={close} />
      <div className="pop" style={{ ...style, ...(fit ? { width: "max-content", maxWidth: W } : null), maxHeight: "60vh", overflowY: "auto" }}>
        {rows.map((m, i) => m.sep ? <div key={i} className="msep" /> : m.head ? <div key={i} className="mhead">{m.head}</div> : (
          // A row is one line: anything that used to sit under the label is a
          // tooltip now, so the list stays scannable at a glance.
          <Tooltip key={i} content={m.desc} side="right" delay={450}>
          <MenuRow style={{ animationDelay: i * 6 + "ms" }} onClick={(e) => { m.run?.(e); if (m.keep) e.stopPropagation(); }}
            onContextMenu={m.ctx ? (e) => { e.preventDefault(); e.stopPropagation(); m.ctx?.(e.clientX, e.clientY); } : undefined}>
            {m.icon && <span style={{ width: 14, flex: "none", textAlign: "center", color: "var(--mut2)", fontSize: 12 }}>{m.icon}</span>}
            <div style={{ flex: 1, minWidth: 0, fontWeight: 500, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", textAlign: "left" }} className={m.mono ? "mono" : ""}>{m.label}</div>
            {m.k && <span style={{ fontSize: 10.5, color: "var(--dim)" }}>{chord(m.k)}</span>}
            {m.toggle !== undefined ? <Switch label={m.label ?? "Toggle"} checked={m.toggle} small /> : <span style={{ width: 12, flex: "none", color: "var(--violet)", display: "flex" }}>{m.check ? <Check size={12} strokeWidth={2} /> : null}</span>}
          </MenuRow>
          </Tooltip>
        ))}
      </div>
    </>
  );
}

function ModelPicker({ z }: { z: number }) {
  const menu = useStore((s) => s.menu);
  const anchor = useStore((s) => s.menuAnchor);
  const models = useStore((s) => s.models);
  const allProvs = useStore((s) => s.providers);
  // Memoized: this list sits in effect and memo dependencies below.
  const provs = useMemo(() => shownProviders(allProvs), [allProvs]);
  const recent = useStore((s) => s.settings?.recent_models ?? NONE);
  const favs = useStore((s) => s.settings?.favorite_models ?? NONE);
  const pickFor = useStore((s) => s.pickFor);
  const settings = useStore((s) => s.settings);
  // The list is filtered by what is connected, so a fresh account or key has to
  // reach this picker while it is open, not only the next time it opens.
  const accounts = useStore((s) => s.accounts);
  const agents = useStore((s) => s.agents);
  // Must mirror currentOpts(): on the home screen that's the new-chat options, not the last task.
  const task = useStore((s) => (s.view !== "home" && s.task ? s.tasks[s.task] : null));
  const home = useStore((s) => s.home);
  const subFor = pickFor && task ? task.subs.find((x) => x.id === pickFor.sub) : null;
  const o = task
    ? { plan: task.plan, ultra: !!task.ultra, ultra_wt: !!task.ultra_wt, ultra_x: task.ultra_x ?? null, perm: task.perm, model: task.model, effort: task.effort, assist: task.assist, agents: task.agents }
    : home;
  // What a sub-agent is really on, resolved the way the backend resolves it — its
  // own model, else the ladder layer for its depth, else the chat's. Reading
  // `subFor.model` here showed a blank row for every agent that just follows the
  // chat, and never showed a queued swap.
  const curModel = subFor ? subModel(task!, subFor) : task ? chatModel(task) : o.model;
  const [q, setQ] = useState("");
  const [prov, setProv] = useState<string>("recent");
  const [hi, setHi] = useState(0);
  const inp = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const enabled = useMemo(() => new Set(provs.filter((p) => p.enabled && isConnected(p)).map((p) => p.id)), [provs, accounts]);
  useEffect(() => {
    if (menu !== "model") return;
    setQ("");
    // Land on something useful: the current model's provider if it is live, else the first live provider.
    const cur = modelInfo(curModel).provider;
    const live = provs.find((p) => p.enabled && isConnected(p))?.id ?? "recent";
    setProv(favs.length ? "fav" : recent.length ? "recent" : enabled.has(cur) ? cur : live);
    setHi(0);
    // The input is mounted by this state change, so the focus has to wait a frame;
    // cleared on cleanup so a closed menu can't steal focus into a detached node.
    const id = window.setTimeout(() => inp.current?.focus(), 30);
    return () => window.clearTimeout(id);
  }, [menu, curModel, provs, favs, recent, enabled]);
  // A route is a fallback chain, not a model, but `modelInfo` already knows how
  // to synthesise one from its first step — so the picker can list routes the
  // same way it lists models. They were left out of the list until now, which is
  // why the "Routes" rail below never had anything to show.
  const routeModels: ModelInfo[] = useMemo(
    () => (settings?.routes ?? []).filter((r) => r.steps.length).map((r) => modelInfo("route/" + r.id)),
    [settings?.routes],
  );
  const list = useMemo(() => {
    const ql = q.trim().toLowerCase();
    const all = [...routeModels, ...models];
    // Routes have no provider of their own, so they must not be dropped by the
    // "is this provider connected" filter the real models go through.
    let l = prov === "fav" && !ql ? favs.map((id) => modelInfo(id)) : prov === "recent" && !ql ? recent.map((id) => modelInfo(id)) : all.filter((m) => ql ? true : m.provider === prov);
    if (ql) l = l.filter((m) => (m.name + " " + m.id).toLowerCase().includes(ql));
    l = l.filter((m) => m.provider === "route" || (enabled.has(m.provider) && m.enabled !== false));
    // The current model stays selectable even when toggled off, so a chat never loses its model.
    if (curModel && !l.some((m) => m.id === curModel)) {
      const cur = modelInfo(curModel);
      if (!ql || (cur.name + " " + cur.id).toLowerCase().includes(ql)) l = [cur, ...l];
    }
    if (ql.includes("/") && !l.some((m) => m.id === ql)) l = [...l, { ...modelInfo(ql), name: `Use ${ql}` }];
    // Favorites float to the top of every other list.
    if (prov !== "fav" && prov !== "recent") l = [...l.filter((m) => favs.includes(m.id)), ...l.filter((m) => !favs.includes(m.id))];
    return l;
  }, [q, prov, models, recent, favs, provs, routeModels, curModel]);
  // Only the rows on screen get mounted. The box is about 8 rows tall and the search
  // box sits above it, so a cap can never hide the only match: `shown` is 0 when a
  // search is typed. Arrow keys still walk the whole `list`, so it stays reachable.
  const shownList = useMemo(() => (q.trim() ? list : list.slice(0, MODEL_ROWS)), [q, list]);
  const toggleFav = (id: string) => {
    const on = favs.includes(id);
    void saveSettings({ favorite_models: on ? favs.filter((x) => x !== id) : [...favs, id] });
    flash(on ? `Removed ${modelInfo(id).name} from favorites` : `★ ${modelInfo(id).name} added to favorites`);
  };
  useEffect(() => { const j = list.findIndex((m) => m.id === curModel); setHi(Math.max(0, j)); }, [prov, q]);
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const top = hi * 30;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (top + 36 > el.scrollTop + el.clientHeight) el.scrollTop = top + 36 - el.clientHeight;
  }, [hi]);
  if (menu !== "model") return null;
  // Opened from a subagent's panel: the effort bar is that subagent's, not the chat's.
  const subDef = subFor ? agents.find((a) => a.id === subFor.role) : undefined;
  const subDefault = subFor && task ? subDef?.effort ?? (subDef?.tools === "read_only" ? Math.max(3, task.effort) : task.effort) : 0;
  const effortVal = subFor ? subFor.effort ?? subDefault : o.effort;
  const setEffort = (e: number) => {
    if (subFor && task) api.subEffort(task.id, subFor.id, e === subDefault ? null : e).catch((err) => flash(String(err)));
    else void setOpts({ effort: e });
  };
  const pick = (id: string, now: boolean) => {
    if (subFor && task) {
      // Sub-agents only switch mid-flight: next request.
      api.subModel(task.id, subFor.id, id).catch((e) => flash(String(e)));
    } else {
      void setOpts({ model: id, now, route: defaultRoute(id) });
    }
    const s = get().settings;
    if (s) saveSettings({ recent_models: [id, ...s.recent_models.filter((x) => x !== id)].slice(0, 6) });
    set({ menu: null, pickFor: null });
  };
  const { style, room } = place(anchor, 460, 360, !anchor || anchor.left > window.innerWidth / 2 ? true : false, z);
  const modelLeft = anchor ? Math.max(10, Math.min((anchor.left + anchor.width / 2) / z - 230, window.innerWidth / z - 470)) : Number(style.left);
  const originX = anchor ? Math.max(16, Math.min((anchor.left + anchor.width / 2) / z - modelLeft, 444)) : 230;
  const modelStyle = { ...style, left: modelLeft, transformOrigin: `${originX}px ${style.bottom === "auto" ? "top" : "bottom"}` };
  const pickH = Math.max(110, Math.min(260, room - 112));
  const mi = modelInfo(curModel);
  // The slider offers the positions that actually change what is sent: a model
  // with three reasoning levels gets three, an off/on model two, a Max-only one
  // a single choice, so nothing here promises an effort the request can't take.
  const steps = effortSteps(mi);
  const stepIdx = Math.max(0, steps.indexOf(clampEffort(steps, effortVal)));
  // A model with no effort control has no rungs to step between, so this is
  // undefined rather than a `steps[0]` that isn't there — and the handlers
  // below leave the setting alone instead of writing an `undefined` effort.
  const stepBy = (delta: number) => steps[Math.max(0, Math.min(steps.length - 1, stepIdx + delta))];
  // `effortSteps` only ever returns positions 0..4, which is what `EFFORTS`
  // indexes, so the lookup can't miss.
  const effortOpts = steps.map((value) => ({ value, label: effortLabel(mi, value) ?? EFFORTS[value]! }));
  const p = provs.find((x) => x.id === (prov === "recent" || prov === "fav" ? mi.provider : prov));
  const rail = [{ id: "fav", name: "Favorites" }, { id: "recent", name: "Recently used" }, ...(routeModels.length ? [{ id: "route", name: "Routes (fallback chains)" }] : []), ...provs.filter((x) => x.enabled && isConnected(x))];
  return (
    <>
      <MenuScrim onClick={() => set({ menu: null })} />
      <div className="pop" style={{ ...modelStyle, width: 460, padding: 0, display: "flex", flexDirection: "column", overflow: "hidden", borderRadius: 14 }}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") { e.preventDefault(); setHi(Math.min(hi + 1, list.length - 1)); }
          else if (e.key === "ArrowUp") { e.preventDefault(); setHi(Math.max(hi - 1, 0)); }
          else if (e.key === "Enter") { e.preventDefault(); const m = list[hi]; m && pick(m.id, e.shiftKey); }
          else if (e.key === "ArrowLeft" && !q) { e.preventDefault(); const n = stepBy(1); if (n !== undefined) setEffort(n); }
          else if (e.key === "ArrowRight" && !q) { e.preventDefault(); const n = stepBy(-1); if (n !== undefined) setEffort(n); }
        }}>
        {subFor && (
          <div style={{ padding: "8px 12px 0", fontSize: 11, color: "var(--mut2)" }}>
            {`Model for the ${subFor.role} subagent · switches on its next request`}
          </div>
        )}
        <div style={{ padding: 8 }}>
          <Input ref={inp} style={{ width: "100%", height: 32, fontSize: 13 }} value={q} onChange={(e) => setQ(e.currentTarget.value)} placeholder="Search models and routes, or type provider/model-id" />
        </div>
        <div style={{ display: "flex", height: pickH, borderTop: "1px solid rgba(255,255,255,0.06)" }}>
          <div style={{ width: 46, flex: "none", display: "flex", flexDirection: "column", alignItems: "center", gap: 3, padding: "6px 0", overflowY: "auto", borderRight: "1px solid rgba(255,255,255,0.06)" }}>
            {rail.map((x) => (
              <IconButton key={x.id} label={x.name} tooltipSide="right" className={"rail" + (prov === x.id ? " on" : "")} style={{ width: 32, height: 30 }} onClick={() => { setProv(x.id); setQ(""); }}>
                {x.id === "fav" ? li(Star, 15) : x.id === "recent" ? li(History, 15) : x.id === "route" ? li(Route, 15) : <ProviderIcon provider={x.id} size={16} />}
              </IconButton>
            ))}
            <div style={{ flex: 1 }} />
            <IconButton label="Manage providers" tooltipSide="right" style={{ width: 32, height: 30 }} onClick={() => go("settings", { settingsTab: "models" })}>{I.gear()}</IconButton>
          </div>
          <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column" }}>
            <div style={{ padding: "7px 10px 3px" }}>
              <span style={{ display: "inline-flex", alignItems: "center", height: 20, padding: "0 7px", borderRadius: 5, border: "1px solid rgba(255,255,255,0.08)", fontSize: 11, color: p && !p.has_key ? "#ff8a8a" : "var(--mut)" }}>
                {prov === "fav" && !q ? "Favorites" : prov === "recent" && !q ? "Recently used" : prov === "route" && !q ? "Your fallback chains · edit in Settings under Routing" : p ? (p.has_key ? p.chip : `${p.name} · no key set`) : "All providers"}
              </span>
            </div>
            <div ref={listRef} style={{ flex: 1, overflowY: "auto", padding: "2px 6px 6px" }}>
              {shownList.map((m, i) => (
                <div key={m.id + i} className={"mrow" + (i === hi ? " hi" : "")} style={{ height: 30, animationDelay: Math.min(i, 14) * 5 + "ms" }} onMouseMove={() => setHi(i)} onClick={(e) => pick(m.id, e.shiftKey)}>
                  {m.provider !== "route" && <ColorDot id={m.id} />}
                  <span style={{ fontWeight: 500, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", minWidth: 0 }}>{m.name}</span>
                  <span style={{ fontSize: 11, color: "var(--dim)", whiteSpace: "nowrap" }}>{m.provider === "route" ? (settings?.routes.find((r) => "route/" + r.id === m.id)?.steps.length ?? 0) + " steps" : m.provider}</span>
                  <div style={{ flex: 1 }} />
                  {/* The price is optional: a subscription model has none, and a bare
                      "· subscription" after the context size read as noise — so
                      the whole clause goes rather than leaving a dangling dot. */}
                  {i === hi && m.provider !== "route" && <span className="meta">{Math.round(m.context / 1000)}k ctx{m.input_price || m.output_price ? ` · $${m.input_price}/${m.output_price}` : ""}</span>}
                  {m.provider !== "route" && (
                    <Tooltip content={favs.includes(m.id) ? "Remove from favorites" : "Add to favorites"}><span className={"favstar" + (favs.includes(m.id) ? " on" : "")} onClick={(e) => { e.stopPropagation(); toggleFav(m.id); }}><Star size={12} strokeWidth={1.8} fill={favs.includes(m.id) ? "currentColor" : "none"} /></span></Tooltip>
                  )}
                  <span style={{ width: 12, color: "#f4f4f5", display: "flex" }}>{m.id === curModel ? <Check size={12} strokeWidth={2} /> : null}</span>
                </div>
              ))}
              {!list.length && <div className="empty">{prov === "fav" ? "No favorites yet · hit ☆ next to any model" : prov === "recent" ? "Nothing yet · pick a provider on the left" : "No models match"}</div>}
              {list.length > shownList.length && <div style={{ padding: "6px 8px 2px", fontSize: 11, color: "var(--dim)" }}>Showing {shownList.length} of {list.length} · search to reach the rest</div>}
            </div>
          </div>
        </div>
        <Tooltip content={steps.length ? "Effort · arrow keys to adjust · applies on the agent's next request" : "This model has no effort control"}><div style={{ padding: 6, borderTop: "1px solid rgba(255,255,255,0.06)", opacity: steps.length ? 1 : 0.35, pointerEvents: steps.length ? "auto" : "none" }}>
          {subFor && <div style={{ fontSize: 10.5, color: "var(--dim)", padding: "0 4px 5px" }}>Effort for this subagent · default {effortLabel(mi, subDefault) ?? EFFORTS[subDefault]}{subFor.effort != null ? " · overridden" : ""}</div>}
          <Segmented label="Thinking effort" options={effortOpts} value={clampEffort(steps, effortVal)} onChange={setEffort} quiet style={{ width: "100%" }} />
        </div></Tooltip>
      </div>
    </>
  );
}

/**
 * Per-model accent: click the dot for the same grid subagents get, right-click to
 * drop the pick and go back to the provider's colour.
 *
 * The grid lives in an `AnchoredPanel` rather than inline, because the picker row
 * it opens from sits inside a `.pop` with `overflow: hidden` and a fixed z-index
 * — a panel rendered in place would be clipped by the list and painted under the
 * next overlay. Portalling to the app root puts it above the picker, which is
 * also what keeps a click on a swatch from being read as a click on the row and
 * switching the chat's model.
 */
function ColorDot({ id }: { id: string }) {
  useStore((s) => s.settings?.model_colors?.[id]);
  const c = modelColor(id);
  const { anchor, open, toggle, close } = useAnchoredPanel();
  const save = (v: string) => {
    const cur = { ...(get().settings?.model_colors ?? {}) };
    if (v) cur[id] = v; else delete cur[id];
    void saveSettings({ model_colors: cur });
  };
  return (
    <>
      <Tooltip content="Model color · click to change, right-click to reset">
        {/* A button, not a label: there is no input to label any more, so it was
            a control with no button, unreachable from the keyboard. The fill
            lives on the inner dot only — painting it on the hit area as well is
            what left each one showing a ring through it. */}
        <button type="button" aria-label={`Color for ${modelInfo(id).name}`} className="mcolor" onClick={(e) => { e.stopPropagation(); toggle(e); }} onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); save(""); }}>
          <span className="mcolor-dot" aria-hidden="true" style={{ background: c }} />
        </button>
      </Tooltip>
      {open && (
        <AnchoredPanel anchor={anchor} onClose={close} width={252}>
          <div className="sp-head"><span style={{ flex: 1 }}>{modelInfo(id).name}</span></div>
          <div className="sp-body">
            <ColorPicker value={c} onChange={save} reset={get().settings?.model_colors?.[id] ? () => save("") : undefined} />
          </div>
        </AnchoredPanel>
      )}
    </>
  );
}

type Cmd = { g: string; icon: React.ReactNode; label: string; k?: string; detail?: string; run: () => void };

/** Shared empty result, so a closed palette hands back one stable array. */
const EMPTY_CMDS: Cmd[] = [];

function Palette() {
  const pal = useStore((s) => s.pal);
  const tasks = useStore((s) => s.tasks);
  const modelRows = useStore((s) => s.models);
  const settings = useStore((s) => s.settings);
  const view = useStore((s) => s.view);
  const task = useStore((s) => s.task);
  const home = useStore((s) => s.home);
  const [q, setQ] = useState("");
  const [i, setI] = useState(0);
  const inp = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!pal) return;
    setQ("");
    setI(0);
    // The input is mounted by `pal` itself, so focus has to wait a frame; cleared on
    // cleanup so a closed palette can't pull focus into a detached node.
    const id = window.setTimeout(() => inp.current?.focus(), 30);
    return () => window.clearTimeout(id);
  }, [pal]);
  const cmds = useMemo<Cmd[]>(() => {
    // Nothing is built while the palette is closed. This used to run anyway, on
    // every task event from every chat, and its last step sorts every chat the
    // user has — so a long-running agent kept a component nobody could see
    // re-sorting a list a few times a second for as long as it worked.
    if (!pal) return EMPTY_CMDS;
    // Mirrors currentOpts(): in a chat the running task's options, on the home
    // screen the new-chat ones. Subscribed above, not read through get().
    const o = { plan: home.plan, ultra: home.ultra, ultra_wt: home.ultra_wt, ultra_x: home.ultra_x };
    const base: Cmd[] = [
      { g: "Action", icon: I.newTask(), label: "New task", k: "Ctrl N", run: () => go("home") },
      { g: "Action", icon: li(ArrowLeftRight), label: view === "session" ? "Swap Models" : "Swap All Models", k: "/models", run: openSwap },
      { g: "Action", icon: li(MessagesSquare), label: "Message all chats", run: openMassAll },
      { g: "Action", icon: li(BrainCircuit), label: o.plan ? "Turn plan mode off" : "Turn plan mode on", k: "Shift Tab", run: togglePlan },
      { g: "Action", icon: li(Network), label: o.ultra ? "Turn ultrathread off" : "Turn ultrathread on", k: "/ultra", run: toggleUltra },
      { g: "Action", icon: li(GitFork), label: o.ultra_wt ? "Turn ultrathread worktrees off" : "Turn ultrathread worktrees on", k: "/ultrawt", run: toggleUltraWt },
      ...(ULTRA_X_SHOWN ? [{ g: "Action", icon: li(Layers), label: xOn(o.ultra_x) ? "Edit the ultrathread X ladder" : "Set up ultrathread X…", k: "/ultrax", run: () => { set({ pal: false }); openUltraXEditor(); } }] : []),
      { g: "Action", icon: li(PanelRight), label: "Toggle details panel", k: "Ctrl J", run: () => set((s) => ({ details: !s.details })) },
      { g: "Action", icon: li(PanelLeft), label: "Toggle sidebar", k: "Ctrl B", run: () => set((s) => ({ sidebar: !s.sidebar })) },
      { g: "Action", icon: li(GitCompare), label: "Review changes", run: () => task && go("diff") },
      { g: "Action", icon: li(FolderOpen), label: "Open project folder…", k: "Ctrl O", run: pickFolder },
      ...projectMenuEntries(settings?.project ?? "", settings?.projects ?? []).slice(1, 6).map((p) => ({ g: "Project", icon: I.folder(), label: baseName(p), run: () => { void openProject(p); } })),
      { g: "Go to", icon: I.bookmark(), label: "Saved prompts", run: () => go("saved") },
      { g: "Go to", icon: I.pause(13), label: "Paused chats", run: () => go("paused") },
      { g: "Go to", icon: I.gear(), label: "Settings", k: "Ctrl ,", run: () => go("settings") },
      { g: "Go to", icon: li(ServerCog), label: "MCP servers", run: () => go("settings", { settingsTab: "mcp" }) },
      { g: "Go to", icon: li(Sparkles), label: "Skills", run: () => go("settings", { settingsTab: "skills" }) },
      { g: "Go to", icon: li(CircleDollarSign), label: "Usage and budget", run: () => go("settings", { settingsTab: "usage" }) },
      { g: "View", icon: li(ZoomIn), label: "Zoom in", k: "Ctrl +", run: () => stepZoom(1) },
      { g: "View", icon: li(ZoomOut), label: "Zoom out", k: "Ctrl −", run: () => stepZoom(-1) },
      ...ZOOMS.map((n) => ({ g: "View", icon: "%", label: `Interface zoom ${n}%${n === DEFAULT_ZOOM ? " (default)" : ""}`, k: n === DEFAULT_ZOOM ? "Ctrl 0" : undefined, run: () => saveSettings({ ui_zoom: n }) })),
      { g: "Action", icon: I.pause(13), label: "Pause all agents", run: () => api.pauseAll().catch((e) => flash(String(e))) },
      { g: "Action", icon: I.play(13), label: "Resume all agents", run: () => api.resumeAll().catch((e) => flash(String(e))) },
      { g: "Go to", icon: li(UserRoundCog), label: "Accounts and usage limits", run: () => go("settings", { settingsTab: "accounts" }) },
      { g: "Go to", icon: li(Network), label: "Routing and fallbacks", run: () => go("settings", { settingsTab: "routing" }) },
      ...ASSISTS.map((a) => ({ g: "Assist", icon: li(Sparkles), label: "Assist mode: " + a.name, run: () => { void setOpts({ assist: a.id, now: true }); } })),
      ...EFFORTS.map((e, j) => ({ g: "Effort", icon: li(BrainCircuit), label: "Effort: " + e, run: () => { void setOpts({ effort: j }); } })),
      ...PERMS.map((p) => ({ g: "Permission", icon: li(Shield), label: "Permission: " + p.name, run: () => { void setOpts({ perm: p.id }); } })),
    ];
    // The two long lists (models, chats) come last, so the nine rows on screen are
    // rarely them: build an entry only for what will still be there. Matching
    // before capping is what keeps search reaching every model and chat however
    // many there are, so a keystroke only maps the few that survive it.
    const ql = q.trim().toLowerCase();
    const keep = (g: string, label: string) => label.toLowerCase().includes(ql) || g.toLowerCase().includes(ql);
    const models = modelRows.filter((m) => m.enabled !== false);
    // Chats the user was last in, same order as the sidebar — not whichever
    // agent happened to be running most recently.
    const chats = Object.values(tasks).sort((a, b) => sortKey(b) - sortKey(a));
    const room = 9 - base.length;
    const modelCmds = (m: ModelInfo) => ({ g: "Model", icon: li(BrainCircuit), label: "Switch to " + m.name, run: () => { void setOpts({ model: m.id }); } });
    // Same rule the sidebar and the paused list use: a chat frozen by "Pause all"
    // carries no `paused` of its own, so `t.status` alone still said "running" and
    // the palette drew it with the live blue spinner — a frozen chat that looked
    // like it was still working, in the one list the user reaches for to jump to it.
    const pausedAll = !!settings?.paused_all;
    const chatCmds = (t: TaskSummary) => ({ g: "Task", icon: <StatusIcon status={isPaused(t, pausedAll) ? "paused" : t.status} color={modelColor(chatModel(t))} />, label: t.title, detail: taskSearchDetail(t), run: () => go("session", { task: t.id }) });
    return ql
      ? [...base.filter((c) => keep(c.g, c.label)),
         ...models.filter((m) => keep("Model", "Switch to " + m.name)).map(modelCmds),
         ...chats.filter((t) => matchesTaskQuery(t, ql)).map(chatCmds)].slice(0, 9)
      : [...base, ...models.slice(0, room).map(modelCmds), ...chats.slice(0, room).map(chatCmds)].slice(0, 9);
  }, [q, pal, settings, task, home, view, modelRows, tasks]);
  if (!pal) return null;
  const run = (c: Cmd) => { set({ pal: false }); c.run(); };
  return (
    <>
      <MenuScrim style={{ zIndex: 50 }} onClick={() => set({ pal: false })} />
      <div className="palette" onKeyDown={(e) => {
        const n = cmds.length || 1;
        if (e.key === "ArrowDown") { e.preventDefault(); setI((i + 1) % n); }
        if (e.key === "ArrowUp") { e.preventDefault(); setI((i - 1 + n) % n); }
        if (e.key === "Enter") { e.preventDefault(); const c = cmds[i]; c && run(c); }
      }}>
        <div style={{ display: "flex", alignItems: "center", gap: 10, padding: "0 16px", height: 48, borderBottom: "1px solid rgba(255,255,255,0.07)" }}>
          <Input ref={inp} className="" value={q} onChange={(e) => { setQ(e.currentTarget.value); setI(0); }} placeholder="Search commands, tasks, folders, branches…" style={{ flex: 1, border: 0, outline: 0, background: "transparent", color: "#f4f4f5", font: "inherit", fontSize: 15 }} />
          <Kbd>Esc</Kbd>
        </div>
        <div style={{ padding: 5, position: "relative" }}>
          <div style={{ position: "absolute", left: 5, right: 5, top: 5, height: 42, borderRadius: 8, background: "rgba(255,255,255,0.08)", transform: `translateY(${i * 42}px)`, opacity: cmds.length ? 1 : 0, transition: "transform .13s cubic-bezier(.32,.72,0,1)" }} />
          {cmds.map((c, j) => (
            <div key={c.g + c.label + j} onMouseMove={() => setI(j)} onClick={() => run(c)} style={{ position: "relative", display: "flex", alignItems: "center", gap: 10, minHeight: 42, padding: "0 10px", borderRadius: 8 }}>
              <span style={{ width: 16, textAlign: "center", color: "var(--mut2)", fontSize: 12 }}>{c.icon}</span>
              <span style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", justifyContent: "center", gap: 1 }}>
                <span style={{ whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", fontWeight: 500 }}>{c.label}</span>
                {c.detail && <span style={{ whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", fontSize: 10, color: "var(--mut3)" }}>{c.detail}</span>}
              </span>
              <span style={{ fontSize: 11, color: "var(--dim)" }}>{c.g}</span>
              <span className="secondary-text" style={{ minWidth: 44 }}>{chord(c.k)}</span>
            </div>
          ))}
          {!cmds.length && <div className="empty">No matches · try a task title, folder, branch, model, or command</div>}
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 14, height: 32, padding: "0 14px", borderTop: "1px solid rgba(255,255,255,0.07)", fontSize: 11, color: "var(--dim)" }}><span>Up/Down to navigate</span><span>Enter to run</span></div>
      </div>
    </>
  );
}

/**
 * One message, many chats. Reached two ways, and the two are different enough to
 * be worth naming: the sidebar's `Message N chats…` sends to the chats that were
 * ticked, and "Message all chats" sends to every agent that is working right now,
 * which needs no picking at all.
 */
export function MassMessage() {
  const open = useStore((s) => s.mass);
  const all = useStore((s) => s.massAll);
  const tasks = useStore((s) => s.tasks);
  const picked = useStore((s) => s.picked);
  const project = useStore((s) => s.settings?.project ?? "");
  const pausedAll = useStore((s) => s.settings?.paused_all ?? false);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  // The two reach options, back to their defaults each time the box opens. They
  // are deliberately not settings: both widen a send past the chats the user is
  // looking at, and that is a per-message decision.
  const [scope, setScope] = useState<MassScope>({ includePaused: false, allProjects: false });
  useEffect(() => { if (open) { setText(""); setScope({ includePaused: false, allProjects: false }); } }, [open]);
  if (!open) return null;

  // Ticked mode: the chats the user picked in the sidebar, newest first, dropping
  // any that were archived meanwhile. All mode: everyone working right now, which
  // needs no selection and so is never empty of intent — only of candidates.
  const list = all ? massTargets(Object.values(tasks), project, pausedAll, scope) : Object.keys(picked).filter((id) => tasks[id]).map((id) => tasks[id]!).filter(messageable);
  const ids = [...list].sort((a, b) => (b.updated_at ?? "").localeCompare(a.updated_at ?? "")).map((t) => t.id);
  const close = () => set({ mass: false });
  const send = async () => {
    const body = text.trim();
    if (busy || !body || !ids.length) return;
    setBusy(true);
    try {
      const n = await api.tasksMessage(ids, body);
      // A ticked send consumes the selection, so leave select mode with it.
      // Staying in select mode with nothing ticked would put the user back in a
      // mode they have to Escape out of, having just finished what they came to
      // do. An all-chats send never touched the selection, so it leaves it alone.
      if (all) set({ mass: false });
      else set({ mass: false, massAll: false, selecting: false, picked: {} });
      flash(n === ids.length ? `Sent to ${n} chat${n === 1 ? "" : "s"}` : `Sent to ${n} of ${ids.length} chats`);
    } catch (e) {
      flash(String(e));
    } finally {
      setBusy(false);
    }
  };
  const setOpt = (patch: Partial<MassScope>) => setScope((s) => ({ ...s, ...patch }));

  return (
    <Modal onClose={close} style={{ width: "min(520px, 100%)" }}>
      <div className="mh">
        <span style={{ flex: 1 }}>{all ? "Message all chats" : `Message ${ids.length} chat${ids.length === 1 ? "" : "s"}`}</span>
        <IconButton label="Close" onClick={close}>{I.close()}</IconButton>
      </div>
      <div className="mb">
        <TextArea className="input" autoFocus rows={4} value={text} placeholder="What should they all do?"
          onChange={(e) => setText(e.currentTarget.value)}
          onKeyDown={(e) => { if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); void send(); } }} />
        {all && (
          // The two ways to reach past the chats working in this project. Both
          // are off by default: each one widens a send to chats the user did not
          // point at, so both have to be chosen on purpose.
          <div className="sgroup" style={{ marginTop: 12 }}>
            <div className="srowx" style={{ alignItems: "flex-start" }}>
              <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Include paused chats</div><div className="desc">A frozen chat was interrupted rather than finished, so its agent is still there. It starts again when you resume it.</div></div>
              <Switch label="Include paused chats" checked={scope.includePaused} onChange={(v) => setOpt({ includePaused: v })} />
            </div>
            <div className="srowx" style={{ alignItems: "flex-start" }}>
              <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Every project</div><div className="desc">Reach the agents working in your other project folders as well, not just {project ? baseName(project) : "this one"}.</div></div>
              <Switch label="Every project" checked={scope.allProjects} onChange={(v) => setOpt({ allProjects: v })} />
            </div>
          </div>
        )}
        <div style={{ marginTop: 12, maxHeight: 220, overflowY: "auto" }} className="sgroup">
          {ids.map((id, i) => (
            <div key={id} style={{ animationDelay: i * 20 + "ms" }}>
              {/* A ticked chat keeps its remove button; one that was found by
                  scope can't be removed without making the rule narrower, which
                  is what the two switches above are for. */}
              {all ? <span className="tdot" aria-hidden="true" style={{ background: modelColor(tasks[id] ? chatModel(tasks[id]!) : "") }} />
                : <span className="pickbox on" aria-hidden="true">{I.close(9)}</span>}
              <div className="srowx" style={{ borderTop: i ? "1px solid rgba(255,255,255,0.05)" : 0 }}>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontWeight: 500, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{tasks[id]?.title || "Untitled chat"}</div>
                  <div className="desc">
                    {tasks[id]?.status ?? ""}{isPaused(tasks[id]!, pausedAll) ? " · paused" : ""}{all && tasks[id] && !chatProjectIsCurrent(tasks[id].project, project) ? ` · ${baseName(tasks[id].project)}` : ""}
                  </div>
                </div>
                {!all && <IconButton label="Remove" onClick={() => set((s) => ({ picked: togglePick(s.picked, id) }))}>{I.close(11)}</IconButton>}
              </div>
            </div>
          ))}
          {!ids.length && (
            <div className="srowx">
              <div className="desc">
                {all
                  ? "No chat is working right now." + (scope.includePaused ? "" : " Tick “Include paused chats” to reach frozen ones.")
                  : "No chat is ticked. Tick some in the sidebar, or pick them with “Select chats…”."}
              </div>
            </div>
          )}
        </div>
        {!all && ids.length > 0 && (
          <div style={{ marginTop: 10, display: "flex", gap: 10, alignItems: "center" }}>
            <TextButton onClick={() => set((s) => ({ picked: pickAll(s.picked, Object.values(s.tasks)) }))}>Tick every chat</TextButton>
            <TextButton onClick={() => set({ picked: {} })}>Clear</TextButton>
          </div>
        )}
      </div>
      <div className="mf">
        <span style={{ fontSize: 11, color: "var(--dim)" }}>Ctrl Enter to send</span>
        <div style={{ flex: 1 }} />
        <Button variant="ghost" onClick={close}>Cancel</Button>
        <Button variant="primary" disabled={!text.trim() || busy || !ids.length} onClick={() => void send()}>{busy && <Loader size={13} />}Send to {ids.length}</Button>
      </div>
    </Modal>
  );
}

/** The two ways a chat can be branched, in the words the backend acts on. */
const FORK_MODES = [
  {
    id: "compact" as const,
    icon: li(ScrollText),
    name: "Compacted",
    desc: "Keeps the context, drops the transcript. The model reads a summary of everything so far and carries on from there — the same thing /compact does. Cheapest way to try a different direction on a long chat.",
  },
  {
    id: "full" as const,
    icon: li(Copy),
    name: "Full copy",
    desc: "The entire conversation, word for word, in a new chat. Pick this when the exact wording of earlier turns matters.",
  },
];

/**
 * Fork a chat. The fork lands stopped in the sidebar, waiting to be told what it
 * is for; both modes run one model call to summarise, so both can take a moment.
 */
export function ForkChat() {
  const id = useStore((s) => s.fork);
  const task = useStore((s) => (id ? s.tasks[id] : null));
  const [mode, setMode] = useState<"full" | "compact">("compact");
  const [busy, setBusy] = useState(false);
  useEffect(() => { if (id) { setMode("compact"); setBusy(false); } }, [id]);
  if (!id || !task) return null;

  const close = () => set({ fork: null });
  const doFork = async (m: "full" | "compact") => {
    if (busy) return;
    setBusy(true);
    try {
      const f = await api.fork(id, m);
      set({ fork: null, tasks: { ...get().tasks, [f.id]: f } });
      await openTask(f.id);
      go("session", { task: f.id });
      flash(`Forked · ${m === "full" ? "full copy" : "compacted"}`);
    } catch (e) {
      flash(String(e));
      setBusy(false);
    }
  };

  return (
    <Modal onClose={close} style={{ width: "min(560px, 100%)" }}>
      <div className="mh">
        {li(GitFork, 15)}
        <span style={{ flex: 1, minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>Fork this chat</span>
        <IconButton label="Close" onClick={close}>{I.close()}</IconButton>
      </div>
      <div className="mb">
        <div className="desc" style={{ fontSize: 11.5, color: "#7c7c85", lineHeight: 1.5 }}>
          A new chat starting from <span style={{ color: "var(--mut)" }}>{task.title || "this chat"}</span>. It opens in the sidebar, stopped, waiting for you to say what the fork is for. This chat is left exactly as it is.
        </div>
        <div className="sgroup" style={{ borderRadius: 10, overflow: "hidden" }}>
          {FORK_MODES.map((m, i) => (
            <button key={m.id} type="button" disabled={busy} onClick={() => void doFork(m.id)}
              className="srowx" style={{ width: "100%", textAlign: "left", cursor: busy ? "default" : "pointer", background: "none", border: 0, borderTop: i ? "1px solid rgba(255,255,255,0.05)" : 0, animation: `olIn .3s ${i * 30}ms both`, opacity: busy ? 0.6 : 1 }}>
              <span style={{ color: "var(--mut2)", display: "flex", flex: "none" }}>{m.icon}</span>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ fontWeight: 500 }}>{m.name}</div>
                <div className="desc" style={{ lineHeight: 1.45 }}>{m.desc}</div>
              </div>
              {busy && m.id === mode ? <Loader size={13} /> : <span style={{ color: "#45454b", flex: "none" }}>›</span>}
            </button>
          ))}
        </div>
      </div>
      <div className="mf">
        <span style={{ fontSize: 11, color: "var(--dim)" }}>One model call either way</span>
        <div style={{ flex: 1 }} />
        <Button variant="ghost" onClick={close}>Cancel</Button>
      </div>
    </Modal>
  );
}

export const openFork = (id: string) => set({ fork: id, menu: null, pal: false });

/** Right-click a folder in the home project's menu. Mounted beside SmallMenu because
 *  closing that menu is what opens this one. */
function ProjCtxMenu({ z }: { z: number }) {  const ctx = useStore((s) => s.projCtx);
  if (!ctx) return null;
  return createPortal(<ProjectCtxMenu path={ctx.p} x={ctx.x} y={ctx.y} z={z} close={() => set({ projCtx: null })} />, document.body);
}

export function Overlays({ z }: { z: number }) {
  const toast = useStore((s) => s.toast);
  return (
    <>
      <SmallMenu z={z} />
      <ProjCtxMenu z={z} />
      <ModelPicker z={z} />
      <Palette />
      <ModelSwap />
      <UltraXEditor />
      <MassMessage />
      <ForkChat />
      <ChatStatsPanel />
      <TrustDialog />
      {toast && createPortal(<div className="toast" role="status">{toast}</div>, document.body)}
      <Notices />
    </>
  );
}
