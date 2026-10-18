import { useEffect, useId, useState } from "react";
import { Hash } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { PERMS } from "../api";
import { connectorsApi, slackPatchOf, splitIds, type SlackPatch, type SlackStatus, type SlackTokenKind } from "../connectors";
import { flash, useStore } from "../store";
import { copyText } from "./clipboard";
import { Button, Dropdown, RemovableChip, Switch } from "./primitives";
import { ChatConnectorHeader, connectorPill } from "./ChatConnectorHeader";

const APPS = "https://api.slack.com/apps";

/**
 * The app manifest for "Create New App → From a manifest". Scopes are the
 * minimum the connector uses, each for one reason:
 * - app_mentions:read, channels/groups/im/mpim:history: receive mentions and
 *   messages (the matching `message.*` events need the history scopes);
 * - chat:write: post replies;
 * - im:read, channels:read, groups:read: name the DM or channel for the agent
 *   (`conversations.info`);
 * - users:read: the author's display name (`users.info`).
 * No files, no admin, no user scopes, and no slash commands: Slack would
 * swallow `/new` before the bot saw it, which is why the commands are `!new`.
 * Token rotation stays off: a rotating token would need a refresh flow.
 */
export const SLACK_MANIFEST = JSON.stringify({
  display_information: { name: "OpenLeash", description: "Talk to your OpenLeash agent from Slack." },
  features: {
    app_home: { messages_tab_enabled: true, messages_tab_read_only_enabled: false },
    bot_user: { display_name: "OpenLeash", always_online: true },
  },
  oauth_config: {
    scopes: {
      bot: ["app_mentions:read", "chat:write", "channels:history", "groups:history", "im:history", "mpim:history", "im:read", "channels:read", "groups:read", "users:read"],
    },
  },
  settings: {
    event_subscriptions: { bot_events: ["app_mention", "message.im", "message.channels", "message.groups", "message.mpim"] },
    interactivity: { is_enabled: false },
    org_deploy_enabled: false,
    socket_mode_enabled: true,
    token_rotation_enabled: false,
  },
}, null, 2);

/** One line saying what the bot is doing right now. */
export function slackSummary(s: SlackStatus | null): string {
  if (!s) return "Checking…";
  if (!s.enabled) return s.has_app_token || s.has_bot_token ? "Off" : "Not set up";
  switch (s.state) {
    case "connected": return `Connected as @${s.bot_name || "bot"}${s.team_name ? ` in ${s.team_name}` : ""} · ${s.chats} chat${s.chats === 1 ? "" : "s"}`;
    case "connecting": return "Connecting…";
    case "reconnecting": return `Reconnecting…${s.error ? ` ${s.error}` : ""}`;
    case "incomplete": return s.error || "Setup isn't finished";
    case "error": return s.error || "Stopped with an error";
    default: return "Off";
  }
}

/** A list of Slack ids as removable chips plus a box to paste more. */
function IdList({ label, hint, ids, onChange, required = false, busy }: { label: string; hint: string; ids: string[]; onChange: (next: string[]) => Promise<boolean>; required?: boolean; busy: boolean }) {
  const [draft, setDraft] = useState("");
  const id = useId();
  const add = async () => {
    const more = splitIds(draft);
    if (!more.length) return;
    if (await onChange([...ids, ...more.filter((x) => !ids.includes(x))])) setDraft("");
  };
  return <div className="srowx" style={{ alignItems: "flex-start", flexDirection: "column", gap: 6 }}>
    <div style={{ width: "100%" }}>
      <label htmlFor={id} style={{ fontWeight: 500 }}>{label}{required && <span aria-hidden="true" style={{ color: "var(--red)" }}> *</span>}</label>
      <div className="desc">{hint}</div>
    </div>
    {ids.length > 0 && <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
      {ids.map((x) => <RemovableChip key={x} label={`${label} ${x}`} onRemove={() => void onChange(ids.filter((y) => y !== x))}><span className="mono">{x}</span></RemovableChip>)}
    </div>}
    <div style={{ display: "flex", gap: 8, width: "100%" }}>
      <input id={id} className="input mono" style={{ flex: 1, minWidth: 0 }} value={draft} disabled={busy} placeholder="Paste one or more ids" onChange={(e) => setDraft(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void add(); } }} />
      <Button disabled={busy || !draft.trim()} onClick={() => void add()}>Add</Button>
    </div>
  </div>;
}

/** A write-only token box: the saved value never comes back, only whether one is set. */
function TokenBox({ kind, label, saved, placeholder, busy, onSave }: { kind: SlackTokenKind; label: string; saved: boolean; placeholder: string; busy: boolean; onSave: (kind: SlackTokenKind, token: string) => Promise<boolean> }) {
  const [value, setValue] = useState("");
  const save = async (t: string) => { if (await onSave(kind, t)) setValue(""); };
  return <div className="srowx">
    <input aria-label={label} className="input mono" type="password" autoComplete="off" spellCheck={false} style={{ flex: 1, minWidth: 0 }} value={value} disabled={busy}
      placeholder={saved ? "Token saved · paste a new one to replace it" : placeholder}
      onChange={(e) => setValue(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter" && value.trim()) void save(value.trim()); }} />
    <Button aria-label={`Save ${label}`} disabled={busy || !value.trim()} onClick={() => void save(value.trim())}>Save</Button>
    {saved && <Button aria-label={`Remove ${label}`} variant="ghost" disabled={busy} onClick={() => void save("")}>Remove</Button>}
  </div>;
}

/**
 * Settings → Connectors → Slack. Status is polled from the backend rather
 * than kept here: the connection changes on its own (Slack refreshes it every
 * few hours, a token gets revoked), and the form must never show a state the
 * backend left. Collapsed on open; the header row's pill carries the state.
 */
export function SlackConnector() {
  const settings = useStore((s) => s.settings);
  const models = useStore((s) => s.models);
  const [st, setSt] = useState<SlackStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [tested, setTested] = useState("");
  const [open, setOpen] = useState(false);
  const detailsId = useId();

  useEffect(() => {
    let live = true;
    const tick = () => connectorsApi.slackStatus().then((s) => {
      if (live) setSt(s);
    }).catch(() => {});
    void tick();
    const t = setInterval(tick, 2000);
    return () => { live = false; clearInterval(t); };
  }, []);

  const run = async (f: () => Promise<SlackStatus>): Promise<boolean> => {
    setBusy(true); setErr("");
    try { setSt(await f()); return true; }
    catch (e) { setErr(String(e)); return false; }
    finally { setBusy(false); }
  };
  const configure = (patch: Partial<SlackPatch>) => st ? run(() => connectorsApi.slackConfigure({ ...slackPatchOf(st), ...patch })) : Promise.resolve(false);
  const saveToken = async (kind: SlackTokenKind, t: string) => {
    const ok = await run(() => connectorsApi.slackSetToken(kind, t));
    if (ok) { setTested(""); flash(`${kind === "app" ? "App-level" : "Bot"} token ${t ? "saved" : "removed"}`); }
    return ok;
  };
  const test = async () => {
    setBusy(true); setErr(""); setTested("");
    try { setTested(await connectorsApi.slackTest()); }
    catch (e) { setErr(String(e)); }
    finally { setBusy(false); }
  };

  const projects = settings?.projects ?? [];
  const projectOpts = [{ value: "", label: "The project open in the app" }, ...projects.map((p) => ({ value: p, label: p }))];
  if (st?.project && !projects.includes(st.project)) projectOpts.push({ value: st.project, label: st.project });
  const modelOpts = [{ value: "", label: "Most recently used model" }, ...models.filter((m) => m.enabled).map((m) => ({ value: m.id, label: m.name || m.id, hint: m.provider }))];
  const perm = PERMS.find((p) => p.id === (st?.perm ?? "auto"));

  return <div className="sgroup">
    <ChatConnectorHeader icon={Hash} name="Slack" pill={connectorPill(st, !!st && (st.has_app_token || st.has_bot_token))} summary={slackSummary(st)}
      open={open} onToggle={() => setOpen(!open)} controls={detailsId} enabled={!!st?.enabled} onEnable={(enabled) => void configure({ enabled })} />
    <div id={detailsId} hidden={!open}>{open && st && <>
      <div className="srowx desc" style={{ display: "block" }}>
        Message your agent from Slack: each DM or channel thread becomes a chat on this computer, and replies, approvals and questions come back to you there. It uses Socket Mode, so nothing needs a public address, but the app has to be running (close to tray keeps it going).
        <ol style={{ margin: "8px 0 0", paddingLeft: 18, lineHeight: 1.6 }}>
          <li>Open Slack apps, press <b>Create New App → From a manifest</b>, pick your workspace and paste the manifest below.</li>
          <li><b>Install to Workspace</b>, then copy the <b>Bot User OAuth Token</b> (<span className="mono">xoxb-…</span>) from OAuth &amp; Permissions.</li>
          <li>In <b>Basic Information → App-Level Tokens</b>, generate a token with the <span className="mono">connections:write</span> scope (<span className="mono">xapp-…</span>).</li>
          <li>Paste both below and add your own member id (your profile → ⋮ → <b>Copy member ID</b>).</li>
          <li>Switch the bot on and DM it, or invite it to a channel with <span className="mono">/invite @OpenLeash</span> and @mention it.</li>
        </ol>
      </div>
      <div className="srowx">
        <Button onClick={() => void openUrl(APPS).catch((e) => flash(String(e)))}>Open Slack apps</Button>
        <Button variant="ghost" onClick={() => copyText(SLACK_MANIFEST, "Manifest copied")}>Copy manifest</Button>
      </div>
      <div className="srowx" style={{ display: "block" }}>
        <pre aria-label="Slack app manifest" className="mono desc" style={{ margin: 0, maxHeight: 160, overflow: "auto", whiteSpace: "pre", fontSize: 11 }}>{SLACK_MANIFEST}</pre>
      </div>
      <TokenBox kind="app" label="Slack app-level token" saved={st.has_app_token} placeholder="App-level token (xapp-…) from Basic Information" busy={busy} onSave={saveToken} />
      <TokenBox kind="bot" label="Slack bot token" saved={st.has_bot_token} placeholder="Bot token (xoxb-…) from OAuth & Permissions" busy={busy} onSave={saveToken} />
      <div className="srowx">
        <Button disabled={busy || !st.has_app_token || !st.has_bot_token} onClick={() => void test()}>Test tokens</Button>
        {tested && <div className="desc" role="status" style={{ flex: 1 }}>{tested}</div>}
      </div>
      <IdList label="Allowed users" required busy={busy} ids={st.allowed_users} onChange={(allowed_users) => configure({ allowed_users })}
        hint="Only these Slack members can talk to the agent. Everyone else is ignored, and nobody gets in until you add someone. Anyone listed can make the agent edit files and run commands here, within the permission mode below." />
      <IdList label="Workspaces" busy={busy} ids={st.allowed_teams} onChange={(allowed_teams) => configure({ allowed_teams })}
        hint="Optional. Answer only in these workspaces (ids start with T). Empty: any workspace the app is installed in." />
      <IdList label="Channels" busy={busy} ids={st.allowed_channels} onChange={(allowed_channels) => configure({ allowed_channels })}
        hint="Optional. Answer only in these channels (channel name → About → Channel ID). Empty: any channel the bot was invited to." />
      <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Direct messages</div><div className="desc">Allowed users can DM the bot.</div></div>
        <Switch label="Allow direct messages" checked={st.allow_dms} onChange={(allow_dms) => void configure({ allow_dms })} /></div>
      <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Only when mentioned in channels</div><div className="desc">In a channel, answer only messages that @mention the bot. Replies in a thread the bot is already in don't need one. DMs never do.</div></div>
        <Switch label="Only when mentioned in channels" checked={st.require_mention} onChange={(require_mention) => void configure({ require_mention })} /></div>
      <div className="srowx"><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Project</div><div className="desc">The folder new Slack chats work in.</div></div>
        <Dropdown value={st.project} options={projectOpts} onChange={(project) => void configure({ project })} style={{ width: 240, height: 28, padding: "0 8px", flex: "0 0 240px" }} /></div>
      <div className="srowx"><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Model</div></div>
        <Dropdown value={st.model} options={modelOpts} onChange={(model) => void configure({ model })} style={{ width: 240, height: 28, padding: "0 8px", flex: "0 0 240px" }} /></div>
      <div className="srowx"><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Permissions</div><div className="desc">{perm?.desc} Approvals are sent to you in Slack; reply yes, always or no.</div></div>
        <Dropdown value={st.perm} search={false} options={PERMS.map((p) => ({ value: p.id, label: p.name }))} onChange={(p) => void configure({ perm: p })} style={{ width: 160, height: 28, padding: "0 8px", flex: "0 0 160px" }} /></div>
      {st.perm === "turbo" && <div className="srowx desc" role="note" style={{ color: "var(--red)" }}>Full Access runs anything a Slack message asks for without checking with you. Use it only if your Slack account is well protected (two-factor on).</div>}
      <div className="srowx desc" style={{ display: "block" }}>In Slack, messages starting with <span className="mono">/</span> are taken by Slack as its own commands, so use <span className="mono">!new</span> to start a fresh chat, <span className="mono">!stop</span> to stop the agent, <span className="mono">!status</span> to see which chat you're in and <span className="mono">!help</span> for the list. Slack chats also appear in your sidebar here.</div>
      {st.last_ignored && <div className="srowx desc" role="status">{st.last_ignored}</div>}
      {st.chats > 0 && <div className="srowx"><div className="desc" style={{ flex: 1 }}>{st.chats} Slack conversation{st.chats === 1 ? " is" : "s are"} linked to a chat.</div><Button variant="ghost" disabled={busy} onClick={() => void run(connectorsApi.slackForgetChats)}>Start all fresh</Button></div>}
    </>}</div>
    {err && <div className="srowx desc" role="alert" style={{ color: "var(--red)" }}>{err}</div>}
  </div>;
}
