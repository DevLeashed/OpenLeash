import { useEffect, useMemo, useState } from "react";
import { api, clampEffort, EFFORTS, effortLabel, effortSteps } from "../api";
import { Button, Dropdown, Modal, Switch } from "./primitives";
import { defaultRoute, flash, get, isConnected, modelInfo, set, shownProviders, useStore } from "../store";

/** One row: a model in use, at the reasoning its agents are actually on. */
type Row = { model: string; effort: number; uses: string[] };

/** Batch model swap. In a chat: that chat's main agent + subagents. On the home page: defaults + every live chat. */
export function ModelSwap() {
  const open = useStore((s) => s.swap);
  const scope = useStore((s) => (s.view === "session" || s.view === "diff" ? s.task : null));
  const title = useStore((s) => (scope ? s.tasks[scope]?.title : null));
  const models = useStore((s) => s.models);
  const provs = useStore((s) => s.providers);
  const [rows, setRows] = useState<Row[] | null>(null);
  // Subagent types a `spread` would reach. Empty for a chat's own swap, which
  // has no reach past the chat — and the toggle is not offered there at all.
  const [agents, setAgents] = useState<{ id: string; name: string }[]>([]);
  // Off by default and remembered per dialog opening, not persisted: "and also
  // change what every future spawn of those types runs on" is a second decision,
  // and it has to be made on purpose every time rather than inherited from last time.
  const [spread, setSpread] = useState(false);
  const [pick, setPick] = useState<Record<string, string>>({});
  // Reasoning is picked per row, against the model that row ends up on — a
  // destination's ladder is not the source's, and only the destination's is real.
  const [effort, setEffort] = useState<Record<string, number>>({});
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!open) return;
    setRows(null);
    setAgents([]);
    setSpread(false);
    setPick({});
    setEffort({});
    api.modelsInUse(scope ?? undefined)
      .then((d) => {
        setRows(d.rows ?? []);
        setAgents(d.agents ?? []);
      })
      .catch((e) => { flash(String(e)); set({ swap: false }); });
  }, [open, scope]);

  const options = useMemo(() => {
    const on = new Set(shownProviders(provs).filter((p) => p.enabled && isConnected(p)).map((p) => p.id));
    return models.filter((m) => on.has(m.provider) && m.enabled !== false);
  }, [models, provs]);

  if (!open) return null;
  const close = () => set({ swap: false });
  const changed = Object.entries(pick).filter(([from, to]) => to && to !== from);
  /** The model a row is on right now, and the level picker for where it's going. */
  const target = (r: Row) => {
    const to = pick[r.model] ?? r.model;
    const mi = modelInfo(to);
    const steps = effortSteps(mi);
    // Seed from what the row is on, snapped to a rung the destination really has:
    // a source model's level means nothing on a destination with a different ladder.
    const cur = effort[r.model] ?? clampEffort(steps, r.effort);
    return { to, mi, steps, cur };
  };
  /** Reasoning rows that have actually moved, whether or not the model did. */
  const effortChanged = rows
    ? Object.entries(effort).filter(([from, e]) => {
        const r = rows.find((x) => x.model === from);
        return !!r && e !== clampEffort(effortSteps(modelInfo(pick[r.model] ?? r.model)), r.effort);
      })
    : [];
  const apply = async () => {
    if (!changed.length && !effortChanged.length) return close();
    setBusy(true);
    try {
      const settings = await api.modelsSwap(
        Object.fromEntries(changed),
        scope ?? undefined,
        // Keyed by the row's model, which the backend re-keys to the destination
        // when that row is also swapping. A row the user only moved the reasoning
        // on is a real change and goes through like any other — it used to be
        // dropped here, which is why the only way to change reasoning on a model
        // you were keeping was to swap away from it and back.
        Object.fromEntries(effortChanged),
        spread,
      );
      const map = Object.fromEntries(changed);
      const eff = Object.fromEntries(effortChanged);
      // Reasoning picked for the row the home default sits on, so the composer
      // does not keep showing a level the swap already replaced.
      const homeModel = get().home.model;
      const homeTo = map[homeModel] ?? homeModel;
      const homeEff = eff[homeModel];
      set((s) => {
        const home = homeTo !== homeModel
          ? { ...s.home, model: homeTo, route: defaultRoute(homeTo), effort: eff[homeModel] ?? s.home.effort }
          : homeEff != null
            ? { ...s.home, effort: homeEff }
            : s.home;
        return { settings, swap: false, home };
      });
      // Only a `spread` rewrites the agent-type models, so only a `spread` leaves
      // the catalog the Subagents panel renders pointing at the old defaults.
      // This re-read used to run on every global swap, which was the tell that the
      // swap had been quietly rewriting those models.
      if (!scope && spread) void api.agents(get().settings?.project).then((a) => set({ agents: a })).catch(() => {});
      const models = changed.length;
      const levels = effortChanged.length;
      flash(
        [models ? `${models} model${models === 1 ? "" : "s"}` : "", levels ? `${levels} reasoning level${levels === 1 ? "" : "s"}` : ""]
          .filter(Boolean)
          .join(" and ")
          + (scope
            ? " · takes effect on the next request"
            : spread
              ? " · new chats and the chosen subagent types start on these"
              : " · new chats start on these"),
      );
    } catch (e) {
      flash(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal onClose={close} style={{ width: "min(780px, 100%)" }}>
      <div className="mh">
        <span style={{ flex: 1 }}>Swap models</span>
        <span style={{ fontSize: 11.5, fontWeight: 400, color: "#7c7c85", maxWidth: 280, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{scope ? `this chat · ${title ?? ""}` : "everywhere · defaults + live chats"}</span>
      </div>
      <div className="mb">
        <div className="desc" style={{ fontSize: 11.5, color: "#7c7c85", lineHeight: 1.5 }}>
          Every model {scope ? "this chat" : "your running and paused chats"} is using, main agents and subagents. Change a model, or only the reasoning it runs at — either way it takes effect on the very next request, no restart.
        </div>
        {!rows && <div className="empty">Looking…</div>}
        {rows && !rows.length && <div className="empty">Nothing is using a model right now.</div>}
        {rows?.map((r, i) => {
          const { to, mi, steps, cur } = target(r);
          const diff = to !== r.model;
          // A level moved on a model that is *not* moving is a change in its own
          // right, so it counts here and colours the row. It used to be gated on
          // `diff`, which is what made reasoning-only edits invisible: the row
          // stayed plain, the counter said no changes, and Apply did nothing.
          const effDiff = cur !== r.effort;
          return (
            <div key={r.model} className="srowx" style={{ alignItems: "flex-start", gap: 12, animation: `olIn .3s ${i * 30}ms both`, borderRadius: 10, background: diff || effDiff ? "rgba(167,139,250,0.06)" : "transparent", padding: "8px 10px" }}>
              <div style={{ flex: 1, minWidth: 0 }}>
                {/* The name is allowed to wrap rather than truncate: at 190px the
                    box cut off most real model names ("Claude Opus 4.5 (1M
                    context)"), and the whole point of the dialog is choosing
                    between names. */}
                <div style={{ fontWeight: 500, lineHeight: 1.35 }}>{modelInfo(r.model).name}</div>
                <div className="desc" style={{ fontSize: 11 }}>{r.uses.join(" · ")}</div>
              </div>
              <span style={{ color: diff ? "#a78bfa" : "#45454b", paddingTop: 6 }}>→</span>
              <Dropdown
                value={to}
                // Wide enough for a full model name on one line. The list that
                // opens is at least this wide too, so what you picked and what
                // you chose from read the same.
                style={{ width: 280, color: diff ? "#c4b5fd" : undefined }}
                onChange={(v) => setPick({ ...pick, [r.model]: v })}
                options={[
                  ...(options.some((m) => m.id === r.model) ? [] : [{ value: r.model, label: modelInfo(r.model).name, hint: "current" }]),
                  ...options.map((m) => ({ value: m.id, label: m.name, hint: m.id === r.model ? "keep" : undefined, group: provs.find((p) => p.id === m.provider)?.name ?? m.provider })),
                ]}
              />
              {/* Reasoning for wherever this row ends up. Hidden rather than
                  disabled when the destination has no reasoning control, so the
                  rows stay aligned and the row reads as one thing. */}
              {steps.length > 0 ? (
                <Dropdown
                  value={String(cur)}
                  // Level names are longer than a rung index ("Very high"), so
                  // this one was clipped too — and a picker you cannot read is a
                  // picker you cannot trust to change.
                  style={{ width: 140, color: effDiff ? "#c4b5fd" : undefined }}
                  onChange={(v) => setEffort({ ...effort, [r.model]: Number(v) })}
                  // `effortSteps` returns positions in 0..4, the same range `EFFORTS` indexes.
                  options={steps.map((e) => ({
                    value: String(e),
                    label: effortLabel(mi, e) ?? EFFORTS[e]!,
                    hint: e === clampEffort(steps, r.effort) ? "current" : undefined,
                  }))}
                />
              ) : (
                <span style={{ width: 140, paddingTop: 8, fontSize: 11, color: "#45454b" }}>no reasoning</span>
              )}
            </div>
          );
        })}
        {/* The one thing a swap here will NOT do unless asked. Subagent types carry
            a model of their own, and that default is not traffic: nothing is
            running on one until it spawns. Swapping a chat used to move it anyway,
            through a row that never mentioned it, so the type's default changed
            for every future spawn in every chat. Naming the types is the point —
            "14 subagent types" would not be saying what is about to change. */}
        {!scope && !!agents.length && (
          <div className="srowx" style={{ marginTop: 6, padding: "10px 10px", borderRadius: 10, background: spread ? "rgba(167,139,250,0.06)" : "transparent", border: spread ? "1px solid rgba(167,139,250,0.22)" : "1px solid rgba(255,255,255,0.07)" }}>
            <Switch
              checked={spread}
              onChange={setSpread}
              label="Also change subagent types"
              hint={spread ? "On" : "Off"}
            />
            <div style={{ flex: 1, minWidth: 0 }}>
              <div style={{ fontWeight: 500, lineHeight: 1.35 }}>Also change the subagent types</div>
              <div className="desc" style={{ fontSize: 11 }}>
                {spread
                  ? `These will point at the new models too: ${agents.map((a) => a.name).join(", ")}.`
                  : `${agents.length} type${agents.length === 1 ? "" : "s"} pin a model of their own (${agents.map((a) => a.name).join(", ")}). Left alone, they keep using it.`}
              </div>
            </div>
          </div>
        )}
      </div>
      <div className="mf">
        <span style={{ flex: 1, fontSize: 11.5, color: "#7c7c85" }}>{changed.length + effortChanged.length ? `${changed.length + effortChanged.length} change${changed.length + effortChanged.length === 1 ? "" : "s"}` : "No changes yet"}</span>
        <Button variant="ghost" onClick={close}>Cancel</Button>
        <Button variant="primary" style={{ opacity: (changed.length || effortChanged.length) && !busy ? 1 : 0.5 }} onClick={() => !busy && void apply()}>{busy ? "Applying…" : "Apply now"}</Button>
      </div>
    </Modal>
  );
}

export const openSwap = () => set({ swap: true, menu: null, pal: false });
