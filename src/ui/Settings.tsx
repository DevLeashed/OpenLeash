import { useState } from "react";
import { DENY_PREFIX, isDenyRule, PERMS, type CustomCommand } from "../api";
import { saveSettings, set, useStore } from "../store";
import { ago } from "../api";
import { ARCHIVE_AGES, ageLabel, archiveStale, archiveTask, DEFAULT_ARCHIVE_AGE, deleteArchivedTasks, deleteTask, exportTasks, importTasks, staleChats } from "./Chrome";
import { AccountsTab, AgentsTab, RoutingTab } from "./Accounts";
import { ModelsTab } from "./ModelManager";
import { SkillsTab } from "./Skills";
import { StatsTab } from "./Stats";
import { AppVersion } from "./AppVersion";
import { ConnectorsTab, PluginsTab } from "./Plugins";
import { McpServers } from "./McpServers";
import { ChecksTab } from "./Hooks";
import { SLASH } from "./Composer";
import { stepZoom, DEFAULT_ZOOM, ZOOMS } from "../store";
import { Archive, Bot, Cable, ChartNoAxesColumn, KeyRound, ListChecks, Minus, Network, Palette, Plus, Puzzle, Settings, Sparkles, SlidersHorizontal, Trash2, UserRound, Terminal } from "lucide-react";
import { Dropdown, Button, ChoiceRow, IconButton, Input, NavTab, Pressable, Segmented, Switch, MorphText } from "./primitives";

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
  return (
    <>
      <div className="settings-head"><div className="settings-head-main"><div className="settings-title">Theme</div><div className="settings-lead">Pick the accent palette for the whole app.</div></div></div>
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

function General() {
  const s = useStore((st) => st.settings)!;
  const dataDir = useStore((st) => st.dataDir);
  const [copied, setCopied] = useState(false);
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
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Show Needs you inbox</div><div className="desc">Show pending approvals and questions from all chats in the sidebar.</div></div>
          <Switch label="Show Needs you inbox" checked={s.needs_you ?? false} onChange={(checked) => void saveSettings({ needs_you: checked })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Notifications</div><div className="desc">When a chat finishes, fails or needs you. A desktop popup if the window is in the background, an in-app notice if you are looking at it.</div></div>
          <Switch label="Desktop notifications" checked={s.notify !== false} onChange={(checked) => void saveSettings({ notify: checked })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Send message with</div></div>
          <Segmented label="Send message with" options={[{ value: "enter", label: "Enter" }, { value: "ctrl-enter", label: "Ctrl Enter" }]} value={s.send_with} onChange={(v) => void saveSettings({ send_with: v })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Show thinking</div><div className="desc">Show the model's internal reasoning blocks in chats.</div></div>
          <Switch label="Show thinking" checked={s.show_thinking ?? false} onChange={(checked) => void saveSettings({ show_thinking: checked })} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>New tasks use a worktree</div><div className="desc">Separate branch and checkout per task.</div></div>
          <Switch label="New tasks use a worktree" checked={s.worktree} onChange={(checked) => { void saveSettings({ worktree: checked }); set((st) => ({ home: { ...st.home, worktree: checked } })); }} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Assist mode for new chats</div><div className="desc">How often the agent asks for guidance.</div></div>
          <Segmented label="Assist mode for new chats" options={[{ value: "guide", label: "Guide" }, { value: "default", label: "Default" }, { value: "necessary", label: "Necessary" }]} value={s.assist} onChange={(v) => { void saveSettings({ assist: v }); set((st) => ({ home: { ...st.home, assist: v } })); }} /></div>
      </div>
      <div className="settings-section-label">Data folder</div>
      <div className="sgroup">
        <div className="srowx">
          <div style={{ flex: 1, minWidth: 0 }}><div className="mono sel" style={{ fontSize: 12 }}>{dataDir}</div><div className="desc">Set OPENLEASH_HOME to move this folder.</div></div>
          <Button onClick={() => { navigator.clipboard?.writeText(dataDir); setCopied(true); window.setTimeout(() => setCopied(false), 1200); }}><MorphText>{copied ? "Copied" : "Copy"}</MorphText></Button>
        </div>
      </div>
      <div className="settings-section-label">Project instructions</div>
      <div className="settings-note">Reads <span className="mono">OPENLEASH.md</span>, <span className="mono">AGENTS.md</span> or <span className="mono">CLAUDE.md</span> from the project, plus <span className="mono">~/.openleash/OPENLEASH.md</span>. Use <span className="mono">/init</span> to create one.</div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Inject global CLAUDE.md</div><div className="desc">Also load <span className="mono">~/.claude/CLAUDE.md</span> into every agent.</div></div>
          <Switch label="Inject global CLAUDE.md" checked={s.inject_global_claude ?? false} onChange={(checked) => void saveSettings({ inject_global_claude: checked })} /></div>
      </div>
      <div className="settings-section-label">Memory</div>
      <div className="sgroup">
        <div className="srowx" style={{ alignItems: "flex-start" }}>
          <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Let the agent remember things</div><div className="desc">Keeps notes per project in <span className="mono">~/.openleash/memory/</span> and reads them back in later chats, so your preferences and corrections survive between sessions. Plain markdown — read, edit or delete any of it by hand.</div></div>
          <Switch label="Let the agent remember things" checked={s.memory ?? false} onChange={(checked) => void saveSettings({ memory: checked })} />
        </div>
      </div>
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
      <div className="settings-note">Read-only commands stay available. Plan mode requires approval for edits. A deny rule always wins over every allow rule and over every permission level, including Full Access. An allow rule never covers a chained command — one with a second command, a redirect or a substitution — so it still asks; a deny rule does reach into one.</div>
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
      <div className="settings-note">{s.deny_env_files === false
        ? <><span className="mono">.env</span> files are readable by <span className="mono">read_file</span> — the built-in deny on them is disabled.</>
        : <><span className="mono">.env</span> files are denied for <span className="mono">read_file</span> by default (<span className="mono">*.env</span>, <span className="mono">*.env.*</span>; <span className="mono">*.env.example</span> / <span className="mono">.sample</span> / <span className="mono">.template</span> / <span className="mono">.dist</span> stay readable). To allow one, add an allow rule such as <span className="mono">read_file .env</span>.</>}
      </div>
    </>
  );
}


function ChatsTab() {
  const tasks = useStore((st) => st.tasks);
  const s = useStore((st) => st.settings)!;
  const [arm, setArm] = useState<string | null>(null);
  const [removeWorktree, setRemoveWorktree] = useState<string | null>(null);
  const [armAll, setArmAll] = useState(false);
  const [armStale, setArmStale] = useState(false);
  // An unset (or nonsensical) saved value falls back to the default rather than
  // archiving everything on the next press: the sweep only ever runs from a
  // button the user just clicked, so the window has to be what the row shows.
  const current = useStore((st) => st.view === "session" || st.view === "diff" ? st.task : null);
  const unread = useStore((st) => st.unread);
  const archiveAfter = ARCHIVE_AGES.includes(s.archive_after_days as (typeof ARCHIVE_AGES)[number]) ? s.archive_after_days! : DEFAULT_ARCHIVE_AGE;
  const ageOptions = ARCHIVE_AGES.map((d) => ({ value: String(d), label: ageLabel(d) }));
  const all = Object.values(tasks);
  const archived = all.filter((t) => t.archived).sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  const stale = staleChats(tasks, archiveAfter, Date.now(), unread, current, s.paused_all);
  return (
    <>
      <div className="settings-head"><div className="settings-head-main settings-title">Chats</div></div>
      <div className="sgroup">
        <div className="srowx" style={{ alignItems: "flex-start" }}>
          <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Let the agent name chats</div><div className="desc">The agent names a chat once the job is clear, and renames it when the work moves on. Titles you set yourself are never renamed — use a chat's right-click menu to hand a name back.</div></div>
          <Switch label="Let the agent name chats" checked={s.agent_titles ?? true} onChange={(checked) => void saveSettings({ agent_titles: checked })} />
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
      <div className="settings-section-label">Archive by age</div>
      <div className="sgroup">
        <div className="srowx" style={{ alignItems: "flex-start" }}>
          <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Archive chats you have not used in</div><div className="desc">Puts the sidebar's quiet tail away in one go, keeping them readable under <span className="mono">Archived</span> below — nothing is deleted. Measured from the last message you sent, not the last agent step, and a chat with a running or waiting agent is always left alone.</div></div>
          <Dropdown value={String(archiveAfter)} options={ageOptions} onChange={(d) => void saveSettings({ archive_after_days: Number(d) })} search={false} style={{ width: 132, height: 26, flex: "0 0 132px" }} />
        </div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Archive idle chats now</div><div className="desc">{stale.length ? `${stale.length} chat${stale.length === 1 ? "" : "s"} idle for over ${ageLabel(archiveAfter).toLowerCase()}` : "Every chat has been used recently"}</div></div>
          {armStale
            ? <Button variant="primary" style={{ height: 26, background: "#ff8a8a" }} onClick={() => { setArmStale(false); void archiveStale(archiveAfter); }}>Archive {stale.length} chat{stale.length === 1 ? "" : "s"}</Button>
            : <Button variant="ghost" disabled={!stale.length} style={{ height: 26 }} onClick={() => setArmStale(true)}>Archive now…</Button>}</div>
      </div>
      <div className="settings-section-label">Archived · {archived.length}</div>
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

const TABS = [
  ["general", "General", Settings], ["theme", "Theme", Palette], ["models", "Models", SlidersHorizontal], ["accounts", "Accounts", UserRound],
  ["routing", "Routing", Network], ["agents", "Subagents", Bot], ["skills", "Skills", Sparkles], ["commands", "Commands", Terminal], ["chats", "Chats", Archive],
  ["perms", "Permissions", KeyRound], ["plugins", "Plugins", Puzzle], ["connectors", "Connectors", Cable], ["checks", "Checks & hooks", ListChecks],
  ["mcp", "MCP servers", Cable], ["usage", "Stats", ChartNoAxesColumn],
] as const;

// Ids listed here keep their entry in TABS *and* their branch in the render
// switch below; only the nav button is dropped. So a hidden tab stays reachable
// from anywhere that sets settingsTab directly (the palette's "Routing and
// fallbacks", the route menu's "Edit routes…"), the page keeps working for
// anyone who already had it selected, and unhiding is deleting one string here
// rather than rewiring every entry point back.
// Filtered once at module load: TABS is a literal, so the icon component types
// stay narrow, and `i` indexes the visible list only, keeping the staggered nav
// animation gapless.
const HIDDEN_TABS = ["routing"] as const;
const VISIBLE_TABS = TABS.filter(([id]) => !HIDDEN_TABS.includes(id as (typeof HIDDEN_TABS)[number]));

export function SettingsView() {
  const tab = useStore((s) => s.settingsTab);
  const ok = useStore((s) => !!s.settings);
  if (!ok) return null;
  return (
    <div style={{ flex: 1, minHeight: 0, display: "flex" }}>
      <div className="stabs">
        <div className="stabs-title">Settings</div>
        <div role="tablist" aria-label="Settings pages" style={{ display: "flex", flexDirection: "column", gap: 2 }}>
          {VISIBLE_TABS.map(([id, l, Icon], i) => <NavTab key={id} style={{ animationDelay: i * 25 + "ms" }} selected={tab === id} onClick={() => set({ settingsTab: id })}><Icon aria-hidden="true" size={14} strokeWidth={1.7} />{l}</NavTab>)}
        </div>
      </div>
      <div style={{ flex: 1, overflow: "auto", padding: "24px 32px" }}>
        <div key={tab} className="settings-page" style={{ maxWidth: 680, display: "flex", flexDirection: "column", gap: 16 }}>
          {tab === "general" && <General />}
          {tab === "theme" && <ThemeTab />}
          {tab === "models" && <ModelsTab />}
          {tab === "accounts" && <AccountsTab />}
          {tab === "routing" && <RoutingTab />}
          {tab === "agents" && <AgentsTab />}
          {tab === "skills" && <SkillsTab />}
          {tab === "commands" && <CommandsTab />}
          {tab === "chats" && <ChatsTab />}
          {tab === "perms" && <Perms />}
          {tab === "plugins" && <PluginsTab />}
          {tab === "connectors" && <ConnectorsTab />}
          {tab === "checks" && <ChecksTab />}
          {tab === "mcp" && <McpServers />}
          {tab === "usage" && <StatsTab />}
        </div>
      </div>
    </div>
  );
}
