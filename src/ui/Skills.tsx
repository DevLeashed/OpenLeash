// Settings → Skills: import and manage agent skills.
//
// A skill is a `SKILL.md` file (Agent Skills / Claude Code format) with
// `name` + `description` frontmatter and workflow instructions in the body,
// optionally with bundled files beside it. Every task lists its enabled
// skills in the system prompt; the agent reads the full `SKILL.md` when the
// job matches the description.
import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";
import { api, SkillDef } from "../api";
import { flash, set, useStore } from "../store";
import { Button, IconButton, Modal, Pressable, Switch } from "./primitives";
import { I } from "./icons";

function Preview({ name, onClose }: { name: string; onClose: () => void }) {
  const [data, setData] = useState<{ skill: SkillDef; content: string; files: string[] } | null>(null);
  useEffect(() => {
    api.skillRead(name).then(setData).catch((e) => { flash(String(e)); onClose(); });
  }, [name]);
  return (
    <Modal onClose={onClose} style={{ width: "min(640px, 100%)" }}>
      <div className="mh"><span className="mono" style={{ color: "#33d6ff" }}>✦</span><span style={{ flex: 1 }}>{name}</span><IconButton label="Close" onClick={onClose}>{I.close()}</IconButton></div>
      <div className="mb">
        {!data ? <div className="empty">Loading…</div> : (
          <>
            <div style={{ color: "#9a9aa2", lineHeight: 1.55 }}>{data.skill.description}</div>
            {(data.skill.disable_model_invocation || data.skill.user_invocable) && (
              <div style={{ fontSize: 11.5, color: "#7c7c85" }}>
                {data.skill.disable_model_invocation
                  ? "Hidden from the agent's skill list (zero context until invoked)."
                  : ""}
                {data.skill.disable_model_invocation && data.skill.user_invocable ? " " : ""}
                {data.skill.user_invocable ? "Invocable by you." : ""}
              </div>
            )}
            {!!data.files.filter((f) => f !== "SKILL.md").length && (
              <div style={{ fontSize: 11.5, color: "#7c7c85" }}>Bundled files: {data.files.filter((f) => f !== "SKILL.md").map((f) => <span key={f} className="mono" style={{ marginRight: 8 }}>{f}</span>)}</div>
            )}
            <pre className="mono" style={{ whiteSpace: "pre-wrap", wordBreak: "break-word", fontSize: 11.5, lineHeight: 1.6, background: "rgba(0,0,0,0.3)", border: "1px solid rgba(255,255,255,0.07)", borderRadius: 8, padding: 12, maxHeight: 380, overflow: "auto", margin: 0 }}>{data.content}</pre>
            <div className="mono" style={{ fontSize: 10.5, color: "#45454b" }}>{data.skill.path}</div>
          </>
        )}
      </div>
      <div className="mf"><div style={{ flex: 1 }} /><Button variant="ghost" onClick={onClose}>Close</Button></div>
    </Modal>
  );
}

export function SkillsTab() {
  const skills = useStore((s) => s.skills);
  const catalogError = useStore((s) => s.catalogError);
  const [preview, setPreview] = useState<string | null>(null);
  const [arm, setArm] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = async () => {
    try { set({ skills: await api.skills() }); } catch (e) { flash(String(e)); }
  };
  const importPath = async (path: string) => {
    setBusy(true);
    try {
      const def = await api.skillImport(path);
      await refresh();
      flash(`${def.name} imported · agents can use it in new tasks`);
    } catch (e) { flash(String(e)); }
    setBusy(false);
  };
  const importFile = async () => {
    const p = await open({ multiple: false, filters: [{ name: "Skill", extensions: ["md"] }] }).catch(() => null);
    if (typeof p === "string") void importPath(p);
  };
  const importFolder = async () => {
    const p = await open({ directory: true, multiple: false, title: "Pick a skill folder (contains SKILL.md)" }).catch(() => null);
    if (typeof p === "string") void importPath(p);
  };
  const toggle = async (sk: SkillDef) => {
    try { set({ skills: await api.skillSetEnabled(sk.name, !sk.enabled) }); } catch (e) { flash(String(e)); }
  };
  const remove = async (sk: SkillDef) => {
    if (arm !== sk.name) { setArm(sk.name); return; }
    setArm(null);
    try {
      set({ skills: await api.skillRemove(sk.name) });
      flash(`${sk.name} removed`);
    } catch (e) { flash(String(e)); }
  };

  return (
    <>
      <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
        <div style={{ flex: 1 }}>
          <div style={{ fontSize: 15, fontWeight: 600 }}>Skills</div>
          <div style={{ color: "#7c7c85", marginTop: 3, fontSize: 12 }}>Extra capabilities for agents: workflows, domain knowledge, file templates. The agent sees the list and reads a skill's instructions when the job calls for it.</div>
        </div>
        <Button onClick={importFile} disabled={busy}>Import file…</Button>
        <Button onClick={importFolder} disabled={busy}>Import folder…</Button>
      </div>
      <div className="sgroup">
        {skills.map((sk, i) => (
          <Pressable
            key={sk.name}
            className="srowx agentrow"
            style={{ animationDelay: i * 35 + "ms", opacity: sk.enabled ? 1 : 0.55 }}
            aria-label={`Preview ${sk.name}`}
            onClick={() => setPreview(sk.name)}
            onMouseLeave={() => arm === sk.name && setArm(null)}
          >
            <span style={{ color: "#33d6ff", fontSize: 13 }}>✦</span>
            <div style={{ flex: 1, minWidth: 0 }}>
              <div style={{ fontWeight: 500, display: "flex", gap: 7, alignItems: "center" }}>
                <span className="mono">{sk.name}</span>
                {sk.source === "project"
                  ? <span className="branchtag" style={{ height: 16, fontSize: 9.5, color: "#fbbf24" }}>project</span>
                  : <span className="branchtag" style={{ height: 16, fontSize: 9.5 }}>yours</span>}
                {/* The two invocation axes. `disable-model-invocation` is the one
                    worth a chip: it means the agent never sees this skill, so a
                    reader who assumed otherwise would be wrong about behaviour. */}
                {sk.disable_model_invocation && <span className="branchtag" style={{ height: 16, fontSize: 9.5, color: "#7ee787" }} title="Hidden from the agent's skill list — costs zero context until invoked. Only you can invoke it.">you only</span>}
                {sk.user_invocable && <span className="branchtag" style={{ height: 16, fontSize: 9.5 }} title="Offered in your command palette">invocable</span>}
                {sk.files > 0 && <span style={{ fontSize: 10.5, color: "#5c5c64" }}>+{sk.files} file{sk.files === 1 ? "" : "s"}</span>}
              </div>
              <div className="desc" style={{ whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{sk.description}</div>
            </div>
            <Switch label={sk.name} small hint={sk.enabled ? "On · click to turn off" : "Off · click to turn on"} checked={sk.enabled} onChange={(_, e) => { e.stopPropagation(); void toggle(sk); }} />
            {sk.source === "user" && (arm === sk.name
              ? <Button variant="primary" style={{ height: 24, fontSize: 11 }} onClick={(e) => { e.stopPropagation(); void remove(sk); }}>Remove</Button>
              : <IconButton label="Remove skill" onClick={(e) => { e.stopPropagation(); void remove(sk); }} style={{ width: 24, height: 24 }}>{I.close()}</IconButton>)}
          </Pressable>
        ))}
        {!skills.length && (catalogError
          // An empty list here is ambiguous on its own: nothing installed, or a
          // backend that never answered. Say which.
          ? <div className="empty" role="alert" style={{ color: "#ff8a8a" }}>Couldn't list skills · {catalogError}</div>
          : <div className="empty">No skills yet.</div>)}
      </div>
      {preview && <Preview name={preview} onClose={() => setPreview(null)} />}
    </>
  );
}
