import { useEffect, useId, useState } from "react";
import { MessageCircle } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { PERMS } from "../api";
import { connectorsApi, patchOf, splitIds, type DiscordPatch, type DiscordStatus } from "../connectors";
import { flash, useStore } from "../store";
import { copyText } from "./clipboard";
import { Button, Dropdown, RemovableChip, Switch } from "./primitives";
import { ChatConnectorHeader, connectorPill } from "./ChatConnectorHeader";

const PORTAL = "https://discord.com/developers/applications";

/** One line saying what the bot is doing right now. */
export function discordSummary(s: DiscordStatus | null): string {
  if (!s) return "Checking…";
  if (!s.enabled) return s.has_token ? "Off" : "Not set up";
  switch (s.state) {
    case "connected": return `Connected as @${s.bot_name || "bot"} · ${s.servers} server${s.servers === 1 ? "" : "s"} · ${s.chats} chat${s.chats === 1 ? "" : "s"}`;
    case "connecting": return "Connecting…";
    case "reconnecting": return `Reconnecting…${s.error ? ` ${s.error}` : ""}`;
    case "incomplete": return s.error || "Setup isn't finished";
    case "error": return s.error || "Stopped with an error";
    default: return "Off";
  }
}

/** A list of Discord ids as removable chips plus a box to paste more. */
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

/**
 * Settings → Connectors → Discord. Status is polled from the backend rather
 * than kept here: the bot's connection changes on its own (reconnects, a
 * revoked token), and the form must never show a state the backend left.
 * Collapsed on open: the header row's pill and summary carry the state, so
 * three platforms fit on the page without scrolling past setup steps.
 */
export function DiscordConnector() {
  const settings = useStore((s) => s.settings);
  const models = useStore((s) => s.models);
  const [st, setSt] = useState<DiscordStatus | null>(null);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [open, setOpen] = useState(false);
  const detailsId = useId();

  useEffect(() => {
    let live = true;
    const tick = () => connectorsApi.discordStatus().then((s) => {
      if (live) setSt(s);
    }).catch(() => {});
    void tick();
    const t = setInterval(tick, 2000);
    return () => { live = false; clearInterval(t); };
  }, []);

  const run = async (f: () => Promise<DiscordStatus>): Promise<boolean> => {
    setBusy(true); setErr("");
    try { setSt(await f()); return true; }
    catch (e) { setErr(String(e)); return false; }
    finally { setBusy(false); }
  };
  const configure = (patch: Partial<DiscordPatch>) => st ? run(() => connectorsApi.discordConfigure({ ...patchOf(st), ...patch })) : Promise.resolve(false);
  const saveToken = async (t: string) => { if (await run(() => connectorsApi.discordSetToken(t))) { setToken(""); flash(t ? "Bot token saved" : "Bot token removed"); } };

  const projects = settings?.projects ?? [];
  const projectOpts = [{ value: "", label: "The project open in the app" }, ...projects.map((p) => ({ value: p, label: p }))];
  if (st?.project && !projects.includes(st.project)) projectOpts.push({ value: st.project, label: st.project });
  const modelOpts = [{ value: "", label: "Most recently used model" }, ...models.filter((m) => m.enabled).map((m) => ({ value: m.id, label: m.name || m.id, hint: m.provider }))];
  const perm = PERMS.find((p) => p.id === (st?.perm ?? "auto"));

  return <div className="sgroup">
    <ChatConnectorHeader icon={MessageCircle} name="Discord" pill={connectorPill(st, !!st?.has_token)} summary={discordSummary(st)}
      open={open} onToggle={() => setOpen(!open)} controls={detailsId} enabled={!!st?.enabled} onEnable={(enabled) => void configure({ enabled })} />
    <div id={detailsId} hidden={!open}>{open && st && <>
      <div className="srowx desc" style={{ display: "block" }}>
        Message your agent from Discord: each channel, thread or DM becomes a chat on this computer, and replies, approvals and questions come back to you there. The app has to be running (close to tray keeps it going).
        <ol style={{ margin: "8px 0 0", paddingLeft: 18, lineHeight: 1.6 }}>
          <li>In the Developer Portal, create an application, open <b>Bot</b> and press <b>Reset Token</b>.</li>
          <li>On the same page turn on <b>Message Content Intent</b>.</li>
          <li>Paste the token below and add your own Discord user id (Discord → Settings → Advanced → Developer Mode, then right-click your name → Copy User ID).</li>
          <li>Switch the bot on, then DM it, or invite it to a server with the link that appears.</li>
        </ol>
      </div>
      <div className="srowx"><Button onClick={() => void openUrl(PORTAL).catch((e) => flash(String(e)))}>Open Developer Portal</Button>
        {st.invite_url && <><Button onClick={() => void openUrl(st.invite_url).catch((e) => flash(String(e)))}>Invite to a server</Button><Button variant="ghost" onClick={() => copyText(st.invite_url)}>Copy invite link</Button></>}
      </div>
      <div className="srowx">
        <input aria-label="Discord bot token" className="input mono" type="password" autoComplete="off" spellCheck={false} style={{ flex: 1, minWidth: 0 }} value={token} disabled={busy}
          placeholder={st.has_token ? "Token saved · paste a new one to replace it" : "Bot token from Developer Portal → Bot"}
          onChange={(e) => setToken(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter" && token.trim()) void saveToken(token.trim()); }} />
        <Button disabled={busy || !token.trim()} onClick={() => void saveToken(token.trim())}>Save token</Button>
        {st.has_token && <Button variant="ghost" disabled={busy} onClick={() => void saveToken("")}>Remove</Button>}
      </div>
      <IdList label="Allowed users" required busy={busy} ids={st.allowed_users} onChange={(allowed_users) => configure({ allowed_users })}
        hint="Only these Discord accounts can talk to the agent. Everyone else is ignored, and nobody gets in until you add someone. Anyone listed can make the agent edit files and run commands here, within the permission mode below." />
      <IdList label="Servers" busy={busy} ids={st.allowed_guilds} onChange={(allowed_guilds) => configure({ allowed_guilds })}
        hint="Optional. Answer only in these servers (right-click the server icon → Copy Server ID). Empty: any server the bot is in." />
      <IdList label="Channels" busy={busy} ids={st.allowed_channels} onChange={(allowed_channels) => configure({ allowed_channels })}
        hint="Optional. Answer only in these channels; threads inside them count too. Empty: any channel in an allowed server." />
      <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Direct messages</div><div className="desc">Allowed users can DM the bot.</div></div>
        <Switch label="Allow direct messages" checked={st.allow_dms} onChange={(allow_dms) => void configure({ allow_dms })} /></div>
      <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Only when mentioned in servers</div><div className="desc">In a server channel, answer only messages that @mention the bot or reply to it. DMs never need a mention.</div></div>
        <Switch label="Only when mentioned in servers" checked={st.require_mention} onChange={(require_mention) => void configure({ require_mention })} /></div>
      <div className="srowx"><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Project</div><div className="desc">The folder new Discord chats work in.</div></div>
        <Dropdown value={st.project} options={projectOpts} onChange={(project) => void configure({ project })} style={{ width: 240, height: 28, padding: "0 8px", flex: "0 0 240px" }} /></div>
      <div className="srowx"><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Model</div></div>
        <Dropdown value={st.model} options={modelOpts} onChange={(model) => void configure({ model })} style={{ width: 240, height: 28, padding: "0 8px", flex: "0 0 240px" }} /></div>
      <div className="srowx"><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Permissions</div><div className="desc">{perm?.desc} Approvals are sent to you in Discord; reply yes, always or no.</div></div>
        <Dropdown value={st.perm} search={false} options={PERMS.map((p) => ({ value: p.id, label: p.name }))} onChange={(p) => void configure({ perm: p })} style={{ width: 160, height: 28, padding: "0 8px", flex: "0 0 160px" }} /></div>
      {st.perm === "turbo" && <div className="srowx desc" role="note" style={{ color: "var(--red)" }}>Full Access runs anything a Discord message asks for without checking with you. Use it only if your Discord account is well protected (two-factor on).</div>}
      <div className="srowx desc" style={{ display: "block" }}>In Discord: <span className="mono">/new</span> starts a fresh chat, <span className="mono">/stop</span> stops the agent, <span className="mono">/status</span> shows which chat you're in. Discord chats also appear in your sidebar here.</div>
      {st.last_ignored && <div className="srowx desc" role="status">{st.last_ignored}</div>}
      {st.chats > 0 && <div className="srowx"><div className="desc" style={{ flex: 1 }}>{st.chats} Discord conversation{st.chats === 1 ? " is" : "s are"} linked to a chat.</div><Button variant="ghost" disabled={busy} onClick={() => void run(connectorsApi.discordForgetChats)}>Start all fresh</Button></div>}
    </>}</div>
    {err && <div className="srowx desc" role="alert" style={{ color: "var(--red)" }}>{err}</div>}
  </div>;
}
