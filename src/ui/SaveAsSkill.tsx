// "Save as skill…": distil one chat into an Agent Skill (SKILL.md).
//
// User-triggered only. The two ways in are clicks (the sidebar's context menu
// and the command palette); there is deliberately no slash command or composer
// hook, because those are reachable from connector messages and model output,
// and this both spends money (a model call) and writes a file that later chats
// load as instructions. The draft itself is a deliberate click too: opening
// the box does nothing on its own.
import { useEffect, useMemo, useRef, useState } from "react";
import { Sparkles } from "lucide-react";
import { api, type SkillDraft, type SkillSaveResult, type SkillScope } from "../api";
import { chatModel, flash, get, modelInfo, set, useStore } from "../store";
import { I } from "./icons";
import { Button, Dropdown, IconButton, Input, Loader, Modal, Segmented, TextArea, type Opt } from "./primitives";

export const openSaveAsSkill = (id: string) => set({ skillFrom: id, menu: null, pal: false });

/** Mirrors the backend's name rule, so the box can say what is wrong before a
 *  round trip. The backend still validates; this is only the early warning. */
export const SKILL_NAME_RE = /^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/;

/** What is wrong with a skill name, or "" when it is usable. Exported for tests. */
export function skillNameProblem(name: string): string {
  if (!name) return "Give the skill a name.";
  if (name.length > 64) return "Names are at most 64 characters.";
  if (!SKILL_NAME_RE.test(name)) return "Use lowercase letters, digits and hyphens, starting and ending with a letter or digit.";
  return "";
}

/** `<dir>/<name>/SKILL.md` with the separator the directory already uses. */
export function skillPath(dir: string, name: string): string {
  const sep = dir.includes("\\") && !dir.includes("/") ? "\\" : "/";
  const base = dir.replace(/[\\/]+$/, "");
  return `${base}${sep}${name || "…"}${sep}SKILL.md`;
}

/** Mounted once beside the other overlays; shows while `skillFrom` names a chat. */
export function SaveAsSkill() {
  const id = useStore((s) => s.skillFrom);
  const task = useStore((s) => (id ? s.tasks[id] : null));
  if (!id || !task) return null;
  return <SkillDialog key={id} taskId={id} title={task.title} chatModelId={chatModel(task)} />;
}

type Phase = "intro" | "drafting" | "edit" | "saving" | "done";
/** A save the backend held back for confirmation, carrying what the user has
 *  already agreed to so a second confirmation does not undo the first. */
type Pending = { kind: "exists"; path: string; overwrite: boolean; allowSecrets: boolean } | { kind: "secrets"; secrets: string[]; overwrite: boolean; allowSecrets: boolean };

export function SkillDialog({ taskId, title, chatModelId }: { taskId: string; title: string; chatModelId: string }) {
  const models = useStore((s) => s.models);
  const [phase, setPhase] = useState<Phase>("intro");
  const [model, setModel] = useState("");
  const [draft, setDraft] = useState<SkillDraft | null>(null);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [body, setBody] = useState("");
  const [scope, setScope] = useState<SkillScope>("user");
  const [pending, setPending] = useState<Pending | null>(null);
  const [result, setResult] = useState<SkillSaveResult | null>(null);
  const [savedName, setSavedName] = useState("");
  const [err, setErr] = useState("");
  // False once the dialog has gone, so late answers are dropped.
  const alive = useRef(true);
  // True while a draft call is in flight: closing the box then cancels it.
  const drafting = useRef(false);
  // Bumped by Cancel, so a draft that lands after it is ignored.
  const gen = useRef(0);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      if (drafting.current) void api.skillDraftCancel(taskId).catch(() => {});
      drafting.current = false;
    };
  }, [taskId]);

  const close = () => set({ skillFrom: null });

  const startDraft = async () => {
    if (phase === "drafting") return;
    const mine = ++gen.current;
    drafting.current = true;
    setErr("");
    setPhase("drafting");
    try {
      const d = await api.skillDraftFromChat(taskId, model || undefined);
      if (!alive.current || mine !== gen.current) return;
      setDraft(d);
      setName(d.name);
      setDescription(d.description);
      setBody(d.body);
      setScope("user");
      setPending(null);
      setPhase("edit");
    } catch (e) {
      if (!alive.current || mine !== gen.current) return;
      setErr(String(e));
      setPhase(draft ? "edit" : "intro");
    } finally {
      if (mine === gen.current) drafting.current = false;
    }
  };

  const cancelDraft = () => {
    gen.current++;
    drafting.current = false;
    void api.skillDraftCancel(taskId).catch(() => {});
    setPhase(draft ? "edit" : "intro");
  };

  const cleanName = name.trim();
  const nameErr = skillNameProblem(cleanName);
  const descErr = description.trim() ? "" : "Add a one-line description of what it does and when to use it.";
  const bodyErr = body.trim() ? "" : "The skill needs instructions.";
  const projectOk = !!draft?.project && !!draft.project_dir;
  const dir = scope === "project" && projectOk ? draft?.project_dir ?? "" : draft?.user_dir ?? "";

  const save = async (overwrite: boolean, allowSecrets: boolean) => {
    if (!draft || nameErr || descErr || bodyErr) return;
    setPhase("saving");
    setPending(null);
    setErr("");
    try {
      const r = await api.skillSaveFromChat({ id: taskId, name: cleanName, description, body, scope: scope === "project" && projectOk ? "project" : "user", overwrite, allow_secrets: allowSecrets });
      if (!alive.current) return;
      if (r.saved) {
        try { set({ skills: await api.skills(get().settings?.project || undefined) }); } catch { /* the list refreshes on the next project load */ }
        if (!alive.current) return;
        flash(`Saved skill ${cleanName}`);
        setSavedName(cleanName);
        setResult(r);
        setPhase("done");
        return;
      }
      setPhase("edit");
      if (r.exists && !overwrite) setPending({ kind: "exists", path: r.path, overwrite, allowSecrets });
      else if (r.secrets.length && !allowSecrets) setPending({ kind: "secrets", secrets: r.secrets, overwrite, allowSecrets });
      else setErr("Nothing was saved.");
    } catch (e) {
      if (!alive.current) return;
      setErr(String(e));
      setPhase("edit");
    }
  };

  const modelOpts = useMemo<Opt[]>(() => [
    { value: "", label: `This chat's model (${modelInfo(chatModelId).name})` },
    ...models.filter((m) => m.enabled !== false && m.id !== chatModelId).map((m) => ({ value: m.id, label: m.name, hint: m.provider })),
  ], [models, chatModelId]);

  const editing = phase === "edit" || phase === "saving";
  const canSave = phase === "edit" && !pending && !nameErr && !descErr && !bodyErr;

  return (
    <Modal onClose={close} ariaLabel="Save as skill" style={{ width: "min(680px, calc(100vw - 24px))" }}>
      <div className="mh">
        <Sparkles size={15} strokeWidth={1.7} aria-hidden="true" />
        <span style={{ flex: 1, minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>Save as skill</span>
        <IconButton label="Close" onClick={close}>{I.close()}</IconButton>
      </div>
      <div className="mb" style={{ display: "flex", flexDirection: "column", gap: 10, maxHeight: "min(70vh, 640px)", overflowY: "auto" }}>
        {phase === "done" && result ? (
          <div role="status" style={{ display: "flex", flexDirection: "column", gap: 8, lineHeight: 1.5 }}>
            <div style={{ fontWeight: 500 }}>Saved skill {savedName}</div>
            <div className="desc mono" style={{ fontSize: 11.5, overflowWrap: "anywhere" }}>{result.path}</div>
            {result.note && <div role="note" style={{ color: "#f0c674", fontSize: 12 }}>{result.note}</div>}
            <div className="desc">New chats pick it up automatically.</div>
          </div>
        ) : (phase === "intro" || phase === "drafting") ? <>
          <div className="desc" style={{ lineHeight: 1.5 }}>
            Distil <span style={{ color: "var(--mut)" }}>{title || "this chat"}</span> into a reusable skill: a SKILL.md with instructions future chats can load when the task comes up again. A model reads the conversation and writes a draft; you review and edit it before anything is saved.
          </div>
          <label className="desc" style={{ fontSize: 11.5 }}>Model that writes the draft</label>
          <Dropdown value={model} options={modelOpts} onChange={setModel} />
          {phase === "drafting" && <div style={{ display: "flex", alignItems: "center", gap: 8 }}><Loader size={13} label="Drafting the skill…" /></div>}
        </> : editing && draft ? <>
          {draft.warnings.length > 0 && (
            <div role="note" aria-label="Warnings" style={{ color: "#f0c674", fontSize: 12, lineHeight: 1.5 }}>
              {draft.warnings.map((w, i) => <div key={i}>{w}</div>)}
            </div>
          )}
          <label className="desc" htmlFor="skill-name" style={{ fontSize: 11.5 }}>Name</label>
          <Input id="skill-name" className="input mono" style={{ fontSize: 12 }} spellCheck={false} value={name}
            aria-invalid={!!nameErr} onChange={(e) => { setName(e.currentTarget.value); setPending(null); }} />
          {nameErr && <div className="desc" style={{ color: "#ff8a8a", fontSize: 11 }}>{nameErr}</div>}
          <label className="desc" htmlFor="skill-desc" style={{ fontSize: 11.5 }}>Description: what it does and when to use it</label>
          <Input id="skill-desc" value={description} onChange={(e) => { setDescription(e.currentTarget.value); setPending(null); }} />
          {descErr && <div className="desc" style={{ color: "#ff8a8a", fontSize: 11 }}>{descErr}</div>}
          <label className="desc" htmlFor="skill-body" style={{ fontSize: 11.5 }}>Instructions (SKILL.md body)</label>
          <TextArea id="skill-body" className="input mono" rows={14} spellCheck={false} style={{ fontSize: 12, resize: "vertical", minHeight: 160 }}
            value={body} onChange={(e) => { setBody(e.currentTarget.value); setPending(null); }} />
          {bodyErr && <div className="desc" style={{ color: "#ff8a8a", fontSize: 11 }}>{bodyErr}</div>}
          <Segmented label="Where to save it" value={scope} onChange={(v) => { setScope(v); setPending(null); }} options={[
            { value: "user", label: "Global (all projects)" },
            { value: "project", label: "This project", disabled: !projectOk, hint: projectOk ? draft.project : "This chat has no local project folder" },
          ]} style={{ width: 360 }} />
          <div className="desc mono" style={{ fontSize: 11, overflowWrap: "anywhere" }}>{skillPath(dir, cleanName)}</div>
          {scope === "project" && projectOk && !draft.project_trusted && (
            <div role="note" style={{ color: "#f0c674", fontSize: 12 }}>This folder isn't trusted, so project skills in it won't load until you trust it.</div>
          )}
          {pending?.kind === "exists" && (
            <div role="group" aria-label="Replace existing skill" style={{ display: "flex", flexDirection: "column", gap: 8, padding: 10, borderRadius: 8, background: "rgba(255,255,255,0.05)" }}>
              <div style={{ fontSize: 12.5, lineHeight: 1.5 }}>A skill named <b>{cleanName}</b> already exists at <span className="mono" style={{ overflowWrap: "anywhere" }}>{pending.path}</span>. Replace its SKILL.md?</div>
              <div style={{ display: "flex", gap: 6, justifyContent: "flex-end" }}>
                <Button variant="ghost" onClick={() => setPending(null)}>Cancel</Button>
                <Button variant="primary" onClick={() => void save(true, pending.allowSecrets)}>Replace</Button>
              </div>
            </div>
          )}
          {pending?.kind === "secrets" && (
            <div role="group" aria-label="Possible secrets" style={{ display: "flex", flexDirection: "column", gap: 8, padding: 10, borderRadius: 8, background: "rgba(255,255,255,0.05)" }}>
              <div style={{ fontSize: 12.5, lineHeight: 1.5 }}>This text still looks like it contains secrets:</div>
              <ul style={{ margin: 0, paddingLeft: 18, fontSize: 12 }}>{pending.secrets.map((s, i) => <li key={i}>{s}</li>)}</ul>
              <div style={{ fontSize: 12.5 }}>Save anyway?</div>
              <div style={{ display: "flex", gap: 6, justifyContent: "flex-end" }}>
                <Button variant="ghost" onClick={() => setPending(null)}>Cancel</Button>
                <Button variant="primary" onClick={() => void save(pending.overwrite, true)}>Save anyway</Button>
              </div>
            </div>
          )}
        </> : null}
        {err && <div role="alert" style={{ color: "#ff8a8a", fontSize: 12, whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>{err}</div>}
      </div>
      <div className="mf">
        <span className="desc" style={{ flex: 1, minWidth: 0, fontSize: 11 }}>{draft && editing ? `Drafted by ${modelInfo(draft.model).name}` : ""}</span>
        {phase === "done" ? <Button variant="primary" onClick={close}>Close</Button>
          : phase === "drafting" ? <Button variant="ghost" onClick={cancelDraft}>Cancel</Button>
          : <>
            <Button variant="ghost" onClick={close}>Cancel</Button>
            {phase === "intro"
              ? <Button variant="primary" onClick={() => void startDraft()}>Draft skill</Button>
              : <Button variant="primary" disabled={!canSave} onClick={() => void save(false, false)}>{phase === "saving" ? <><Loader size={12} /> Saving…</> : "Save"}</Button>}
          </>}
      </div>
    </Modal>
  );
}
