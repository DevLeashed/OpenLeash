import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { DENY_PREFIX, isDenyRule, normPath, PERMS, type CustomCommand, type Race } from "../api";
import { flash, isNarrow, saveSettings, set, useStore } from "../store";
import { ago } from "../api";
import { archiveTask, deleteArchivedTasks, deleteTask, exportArchived, exportTasks, importTasks } from "./Chrome";
import { copyText } from "./clipboard";
import { AccountsTab, AgentsTab } from "./Accounts";
import { BurnerSection } from "./Burner";
import { RoutersTab } from "./Routers";
import { ModelsTab } from "./ModelManager";
import { SkillsTab } from "./Skills";
import { StatsTab } from "./Stats";
import { AppVersion } from "./AppVersion";
import { ConnectorsTab, PluginsTab } from "./Plugins";
import { McpServers } from "./McpServers";
import { ChecksTab } from "./Hooks";
import { RemoteTab } from "./Remote";
import { WorkspacesTab } from "./Workspaces";
import { defaultMode } from "../workspaces";
import { archiveRace, deleteRace, useRaces } from "../race";
import { SLASH } from "./Composer";
import { stepZoom, DEFAULT_ZOOM, ZOOMS } from "../store";
import { Minus, Plus, Trash2 } from "lucide-react";
import { Dropdown, Button, ChoiceRow, ConfirmDialog, IconButton, Input, NavTab, Pressable, Segmented, Switch, MorphText } from "./primitives";
import { SAVED_SORT_OPTIONS, savedSortOf } from "./Saved";
import { HIDDEN_SETTINGS_TABS, SETTINGS_TABS, settingsTabDef } from "./settingsTabs";

const THEMES = [
  { id: "raycast", name: "Default", accent: "#ff6363", swatch: ["#161618", "#28282b", "#ff6363", "#8b5cf6", "#33d6ff"] },
  { id: "lavender", name: "Lavender", accent: "#a78bfa", swatch: ["#0f0f11", "#1c1c1f", "#a78bfa", "#92aa99", "#e98585"] },
  { id: "nord", name: "Nord", accent: "#88c0d0", swatch: ["#242933", "#3b4252", "#88c0d0", "#81a1c1", "#bf616a"] },
  { id: "dracula", name: "Dracula", accent: "#bd93f9", swatch: ["#1e1f29", "#343746", "#bd93f9", "#8be9fd", "#ff79c6"] },
  { id: "gruvbox", name: "Gruvbox", accent: "#fabd2f", swatch: ["#1d2021", "#3c3836", "#fabd2f", "#83a598", "#fb4934"] },
  { id: "tokyo", name: "Tokyo Night", accent: "#7aa2f7", swatch: ["#16161e", "#292e42", "#7aa2f7", "#7dcfff", "#f7768e"] },
  { id: "rosepine", name: "Rosé Pine", accent: "#ebbcba", swatch: ["#191724", "#2a2740", "#ebbcba", "#9ccfd8", "#eb6f92"] },
  { id: "catppuccin", name: "Catppuccin", accent: "#cba6f7", swatch: ["#1e1e2e", "#313244", "#cba6f7", "#89dceb", "#f38ba8"] },
  { id: "solarized", name: "Solarized", accent: "#b58900", swatch: ["#002b36", "#073642", "#b58900", "#2aa198", "#dc322f"] },
  { id: "everforest", name: "Everforest", accent: "#a7c080", swatch: ["#2d353b", "#3d484d", "#a7c080", "#7fbbb3", "#e67e80"] },
  { id: "monokai", name: "Monokai", accent: "#a6e22e", swatch: ["#272822", "#3e3d32", "#a6e22e", "#66d9ef", "#f92672"] },
  { id: "ayu", name: "Ayu", accent: "#e6b450", swatch: ["#0f131a", "#1c222d", "#e6b450", "#59c2ff", "#f07178"] },
  { id: "ocean", name: "Ocean", accent: "#4fd1c5", swatch: ["#102030", "#1c3348", "#4fd1c5", "#63b3ed", "#fc8181"] },
] as const;

function ThemeTab() {
  const cur = useStore((st) => st.settings?.theme ?? "raycast");
  const mode = useStore((st) => st.settings?.mode ?? "dark");
  return (
    <>
      <div className="settings-head"><div className="settings-head-main"><div className="settings-title">Theme</div><div className="settings-lead">Pick the accent palette for the whole app, and whether it is painted light or dark.</div></div></div>
      <div className="settings-section-label">Light or dark</div>
      <Segmented
        label="Light or dark"
        options={[
          { value: "dark", label: "Dark", hint: "The default palette" },
          { value: "light", label: "Light", hint: "Light surfaces. Floating panels and popovers stay dark." },
          { value: "system", label: "System", hint: "Follow your OS setting, and keep following it when it changes" },
        ]}
        value={mode}
        onChange={(v) => void saveSettings({ mode: v })}
        style={{ width: 300 }}
      />
      <div className="settings-section-label">Accent palette</div>
      <div className="theme-grid">
        {THEMES.map((t) => (
          <Pressable key={t.id} className={"theme-card" + (cur === t.id ? " on" : "")} style={{ ["--card-accent" as string]: t.accent }} aria-pressed={cur === t.id} onClick={() => void saveSettings({ theme: t.id })}>
            <div className="theme-swatch">{t.swatch.map((c) => <span key={c} style={{ background: c }} />)}</div>
            <div style={{ fontWeight: 600 }}>{t.name}</div>
          </Pressable>
        ))}
      </div>
    </>
  );
}

/**
 * Where this project's memory notes live: the app's own folder (the default) or
 * `<project>/.openleash/memory/`, where they can be committed with the code.
 *
 * Per project, and stored as the normalized folder paths `memory_in_project`
 * holds — the same shape `project_docs` uses, so a path picked one way matches
 * when compared another. Turning it off does not hide notes already committed
 * there: the backend keeps reading them and merges them with the private ones,
 * the private copy winning a conflict. A remote (SSH) project has no local
 * folder to write to, so it gets no switch.
 */
function ProjectMemory() {
  const s = useStore((st) => st.settings)!;
  const project = s.project;
  if (!project || project.startsWith("ssh://")) return null;
  const listed = s.memory_in_project ?? [];
  const on = listed.some((p) => normPath(p) === normPath(project));
  const toggle = (checked: boolean) => {
    const rest = listed.filter((p) => normPath(p) !== normPath(project));
    void saveSettings({ memory_in_project: checked ? [...rest, project] : rest });
  };
  return (
    <div className="srowx" style={{ alignItems: "flex-start" }}>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontWeight: 500 }}>Keep this project's memory in the project</div>
        <div className="desc">Save this project's notes in <span className="mono">.openleash/memory/</span> inside the folder instead of <span className="mono">~/.openleash</span>, so they can be committed and shared. Notes about you stay private. Notes already in that folder are always read, and where both folders hold the same note the <span className="mono">~/.openleash</span> one wins. Only used once the folder is trusted. Applies to new chats.</div>
      </div>
      <Switch label="Keep this project's memory in the project" checked={on} onChange={toggle} />
    </div>
  );
}

/**
 * Per-project instruction files, with a switch to stop any of them reaching the
 * agent's prompt.
 *
 * The reason this exists: a repository can carry a `CLAUDE.md` (or `AGENTS.md`,
 * …) that a particular project should not have injected into every agent. That
 * is a selection, not a security decision, so it lives here rather than in the
 * folder-trust dialog — trust decides whether *anything* from the folder may be
 * loaded and fails closed; this only narrows which of the files that are already
 * allowable actually go in, and fails open.
 *
 * The list is the project's own inventory from `project_info` (`docs`), not the
 * files that survived the selection (`disabled_docs` tells the two apart), so a
 * switched-off file stays visible and can be switched back on. Toggling writes
 * the whole rule list in one settings patch; `changed_doc_prefixes` on the Rust
 * side drops any frozen prompt built under the old selection.
 */
function ProjectDocs() {
  const s = useStore((st) => st.settings)!;
  const info = useStore((st) => st.projectInfo);
  const project = s.project;
  const docs = info?.docs ?? [];
  const disabled = info?.disabled_docs ?? [];
  if (!project) {
    return <div className="settings-note">No project open. Pick a folder to choose which of its instruction files the agent reads.</div>;
  }
  if (!docs.length) {
    return <div className="settings-note">This project has no instruction files of its own. OpenLeash reads <span className="mono">OPENLEASH.md</span>, <span className="mono">AGENTS.md</span>, <span className="mono">CLAUDE.md</span> or <span className="mono">.openleash/instructions.md</span> from the project root.</div>;
  }
  // One rule per project, replaced whole: the disabled list is small and a
  // per-file upsert would need the same read-modify-write anyway.
  const setDisabled = (next: string[]) => {
    const rules = (s.project_docs ?? []).filter((r) => normPath(r.path) !== normPath(project));
    if (next.length) rules.push({ path: project, disabled: next });
    void saveSettings({ project_docs: rules });
  };
  const toggle = (name: string, on: boolean) => {
    const set = new Set(disabled);
    if (on) set.delete(name); else set.add(name);
    setDisabled(docs.filter((d) => set.has(d)));
  };
  return <>
    <div className="settings-note">Files this project ships that are read into every agent's system prompt. Switch one off to keep it out of the agent entirely — it is skipped by name and by content, for this project only.</div>
    {info && !info.trusted && <div className="settings-note">This folder is <b>not trusted</b>, so none of its files reach the agent anyway. Your selection is remembered and applies if you trust it later.</div>}
    <div className="sgroup">
      {docs.map((name) => (
        <div key={name} className="srowx">
          <div style={{ flex: 1, minWidth: 0 }}><div className="mono" style={{ fontSize: 12 }}>{name}</div></div>
          <Switch label={`Inject ${name}`} checked={!disabled.includes(name)} onChange={(checked) => toggle(name, checked)} />
        </div>
      ))}
    </div>
  </>;
}

/**
 * Import another coding tool's project rules into this app's own instruction
 * file (`OPENLEASH.md`).
 *
 * Two steps on purpose. "Scan" is read-only (`import_scan`) and shows the user
 * the count, a preview per source and what was skipped; nothing is written
 * until they press "Import N items" (`import_apply`), which appends a labelled,
 * deletable block per source and never overwrites the file. The backend
 * (`importer.rs`) refuses an untrusted folder and refuses again on a re-run, so
 * this button is not the only guard: it is the *ask*, and the gate is behind it.
 *
 * The wire types are declared here rather than in `../api` because this row
 * owns its two commands; the command surface is `import_scan` / `import_apply`.
 */
type ImportItem = { id: string; tool: string; tool_id: string; kind: "rules" | "agent"; path: string; label: string; chars: number; meta: [string, string][]; preview: string; flagged: boolean; flag_reason: string; note: string };
type ImportSkipped = { tool: string; path: string; reason: string };
type ImportReport = { project: string; trusted: boolean; target: string; items: ImportItem[]; skipped: ImportSkipped[]; warnings: string[] };
type ImportOutcome = { imported: number; skipped: number; target: string; tools: string[] };

const baseName = (p: string) => p.split(/[\\/]/).pop() || p;
const items = (n: number) => `${n} item${n === 1 ? "" : "s"}`;

function ProjectImport() {
  const s = useStore((st) => st.settings)!;
  const project = s.project;
  const [report, setReport] = useState<ImportReport | null>(null);
  const [busy, setBusy] = useState(false);
  // A scan belongs to the folder it was taken in: switching projects drops it
  // rather than offering to file one repo's rules under another one's heading.
  useEffect(() => { setReport(null); }, [project]);
  if (!project) {
    return <div className="settings-note">No project open. Pick a folder to import another tool's rules from.</div>;
  }
  const scan = () => {
    setBusy(true);
    invoke<ImportReport>("import_scan", { path: project })
      .then((r) => { setReport(r); flash(r.items.length ? `${items(r.items.length)} to import from this project.` : "Nothing new to import."); })
      .catch((e) => flash(String(e)))
      .finally(() => setBusy(false));
  };
  // Confirm: append, then re-scan so the list shows what is left (nothing).
  const run = () => {
    setBusy(true);
    invoke<ImportOutcome>("import_apply", { path: project })
      .then((r) => { flash(r.imported ? `Imported ${items(r.imported)} into ${baseName(r.target)}` : "Nothing new to import."); })
      .then(() => invoke<ImportReport>("import_scan", { path: project }))
      .then(setReport)
      .catch((e) => flash(String(e)))
      .finally(() => setBusy(false));
  };
  const n = report?.items.length ?? 0;
  return <>
    <div className="settings-note">Bring another tool's project rules — Cursor (<span className="mono">.cursor/rules</span>, <span className="mono">.cursorrules</span>), Cline (<span className="mono">.clinerules</span>, <span className="mono">.cline/rules</span>), GitHub Copilot (<span className="mono">.github/copilot-instructions.md</span>, <span className="mono">.github/instructions</span>), Gemini CLI (<span className="mono">GEMINI.md</span>) and <span className="mono">.claude/agents</span> — into <span className="mono">OPENLEASH.md</span>. Scanning writes nothing. Importing appends a labelled block per source that you can find and delete, never overwrites your text, and never writes the same source twice.</div>
    <div className="sgroup">
      <div className="srowx" style={{ alignItems: "flex-start" }}>
        <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Import another tool's rules</div><div className="desc">Reads this project folder only. Skips files that are empty, that you already have, or that are already in your instruction files.</div></div>
        <Button disabled={busy} onClick={scan}><MorphText>{busy ? "Scanning…" : "Scan"}</MorphText></Button>
      </div>
    </div>
    {report && <>
      {!report.trusted && <div className="settings-note">This folder is <b>not trusted</b>, so OpenLeash will not import from it — a repository you have not looked at cannot write into the instructions your agents read. Answer the folder-trust prompt for this project first.</div>}
      {report.warnings.map((w) => <div key={w} className="settings-note">{w}</div>)}
      <div className="settings-section-label">Found {items(n)} to import</div>
      {n > 0 && <div className="sgroup">
        {report.items.map((it) => (
          <div key={it.id} className="srowx" style={{ alignItems: "flex-start" }}>
            <div style={{ flex: 1, minWidth: 0 }}>
              <div className="mono" style={{ fontSize: 12 }}>{it.path}</div>
              <div className="desc">{it.tool} · {it.kind === "agent" ? "subagent definition" : "rules"}{it.chars ? ` · ${it.chars} chars` : ""}{it.label && it.label !== baseName(it.path) ? ` · ${it.label}` : ""}{it.note ? ` · ${it.note}` : ""}</div>
              {/* Imported either way, but never folded in silently: an override reads as text, and the flag rides with it into the file. */}
              {it.flagged && <div className="desc" style={{ color: "#ff8a8a" }}>⚠ {it.flag_reason}. It is imported as text only — it cannot change an allow rule, a hook, a permission mode or a tool list.</div>}
              <div className="desc mono" style={{ whiteSpace: "pre-wrap", maxHeight: 108, overflow: "hidden" }}>{it.preview}{it.chars > it.preview.length ? "…" : ""}</div>
            </div>
          </div>
        ))}
      </div>}
      {report.skipped.length > 0 && <>
        <div className="settings-section-label">Skipped {report.skipped.length}</div>
        <div className="sgroup">
          {report.skipped.map((sk) => (
            <div key={`${sk.tool}:${sk.path}`} className="srowx">
              <div style={{ flex: 1, minWidth: 0 }}><div className="mono" style={{ fontSize: 12 }}>{sk.path}</div><div className="desc">{sk.reason}</div></div>
            </div>
          ))}
        </div>
      </>}
      <div className="sgroup">
        <div className="srowx" style={{ alignItems: "flex-start" }}>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontWeight: 500 }}>{n ? `Append to ${baseName(report.target)}` : "Nothing to import"}</div>
            <div className="desc">{n ? "A labelled block per source, added under a heading naming the tool and the source path so you can find and delete it. Your own text in the file is never overwritten." : "No Cursor, Cline, Copilot, Gemini or .claude/agents rules were found that you do not already have."}</div>
          </div>
          <Button variant="primary" disabled={busy || !report.trusted || !n} onClick={run}><MorphText>{busy ? "Importing…" : n ? `Import ${items(n)}` : "Import"}</MorphText></Button>
        </div>
      </div>
    </>}
  </>;
}

function General() {
  const s = useStore((st) => st.settings)!;
  const dataDir = useStore((st) => st.dataDir);
  const [copied, setCopied] = useState(false);
  // Trace export. The endpoint is persisted like any other setting, but the
  // exporter itself is process state, so applying it is a second, separate call
  // — and it is the one that reports a refusal or a fact worth knowing.
  const [endpoint, setEndpoint] = useState(s.trace_endpoint ?? "");
  const [traceNote, setTraceNote] = useState("");
  const [traceErr, setTraceErr] = useState(false);
  const saveTrace = async () => {
    if (!(await saveSettings({ trace_endpoint: endpoint.trim() }))) return;
    try {
      const st = await invoke<{ enabled: boolean; endpoint: string; error: string | null }>("trace_configure");
      setTraceErr(!!st.error);
      setTraceNote(st.error ?? (st.enabled ? `Sending spans to ${st.endpoint}` : "Trace export is off"));
    } catch (e) {
      setTraceErr(true);
      setTraceNote(String(e));
    }
  };
  return (
    <>
      <div className="settings-head"><div className="settings-head-main settings-title">General</div></div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Interface zoom</div><div className="desc">Ctrl + / Ctrl − / Ctrl 0</div></div>
          <div className="zoom">
            <IconButton label="Zoom out" onClick={() => stepZoom(-1)}><Minus size={14} /></IconButton>
            <Dropdown value={String(s.ui_zoom)} options={Array.from(new Set([...ZOOMS, s.ui_zoom])).sort((a, b) => a - b).map((z) => ({ value: String(z), label: `${z}%` }))} onChange={(z) => saveSettings({ ui_zoom: Number(z) })} search={false} style={{ width: 76, height: 28, padding: "0 8px" }} />
            <IconButton label="Zoom in" onClick={() => stepZoom(1)}><Plus size={14} /></IconButton>
            {s.ui_zoom !== DEFAULT_ZOOM && <Button variant="ghost" style={{ height: 26 }} onClick={() => stepZoom(0)}>Reset</Button>}
          </div></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Close to tray</div><div className="desc">Keep agents running after closing the window.</div></div>
          <Switch label="Close to tray" checked={s.close_to_tray !== false} onChange={(checked) => void saveSettings({ close_to_tray: checked })} /></div>
        {/* Memory sits up here because it changes how every chat behaves, and it
            is on by default: the switch to turn it off should not be buried. */}
        <div className="srowx" style={{ alignItems: "flex-start" }}>
          <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Memory</div><div className="desc">The agent keeps notes about you (shared by every project) and about each project, and loads them at the start of every new chat — so preferences and corrections stick without you repeating them. It saves on its own and keeps them tidy, and never stores secrets. Plain markdown in <span className="mono">~/.openleash/memory/</span>: read, edit or delete any of it by hand.</div></div>
          <Switch label="Memory" checked={s.memory !== false} onChange={(checked) => void saveSettings({ memory: checked })} />
        </div>
        {s.memory !== false && <ProjectMemory />}
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Automatically resume when an account is available</div><div className="desc">Restart chats paused because all provider accounts or keys were out of usage, or because every account was held by a usage cap.</div></div>
          <Switch label="Automatically resume when an account is available" checked={s.automatic_resume !== false} onChange={(checked) => void saveSettings({ automatic_resume: checked })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Notifications</div><div className="desc">When a chat finishes, fails or needs you. A desktop popup if the window is in the background, an in-app notice if you are looking at it.</div></div>
          <Switch label="Desktop notifications" checked={s.notify !== false} onChange={(checked) => void saveSettings({ notify: checked })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Send message with</div></div>
          <Segmented label="Send message with" options={[{ value: "enter", label: "Enter" }, { value: "ctrl-enter", label: "Ctrl Enter" }]} value={s.send_with} onChange={(v) => void saveSettings({ send_with: v })} /></div>
        {/* Off by default, and the default is the point: a phone's on-screen
            Return key cannot be relied on to say whether Shift is held, so
            Enter-sends there also sends the messages someone was breaking into
            paragraphs. The desktop switch above cannot cover a phone — a touch
            keyboard has no Ctrl — so this is a switch of its own. */}
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Enter sends on a phone</div><div className="desc">Off, Enter starts a new line in a phone browser and the send button sends. On, Enter sends and Shift Enter starts a new line. This window is unaffected.</div></div>
          <Switch label="Enter sends on a phone" checked={s.phone_enter_sends === true} onChange={(checked) => void saveSettings({ phone_enter_sends: checked })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Show thinking</div><div className="desc">Show the model's internal reasoning blocks in chats.</div></div>
          <Switch label="Show thinking" checked={s.show_thinking ?? false} onChange={(checked) => void saveSettings({ show_thinking: checked })} /></div>
        {/* Off by default, and the default is the point: the split is an estimate
            rescaled to the provider's prompt count, so the measured ring and its
            token pair are what everyone sees unless they ask for the rest. */}
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Context breakdown</div><div className="desc">Under the context meter, show the estimated split — system prompt, project instructions, skills and memory, tool schemas, attachments, conversation, summary. Off shows the ring and its token counts only.</div></div>
          <Switch label="Context breakdown" checked={s.ctx_breakdown ?? false} onChange={(checked) => void saveSettings({ ctx_breakdown: checked })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Automatically rebuild Docker images</div><div className="desc">Changed Docker config rebuilds before next use. When disabled, OpenLeash uses an existing image; first builds and manual rebuilds still work.</div></div>
          <Switch label="Automatically rebuild Docker images" checked={s.docker_auto_rebuild !== false} onChange={(checked) => void saveSettings({ docker_auto_rebuild: checked })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>New tasks use</div><div className="desc">The project folder, a separate git worktree per task, or a container (Settings → Workspaces).</div></div>
          <Segmented label="New tasks use" value={defaultMode(s)} onChange={(v) => { void saveSettings({ workspace: v, worktree: v === "worktree" }); set((st) => ({ home: { ...st.home, worktree: v === "worktree", docker: v === "docker" } })); }} options={[{ value: "local", label: "Local" }, { value: "worktree", label: "Worktree" }, { value: "docker", label: "Docker" }]} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Assist mode for new chats</div><div className="desc">How often the agent asks for guidance.</div></div>
          <Segmented label="Assist mode for new chats" options={[{ value: "guide", label: "Guide" }, { value: "default", label: "Default" }, { value: "necessary", label: "Necessary" }]} value={s.assist} onChange={(v) => { void saveSettings({ assist: v }); set((st) => ({ home: { ...st.home, assist: v } })); }} /></div>
      </div>
      <div className="settings-section-label">Data folder</div>
      <div className="sgroup">
        <div className="srowx">
          <div style={{ flex: 1, minWidth: 0 }}><div className="mono sel" style={{ fontSize: 12 }}>{dataDir}</div><div className="desc">Set OPENLEASH_HOME to move this folder.</div></div>
          <Button onClick={() => copyText(dataDir, { onResult: (ok) => { if (!ok) return; setCopied(true); window.setTimeout(() => setCopied(false), 1200); } })}><MorphText>{copied ? "Copied" : "Copy"}</MorphText></Button>
        </div>
      </div>
      <div className="settings-section-label">Project instructions</div>
      <div className="settings-note">Reads <span className="mono">OPENLEASH.md</span>, <span className="mono">AGENTS.md</span> or <span className="mono">CLAUDE.md</span> from the project, plus <span className="mono">~/.openleash/OPENLEASH.md</span>. Use <span className="mono">/init</span> to create one.</div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Inject global CLAUDE.md</div><div className="desc">Also load <span className="mono">~/.claude/CLAUDE.md</span> into every agent.</div></div>
          <Switch label="Inject global CLAUDE.md" checked={s.inject_global_claude ?? false} onChange={(checked) => void saveSettings({ inject_global_claude: checked })} /></div>
      </div>
      <div className="settings-section-label">Project docs</div>
      <ProjectDocs />
      <div className="settings-section-label">Import from another tool</div>
      <ProjectImport />
      <div className="settings-section-label">Trace export</div>
      <div className="sgroup">
        <div className="srowx" style={{ alignItems: "flex-start" }}>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontWeight: 500 }}>Send spans to an OpenTelemetry collector</div>
            <div className="desc">Off unless you name a collector you run. Each model request and tool call becomes one span carrying the tool name, model id, provider-reported token counts and a duration — never prompts, tool arguments, file contents or keys. Nothing is sent anywhere else, ever.</div>
          </div>
          <Input className="input mono" style={{ width: 250, flex: "none" }} aria-label="OTLP endpoint" value={endpoint} placeholder="http://localhost:4318/v1/traces" onChange={(e) => setEndpoint(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter") void saveTrace(); }} />
          <Button style={{ flex: "none" }} disabled={endpoint.trim() === (s.trace_endpoint ?? "")} onClick={() => void saveTrace()}>Apply</Button>
        </div>
      </div>
      {traceNote && <div className="settings-note" role={traceErr ? "alert" : undefined} style={traceErr ? { color: "#ff8a8a" } : undefined}>{traceNote}</div>}
      <AppVersion />
    </>
  );
}

function Perms() {
  const s = useStore((st) => st.settings)!;
  const [rule, setRule] = useState("");
  const [denyRule, setDenyRule] = useState("");
  // Deny rules share the `allow` array with allow rules, told apart by the `!`
  // prefix. Each list keeps the index into the *full* array so a remove filters
  // `s.allow` and both sections round-trip through the same save.
  const allows = s.allow.map((a, i) => ({ a, i })).filter(({ a }) => !isDenyRule(a.pattern));
  const denies = s.allow.map((a, i) => ({ a, i })).filter(({ a }) => isDenyRule(a.pattern));
  return (
    <>
      <div className="settings-head"><div className="settings-head-main settings-title">Permissions</div></div>
      <div className="settings-section-label">Default mode for new tasks</div>
      <div className="sgroup">
        {PERMS.map((p) => (
          <ChoiceRow key={p.id} className="srowx" variant="radio" selected={s.perm === p.id} style={{ alignItems: "flex-start" }} onClick={() => { void saveSettings({ perm: p.id }).then((saved) => { if (saved) set((st) => ({ home: { ...st.home, perm: p.id } })); }); }}>
            <div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>{p.name}</div><div className="desc">{p.desc}</div></div>
          </ChoiceRow>
        ))}
      </div>
      <div className="settings-note">Read-only commands stay available. Plan mode requires approval for edits. A deny rule always wins over every allow rule and over every permission level, including Full Access. An allow rule never covers a chained command — one with a second command, a redirect or a substitution — so it still asks; a deny rule does reach into one. In Reviewer mode a second model answers those prompts in your place and a refusal is final for that call, but a deny rule still decides before the reviewer is ever asked.</div>
      <div className="settings-section-label">Always-allowed commands</div>
      <div className="sgroup">
        {allows.map(({ a, i }) => (
          <div key={i} className="srowx" style={{ minHeight: 38 }}>
            <span className="mono" style={{ flex: 1, fontSize: 12 }}>{a.pattern}</span>
            <span style={{ fontSize: 11, color: "var(--dim)" }}>{a.project ? a.project.split(/[\\/]/).pop() : "all projects"}</span>
            <IconButton label={`Remove ${a.pattern}`} style={{ width: 24, height: 24 }} onClick={() => saveSettings({ allow: s.allow.filter((_, j) => j !== i) })}><Trash2 size={12} /></IconButton>
          </div>
        ))}
        <div className="srowx" style={{ minHeight: 42 }}>
          <Input className="input mono" style={{ flex: 1, fontSize: 12 }} value={rule} placeholder="npm run build *" onChange={(e) => setRule(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter" && rule.trim()) { saveSettings({ allow: [...s.allow, { pattern: rule.trim(), project: "" }] }); setRule(""); } }} />
        </div>
      </div>
      <div className="settings-section-label">Never-allowed commands (deny)</div>
      <div className="settings-note">A deny rule always wins — over allow rules, over the allowlist and over every permission level, including Full Access. It also reaches inside a chained command, so <span className="mono">rm -rf *</span> still catches the <span className="mono">rm</span> in <span className="mono">npm test &amp;&amp; rm -rf /</span>.</div>
      <div className="sgroup">
        {denies.map(({ a, i }) => (
          <div key={i} className="srowx" style={{ minHeight: 38 }}>
            <span className="mono" style={{ flex: 1, fontSize: 12 }}>{a.pattern.replace(/^!/, "")}</span>
            <span style={{ fontSize: 11, color: "var(--dim)" }}>{a.project ? a.project.split(/[\\/]/).pop() : "all projects"}</span>
            <IconButton label={`Remove ${a.pattern}`} style={{ width: 24, height: 24 }} onClick={() => saveSettings({ allow: s.allow.filter((_, j) => j !== i) })}><Trash2 size={12} /></IconButton>
          </div>
        ))}
        <div className="srowx" style={{ minHeight: 42 }}>
          <Input className="input mono" style={{ flex: 1, fontSize: 12 }} value={denyRule} placeholder="rm -rf *" onChange={(e) => setDenyRule(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter" && denyRule.trim()) { saveSettings({ allow: [...s.allow, { pattern: DENY_PREFIX + denyRule.trim(), project: "" }] }); setDenyRule(""); } }} />
        </div>
      </div>
      <div className="settings-section-label">Secret files</div>
      <div className="sgroup">
        <div className="srowx" style={{ minHeight: 42 }}>
          <div style={{ flex: 1 }}>
            <div style={{ fontWeight: 500 }}>Deny reading <span className="mono">.env</span> files</div>
            <div className="desc"><span className="mono">*.env</span> and <span className="mono">*.env.*</span> are denied for <span className="mono">read_file</span>. <span className="mono">*.env.example</span> / <span className="mono">.sample</span> / <span className="mono">.template</span> / <span className="mono">.dist</span> stay readable.</div>
          </div>
          <Switch
            label="Deny reading .env files"
            hint="A deny that needs no rule. Reads are not sandboxed either way."
            small
            checked={s.deny_env_files !== false}
            onChange={(v) => void saveSettings({ deny_env_files: v })}
          />
        </div>
      </div>
      <div className="settings-note">{s.deny_env_files === false
        ? <>This is off, so <span className="mono">read_file</span> can read <span className="mono">.env</span> files. Turn it back on unless you have a reason.</>
        : <>To let the agent read one specific file, add an allow rule such as <span className="mono">read_file .env</span>. Turning this off lifts the deny for every <span className="mono">.env</span> file in every project.</>}
      </div>
    </>
  );
}


/** The last step before an archived race is deleted for good.
 *
 *  A race is a group of real chats, so deleting one takes work away that the
 *  user may still want. The dialog names what goes (the race and the chats
 *  still archived with it) and offers the worktree choice, because a kept
 *  worktree is a checkout the user has to clean up by hand. */
function RaceDeleteDialog({ race, removeWorktree, onRemoveWorktree, onConfirm, onClose }: {
  race: Race;
  removeWorktree: boolean;
  onRemoveWorktree: (v: boolean) => void;
  onConfirm: (removeWorktree: boolean) => void | Promise<void>;
  onClose: () => void;
}) {
  const attempts = race.attempts.length;
  return (
    <ConfirmDialog title="Delete this race for good?" confirm="Delete forever" danger onClose={onClose} onConfirm={() => void onConfirm(removeWorktree)}
      body={<div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
        <div className="desc">
          The race record and its ranking are dropped, along with the {attempts} attempt chat{attempts === 1 ? "" : "s"} still archived with it. This cannot be undone.
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 8, color: "var(--mut3)", fontSize: 11.5 }}>
          <Switch label="Also permanently remove the attempt worktrees" checked={removeWorktree} small onChange={onRemoveWorktree} />
          <span>Also remove their worktrees and any uncommitted files in them; branches remain.</span>
        </div>
        {!removeWorktree && <div style={{ fontSize: 11, color: "var(--dim)" }}>Their worktrees stay on disk.</div>}
      </div>} />
  );
}

function ChatsTab() {
  const tasks = useStore((st) => st.tasks);
  const s = useStore((st) => st.settings)!;
  const [arm, setArm] = useState<string | null>(null);
  const [removeWorktree, setRemoveWorktree] = useState<string | null>(null);
  const [armAll, setArmAll] = useState(false);
  const [armRace, setArmRace] = useState<string | null>(null);
  const [raceWorktree, setRaceWorktree] = useState(false);
  const [deletingRace, setDeletingRace] = useState<string | null>(null);
  const all = Object.values(tasks);
  const archived = all.filter((t) => t.archived).sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  const raceMap = useRaces((st) => st.races);
  const archivedRaces = Object.values(raceMap).filter((r) => r.archived).sort((a, b) => b.created_at.localeCompare(a.created_at));
  const armedRace = deletingRace ? archivedRaces.find((r) => r.id === deletingRace) : undefined;
  return (
    <>
      <div className="settings-head"><div className="settings-head-main settings-title">Chats</div></div>
      <div className="sgroup">
        <div className="srowx" style={{ alignItems: "flex-start" }}>
          <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Let the agent name chats</div><div className="desc">The agent names a chat once the job is clear, and renames it when the work moves on. Titles you set yourself are never renamed — use a chat's right-click menu to hand a name back.</div></div>
          <Switch label="Let the agent name chats" checked={s.agent_titles ?? true} onChange={(checked) => void saveSettings({ agent_titles: checked })} />
        </div>
      </div>
      <div className="settings-section-label">Other chats</div>
      <div className="sgroup">
        <div className="srowx" style={{ alignItems: "flex-start" }}>
          <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Let agents see and message your other chats</div><div className="desc">Lets a chat's main agent search your other chats (<span className="mono">list_chats</span>) and message them (<span className="mono">message_chat</span>). Each message still asks unless you always-allow it.</div></div>
          <Switch label="Let agents see and message your other chats" checked={s.cross_chat ?? true} onChange={(checked) => void saveSettings({ cross_chat: checked })} />
        </div>
      </div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Export all chats</div><div className="desc">{all.length - archived.length} active chats · JSON</div></div>
          <Button onClick={() => exportTasks("all")}>Export…</Button></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Import chats</div><div className="desc">OpenLeash JSON · keeps existing chats</div></div>
          <Button onClick={importTasks}>Import…</Button></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Delete archived chats</div><div className="desc">{archived.length ? `Deletes ${archived.length} chat${archived.length === 1 ? "" : "s"} from history and keeps their worktrees` : "Nothing archived"}</div></div>
          {armAll
            ? <Button variant="primary" style={{ height: 26, background: "#ff8a8a" }} onClick={() => { setArmAll(false); void deleteArchivedTasks(); }}>Delete {archived.length} chat{archived.length === 1 ? "" : "s"}</Button>
            : <Button variant="ghost" disabled={!archived.length} style={{ height: 26, color: "#ff8a8a" }} onClick={() => setArmAll(true)}>Delete all…</Button>}</div>
      </div>
    <div className="settings-section-label">Chat list</div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Automatic chat reorder</div><div className="desc">Sort by attention and recent activity, while keeping manually dragged chats in place until you return to them.</div></div>
          <Switch label="Automatic chat reorder" checked={s.automatic_chat_reorder !== false} onChange={(checked) => void saveSettings({ automatic_chat_reorder: checked })} /></div>
      </div>
      <div className="settings-section-label">Archived · {archived.length}</div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Export archived chats</div><div className="desc">{archived.length ? `${archived.length} archived chat${archived.length === 1 ? "" : "s"} · full history, JSON` : "Nothing archived"}</div></div>
          {/* The label is what this export is *for*, not a generic "Export…" like
              the active-chats row above: the archive is the only thing it can
              reach, so naming the set beats repeating the verb. */}
          <Button variant="ghost" disabled={!archived.length} style={{ height: 26 }} onClick={() => void exportArchived()}>Export archived</Button></div>
      </div>
      {archivedRaces.length > 0 && <>
        <div className="settings-section-label">Archived races · {archivedRaces.length}</div>
        <div className="sgroup">
          {archivedRaces.map((r) => (
            <div key={r.id} className="srowx agentrow" style={{ minHeight: 42 }} onMouseLeave={() => { if (armRace === r.id) setArmRace(null); }}>
              <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>Race · {r.prompt}</div><div className="desc">{r.attempts.length} attempts · {baseName(r.project)}{r.winner ? (r.merged ? " · merged" : " · winner picked") : ""}</div></div>
              <Button variant="ghost" style={{ height: 26 }} onClick={() => void archiveRace(r.id, false)}>Restore</Button>
              {armRace === r.id
                ? <Button variant="primary" style={{ height: 26, background: "#ff8a8a" }} onClick={() => { setArmRace(null); setRaceWorktree(false); setDeletingRace(r.id); }}>Delete forever</Button>
                : <Button variant="ghost" style={{ height: 26, color: "#ff8a8a" }} onClick={() => setArmRace(r.id)}>Delete…</Button>}
            </div>
          ))}
        </div>
      </>}
      {deletingRace && armedRace && <RaceDeleteDialog race={armedRace} removeWorktree={raceWorktree} onRemoveWorktree={setRaceWorktree}
        onClose={() => { setDeletingRace(null); setRaceWorktree(false); }}
        onConfirm={async (removeWorktree) => { setDeletingRace(null); setRaceWorktree(false); await deleteRace(deletingRace, removeWorktree); }} />}
      {archived.length ? <div className="sgroup">
        {archived.map((t) => (
          <div key={t.id} className="srowx agentrow" style={{ minHeight: 42 }} onMouseLeave={() => { if (arm === t.id) setArm(null); if (removeWorktree === t.id) setRemoveWorktree(null); }}>
            <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{t.title}</div><div className="desc">{ago(t.updated_at)} ago · {t.project.split(/[\/]/).pop()}</div></div>
            <Button variant="ghost" style={{ height: 26 }} onClick={() => archiveTask(t.id, false)}>Restore</Button>
            {removeWorktree === t.id
              ? <div style={{ display: "flex", flexDirection: "column", alignItems: "flex-end", gap: 2 }}>
                  <span style={{ color: "var(--mut3)", fontSize: 10 }}>Also delete uncommitted files in {t.cwd}?</span>
                  <div style={{ display: "flex", gap: 5 }}>
                    <Button variant="primary" style={{ height: 26, background: "#ff8a8a" }} onClick={() => { setRemoveWorktree(null); void deleteTask(t.id, true); }}>Delete worktree + chat</Button>
                    <Button variant="ghost" style={{ height: 26 }} onClick={() => setRemoveWorktree(null)}>Keep</Button>
                  </div>
                </div>
              : arm === t.id
                ? <Button variant="primary" style={{ height: 26 }} onClick={() => { setArm(null); if (t.worktree) setRemoveWorktree(t.id); else void deleteTask(t.id); }}>Delete forever</Button>
                : <Button variant="ghost" style={{ height: 26, color: "#ff8a8a" }} onClick={() => setArm(t.id)}>Delete…</Button>}
          </div>
        ))}
      </div> : <div className="empty">No archived chats.</div>}
    </>
  );
}

const EMPTY_CUSTOM_COMMANDS: CustomCommand[] = [];

function CommandsTab() {
  const settings = useStore((st) => st.settings)!;
  const commands: CustomCommand[] = settings.custom_commands ?? EMPTY_CUSTOM_COMMANDS;
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [prompt, setPrompt] = useState("");
  const builtins = SLASH.map((c) => c.cmd.slice(1));
  const normalized = name.trim().toLowerCase().replace(/[^a-z0-9-]/g, "-").replace(/-+/g, "-").replace(/^-|-$/g, "").slice(0, 32);
  const save = (next: CustomCommand[]) => void saveSettings({ custom_commands: next });
  return <>
    <div className="settings-head"><div className="settings-head-main"><div className="settings-title">Commands</div><div className="settings-lead">Prompt templates invoked with /name. Use <span className="mono">{'{{args}}'}</span> where supplied text should go.</div></div></div>
    <div className="settings-note">Commands expand into an ordinary user prompt. They do not run shell commands or code. Editing harness source files with the agent is separate from runtime in-process mods.</div>
    <div className="sgroup">
      {commands.map((c, i) => <div key={`${c.name}-${i}`} className="srowx" style={{ alignItems: "flex-start" }}>
        <div style={{ flex: 1, minWidth: 0 }}><div className="mono">/{c.name} · {c.description}</div><div className="desc" style={{ whiteSpace: "pre-wrap" }}>{c.prompt}</div></div>
        <IconButton label={`Remove /${c.name}`} onClick={() => save(commands.filter((_, j) => j !== i))}><Trash2 size={13} /></IconButton>
      </div>)}
    </div>
    <div className="sgroup">
      <div className="srowx"><Input aria-label="Command name" className="input mono" value={name} placeholder="review-pr" onChange={(e) => setName(e.currentTarget.value)} /></div>
      <div className="srowx"><Input aria-label="Command description" className="input" value={description} placeholder="Review changes" onChange={(e) => setDescription(e.currentTarget.value)} /></div>
      <div className="srowx"><textarea aria-label="Command prompt" className="input" rows={5} value={prompt} placeholder="Review this project. {{args}}" onChange={(e) => setPrompt(e.currentTarget.value)} /></div>
      <div className="srowx"><Button disabled={!normalized || !prompt.trim() || builtins.includes(normalized) || commands.some((c) => c.name === normalized)} onClick={() => { save([...commands, { name: normalized, description: description.trim(), prompt: prompt.trim() }]); setName(""); setDescription(""); setPrompt(""); }}>Add command</Button></div>
    </div>
  </>;
}

/**
 * Settings → Tabs: every setting that decides which screens the sidebar offers
 * or how one of them behaves, grouped by the tab it belongs to. They used to be
 * rows in General; nothing about where they are stored has changed.
 */
function TabsTab() {
  const s = useStore((st) => st.settings)!;
  return (
    <>
      <div className="settings-head"><div className="settings-head-main settings-title">Tabs</div></div>
      <div className="settings-note">What the sidebar shows, and how each screen behaves. Grouped by the tab they belong to.</div>
      <div className="settings-section-label">Sidebar</div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Show chats from all projects</div><div className="desc">List chats from every project folder, not just the current one.</div></div>
          <Switch label="Show chats from all projects" checked={s.all_projects ?? false} onChange={(checked) => void saveSettings({ all_projects: checked })} /></div>
        {/* The footer button sits beside the usage bar, which is about accounts, so
            Accounts is what it has always opened and stays the default. Any page
            is offered, including the ones the nav hides: see `settingsTabs.ts`. */}
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Shortcut button</div><div className="desc">The button beside the usage bar opens this settings page.</div></div>
          <Dropdown placeholder="Settings page" value={settingsTabDef(s.sidebar_button_tab).id} options={SETTINGS_TABS.map((t) => ({ value: t.id, label: t.label, icon: <t.Icon size={13} strokeWidth={1.7} aria-hidden="true" /> }))} onChange={(v) => void saveSettings({ sidebar_button_tab: v })} style={{ width: 176, height: 28, padding: "0 8px", flex: "0 0 176px" }} /></div>
      </div>
      <div className="settings-section-label">Needs you</div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Show Needs you inbox</div><div className="desc">Show pending approvals and questions from all chats in the sidebar.</div></div>
          <Switch label="Show Needs you inbox" checked={s.needs_you ?? false} onChange={(checked) => void saveSettings({ needs_you: checked })} /></div>
      </div>
      <div className="settings-section-label">Pull requests</div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Show Pull requests</div><div className="desc">Add a GitHub pull-request screen beside Saved prompts. Shows this project's PRs only. Off by default.</div></div>
          <Switch label="Show Pull requests" checked={s.show_pull_requests ?? false} onChange={(checked) => void saveSettings({ show_pull_requests: checked })} /></div>
      </div>
      <div className="settings-section-label">Schedules</div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Show Schedules</div><div className="desc">Add a Schedules screen to the sidebar for recurring agent runs. Off by default. Hiding it does not pause schedules — enabled schedules keep running; pause them from the Schedules screen.</div></div>
          <Switch label="Show Schedules" checked={s.show_schedules ?? false} onChange={(checked) => void saveSettings({ show_schedules: checked })} /></div>
      </div>
      <div className="settings-section-label">Saved prompts</div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Sort order</div><div className="desc">How the Saved prompts list is ordered. The Sort control on that screen appears while chats from all projects are shown.</div></div>
          <Dropdown value={savedSortOf(s.saved_sort)} options={SAVED_SORT_OPTIONS} onChange={(v) => void saveSettings({ saved_sort: v })} search={false} placeholder="Sort saved prompts" style={{ width: 232, height: 28, padding: "0 8px", flex: "0 0 232px" }} /></div>
      </div>
    </>
  );
}

// Filtered once at module load, so `i` indexes the visible list only and the
// staggered nav animation stays gapless. A page listed as hidden keeps its
// branch in the render switch below and simply gets no nav button — the
// "Shortcut button" choice in Tabs still offers it. See
// `settingsTabs.ts` for the list itself.
const VISIBLE_TABS = SETTINGS_TABS.filter((t) => !HIDDEN_SETTINGS_TABS.includes(t.id));

export function SettingsView() {
  const tab = useStore((s) => s.settingsTab);
  const ok = useStore((s) => !!s.settings);
  // On a phone the nav is a strip that scrolls sideways (App.css), so a page
  // opened from elsewhere (the titlebar's Remote chip) brings its tab into view.
  useEffect(() => { if (isNarrow()) document.querySelector(".stabs .stab.on")?.scrollIntoView?.({ block: "nearest", inline: "nearest" }); }, [tab, ok]);
  if (!ok) return null;
  return (
    <div className="settings-view">
      <div className="stabs">
        <div className="stabs-title">Settings</div>
        <div role="tablist" aria-label="Settings pages" className="stabs-list">
          {VISIBLE_TABS.map(({ id, label, Icon }, i) => <NavTab key={id} style={{ animationDelay: i * 25 + "ms" }} selected={tab === id} onClick={() => set({ settingsTab: id })}><Icon aria-hidden="true" size={14} strokeWidth={1.7} />{label}</NavTab>)}
        </div>
      </div>
      <div className="settings-content">
        <div key={tab} className="settings-page" style={{ maxWidth: 680, display: "flex", flexDirection: "column", gap: 16 }}>
          {tab === "general" && <General />}
          {tab === "theme" && <ThemeTab />}
          {tab === "models" && <ModelsTab />}
          {tab === "accounts" && <><AccountsTab /><BurnerSection /></>}
          {tab === "routing" && <RoutersTab />}
          {tab === "agents" && <AgentsTab />}
          {tab === "skills" && <SkillsTab />}
          {tab === "commands" && <CommandsTab />}
          {tab === "chats" && <ChatsTab />}
          {tab === "tabs" && <TabsTab />}
          {tab === "workspaces" && <WorkspacesTab />}
          {tab === "perms" && <Perms />}
          {tab === "plugins" && <PluginsTab />}
          {tab === "connectors" && <ConnectorsTab />}
          {tab === "checks" && <ChecksTab />}
          {tab === "mcp" && <McpServers />}
          {tab === "remote" && <RemoteTab />}
          {tab === "usage" && <StatsTab />}
        </div>
      </div>
    </div>
  );
}
