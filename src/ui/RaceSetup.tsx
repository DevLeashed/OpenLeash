// The composer's race options: the pill in the new-task composer and the
// setup dialog behind it. One prompt, N attempts, each on the model and effort
// picked here; a judge model ranks them once they are done.
import { useMemo } from "react";
import { clampEffort, EFFORTS, effortLabel, effortSteps, MAX_RACE_ATTEMPTS, MIN_RACE_ATTEMPTS, type ModelInfo, type RaceAttemptSpec } from "../api";
import { isConnected, modelInfo, shownProviders, useStore } from "../store";
import { clampRaceN, resolveAttempts, resolveJudge, setRaces, toggleRace, useRaces, type RaceConfig } from "../race";
import { I } from "./icons";
import { Button, Dropdown, Modal, Pressable, Segmented, Tooltip, type Opt } from "./primitives";
import "./Race.css";

/** Connected, enabled models, plus any model already in use so a pick that
 *  has since been disabled never silently drops out of its row. */
export function useModelOptions(keep: string[]): Opt[] {
  const models = useStore((s) => s.models);
  const provs = useStore((s) => s.providers);
  const key = keep.join("\n");
  return useMemo(() => {
    const on = new Set(shownProviders(provs).filter((p) => p.enabled && isConnected(p)).map((p) => p.id));
    const all: ModelInfo[] = models.filter((m) => on.has(m.provider) && m.enabled !== false);
    for (const id of key.split("\n")) if (id && !all.some((m) => m.id === id)) all.push(modelInfo(id));
    return all.map((m) => ({ value: m.id, label: m.name, group: provs.find((p) => p.id === m.provider)?.name ?? m.provider }));
  }, [models, provs, key]);
}

/** The effort rungs a model really has; empty when it has no effort control. */
export function effortOpts(model: string): Opt[] {
  const mi = modelInfo(model);
  return effortSteps(mi).map((v) => ({ value: String(v), label: effortLabel(mi, v) ?? EFFORTS[v] ?? String(v) }));
}

/** Pick a new model for a row, keeping its effort on a rung the model has. */
const withModel = (spec: RaceAttemptSpec, model: string): RaceAttemptSpec => ({ model, effort: clampEffort(effortSteps(modelInfo(model)), spec.effort) });

/** A short "Opus 5 ×2 · Sonnet" summary of the lineup. */
export function lineupLabel(specs: RaceAttemptSpec[]): string {
  const counts = new Map<string, number>();
  for (const s of specs) counts.set(s.model, (counts.get(s.model) ?? 0) + 1);
  return [...counts].map(([m, n]) => `${modelInfo(m).name}${n > 1 ? ` ×${n}` : ""}`).join(" · ");
}

function ModelEffort({ spec, options, onChange, label }: { spec: RaceAttemptSpec; options: Opt[]; onChange: (s: RaceAttemptSpec) => void; label: string }) {
  const efforts = effortOpts(spec.model);
  return (
    <div className="race-setup-row" aria-label={label}>
      <span className="race-setup-label">{label}</span>
      <Dropdown value={spec.model} options={options} onChange={(m) => onChange(withModel(spec, m))} style={{ fontSize: 12 }} />
      {efforts.length
        ? <Dropdown value={String(clampEffort(effortSteps(modelInfo(spec.model)), spec.effort))} options={efforts} onChange={(v) => onChange({ ...spec, effort: Number(v) })} search={false} style={{ fontSize: 12 }} />
        : <span className="race-setup-noeffort">no effort control</span>}
    </div>
  );
}

/** The dialog, bound to the composer's race config and the new-chat model. */
export function RaceSetupDialog() {
  const open = useRaces((s) => s.setup);
  if (!open) return null;
  return <RaceSetupBody />;
}

function RaceSetupBody() {
  const cfg = useRaces((s) => s.config);
  const model = useStore((s) => s.home.model);
  const effort = useStore((s) => s.home.effort);
  const attempts = resolveAttempts(cfg, model, effort);
  const judge = resolveJudge(cfg, model, effort);
  const options = useModelOptions([...attempts.map((a) => a.model), judge.model, model]);
  const close = () => setRaces({ setup: false });
  const patch = (p: Partial<RaceConfig>) => setRaces((s) => ({ config: { ...s.config, ...p } }));
  const setAttempt = (i: number, spec: RaceAttemptSpec) => patch({ attempts: attempts.map((a, j) => (j === i ? spec : a)) });
  return (
    <Modal onClose={close} ariaLabel="Race setup" style={{ width: "min(620px, 100%)" }}>
      <div className="mh">
        <span style={{ flex: 1 }}>Race</span>
        <span style={{ fontSize: 11.5, fontWeight: 400, color: "var(--mut3)" }}>{attempts.length} attempts · best one wins</span>
      </div>
      <div className="mb" style={{ display: "flex", flexDirection: "column", gap: 14 }}>
        <div className="desc" style={{ fontSize: 11.5, color: "var(--mut3)", lineHeight: 1.55 }}>
          Your prompt runs as {attempts.length} independent chats, each in its own {cfg.workspace === "docker" ? "docker workspace (a private clone, commands in the project's container)" : "git worktree"}. When they finish, the project's checks run on each and a
          read-only judge ranks them. You pick the winner and decide whether to merge it.
        </div>
        <div style={{ display: "flex", gap: 9, alignItems: "center", flexWrap: "wrap" }}>
          <span style={{ fontSize: 11, color: "var(--mut3)" }}>Attempts</span>
          <Segmented label="Attempts" quiet value={String(clampRaceN(cfg.n))} onChange={(v) => patch({ n: clampRaceN(Number(v)) })}
            options={Array.from({ length: MAX_RACE_ATTEMPTS - MIN_RACE_ATTEMPTS + 1 }, (_, i) => ({ value: String(i + MIN_RACE_ATTEMPTS), label: String(i + MIN_RACE_ATTEMPTS) }))} />
          {cfg.attempts && <Button variant="ghost" style={{ fontSize: 11.5, padding: "3px 8px" }} onClick={() => patch({ attempts: null })}>Use {modelInfo(model).name} for all</Button>}
        </div>
        <div style={{ display: "flex", gap: 9, alignItems: "center", flexWrap: "wrap" }}>
          <span style={{ fontSize: 11, color: "var(--mut3)" }}>Each attempt works in</span>
          <Segmented label="Attempt workspace" quiet value={cfg.workspace ?? "worktree"} onChange={(v) => patch({ workspace: v === "docker" ? "docker" : "worktree" })}
            options={[{ value: "worktree", label: "Worktree" }, { value: "docker", label: "Docker" }]} />
          {cfg.workspace === "docker" && <span style={{ fontSize: 11, color: "var(--dim)" }}>Needs a trusted project with .openleash_docker/</span>}
        </div>
        <div className="race-setup-list">
          {attempts.map((a, i) => <ModelEffort key={i} label={`Attempt ${i + 1}`} spec={a} options={options} onChange={(s) => setAttempt(i, s)} />)}
          <div className="race-setup-sep" />
          <ModelEffort label="Judge" spec={judge} options={options} onChange={(s) => patch({ judge: s })} />
        </div>
        <div style={{ fontSize: 11, color: "var(--dim)" }}>Permissions, plan mode, assist and subagents follow the composer. Ultrathread is not used in a race.</div>
      </div>
      <div className="mf">
        <Button variant="ghost" onClick={() => { close(); if (cfg.on) toggleRace(); }}>Turn race off</Button>
        <div style={{ flex: 1 }} />
        <Button variant="primary" onClick={close}>Done</Button>
      </div>
    </Modal>
  );
}

/** The pill beside plan/ultrathread in the new-task composer, while race is on. */
export function RaceChip() {
  const cfg = useRaces((s) => s.config);
  const model = useStore((s) => s.home.model);
  const effort = useStore((s) => s.home.effort);
  if (!cfg.on) return null;
  const attempts = resolveAttempts(cfg, model, effort);
  return (
    <Tooltip content={`Race · ${lineupLabel(attempts)} · judged by ${modelInfo(resolveJudge(cfg, model, effort).model).name}. Click to configure.`}>
      <Pressable className="planpill racepill" onClick={() => setRaces({ setup: true })}>
        Race ×{attempts.length}
        <span role="button" aria-label="Turn race off" style={{ fontSize: 10, opacity: 0.7, display: "flex" }} onClick={(e) => { e.stopPropagation(); toggleRace(); }}>{I.close(9)}</span>
      </Pressable>
    </Tooltip>
  );
}
