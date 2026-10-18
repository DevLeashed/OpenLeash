// Settings → Accounts → "Spend quota before it resets": the backlog of
// low-priority prompts the quota burner runs on subscription quota that would
// otherwise be forfeited at a window's rollover, and the thresholds it acts on.
// The watcher itself lives in the backend (`agent::burner`); this panel only
// edits `settings.burner` and reads `burner_status`.
import { useEffect, useState } from "react";
import { api, baseName, BacklogItem, BURNER_DEFAULTS, BurnerCfg, BurnerStatus, Perm, PERMS, permLabel, permRung, until } from "../api";
import { flash, openTask, saveSettings, useStore } from "../store";
import { Button, Dropdown, Input, Switch, TextArea, TextButton, Tooltip } from "./primitives";

const RANK: Record<Perm, number> = { disabled: 0, allowlist: 1, reviewer: 1, auto: 2, turbo: 3 };

/** Permission modes an item may pick: none stronger than the user's default
 *  (the backend clamps too — this only keeps the menu honest). */
export function allowedPerms(defaultPerm: string | undefined) {
  const cap = RANK[permRung(defaultPerm)];
  return PERMS.filter((p) => RANK[permRung(p.id)] <= cap);
}

/** The burner config with defaults filled in, for a backend or settings file
 *  that predates it. */
export function burnerOf(cfg: Partial<BurnerCfg> | undefined): BurnerCfg {
  return { ...BURNER_DEFAULTS, ...cfg, queue: cfg?.queue ?? [] };
}

const clampNum = (v: string, lo: number, hi: number, fallback: number) => {
  const n = Number(v);
  return Number.isFinite(n) ? Math.max(lo, Math.min(hi, Math.round(n))) : fallback;
};

function NumberField({ label, value, unit, lo, hi, onSave }: { label: string; value: number; unit: string; lo: number; hi: number; onSave: (v: number) => void }) {
  const [text, setText] = useState(String(value));
  useEffect(() => setText(String(value)), [value]);
  const commit = () => {
    const v = clampNum(text, lo, hi, value);
    setText(String(v));
    if (v !== value) onSave(v);
  };
  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
      <Input aria-label={label} inputMode="numeric" value={text} onChange={(e) => setText(e.target.value)} onBlur={commit} onKeyDown={(e) => { if (e.key === "Enter") commit(); }} style={{ width: 64, height: 28, textAlign: "right" }} />
      <span className="desc">{unit}</span>
    </span>
  );
}

export function BurnerSection() {
  const s = useStore((st) => st.settings)!;
  const providers = useStore((st) => st.providers);
  const models = useStore((st) => st.models);
  const cfg = burnerOf(s.burner);
  const [status, setStatus] = useState<BurnerStatus | null>(null);
  const [prompt, setPrompt] = useState("");
  const [project, setProject] = useState(s.project || s.projects[0] || "");
  const [model, setModel] = useState("");
  const [perm, setPerm] = useState("");

  useEffect(() => {
    let live = true;
    const load = () => api.burnerStatus().then((st) => { if (live && st) setStatus(st); }).catch(() => {});
    load();
    const t = window.setInterval(load, 30_000);
    return () => { live = false; window.clearInterval(t); };
  }, [cfg.enabled, cfg.queue.length]);

  const save = (patch: Partial<BurnerCfg>) => void saveSettings({ burner: { ...cfg, ...patch } });
  const pools = new Set(providers.filter((p) => p.account).map((p) => p.id));
  const poolModels = models.filter((m) => pools.has(m.provider) && m.enabled !== false);
  const perms = allowedPerms(s.perm);
  const projects = Array.from(new Set([s.project, ...s.projects].filter(Boolean)));

  const add = () => {
    const text = prompt.trim();
    if (!text) return;
    if (!project) { flash("Pick a project for this backlog item"); return; }
    const item: BacklogItem = { id: `bl-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`, project, prompt: text, model, perm, created_at: Math.floor(Date.now() / 1000), error: "" };
    save({ queue: [...cfg.queue, item] });
    setPrompt("");
  };
  const remove = (id: string) => save({ queue: cfg.queue.filter((i) => i.id !== id) });
  const retry = (id: string) => save({ queue: cfg.queue.map((i) => (i.id === id ? { ...i, error: "" } : i)) });
  const move = (id: string, dir: -1 | 1) => {
    const q = [...cfg.queue];
    const i = q.findIndex((x) => x.id === id);
    const j = i + dir;
    if (i < 0 || j < 0 || j >= q.length) return;
    [q[i], q[j]] = [q[j]!, q[i]!];
    save({ queue: q });
  };

  const runs = [...(status?.runs ?? [])].sort((a, b) => (a.ended_at === 0 ? 0 : 1) - (b.ended_at === 0 ? 0 : 1) || b.started_at - a.started_at).slice(0, 10);

  return (
    <>
      <div className="settings-section-label">Spend quota before it resets</div>
      <div className="settings-note">When a subscription window is about to roll over with quota left, start the next backlog prompt as a new chat in its own git worktree, pinned to that provider. The chat is stopped when the window resets or a spend cap is reached; your usage caps always win. Burner chats are titled <span className="mono">[burner]</span>.</div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Quota burner</div><div className="desc">Off by default. Turning it off also stops any burner chat still running.</div></div>
          <Switch label="Quota burner" checked={cfg.enabled} onChange={(checked) => save({ enabled: checked })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Start when a window resets within</div><div className="desc">How close to the rollover a run may start.</div></div>
          <NumberField label="Reset within minutes" value={cfg.reset_within_min} unit="min" lo={1} hi={24 * 60} onSave={(v) => save({ reset_within_min: v })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>…and at least this much is left to spend</div><div className="desc">Percentage points still under the ceiling below (or your cap on that window, if lower).</div></div>
          <NumberField label="Minimum headroom percent" value={cfg.min_headroom} unit="%" lo={1} hi={100} onSave={(v) => save({ min_headroom: v })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Stop at</div><div className="desc">Percent used of any window of the account. A lower usage cap on a window takes precedence.</div></div>
          <NumberField label="Spend cap percent" value={cfg.spend_cap} unit="% used" lo={1} hi={100} onSave={(v) => save({ spend_cap: v })} /></div>
        {status && status.accounts.length > 0 && status.accounts.map((a) => (
          <div key={a.account_id} className="srowx" data-testid="burner-account">
            <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>{a.label || a.kind} <span className="desc">· {a.kind}</span></div>
              <div className="desc">{a.window ? `Ready: the ${a.window.label} window resets in ${until(a.window.resets_at)} with ${(100 - a.window.used).toFixed(0)}% left${cfg.enabled ? "" : " (burner is off)"}` : `Waiting: ${a.reason}`}</div></div>
          </div>
        ))}
        {status && status.accounts.length === 0 && <div className="srowx"><div className="desc">No subscription accounts connected — add one above.</div></div>}
      </div>

      <div className="settings-section-label">Backlog</div>
      <div className="sgroup">
        {cfg.queue.length === 0 && <div className="srowx"><div className="desc">Nothing queued. Add low-priority work like “sweep TODOs” or “audit dependencies”.</div></div>}
        {cfg.queue.map((i, n) => (
          <div key={i.id} className="srowx" data-testid="burner-item" style={{ alignItems: "flex-start" }}>
            <div style={{ flex: 1, minWidth: 0 }}>
              <div style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>{i.prompt}</div>
              <div className="desc">{baseName(i.project)} · {i.model || "any subscription"} · {i.perm ? permLabel(i.perm) : `default (${permLabel(s.perm)})`}</div>
              {i.error && <div className="desc" style={{ color: "var(--red)" }}>Couldn't start: {i.error}</div>}
            </div>
            <span style={{ display: "inline-flex", gap: 4 }}>
              {i.error && <TextButton onClick={() => retry(i.id)}>Retry</TextButton>}
              <TextButton aria-label="Move up" disabled={n === 0} onClick={() => move(i.id, -1)}>↑</TextButton>
              <TextButton aria-label="Move down" disabled={n === cfg.queue.length - 1} onClick={() => move(i.id, 1)}>↓</TextButton>
              <TextButton aria-label={`Remove ${i.prompt}`} onClick={() => remove(i.id)}>Remove</TextButton>
            </span>
          </div>
        ))}
        <div className="srowx" style={{ flexDirection: "column", alignItems: "stretch", gap: 8 }}>
          <TextArea aria-label="Backlog prompt" placeholder="A low-priority prompt, e.g. sweep TODOs and open issues for each" value={prompt} onChange={(e) => setPrompt(e.target.value)} rows={2} style={{ resize: "vertical" }} />
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
            <Dropdown value={project} search={projects.length > 8} options={projects.map((p) => ({ value: p, label: baseName(p) }))} onChange={setProject} style={{ width: 170, height: 28, padding: "0 8px" }} />
            <Dropdown value={model} options={[{ value: "", label: "Any subscription" }, ...poolModels.map((m) => ({ value: m.id, label: m.name || m.id }))]} onChange={setModel} style={{ width: 190, height: 28, padding: "0 8px" }} />
            <Tooltip content="Never stronger than your default permission mode">
              <span><Dropdown value={perm} search={false} options={[{ value: "", label: `Default (${permLabel(s.perm)})` }, ...perms.map((p) => ({ value: p.id, label: p.name }))]} onChange={setPerm} style={{ width: 170, height: 28, padding: "0 8px" }} /></span>
            </Tooltip>
            <Button variant="primary" disabled={!prompt.trim()} onClick={add}>Add to backlog</Button>
          </div>
        </div>
      </div>

      {runs.length > 0 && <>
        <div className="settings-section-label">Started by the burner</div>
        <div className="sgroup">
          {runs.map((r) => (
            <div key={r.task_id} className="srowx" data-testid="burner-run">
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{r.prompt}</div>
                <div className="desc">{r.account_label || r.kind} · {r.window} · {r.ended_at === 0 ? `running, stops by ${until(r.resets_at)}` : r.end_reason}</div>
              </div>
              <TextButton onClick={() => void openTask(r.task_id)}>Open</TextButton>
            </div>
          ))}
        </div>
      </>}
    </>
  );
}
