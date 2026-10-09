// ULTRATHREAD X: the ladder editor. One row per layer below the orchestrator,
// each with its own model, effort and fanout. Six empty pickers is a feature
// nobody turns on, so it opens on a preset the user can edit down.
import { useMemo, useState } from "react";
import { EFFORTS, effortLabel, effortSteps, ModelInfo, ProviderView, UltraX, UltraXLayer, X_DEFAULT_RUNNING, X_DEFAULT_TOTAL, X_MAX_FANOUT, X_MAX_LAYERS, defaultX, emptyXLayer, xDepth } from "../api";
import { flash, get, isConnected, modelInfo, set, shownProviders, useStore } from "../store";
import { setOpts } from "./Composer";
import { Dropdown, Modal, Segmented } from "./primitives";

/** Presets that make the common ladders one click. */
const PRESETS = [
  { id: "smart", name: "Smart", desc: "Your model coordinates, the cheapest model does the work" },
  { id: "all", name: "One model", desc: "Every layer on the chat's model, full effort" },
  { id: "tiered", name: "Tiered", desc: "Big model up top, cheaper models deeper, effort falls away" },
] as const;
type PresetId = (typeof PRESETS)[number]["id"];

/** Effort rungs for one layer's model, or the full ladder when the layer has
 *  no model of its own and runs on the chat's. */
const EFFORT_NAMES: string[] = EFFORTS;

/** Effort rungs for one layer's model, or the full ladder when the layer has
 *  no model of its own and runs on the chat's. */
function effortOptions(l: UltraXLayer, chatModel: string) {
  const mi = modelInfo(l.model || chatModel);
  return effortSteps(mi).map((value) => ({ value: String(value), label: effortLabel(mi, value) ?? EFFORT_NAMES[value] ?? "" }));
}

/** The cheapest connected model, used to fill the worker layers in "smart". */
function cheapest(models: ModelInfo[], provs: ProviderView[]): string {
  // A subscription model is priced at zero, so left in it would always win "cheapest".
  const on = new Set(shownProviders(provs).filter((p) => p.enabled && isConnected(p)).map((p) => p.id));
  const usable = models.filter((m) => on.has(m.provider) && m.enabled !== false);
  if (!usable.length) return "";
  const price = (m: ModelInfo) => m.input_price + m.output_price;
  return [...usable].sort((a, b) => price(a) - price(b))[0]!.id;
}

export function UltraXDialog({ x, chatModel, onSave, onClose }: { x: UltraX; chatModel: string; onSave: (x: UltraX) => void; onClose: () => void }) {
  const models = useStore((s) => s.models);
  const provs = useStore((s) => s.providers);
  const [draft, setDraft] = useState<UltraX>(() => structuredClone(x));

  // Every row offers the connected models plus whatever that layer already has,
  // so a model that's since been disabled never silently drops out of its row.
  const modelOpts = useMemo(() => {
    const on = new Set(shownProviders(provs).filter((p) => p.enabled && isConnected(p)).map((p) => p.id));
    const all: ModelInfo[] = models.filter((m) => on.has(m.provider) && m.enabled !== false);
    for (const l of draft.layers) {
      if (l.model && !all.some((m) => m.id === l.model)) all.push(modelInfo(l.model));
    }
    return [
      { value: "", label: "Inherit", hint: "same as above" },
      ...all.map((m) => ({ value: m.id, label: m.name, group: provs.find((p) => p.id === m.provider)?.name ?? m.provider })),
    ];
  }, [models, provs, draft.layers]);

  const set = (i: number, patch: Partial<UltraXLayer>) => setDraft((d) => ({ ...d, layers: d.layers.map((l, j) => (j === i ? { ...l, ...patch } : l)) }));
  const setHeight = (n: number) => setDraft((d) => ({ ...d, layers: Array.from({ length: n }, (_, i) => d.layers[i] ?? emptyXLayer()) }));

  const applyPreset = (p: PresetId) => {
    const cheap = cheapest(models, provs);
    setDraft((d) => {
      const layers = d.layers.map((l, i) => {
        if (p === "all") return { ...l, model: chatModel, effort: null, fanout: 0 };
        if (p === "tiered") return { ...l, model: i === 0 ? chatModel : cheap || l.model, effort: i === 0 ? null : 4, fanout: i === 0 ? 0 : l.fanout || 3 };
        return i === 0 ? { ...l, model: chatModel, effort: null, fanout: 0 } : { ...l, model: cheap || l.model, effort: 4, fanout: 0 };
      });
      return { ...d, layers };
    });
    flash(`${PRESETS.find((p2) => p2.id === p)?.name} ladder applied`);
  };

  const depth = xDepth(draft);
  // Worst case if every layer fans out as far as it is allowed.
  const leaves = draft.layers.slice(0, depth).reduce((n, l) => n * Math.max(1, l.fanout || 1), 1);
  const overBudget = leaves > draft.max_total;
  const grid = "86px 1fr 108px 96px";

  return (
    <Modal onClose={onClose} style={{ width: "min(720px, 100%)" }}>
      <div className="mh">
        <span style={{ flex: 1 }}>Ultrathread X</span>
        <span style={{ fontSize: 11.5, fontWeight: 400, color: "var(--mut3)" }}>{depth} layers below you</span>
      </div>
      <div className="mb" style={{ display: "flex", flexDirection: "column", gap: 14 }}>
        <div className="desc" style={{ fontSize: 11.5, color: "var(--mut3)", lineHeight: 1.55 }}>
          You stay the orchestrator. Each layer below you runs on the model you pick here, at the effort you pick, and one of its agents may launch that
          many subagents at a time. Put the big model on the planning layers and a cheap one on the work.
        </div>

        <div style={{ display: "flex", gap: 7, alignItems: "center", flexWrap: "wrap" }}>
          <span style={{ fontSize: 11, color: "var(--mut3)" }}>Preset</span>
          {PRESETS.map((p) => (
            <button key={p.id} className="btn" style={{ fontSize: 11.5, padding: "4px 9px" }} title={p.desc} onClick={() => applyPreset(p.id)}>
              {p.name}
            </button>
          ))}
        </div>

        <div style={{ display: "flex", gap: 9, alignItems: "center", flexWrap: "wrap" }}>
          <span style={{ fontSize: 11, color: "var(--mut3)" }}>Depth</span>
          <Segmented
            label="Depth"
            quiet
            value={String(depth)}
            onChange={(n) => setHeight(Number(n))}
            options={Array.from({ length: X_MAX_LAYERS - 1 }, (_, i) => ({ value: String(i + 2), label: String(i + 2) }))}
          />
          <span style={{ fontSize: 11, color: "var(--dim)" }}>two layers is ultrathread with per-layer models</span>
        </div>

        <div style={{ border: "1px solid var(--line)", borderRadius: 10, overflow: "hidden" }}>
          <div style={{ display: "grid", gridTemplateColumns: grid, gap: 8, alignItems: "center", padding: "7px 10px" }}>
            <span style={{ fontSize: 10.5, color: "var(--dim)", textTransform: "uppercase", letterSpacing: ".04em" }}>Layer</span>
            <span style={{ fontSize: 10.5, color: "var(--dim)", textTransform: "uppercase", letterSpacing: ".04em" }}>Model</span>
            <span style={{ fontSize: 10.5, color: "var(--dim)", textTransform: "uppercase", letterSpacing: ".04em" }}>Effort</span>
            <span style={{ fontSize: 10.5, color: "var(--dim)", textTransform: "uppercase", letterSpacing: ".04em" }}>Max at once</span>
          </div>
          {draft.layers.slice(0, depth).map((l, i) => (
            <div key={i} style={{ display: "grid", gridTemplateColumns: grid, gap: 8, alignItems: "center", padding: "5px 10px", borderTop: "1px solid var(--line)" }}>
              <span style={{ fontSize: 11.5, color: i === 0 ? "var(--violet)" : "var(--mut)", fontWeight: i === 0 ? 600 : 400 }}>
                {i === 0 ? "You" : `Layer ${i}`}
                {i === depth - 1 && <span style={{ color: "var(--dim)", fontWeight: 400 }}> · leaf</span>}
              </span>
              <Dropdown value={l.model} options={modelOpts} onChange={(v) => set(i, { model: v })} style={{ fontSize: 12 }} />
              <Dropdown
                value={l.effort === null ? "" : String(l.effort)}
                options={[{ value: "", label: "Default", hint: "as the chat" }, ...effortOptions(l, chatModel)]}
                onChange={(v) => set(i, { effort: v === "" ? null : Number(v) })}
                search={false}
                style={{ fontSize: 12 }}
              />
              <Dropdown
                value={String(l.fanout)}
                options={[{ value: "0", label: "No cap", hint: "tree-wide only" }, ...Array.from({ length: X_MAX_FANOUT }, (_, n) => ({ value: String(n + 1), label: String(n + 1) }))]}
                onChange={(v) => set(i, { fanout: Number(v) })}
                search={false}
                style={{ fontSize: 12 }}
              />
            </div>
          ))}
        </div>

        <div style={{ display: "flex", gap: 14, alignItems: "center", flexWrap: "wrap", fontSize: 11.5, color: "var(--mut3)" }}>
          <label style={{ display: "flex", gap: 6, alignItems: "center" }}>
            Running at once
            <input
              type="number"
              className="input"
              style={{ width: 66 }}
              min={1}
              max={64}
              value={draft.max_running}
              onChange={(e) => setDraft((d) => ({ ...d, max_running: Math.max(1, Math.min(64, Number(e.target.value) || X_DEFAULT_RUNNING)) }))}
            />
          </label>
          <label style={{ display: "flex", gap: 6, alignItems: "center" }}>
            Total spawned
            <input type="number" className="input" style={{ width: 66 }} min={1} value={draft.max_total} onChange={(e) => setDraft((d) => ({ ...d, max_total: Math.max(1, Number(e.target.value) || X_DEFAULT_TOTAL) }))} />
          </label>
          <span style={{ color: overBudget ? "#f59e0b" : "var(--dim)" }}>
            {leaves.toLocaleString()} agent{leaves === 1 ? "" : "s"} if every layer fans out fully
          </span>
        </div>

        <label style={{ display: "flex", gap: 8, alignItems: "flex-start", fontSize: 11.5, color: "var(--mut2)", cursor: "pointer" }}>
          <input type="checkbox" checked={draft.wt} onChange={(e) => setDraft((d) => ({ ...d, wt: e.target.checked }))} style={{ marginTop: 2 }} />
          <span>Worktrees: each top-level worker gets its own git worktree and branch, its own subagents share it, and a fuze agent merges the branches back.</span>
        </label>
      </div>
      <div className="mf">
        <span style={{ flex: 1, fontSize: 11.5, color: "var(--dim)" }}>Only you can turn X on — agents can only ask for plain ultrathread</span>
        <div className="btn ghost" onClick={onClose}>
          Cancel
        </div>
        <div className="btn primary" onClick={() => onSave(draft)}>
          Save ladder
        </div>
      </div>
    </Modal>
  );
}

/** Mounts the ladder editor when the store says it's open, bound to the current
 *  chat (or the new-chat defaults), so the pill, the menu and /ultrax all land here. */
export function UltraXEditor() {
  const open = useStore((s) => s.ultraXOpen);
  const task = useStore((s) => (s.view !== "home" && s.task ? s.tasks[s.task] : null));
  useStore((s) => s.home);
  const close = () => set({ ultraXOpen: false });
  if (!open) return null;
  const cur = task ?? null;
  const chatModel = cur ? (cur.pending.model ?? cur.model) : get().home.model;
  const existing = (cur ? cur.ultra_x : get().home.ultra_x) ?? defaultX();
  return (
    <UltraXDialog
      x={existing}
      chatModel={chatModel}
      onClose={close}
      onSave={(x) => {
        close();
        void setOpts({ ultra: true, ultra_x: x }).then(() => flash(`Ultrathread X on · ${xDepth(x)} layers, each with its own model, effort and fanout`));
      }}
    />
  );
}
