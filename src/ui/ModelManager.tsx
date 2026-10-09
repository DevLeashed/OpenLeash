// Settings: Models - providers (built-in + custom), their models, and the
// add/edit model dialog with smart configuration.
import { useEffect, useMemo, useState } from "react";
import { api, EFFORTS, effortLabel, effortLevel, effortSteps, fmtK, ModelInfo, ProviderView, RemoteModel } from "../api";
import { flash, get, isConnected, modelInfo, saveSettings, set, shownProviders, useStore } from "../store";
import { openMenu } from "./Composer";
import { I } from "./icons";
import { Modal, Loader } from "./primitives";
import { ProviderIcon } from "./ProviderIcon";
import { Tooltip, ColorPicker, Dropdown, Opt, Button, ChipButton, ChoiceChip, IconButton, Input, Kbd, Pressable, Switch, TextButton, randomAccent } from "./primitives";

const PARAMS: { id: string; label: string; hint: string }[] = [
  { id: "none", label: "Not supported", hint: "Model has no reasoning control (or always reasons)" },
  { id: "reasoning_effort", label: "reasoning_effort", hint: "OpenAI, Gemini, xAI, gpt-oss, most OpenAI-compatible APIs" },
  { id: "reasoning.effort", label: "reasoning: { effort }", hint: "OpenRouter unified reasoning" },
  { id: "thinking", label: "thinking: { type }", hint: "Z.ai GLM, DeepSeek-style on/off. Levels like off / on" },
  { id: "thinking_budget", label: "thinking.budget_tokens", hint: "Token budgets. Levels are numbers, e.g. 4000, 16000" },
  { id: "anthropic_effort", label: "Anthropic effort", hint: "Adaptive thinking + output_config.effort (Claude 4.6+)" },
];
const INPUTS = [["text", "Text"], ["image", "Image"], ["video", "Video"], ["pdf", "PDF"]] as const;
const CAPS = [["structured_output", "Structured output"], ["web_search", "Native web search"], ["mid_system", "Mid-conversation system messages"]] as const;

type Guess = Partial<Pick<ModelInfo, "context" | "output" | "reasoning_levels" | "reasoning_param" | "input_types" | "name">>;

/** Smart configuration: sensible specs from the model id alone. */
export function guess(id: string, kind: string): Guess {
  const s = id.toLowerCase();
  const LMH = ["low", "medium", "high"];
  const rules: [RegExp, Guess][] = [
    [/claude.*haiku/, { context: 200_000, output: 64_000, reasoning_levels: kind === "anthropic" ? ["0", "4000", "8000", "16000", "32000"] : [], reasoning_param: kind === "anthropic" ? "thinking_budget" : "none", input_types: ["text", "image", "pdf"] }],
    [/claude/, { context: 1_000_000, output: 128_000, reasoning_levels: kind === "anthropic" ? ["low", "medium", "high", "xhigh", "max"] : [], reasoning_param: kind === "anthropic" ? "anthropic_effort" : "none", input_types: ["text", "image", "pdf"] }],
    [/gpt-oss/, { context: 128_000, output: 32_000, reasoning_levels: LMH, reasoning_param: "reasoning_effort" }],
    [/gpt-[56]|(^|\/)o[134](-|$)/, { context: 400_000, output: 128_000, reasoning_levels: ["minimal", ...LMH], reasoning_param: "reasoning_effort", input_types: ["text", "image"] }],
    [/gpt-4\.1/, { context: 1_000_000, output: 32_000, input_types: ["text", "image"] }],
    [/gpt-4o/, { context: 128_000, output: 16_000, input_types: ["text", "image"] }],
    [/gemini/, { context: 1_000_000, output: 64_000, reasoning_levels: LMH, reasoning_param: "reasoning_effort", input_types: ["text", "image", "video", "pdf"] }],
    [/deepseek.*(r1|reason)/, { context: 128_000, output: 32_000, reasoning_levels: ["off", "on"], reasoning_param: "thinking" }],
    [/deepseek/, { context: 128_000, output: 8_000 }],
    [/qwen3?-coder/, { context: 256_000, output: 64_000 }],
    [/qwen/, { context: 128_000, output: 32_000 }],
    [/glm-(4\.[5-9]|5)/, { context: 200_000, output: 128_000, reasoning_levels: ["off", "on"], reasoning_param: "thinking" }],
    [/glm/, { context: 128_000, output: 16_000 }],
    [/kimi|moonshot/, { context: 256_000, output: 32_000 }],
    [/grok-4/, { context: 256_000, output: 64_000, input_types: ["text", "image"] }],
    [/muse-spark/, { context: 200_000, output: 64_000, reasoning_levels: LMH, reasoning_param: "reasoning_effort", input_types: ["text", "image"] }],
    [/grok.*mini/, { context: 128_000, output: 32_000, reasoning_levels: ["low", "high"], reasoning_param: "reasoning_effort" }],
    [/devstral|codestral|mistral/, { context: 128_000, output: 32_000 }],
    [/llama/, { context: 128_000, output: 8_000 }],
    [/minimax/, { context: 200_000, output: 128_000 }],
  ];
  const hit = rules.find(([r]) => r.test(s))?.[1] ?? { context: 128_000, output: 16_000 };
  return { reasoning_levels: [], reasoning_param: "none", input_types: ["text"], ...hit };
}

export function remoteReasoning(m: Record<string, unknown>): { levels: string[]; param: string } {
  const caps = Array.isArray(m.supported_parameters) ? m.supported_parameters : [];
  const explicitLevels = m.reasoning_levels ?? m.supported_reasoning_levels;
  const levels = Array.isArray(explicitLevels) ? explicitLevels.map((level: unknown) => typeof level === "string" ? level : level && typeof level === "object" && "effort" in level ? String(level.effort) : "").filter(Boolean) : [];
  const declared = m.reasoning_param ?? m.reasoning_parameter;
  const param = typeof declared === "string" && ["reasoning_effort", "reasoning.effort", "thinking", "thinking_budget", "anthropic_effort"].includes(declared) ? declared :
    caps.includes("reasoning_effort") ? "reasoning_effort" :
    caps.includes("reasoning") ? "reasoning.effort" : "";
  return { levels, param };
}

function Help({ t }: { t: string }) {
  return <Tooltip content={t}><span className="help">?</span></Tooltip>;
}

function blank(provider: string): ModelInfo {
  return { id: "", name: "", provider, context: 128_000, output: 16_000, input_price: 0, output_price: 0, effort: false, input_types: ["text"], capabilities: [], reasoning_levels: [], reasoning_param: "none", custom: true, enabled: true };
}

function ModelDialog({ prov, edit, onClose }: { prov: ProviderView; edit: ModelInfo | null; onClose: () => void }) {
  const init = (): ModelInfo => (edit ? { ...edit, id: edit.id.slice(prov.id.length + 1) } : blank(prov.id));
  const [m, setM] = useState<ModelInfo>(init);
  const [smart, setSmart] = useState(!edit);
  const [adv, setAdv] = useState(!!edit);
  const [lvl, setLvl] = useState("");
  const [remote, setRemote] = useState<RemoteModel[] | null>(null);
  const [remoteErr, setRemoteErr] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const up = (p: Partial<ModelInfo>) => setM((x) => ({ ...x, ...p }));

  // The accent lives in `settings.model_colors` keyed by the full id, not on the
  // model record, so it has to be written through settings like the picker's dot
  // does. A brand-new model has no id to key on until it is saved, and writing a
  // key for a half-typed id would strand a colour that then outlives the dialog
  // it was picked in — so an unsaved pick stays a local draft and lands on the
  // model record's own id, written once, on save.
  const fullId = `${prov.id}/${m.id.trim()}`;
  // Untouched, a new model inherited its provider's colour, so every model added
  // under one provider opened on the same hue and the picker's dots were a column
  // of identical circles — in the one list whose job is telling models apart at a
  // glance. A new model draws its own swatch instead, once per dialog (lazily, so
  // it cannot repaint while the user types) and never one already in use, so a
  // fresh dot cannot hide among the ones they can already see.
  const [color, setColor] = useState(() =>
    edit ? get().settings?.model_colors?.[edit.id] ?? "" : randomAccent(prov.color || undefined, Object.values(get().settings?.model_colors ?? {})));
  const curColor = get().settings?.model_colors?.[fullId] ?? color;
  const defColor = prov.color || "var(--violet)";
  const pickColor = (v: string) => {
    // Editing a model that already exists writes straight through, so the dot
    // beside it in the picker changes with the click rather than on Save.
    if (edit) {
      const cur = { ...(get().settings?.model_colors ?? {}) };
      if (v) cur[edit.id] = v; else delete cur[edit.id];
      void saveSettings({ model_colors: cur });
    }
    setColor(v);
  };

  useEffect(() => {
    api.providerModels(prov.id).then(setRemote).catch((e) => setRemoteErr(String(e)));
  }, [prov.id]);

  const applySmart = (id: string) => {
    const g = guess(id, prov.kind);
    const r = remote?.find((x) => x.id === id);
    const reasoning = r ? remoteReasoning(r as unknown as Record<string, unknown>) : { levels: [], param: "" };
    up({
      id, ...g,
      name: r?.name || m.name,
      context: r?.context ?? g.context ?? 128_000,
      output: r?.output ?? g.output ?? 16_000,
      input_price: r?.input_price ?? m.input_price,
      output_price: r?.output_price ?? m.output_price,
      input_types: r?.modalities?.length ? r.modalities.filter((x) => ["text", "image", "video", "pdf", "file"].includes(x)).map((x) => (x === "file" ? "pdf" : x)) : g.input_types,
      reasoning_levels: reasoning.levels.length ? reasoning.levels : g.reasoning_levels,
      reasoning_param: reasoning.param || g.reasoning_param,
    });
  };
  const onId = (id: string) => (smart && id.trim() ? applySmart(id.trim()) : up({ id }));
  // The key is one of the two string-list fields, so the computed patch is a
  // real `Partial<ModelInfo>` — no cast needed once `k` is narrowed.
  const toggle = (k: "input_types" | "capabilities", v: string) => up({ [k]: m[k].includes(v) ? m[k].filter((x) => x !== v) : [...m[k], v] });
  const addLevel = () => {
    const v = lvl.trim();
    if (v && !m.reasoning_levels.includes(v)) up({ reasoning_levels: [...m.reasoning_levels, v] });
    setLvl("");
  };
  const save = async () => {
    const id = m.id.trim();
    if (!id) return flash("Model ID is required");
    setSaving(true);
    try {
      const models = await api.modelSave({ ...m, id: `${prov.id}/${id}`, provider: prov.id, input_types: m.input_types.includes("text") ? m.input_types : ["text", ...m.input_types] }, edit?.id);
      set({ models });
      // `model_colors` is keyed by the full id, so a colour picked before the
      // model had an id lands now — and a rename has to move the key it was
      // already written under, or the model silently falls back to the provider
      // default it had when it was created.
      const cur = { ...(get().settings?.model_colors ?? {}) };
      const was = edit ? cur[edit.id] : undefined;
      const next = { ...cur };
      if (was) delete next[edit!.id];
      if (color) next[`${prov.id}/${id}`] = color; else delete next[`${prov.id}/${id}`];
      if (JSON.stringify(next) !== JSON.stringify(cur)) await saveSettings({ model_colors: next });
      onClose();
    } catch (e) {
      flash(String(e));
    } finally {
      setSaving(false);
    }
  };
  // A model saved against a mapping this build no longer lists still has to
  // render, and `PARAMS` always starts with the "not supported" entry.
  const param = PARAMS.find((p) => p.id === m.reasoning_param) ?? PARAMS[0]!;

  return (
    <Modal onClose={onClose}>
        <div className="mh">
          <ProviderIcon provider={prov.id} size={18} />
          <span style={{ flex: 1 }}>{edit ? (edit.custom ? "Edit model" : "Customize model") : "Add model"}<span style={{ color: "var(--mut3)", fontWeight: 400, fontSize: 12.5 }}> · {prov.name}</span></span>
          <IconButton label="Close" onClick={onClose}>{I.close()}</IconButton>
        </div>
        <div className="mb">
          <div className="srowx" style={{ padding: 0, minHeight: 0 }}>
            <span style={{ fontWeight: 500 }}>Smart configuration</span>
            <Help t="Fills context window, output limit, input types and reasoning settings from the model ID and the provider's model list." />
            <Switch label="Smart configuration" checked={smart} onChange={(on) => { setSmart(on); if (on && m.id.trim()) applySmart(m.id.trim()); }} />
          </div>

          <div className="field">
            <label>Model ID {remote && <span style={{ color: "var(--hint)" }}>· {remote.length} available from {prov.name}</span>}{remoteErr && <Tooltip content={remoteErr}><span style={{ color: "var(--hint)" }}>· couldn't list models</span></Tooltip>}</label>
            <Input className="input mono" list="ol-remote-models" autoFocus={!edit} value={m.id} placeholder={prov.kind === "anthropic" ? "claude-sonnet-5" : "model-id"} onChange={(e) => onId(e.currentTarget.value)} />
            <datalist id="ol-remote-models">{remote?.slice(0, 500).map((r) => <option key={r.id} value={r.id}>{r.name}</option>)}</datalist>
          </div>
          <div className="field">
            <label>Display name <span style={{ color: "var(--dim)" }}>optional</span></label>
            <Input value={m.name} placeholder={m.id || "Shown in the model picker"} onChange={(e) => up({ name: e.currentTarget.value })} />
          </div>
          <div style={{ display: "flex", gap: 12 }}>
            <div className="field" style={{ flex: 1 }}>
              <label>Context window <Help t="Total tokens the model accepts. Drives the context meter and auto-compaction at 80%." /></label>
              <Input type="number" value={m.context || ""} onChange={(e) => up({ context: Number(e.currentTarget.value) })} />
            </div>
            <div className="field" style={{ flex: 1 }}>
              <label>Max output tokens <Help t="Upper bound for one response." /></label>
              <Input type="number" value={m.output || ""} onChange={(e) => up({ output: Number(e.currentTarget.value) })} />
            </div>
          </div>
          {m.context > 0 && <div style={{ fontSize: 11, color: "var(--dim)", marginTop: -8 }}>{fmtK(m.context)} context · {fmtK(m.output)} output</div>}
          <div className="field">
            <label>Color <Help t="The accent for this model: the working spinner, the sidebar dot, its line in the stats. A new model gets a random one so it reads apart from its neighbours; clearing it falls back to the provider's colour." /></label>
            <ColorPicker value={curColor || defColor} onChange={pickColor} reset={color ? () => pickColor("") : undefined} />
          </div>

          <ChipButton style={{ alignSelf: "flex-start", marginLeft: -8, color: "#d4d4d8" }} onClick={() => setAdv(!adv)}>
            <span style={{ display: "flex", transform: `rotate(${adv ? 0 : -90}deg)`, transition: "transform .18s" }}>{I.chev("var(--mut)")}</span>Advanced settings
          </ChipButton>

          {adv && (
            <div style={{ display: "flex", flexDirection: "column", gap: 14, animation: "olIn .18s ease both" }}>
              <div className="field">
                <label>Input types <Help t="What the model can read. Text is always on." /></label>
                <div className="chips">
                  {INPUTS.map(([k, l]) => <ChoiceChip key={k} checkbox selected={m.input_types.includes(k) || k === "text"} locked={k === "text"} hint={k === "text" ? "Text is always on" : undefined} onClick={() => toggle("input_types", k)}>{l}</ChoiceChip>)}
                </div>
              </div>
              <div className="field">
                <label>Model capabilities <Help t="Informational for now; used to decide which features the harness may use with this model." /></label>
                <div className="chips">
                  {CAPS.map(([k, l]) => <ChoiceChip key={k} checkbox selected={m.capabilities.includes(k)} onClick={() => toggle("capabilities", k)}>{l}</ChoiceChip>)}
                </div>
              </div>
              <div className="field">
                <label>Reasoning levels (low to high) <Help t="The values this model accepts, lowest first. The 5-step effort slider (Low to Max) is spread across them." /></label>
                <div className="chips">
                  {m.reasoning_levels.map((l, i) => (
                    <div key={l} className="lvl">{l}<IconButton label={`Remove ${l}`} className="lvl-remove" onClick={() => up({ reasoning_levels: m.reasoning_levels.filter((_, j) => j !== i) })}>{I.close(10)}</IconButton></div>
                  ))}
                  <Input className="input mono" style={{ width: 110, height: 28, fontSize: 11.5 }} value={lvl} placeholder="add level" onChange={(e) => setLvl(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); addLevel(); } }} />
                  <IconButton label="Add reasoning level" onClick={addLevel}>{I.plus(12)}</IconButton>
                </div>
                {m.reasoning_levels.length > 0 && m.reasoning_param !== "none" && (
                  // `effortLevel` is the spread Rust's `pick_level` applies, so this preview
                  // and the wire agree by construction instead of by a copy that
                  // can drift. The guard above proves the model has levels, so it
                  // never returns null here.
                  <div style={{ fontSize: 11, color: "var(--dim)" }}>Slider maps: {effortSteps(m).map((e) => `${effortLabel(m, e) ?? EFFORTS[e]} = ${effortLevel(m, e)}`).join(" · ")}</div>
                )}
              </div>
              <div className="field">
                <label>Reasoning parameter mapping <Help t="How the chosen level is sent in the request body." /></label>
                <Dropdown value={m.reasoning_param} options={PARAMS.map((p) => ({ value: p.id, label: p.label }))} onChange={(v) => up({ reasoning_param: v })} search={false} style={{ width: "100%" }} />
                <div style={{ fontSize: 11, color: "var(--dim)" }}>{param.hint}</div>
              </div>
              <div style={{ display: "flex", gap: 12 }}>
                <div className="field" style={{ flex: 1 }}>
                  <label>Input $ / 1M tokens</label>
                  <Input type="number" step="0.01" value={m.input_price} onChange={(e) => up({ input_price: Number(e.currentTarget.value) })} />
                </div>
                <div className="field" style={{ flex: 1 }}>
                  <label>Output $ / 1M tokens</label>
                  <Input type="number" step="0.01" value={m.output_price} onChange={(e) => up({ output_price: Number(e.currentTarget.value) })} />
                </div>
              </div>
            </div>
          )}
        </div>
        <div className="mf">
          <TextButton style={{ color: "var(--mut)", textDecoration: "underline", textUnderlineOffset: 3 }} onClick={() => setM(init())}>Reset form</TextButton>
          <div style={{ flex: 1 }} />
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={saving || !m.id.trim()} onClick={save}>{saving && <Loader size={13} />}Save</Button>
        </div>
    </Modal>
  );
}

function ProviderDialog({ onClose }: { onClose: () => void }) {
  const provs = useStore((s) => s.providers);
  const presets = useStore((s) => s.providerPresets);
  const [choice, setChoice] = useState("");
  const [p, setP] = useState<{ name: string; base_url: string; kind: "openai" | "anthropic"; api_key: string; builtin?: string }>({ name: "", base_url: "", kind: "openai", api_key: "" });
  const [saving, setSaving] = useState(false);
  const builtins = provs.filter((x) => !x.custom && !x.connected && !x.account);
  const b = p.builtin ? provs.find((x) => x.id === p.builtin) : null;
  const choices: Opt[] = [
    ...builtins.map((x) => ({ value: `builtin:${x.id}`, label: x.name, hint: x.local ? "Local" : "API key", group: "Built-in", icon: <ProviderIcon provider={x.id} size={15} /> })),
    ...presets.map((x) => ({ value: `preset:${x.name}`, label: x.name, group: "Compatible services", icon: <ProviderIcon provider={x.name} icon={x.icon} size={15} /> })),
    { value: "custom", label: "Custom endpoint", group: "Compatible services" },
  ];
  const choose = (value: string) => {
    setChoice(value);
    const builtin = builtins.find((x) => value === `builtin:${x.id}`);
    const preset = presets.find((x) => value === `preset:${x.name}`);
    setP(builtin
      ? { name: builtin.name, base_url: builtin.base_url, kind: builtin.kind === "anthropic" ? "anthropic" : "openai", api_key: "", builtin: builtin.id }
      : preset
        ? { name: preset.name, base_url: preset.url, kind: preset.kind, api_key: "" }
        : { name: "", base_url: "", kind: "openai", api_key: "" });
  };
  const canSave = !!choice && (b ? b.local || !!p.api_key.trim() : !!p.name.trim() && /^https?:\/\//i.test(p.base_url.trim()));
  const save = async () => {
    if (!canSave || saving) return;
    setSaving(true);
    try {
      set({ providers: await api.providerAdd(p) });
      onClose();
    } catch (e) {
      flash(String(e));
    } finally {
      setSaving(false);
    }
  };
  return (
    <Modal onClose={onClose} style={{ width: "min(500px, 100%)" }}>
        <div className="mh"><span style={{ flex: 1 }}>Add provider</span><IconButton label="Close" onClick={onClose}>{I.close()}</IconButton></div>
        <div className="mb">
          <div className="field">
            <label>Provider</label>
            <Dropdown value={choice} options={choices} onChange={choose} placeholder="Choose a provider…" style={{ width: "100%" }} />
          </div>
          {choice && (b ? (
            b.local
              ? <div className="secondary-text">{b.name} runs locally; no API key is needed.</div>
              : <div className="field"><label>{b.chip}</label><Input type="password" value={p.api_key} placeholder="Paste API key" onChange={(e) => setP({ ...p, api_key: e.currentTarget.value })} onKeyDown={(e) => e.key === "Enter" && save()} /></div>
          ) : <>
            <div className="field"><label>Name</label><Input value={p.name} placeholder="My provider" onChange={(e) => setP({ ...p, name: e.currentTarget.value })} /></div>
            <div className="field">
              <label>API type <Help t="Most providers and local servers speak the OpenAI chat-completions API. Pick Anthropic-compatible for proxies of the Messages API." /></label>
              <Dropdown value={p.kind} options={[{ value: "openai", label: "OpenAI-compatible" }, { value: "anthropic", label: "Anthropic-compatible" }]} onChange={(kind) => setP({ ...p, kind: kind as "openai" | "anthropic" })} search={false} style={{ width: "100%" }} />
            </div>
            <div className="field">
              <label>Base URL</label>
              <Input className="input mono" style={{ fontSize: 12 }} value={p.base_url} placeholder={p.kind === "anthropic" ? "https://my-proxy.example.com" : "https://api.example.com/v1"} onChange={(e) => setP({ ...p, base_url: e.currentTarget.value })} />
              <div className="secondary-text">{p.kind === "anthropic" ? "Requests go to {base}/v1/messages" : "Requests go to {base}/chat/completions"}</div>
            </div>
            <div className="field"><label>API key <span className="secondary-text">optional for local servers</span></label><Input type="password" value={p.api_key} onChange={(e) => setP({ ...p, api_key: e.currentTarget.value })} onKeyDown={(e) => e.key === "Enter" && save()} /></div>
          </>)}
          <div className="secondary-text">For ChatGPT subscriptions, use <TextButton link onClick={() => { onClose(); set({ settingsTab: "accounts" }); }}>Accounts</TextButton>.</div>
        </div>
        <div className="mf">
          <div style={{ flex: 1 }} />
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={!canSave || saving} onClick={save}>{saving && <Loader size={13} />}{b ? "Connect" : "Add provider"}</Button>
        </div>
    </Modal>
  );
}

function ProviderCard({ p, onAddModel, onEdit, i }: { p: ProviderView; onAddModel: () => void; onEdit: (m: ModelInfo) => void; i: number }) {
  const models = useStore((s) => s.models);
  const accts = useStore((s) => s.accounts);
  const pool = !!p.account;
  const nAcct = accts.filter((a) => a.kind === p.id).length;
  const [open, setOpen] = useState(false);
  const [key, setKey] = useState("");
  const [url, setUrl] = useState(p.base_url_override);
  const [newKey, setNewKey] = useState("");
  const [confirmDel, setConfirmDel] = useState(false);
  const list = useMemo(() => models.filter((m) => m.provider === p.id), [models, p.id]);
  const saveKey = async () => {
    set({ providers: await api.provider(p.id, { apiKey: key || undefined, baseUrl: url }) });
    setKey("");
  };
  const toggleKeyPool = async () => {
    set({ providers: await api.provider(p.id, { keyPool: !p.key_pool }) });
    flash(p.key_pool ? "Key pool off" : (p.has_key ? "Key pool on" : "Key pool on · add keys below"));
  };
  const addKey = async () => {
    const k = newKey.trim();
    if (!k) return;
    set({ providers: await api.providerKey(p.id, { op: "add", key: k }) });
    setNewKey("");
    flash("Key added · shows as key …" + k.slice(-4));
  };
  const removeKey = async (idx: number) => {
    set({ providers: await api.providerKey(p.id, { op: "remove", index: idx }) });
    flash("Key removed");
  };
  const replaceKey = async (idx: number, val: string) => {
    set({ providers: await api.providerKey(p.id, { op: "replace", index: idx, key: val }) });
    flash("Key updated");
  };
  const removeModel = async (m: ModelInfo) => {
    set({ models: await api.modelRemove(m.id) });
    flash("Model removed · re-add it any time with Add model");
  };
  const toggleModel = async (m: ModelInfo) => {
    const on = m.enabled === false;
    set({ models: await api.modelSetEnabled(m.id, on) });
    flash(on ? `${m.name} on` : `${m.name} off · hidden from pickers`);
  };
  return (
    <div className="sgroup pcard" style={{ opacity: p.enabled ? 1 : 0.55, animationDelay: i * 40 + "ms" }}>
      <Pressable className="srowx" onClick={() => setOpen(!open)}>
        <ProviderIcon provider={p.id} size={18} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ fontWeight: 500, display: "flex", alignItems: "center", gap: 6 }}>{p.name}{p.custom && <span className="branchtag" style={{ height: 17, fontSize: 10 }}>custom</span>}{p.insist && <span className="branchtag" style={{ height: 17, fontSize: 10, color: "#fbbf24" }}>insist</span>}</div>
          <div className="desc" style={{ whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
            {list.length} model{list.length === 1 ? "" : "s"} · {pool ? `${nAcct} account${nAcct === 1 ? "" : "s"}` : p.has_key ? (p.local ? "local" : p.key_pool ? (p.key_hints?.length ? `${p.key_hints.length} key${p.key_hints.length === 1 ? "" : "s"}` : "no keys") : p.key_hint ? `key ${p.key_hint}` : p.custom ? "no key" : "key set") : <span style={{ color: "#ff8a8a" }}>no key{p.env ? ` · or set ${p.env}` : ""}</span>}
          </div>
        </div>
        <Switch label={`${p.enabled ? "Disable" : "Enable"} ${p.name}`} checked={p.enabled} onChange={async (enabled, e) => { e.stopPropagation(); set({ providers: await api.provider(p.id, { enabled }) }); }} />
        <span style={{ display: "flex", transform: `rotate(${open ? 180 : 0}deg)`, transition: "transform .2s cubic-bezier(.32,.72,0,1)" }}>{I.chev("var(--mut3)")}</span>
      </Pressable>
      {open && (
        <div style={{ borderTop: "1px solid rgba(255,255,255,0.05)", padding: "10px 14px 12px", display: "flex", flexDirection: "column", gap: 10, animation: "olFade .25s ease both" }}>
          {pool ? (
            <div style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12, color: "var(--mut)" }}>Requests rotate across your {p.name} accounts by priority.<div style={{ flex: 1 }} /><Button onClick={() => set({ settingsTab: "accounts" })}>Manage accounts</Button></div>
          ) : !p.local && p.key_pool ? (
            <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              {(p.key_hints ?? []).map((hint, idx) => (
                <div key={idx} className="srowx" style={{ padding: "4px 6px", gap: 6 }}>
                  <div className="mono" style={{ fontSize: 10, color: "var(--dim)", minWidth: 80 }}>key {hint}</div>
                  <Button variant="ghost" style={{ fontSize: 10, padding: "2px 6px" }} onClick={() => {
                    const val = prompt(`Replace key ${idx + 1}`);
                    if (val !== null && val.trim()) void replaceKey(idx, val.trim());
                  }}>Replace</Button>
                  <IconButton label={`Remove key ${hint}`} style={{ width: 22, height: 22, color: "#ff8a8a" }} onClick={() => removeKey(idx)}>{I.close(11)}</IconButton>
                </div>
              ))}
              <div style={{ display: "flex", gap: 6 }}>
                <Input className="input mono" style={{ flex: 1, fontSize: 11 }} type="password" placeholder="New API key…" value={newKey} onChange={(e) => setNewKey(e.currentTarget.value)} onKeyDown={(e) => e.key === "Enter" && addKey()} />
                <Button onClick={addKey}>Add</Button>
              </div>
              <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                <div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Key pool</div><div style={{ fontSize: 11, color: "var(--mut3)" }}>Rotate across keys. On rate-limit, tries the next key then the next model.</div></div>
                <Switch label={`Key pool for ${p.name}`} checked={p.key_pool} onChange={() => void toggleKeyPool()} />
              </div>
            </div>
          ) : (
          <div style={{ display: "flex", gap: 6 }}>
            {!p.local && <Input type="password" style={{ flex: 1 }} placeholder={p.has_key ? "Replace key…" : p.chip} value={key} onChange={(e) => setKey(e.currentTarget.value)} onKeyDown={(e) => e.key === "Enter" && saveKey()} />}
            <Input className="input mono" style={{ flex: 1, fontSize: 11 }} placeholder={p.base_url} value={url} onChange={(e) => setUrl(e.currentTarget.value)} onKeyDown={(e) => e.key === "Enter" && saveKey()} />
            <Button onClick={saveKey}>Save</Button>
          </div>
          )}
          {!p.local && !pool && !p.key_pool && p.has_key && (
            <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
              <div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Key pool</div><div style={{ fontSize: 11.5, color: "var(--mut3)" }}>Add multiple keys. On rate-limit, tries the next key then the next model.</div></div>
              <Switch label={`Key pool for ${p.name}`} checked={p.key_pool} onChange={() => void toggleKeyPool()} />
            </div>
          )}
          {p.custom && (
            <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
              <div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Insist</div><div style={{ fontSize: 11.5, color: "var(--mut3)" }}>Retry up to 40 times on errors before falling back. For providers that flake with weird errors.</div></div>
              <Switch label={`Insist for ${p.name}`} checked={p.insist} onChange={async (insist) => set({ providers: await api.providerInsist(p.id, insist) })} />
            </div>
          )}
          <div className="mlist" style={{ borderRadius: 8, border: "1px solid rgba(255,255,255,0.05)", overflow: "hidden" }}>
            {list.map((m) => {
              const on = m.enabled !== false;
              return (
              <div key={m.id} className="srowx" style={{ padding: "6px 10px", gap: 10, opacity: on ? 1 : 0.55 }}>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ display: "flex", alignItems: "center", gap: 6, fontWeight: 500 }}>{m.name}{m.custom && <span style={{ fontSize: 10, color: "var(--mut3)" }}>custom</span>}{!on && <span style={{ fontSize: 10, color: "var(--dim)" }}>off</span>}</div>
                  <div className="mono" style={{ fontSize: 10.5, color: "var(--dim)", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{m.id.slice(p.id.length + 1)}</div>
                </div>
                <span style={{ fontSize: 11, color: "var(--mut3)", whiteSpace: "nowrap", fontVariantNumeric: "tabular-nums" }}>{fmtK(m.context)} ctx</span>
                {m.reasoning_levels.length > 0 && m.reasoning_param !== "none" && <Tooltip content={m.reasoning_param}><Kbd>{`${m.reasoning_levels.length} levels`}</Kbd></Tooltip>}
                <span style={{ fontSize: 11, color: "var(--dim)", width: 70, textAlign: "right", fontVariantNumeric: "tabular-nums" }}>{m.input_price || m.output_price ? `$${m.input_price}/${m.output_price}` : "free"}</span>
                <Switch label={on ? `Hide ${m.name} from pickers` : `Show ${m.name} in pickers`} hint={on ? "Hide from pickers" : "Show in pickers"} small checked={on} onChange={() => void toggleModel(m)} />
                <IconButton label={m.custom ? "Edit" : "Customize"} style={{ width: 24, height: 24 }} onClick={() => onEdit(m)}>{I.edit()}</IconButton>
                <IconButton label="Remove model (re-add any time)" style={{ width: 24, height: 24 }} onClick={() => removeModel(m)}>{I.trash()}</IconButton>
              </div>
              );
            })}
            {!list.length && <div className="empty" style={{ padding: 14 }}>No models yet</div>}
          </div>
          <div style={{ display: "flex", gap: 6 }}>
            <Button onClick={onAddModel}>{I.plus(12)}Add model</Button>
            <div style={{ flex: 1 }} />
            {!pool && (confirmDel
              ? <Button variant="primary" onClick={async () => { set({ providers: await api.providerRemove(p.id), models: await api.models() }); }}>{p.custom ? `Remove ${p.name} and its models` : `Forget the ${p.name} key`}</Button>
              : <Button variant="ghost" onClick={() => setConfirmDel(true)}>{p.custom ? "Remove provider…" : "Disconnect…"}</Button>)}
          </div>
        </div>
      )}
    </div>
  );
}

export function ModelsTab() {
  const provs = useStore((s) => s.providers);
  useStore((s) => s.accounts);
  const home = useStore((s) => s.home);
  const models = useStore((s) => s.models);
  const setupOpen = useStore((s) => s.modelSetupOpen);
  const [dialog, setDialog] = useState<null | { kind: "provider" } | { kind: "model"; prov: ProviderView; edit: ModelInfo | null }>(null);
  const mi = modelInfo(home.model);
  // Readiness reads every provider, hidden or not: a chat already on a hidden
  // model is still usable, and only the lists below are filtered for display.
  const modelReady = mi.provider === "route" || (
    models.some((m) => m.id === mi.id)
    && provs.some((p) => p.id === mi.provider && p.enabled && isConnected(p))
  );
  const shown = shownProviders(provs);
  useEffect(() => {
    if (!setupOpen) return;
    setDialog({ kind: "provider" });
    set({ modelSetupOpen: false });
  }, [setupOpen]);
  return (
    <>
      <div className="settings-head">
        <div className="settings-head-main settings-title">Models</div>
        <Button onClick={() => setDialog({ kind: "provider" })}>{I.plus(12)}Add provider</Button>
      </div>
      <div className="sgroup"><div className="srowx">
        <div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Default for new tasks</div><div className="desc">Change per task in the composer.</div></div>
          {modelReady ? <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <Button onClick={(e) => openMenu("model", e)}>{mi.name}</Button>
            {effortLabel(mi, home.effort) && <span className="secondary-text">{effortLabel(mi, home.effort)}</span>}
          </div> : shown.some(isConnected) ? <Button onClick={(e) => openMenu("model", e)}>Choose model</Button> : <Button onClick={() => setDialog({ kind: "provider" })}>{I.plus(12)}Add provider</Button>}
      </div>
      </div>
      {shown.filter(isConnected).map((p, i) => (
        <ProviderCard key={p.id} i={i} p={p} onAddModel={() => setDialog({ kind: "model", prov: p, edit: null })} onEdit={(m) => setDialog({ kind: "model", prov: p, edit: m })} />
      ))}
      {!shown.some(isConnected) && <div className="empty">No providers connected. <TextButton link onClick={() => set({ settingsTab: "accounts" })}>Use a subscription</TextButton>.</div>}
      {dialog?.kind === "provider" && <ProviderDialog onClose={() => setDialog(null)} />}
      {dialog?.kind === "model" && <ModelDialog prov={dialog.prov} edit={dialog.edit} onClose={() => setDialog(null)} />}
    </>
  );
}
