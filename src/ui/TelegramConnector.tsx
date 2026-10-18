import { useEffect, useId, useState } from "react";
import { Send } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { PERMS } from "../api";
import { connectorsApi, splitIds, telegramPatchOf, type TelegramPatch, type TelegramStatus } from "../connectors";
import { flash, useStore } from "../store";
import { Button, Dropdown, RemovableChip, Switch } from "./primitives";
import { ChatConnectorHeader, connectorPill } from "./ChatConnectorHeader";

const BOTFATHER = "https://t.me/BotFather";

/** One line saying what the bot is doing right now. */
export function telegramSummary(s: TelegramStatus | null): string {
  if (!s) return "Checking…";
  if (!s.enabled) return s.has_token ? "Off" : "Not set up";
  switch (s.state) {
    case "connected": return `Connected as @${s.bot_name || "bot"} · ${s.chats} chat${s.chats === 1 ? "" : "s"}`;
    case "connecting": return "Connecting…";
    case "reconnecting": return `Reconnecting…${s.error ? ` ${s.error}` : ""}`;
    case "incomplete": return s.error || "Setup isn't finished";
    case "error": return s.error || "Stopped with an error";
    default: return "Off";
  }
}

/** A list of Telegram ids as removable chips plus a box to paste more. */
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
 * Settings → Connectors → Telegram. Status is polled from the backend rather
 * than kept here: the poller's connection changes on its own (retries, a
 * revoked token, another program polling the bot), and the form must never
 * show a state the backend left.
 */
export function TelegramConnector() {
  const settings = useStore((s) => s.settings);
  const models = useStore((s) => s.models);
  const [st, setSt] = useState<TelegramStatus | null>(null);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [tested, setTested] = useState<{ ok: boolean; text: string } | null>(null);
  const [open, setOpen] = useState(false);
  const detailsId = useId();

  useEffect(() => {
    let live = true;
    const tick = () => connectorsApi.telegramStatus().then((s) => { if (live) setSt(s); }).catch(() => {});
    void tick();
    const t = setInterval(tick, 2000);
    return () => { live = false; clearInterval(t); };
  }, []);

  const run = async (f: () => Promise<TelegramStatus>): Promise<boolean> => {
    setBusy(true); setErr("");
    try { setSt(await f()); return true; }
    catch (e) { setErr(String(e)); return false; }
    finally { setBusy(false); }
  };
  const configure = (patch: Partial<TelegramPatch>) => st ? run(() => connectorsApi.telegramConfigure({ ...telegramPatchOf(st), ...patch })) : Promise.resolve(false);
  const saveToken = async (t: string) => { if (await run(() => connectorsApi.telegramSetToken(t))) { setToken(""); setTested(null); flash(t ? "Bot token saved" : "Bot token removed"); } };
  const test = async () => {
    setBusy(true); setTested(null);
    try { const text = await connectorsApi.telegramTest(); setTested({ ok: true, text }); flash(text); }
    catch (e) { setTested({ ok: false, text: String(e) }); }
    finally { setBusy(false); }
  };

  const projects = settings?.projects ?? [];
  const projectOpts = [{ value: "", label: "The project open in the app" }, ...projects.map((p) => ({ value: p, label: p }))];
  if (st?.project && !projects.includes(st.project)) projectOpts.push({ value: st.project, label: st.project });
  const modelOpts = [{ value: "", label: "Most recently used model" }, ...models.filter((m) => m.enabled).map((m) => ({ value: m.id, label: m.name || m.id, hint: m.provider }))];
  const perm = PERMS.find((p) => p.id === (st?.perm ?? "auto"));

  return <div className="sgroup">
    <ChatConnectorHeader icon={Send} name="Telegram" pill={connectorPill(st, !!st?.has_token)} summary={telegramSummary(st)}
      open={open} onToggle={() => setOpen(!open)} controls={detailsId} enabled={!!st?.enabled} onEnable={(enabled) => void configure({ enabled })} />
    <div id={detailsId} hidden={!open}>{open && st && <>
      <div className="srowx desc" style={{ display: "block" }}>
        Message your agent from Telegram: each private chat, group or forum topic becomes a chat on this computer, and replies, approvals and questions come back to you there. The app has to be running (close to tray keeps it going); messages sent while it wasn't are skipped, not run later.
        <ol style={{ margin: "8px 0 0", paddingLeft: 18, lineHeight: 1.6 }}>
          <li>In Telegram, open <b>@BotFather</b>, send <span className="mono">/newbot</span>, pick a name and a username, and copy the token it gives you.</li>
          <li>Paste the token below and add your own numeric Telegram user id (message <b>@userinfobot</b> to see it; your @username won't do).</li>
          <li>Switch the bot on, then open your bot in Telegram and press <b>Start</b>.</li>
          <li>For a group: turn on <b>Allow groups</b> and add the bot to the group. By default Telegram bots only see messages that @mention them, reply to them or are commands, which is what <b>Only when mentioned</b> expects. To have it answer everything, turn that off and also disable privacy mode (@BotFather → <span className="mono">/setprivacy</span> → Disable, then remove and re-add the bot).</li>
        </ol>
      </div>
      <div className="srowx"><Button onClick={() => void openUrl(BOTFATHER).catch((e) => flash(String(e)))}>Open BotFather</Button></div>
      <div className="srowx">
        <input aria-label="Telegram bot token" className="input mono" type="password" autoComplete="off" spellCheck={false} style={{ flex: 1, minWidth: 0 }} value={token} disabled={busy}
          placeholder={st.has_token ? "Token saved · paste a new one to replace it" : "Bot token from @BotFather (123456:ABC…)"}
          onChange={(e) => setToken(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter" && token.trim()) void saveToken(token.trim()); }} />
        <Button disabled={busy || !token.trim()} onClick={() => void saveToken(token.trim())}>Save token</Button>
        {st.has_token && <Button variant="ghost" disabled={busy} onClick={() => void test()}>Test token</Button>}
        {st.has_token && <Button variant="ghost" disabled={busy} onClick={() => void saveToken("")}>Remove</Button>}
      </div>
      {tested && <div className="srowx desc" role={tested.ok ? "status" : "alert"} style={tested.ok ? undefined : { color: "var(--red)" }}>{tested.text}</div>}
      <IdList label="Allowed users" required busy={busy} ids={st.allowed_users} onChange={(allowed_users) => configure({ allowed_users })}
        hint="Only these Telegram accounts (numeric user ids) can talk to the agent. Everyone else is ignored, and nobody gets in until you add someone. Anyone listed can make the agent edit files and run commands here, within the permission mode below." />
      <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Direct messages</div><div className="desc">Allowed users can message the bot privately.</div></div>
        <Switch label="Allow direct messages" checked={st.allow_dms} onChange={(allow_dms) => void configure({ allow_dms })} /></div>
      <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Allow groups</div><div className="desc">Answer allowed users in groups the bot has been added to. Other members of the group can read the replies.</div></div>
        <Switch label="Allow groups" checked={st.allow_groups} onChange={(allow_groups) => void configure({ allow_groups })} /></div>
      {st.allow_groups && <>
        <IdList label="Group chats" busy={busy} ids={st.allowed_chats} onChange={(allowed_chats) => configure({ allowed_chats })}
          hint="Optional. Answer only in these groups (ids are negative numbers like -1001234567890; message the bot in a group and its id shows up below). Empty: any group the bot is in." />
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Only when mentioned in groups</div><div className="desc">In a group, answer only messages that @mention the bot, reply to it, or are a command for it. Private chats never need a mention.</div></div>
          <Switch label="Only when mentioned in groups" checked={st.require_mention} onChange={(require_mention) => void configure({ require_mention })} /></div>
      </>}
      <div className="srowx"><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Project</div><div className="desc">The folder new Telegram chats work in.</div></div>
        <Dropdown value={st.project} options={projectOpts} onChange={(project) => void configure({ project })} style={{ width: 240, height: 28, padding: "0 8px", flex: "0 0 240px" }} /></div>
      <div className="srowx"><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Model</div></div>
        <Dropdown value={st.model} options={modelOpts} onChange={(model) => void configure({ model })} style={{ width: 240, height: 28, padding: "0 8px", flex: "0 0 240px" }} /></div>
      <div className="srowx"><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Permissions</div><div className="desc">{perm?.desc} Approvals are sent to you in Telegram; reply yes, always or no.</div></div>
        <Dropdown value={st.perm} search={false} options={PERMS.map((p) => ({ value: p.id, label: p.name }))} onChange={(p) => void configure({ perm: p })} style={{ width: 160, height: 28, padding: "0 8px", flex: "0 0 160px" }} /></div>
      {st.perm === "turbo" && <div className="srowx desc" role="note" style={{ color: "var(--red)" }}>Full Access runs anything a Telegram message asks for without checking with you. Use it only if your Telegram account is well protected (two-step verification on).</div>}
      <div className="srowx desc" style={{ display: "block" }}>In Telegram: <span className="mono">/new</span> starts a fresh chat, <span className="mono">/stop</span> stops the agent, <span className="mono">/status</span> shows which chat you're in. Telegram chats also appear in your sidebar here. Bot chats aren't end-to-end encrypted: Telegram can read them.</div>
      {st.last_ignored && <div className="srowx desc" role="status">{st.last_ignored}</div>}
      {st.chats > 0 && <div className="srowx"><div className="desc" style={{ flex: 1 }}>{st.chats} Telegram conversation{st.chats === 1 ? " is" : "s are"} linked to a chat.</div><Button variant="ghost" disabled={busy} onClick={() => void run(connectorsApi.telegramForgetChats)}>Start all fresh</Button></div>}
    </>}</div>
    {err && <div className="srowx desc" role="alert" style={{ color: "var(--red)" }}>{err}</div>}
  </div>;
}
