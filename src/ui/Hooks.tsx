import { useCallback, useEffect, useState } from "react";
import { api, Hook, HookEvent, HookView, HooksStatus } from "../api";
import { flash, saveSettings, set, useStore } from "../store";
import { Button, Dropdown, IconButton, Switch, TextButton } from "./primitives";
import { I } from "./icons";

const EVENTS: { id: HookEvent; label: string; desc: string }[] = [
  { id: "session_start", label: "When a run starts", desc: "Runs once when a run starts, in the chat's folder." },
  { id: "user_prompt_submit", label: "When you send a message", desc: "Runs when you send a message. A non-zero exit or JSON block/deny refuses the message and shows the reason (or a default refusal)." },
  { id: "pre_tool", label: "Before a tool", desc: "A non-zero exit blocks the call; JSON block/deny also blocks it, even with exit code 0. Its refusal is shown to the agent." },
  { id: "post_tool", label: "After a tool", desc: "Output is appended to the tool result (e.g. a formatter or linter)." },
  { id: "post_setup_worktree", label: "After a worktree is made", desc: "Runs inside a fresh worktree right after it is created. `OL_ROOT` / `$ROOT_WORKSPACE_PATH` points at the original checkout, so you can symlink or copy `.env`, `node_modules`, untracked files." },
  { id: "subagent_stop", label: "When a subagent finishes", desc: "Runs when a subagent finishes. The matcher matches its agent id." },
  { id: "error", label: "When a run fails", desc: "Runs when a run fails." },
  { id: "stop", label: "When the agent finishes", desc: "Non-zero exit sends the output back and the agent keeps going (max 3 rounds)." },
  { id: "session_end", label: "When a run ends", desc: "Runs once when a run ends, whatever the outcome." },
];

/** Events whose hook gets a matcher: they have a subject to match against (a
 *  tool name, or a subagent's agent id). Every other event runs unconditionally,
 *  so a matcher box there would be a field that does nothing. */
const MATCHED = new Set<HookEvent>(["pre_tool", "post_tool", "subagent_stop"]);

/** Amber is the app's one "caution" token — the same one the consent gate uses.
 *  A hook that will not run is a warning, not an error, and red for it would sit
 *  next to the primary Trust button with almost no contrast on the light themes. */
const AMBER = "var(--st-pause)";

const chip = (bg: string, color: string) => ({ flex: "none", fontSize: 10, padding: "1px 6px", borderRadius: 5, background: bg, color } as const);
const block = { padding: "6px 8px", borderRadius: 7, background: "var(--ov-40)", border: "1px solid var(--ov-70)", whiteSpace: "pre-wrap", overflowWrap: "anywhere", fontSize: 11.5 } as const;

/**
 * One hook, user or project. Trust is the part that matters: the backend will
 * not run a hook the user has not reviewed, so the row has to say so rather than
 * let a switch that says "on" imply the command is live.
 */
function HookRow({ h, v, origin, onToggle, onRemove, onTrust }: {
  h: Hook; v?: HookView; origin?: string;
  onToggle?: (on: boolean) => void; onRemove?: () => void; onTrust?: (approve: boolean) => void;
}) {
  const stale = !!v?.stale;
  const label = EVENTS.find((e) => e.id === h.event)?.label ?? h.event;
  const match = MATCHED.has(h.event) ? ` · ${h.matcher || (h.event === "subagent_stop" ? "every agent" : "every tool")}` : "";
  return (
    <div className="srowx" style={{ minHeight: 44, flexDirection: stale ? "column" : undefined, alignItems: stale ? "stretch" : undefined, background: stale ? "color-mix(in srgb, var(--st-pause) 7%, transparent)" : undefined }}>
      <div style={{ display: "flex", alignItems: "center", gap: 12, minWidth: 0, width: "100%" }}>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
            <div className="mono" style={{ fontSize: 12, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }} title={h.command}>{h.command}</div>
            {h.updated_input && <span style={chip("rgba(var(--accent-rgb), 0.12)", "var(--accent2)")}>rewrites args</span>}
          </div>
          <div className="desc">{label}{match}</div>
          {origin && <div className="desc mono" style={{ fontSize: 10.5 }}>{origin}</div>}
          {stale && <div style={{ marginTop: 4, fontSize: 11.5, fontWeight: 500, color: AMBER }}>Changed since you approved it — it will not run.</div>}
          {v && !v.trusted && !v.stale && <div style={{ marginTop: 4, fontSize: 11.5, fontWeight: 500, color: AMBER }}>Needs review — it will not run.</div>}
        </div>
        {v?.id ? (v.trusted
          ? <span style={{ display: "inline-flex", alignItems: "center", gap: 8, flex: "none" }}><span style={{ fontSize: 11, color: "var(--mut3)" }}>Trusted</span><TextButton style={{ fontSize: 11.5, color: "var(--mut3)" }} onClick={() => onTrust?.(false)}>Revoke</TextButton></span>
          : <Button variant="primary" style={{ height: 26, flex: "none" }} onClick={() => onTrust?.(true)}>Trust</Button>) : null}
        {onToggle && <Switch label={h.command} hint={h.enabled ? "On · click to turn off" : "Off · click to turn on"} checked={h.enabled} onChange={(checked) => onToggle(checked)} />}
        {onRemove && <IconButton label={`Remove hook ${h.command}`} onClick={onRemove} style={{ width: 24, height: 24 }}>{I.close()}</IconButton>}
      </div>
      {stale && (
        <div style={{ display: "flex", flexDirection: "column", gap: 5 }}>
          <div className="desc" style={{ fontSize: 10.5, textTransform: "uppercase", letterSpacing: "0.05em" }}>You approved</div>
          <div className="mono" style={block}>{v?.changed_from || "(empty)"}</div>
          <div className="desc" style={{ fontSize: 10.5, textTransform: "uppercase", letterSpacing: "0.05em" }}>Now</div>
          <div className="mono" style={block}>{h.command}</div>
        </div>
      )}
    </div>
  );
}

export function ChecksTab() {
  const s = useStore((st) => st.settings)!;
  const hooks = s.hooks ?? [];
  const [d, setD] = useState<Hook>({ event: "post_tool", matcher: "edit_file|multi_edit|write_file", command: "", enabled: true, updated_input: false });
  // Trust lives outside settings.json — it is a review record, not a preference —
  // so it comes from `hooks_status` and has to be re-read after anything that
  // could move it. `settings.hooks` stays the source of truth for the editable
  // list; this is only the trust state and the project hooks, which are not in
  // settings at all.
  const [st, setSt] = useState<HooksStatus | null>(null);
  // A failed fetch is surfaced rather than swallowed: without it every hook
  // renders untrusted with no Trust button, and a silent empty trust list reads
  // as "nothing to do" when the hooks are in fact still blocked.
  const load = useCallback(() => api.hooksStatus().then(setSt).catch((e) => flash(`Couldn't load hook status · ${String(e)}`)), []);
  useEffect(() => { void load(); }, [load]);
  // Hooks go through their own command: the generic settings patch refuses them,
  // so this is the only path that can set one.
  const save = (h: Hook[]) => api.hooks(h).then((next) => { set({ settings: next }); void load(); }).catch((e) => flash(`Couldn't save hooks · ${String(e)}`));
  const trust = (id: string, origin: string, approve: boolean) => api.hooksTrust(id, origin, approve).then((next) => { set({ settings: next }); void load(); }).catch((e) => flash(`Couldn't ${approve ? "trust" : "revoke"} hook · ${String(e)}`));
  const add = () => { if (!d.command.trim()) return; save([...hooks, { ...d, command: d.command.trim(), matcher: d.matcher.trim(), updated_input: d.event === "pre_tool" ? !!d.updated_input : undefined }]); setD({ ...d, command: "" }); };

  const views = st?.hooks ?? [];
  const userViews = views.filter((v) => v.source !== "project");
  const projectHooks = views.filter((v) => v.source === "project");
  const byIdentity = new Map<string, HookView>();
  for (const v of views) if (v.id) byIdentity.set(`${v.origin ?? ""}\u0000${v.id}`, v);
  // A hook written before ids existed carries none, and the backend lists hooks
  // in settings order, so position is the fallback — but only then: matching a
  // hook that *has* an id by position would put one row's trust state on another
  // while the status is still on its way.
  const viewOf = (h: Hook, i: number): HookView | undefined => (h.id ? byIdentity.get(`\u0000${h.id}`) : userViews[i]);
  // Count reviewable hooks, not just the user's: a project hook in the repo is
  // exactly as able to run and just as gated, and it is the one the user has not
  // looked at yet.
  const untrusted = views.filter((v) => !v.trusted).length;

  return (
    <>
      <div style={{ fontSize: 15, fontWeight: 600 }}>Checks &amp; hooks</div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Check for errors after edits</div><div className="desc">After the agent edits files, runs tsc / cargo check / go build / python syntax on that project and hands back errors in the files it just touched. Only uses tools already installed.</div></div>
          <Switch label="Check for errors after edits" hint="Run the project's type-check, linter or build after the agent edits" checked={s.diagnostics !== false} onChange={(checked) => void saveSettings({ diagnostics: checked })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Make agents verify their work</div><div className="desc">An agent that changed code but never built, tested or ran anything gets one reminder to check before it says it's done.</div></div>
          <Switch label="Make agents verify their work" hint="Nudge an agent to build, test or run before it says it's done" checked={s.verify_nudge !== false} onChange={(checked) => void saveSettings({ verify_nudge: checked })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Snapshot files for rewind</div><div className="desc">Keeps a shadow git copy of this project folder under <span className="mono">~/.openleash/checkpoints</span>, committed after each turn, so rewinding a chat can put the files back and not only the conversation. It never touches your own repository. Off, rewind offers the conversation only.</div></div>
          <Switch label="Snapshot files for rewind" hint="Keep a shadow copy of the working folder so rewind can restore files" checked={s.checkpoints !== false} onChange={(checked) => void saveSettings({ checkpoints: checked })} /></div>
      </div>
      <div className="label" style={{ padding: "4px 2px 0" }}>Hooks</div>
      {untrusted > 0 && (
        <div style={{ display: "flex", alignItems: "flex-start", gap: 9, padding: "10px 12px", borderRadius: 10, border: "1px solid color-mix(in srgb, var(--st-pause) 34%, transparent)", background: "color-mix(in srgb, var(--st-pause) 11%, transparent)", color: AMBER, fontSize: 12, lineHeight: 1.55 }}>
          <span aria-hidden="true" style={{ flex: "none", marginTop: 1 }}>{I.shield()}</span>
          <div>{untrusted} hook{untrusted === 1 ? " is" : "s are"} waiting to be reviewed and will not run until you trust {untrusted === 1 ? "it" : "them"}.</div>
        </div>
      )}
      <div className="sgroup">
        {hooks.map((h, i) => (
          <HookRow key={h.id ?? i} h={h} v={viewOf(h, i)}
            onToggle={(on) => save(hooks.map((x, j) => (j === i ? { ...x, enabled: on } : x)))}
            onRemove={() => save(hooks.filter((_, j) => j !== i))}
            onTrust={(approve) => { const v = viewOf(h, i); if (v?.id) void trust(v.id, v.origin ?? "", approve); }} />
        ))}
        {!hooks.length && <div className="empty">No hooks yet. Example: after edits, run <span className="mono">npx prettier --write "$(echo $OL_INPUT | jq -r .path)"</span>.</div>}
        <div className="srowx" style={{ flexWrap: "wrap", gap: 8 }}>
          <Dropdown search={false} style={{ width: 190 }} value={d.event} onChange={(v) => setD({ ...d, event: v as HookEvent })} options={EVENTS.map((e) => ({ value: e.id, label: e.label }))} />
          {MATCHED.has(d.event) && <input className="input mono" style={{ width: 200, fontSize: 11.5 }} placeholder={d.event === "subagent_stop" ? "agent id (empty = all)" : "tool regex (empty = all)"} value={d.matcher} onChange={(e) => setD({ ...d, matcher: e.currentTarget.value })} />}
          {d.event === "pre_tool" && (
            <span style={{ display: "inline-flex", alignItems: "center", gap: 6, flex: "none" }}>
              <Switch small label="Let this hook rewrite the tool call's arguments" hint={'Before the tool runs, this hook may replace the whole call\'s arguments or patch them, by printing {"updatedInput": {...}}. Only applies once you have trusted the hook.'} checked={!!d.updated_input} onChange={(on) => setD({ ...d, updated_input: on })} />
              <span style={{ fontSize: 11.5, color: "var(--hint)" }}>may rewrite arguments</span>
            </span>
          )}
          <input className="input mono" style={{ flex: 1, minWidth: 200, fontSize: 11.5 }} placeholder="command" value={d.command} onChange={(e) => setD({ ...d, command: e.currentTarget.value })} onKeyDown={(e) => e.key === "Enter" && add()} />
          <Button onClick={add}>Add</Button>
        </div>
      </div>
      {projectHooks.length > 0 && (
        <>
          <div className="label" style={{ padding: "4px 2px 0" }}>Hooks from this project</div>
          <div className="sgroup">
            {projectHooks.map((v, i) => <HookRow key={v.id ?? i} h={v} v={v} origin={v.origin || st?.project} onTrust={(approve) => { if (v.id && v.origin) void trust(v.id, v.origin, approve); }} />)}
          </div>
          <div className="desc" style={{ fontSize: 11.5, color: "#7c7c85", lineHeight: 1.55 }}>
            Defined in <span className="mono">.openleash/hooks.json</span> at the root of this project, so they travel with the repo and everyone who opens it gets them. They run in this project only. Each one has to be reviewed before it runs — edit them in the repo, not here.
          </div>
        </>
      )}
      <div className="desc" style={{ fontSize: 11.5, color: "#7c7c85", lineHeight: 1.55 }}>
        {EVENTS.find((e) => e.id === d.event)?.desc} Hooks run in the chat's folder with <span className="mono">OL_EVENT</span>, <span className="mono">OL_TOOL</span>, <span className="mono">OL_INPUT</span> (JSON), <span className="mono">OL_OUTPUT</span>, <span className="mono">OL_CWD</span> and <span className="mono">OL_TASK</span> set; <span className="mono">OL_PROMPT</span> carries your message (user_prompt_submit), <span className="mono">OL_SUBAGENT</span> and <span className="mono">OL_REPORT</span> the agent id and its report (subagent_stop), and <span className="mono">OL_WORKTREE</span> / <span className="mono">OL_ROOT</span> the new worktree and the checkout it came from (post_setup_worktree). 60 s timeout. A trusted pre_tool hook can print <span className="mono">{`{"updatedInput": {...}}`}</span> to rewrite the call's arguments, and a hook can print <span className="mono">{`{"decision":"block"}`}</span> / <span className="mono">{`{"decision":"deny"}`}</span> to refuse regardless of exit code.
      </div>
    </>
  );
}
