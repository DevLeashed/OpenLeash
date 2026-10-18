import { useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, type Plugins, type PluginsStatus, type ServicePluginCfg, type ServicePluginId, type ServiceTestResult } from "../api";
import { Button, Switch } from "./primitives";
import { flash } from "../store";

/** One credential or setting a service plugin card offers. `secret` fields are
 *  write-only: they go through `plugin_secret_set` and the UI only ever learns
 *  whether one is saved. The rest are ordinary settings. */
interface Field { key: string; label: string; placeholder: string; secret: boolean; optional?: boolean }
interface Spec { id: ServicePluginId; name: string; blurb: string; fields: Field[]; env: string; help: { label: string; url: string } }

/** In the order the backend serves the tools (`plugin_services::SERVICES`). */
export const SERVICE_SPECS: Spec[] = [
  {
    id: "linear", name: "Linear",
    blurb: "Search, read, create and update Linear issues, comment, list teams, states and projects, and run GraphQL queries — as you. Reads run freely; changes (and GraphQL mutations) ask first unless you're in Full access.",
    fields: [{ key: "token", label: "Linear API key", placeholder: "lin_api_…", secret: true }],
    env: "LINEAR_API_KEY",
    help: { label: "Create a personal API key", url: "https://linear.app/settings/account/security" },
  },
  {
    id: "jira", name: "Jira",
    blurb: "JQL search, issues with comments, create and update issues, transitions, comments and projects on Jira Cloud or Server/Data Center — as you. Cloud uses your email plus an API token; leave the email empty to use a Server/DC personal access token. Changes ask first unless you're in Full access.",
    fields: [
      { key: "base_url", label: "Jira site URL", placeholder: "https://acme.atlassian.net", secret: false },
      { key: "email", label: "Jira account email", placeholder: "you@example.com (Cloud only)", secret: false, optional: true },
      { key: "token", label: "Jira API token", placeholder: "API token (Cloud) or personal access token (Server/DC)", secret: true },
    ],
    env: "JIRA_BASE_URL, JIRA_EMAIL, JIRA_API_TOKEN",
    help: { label: "Create an Atlassian API token", url: "https://id.atlassian.com/manage-profile/security/api-tokens" },
  },
  {
    id: "slack", name: "Slack",
    blurb: "Gives agents Slack tools through your Slack app's bot: list channels, read history and threads, look up people, post, reply in threads and react. Posting asks first unless you're in Full access. Searching messages needs an optional user token with search:read. (To chat with your agent from Slack, use the chat connector instead.)",
    fields: [
      { key: "token", label: "Slack bot token", placeholder: "xoxb-… (channels:read, channels:history, chat:write, reactions:write, users:read)", secret: true },
      { key: "user_token", label: "Slack user token", placeholder: "xoxp-… (optional, search:read — only for search)", secret: true, optional: true },
    ],
    env: "SLACK_BOT_TOKEN, SLACK_USER_TOKEN",
    help: { label: "Create a Slack app", url: "https://api.slack.com/apps" },
  },
  {
    id: "notion", name: "Notion",
    blurb: "Search, read pages as text, query databases, create pages, append content and update properties — through an internal integration. It only sees pages you share with it (… → Connections). Changes ask first unless you're in Full access.",
    fields: [{ key: "token", label: "Notion integration secret", placeholder: "ntn_… or secret_…", secret: true }],
    env: "NOTION_TOKEN",
    help: { label: "Create an internal integration", url: "https://www.notion.so/profile/integrations" },
  },
  {
    id: "gitlab", name: "GitLab",
    blurb: "Issues, merge requests with diffs and notes, pipelines, jobs and logs, files and search on gitlab.com or your own instance — as you. Changes ask first unless you're in Full access.",
    fields: [
      { key: "base_url", label: "GitLab URL", placeholder: "https://gitlab.com (or your instance)", secret: false, optional: true },
      { key: "token", label: "GitLab access token", placeholder: "glpat-… (api, or read_api for reading only)", secret: true },
    ],
    env: "GITLAB_TOKEN, GITLAB_URL",
    help: { label: "Create a personal access token", url: "https://gitlab.com/-/user_settings/personal_access_tokens" },
  },
];

const OFF: ServicePluginCfg = { enabled: false };

/** One card per service plugin, in the Connectors tab's "Accounts and apps" group. */
export function ServicePluginCard({ spec, p, st, save, busy }: { spec: Spec; p: Plugins; st: PluginsStatus | null; save: (next: Plugins) => Promise<unknown>; busy: boolean }) {
  const cfg = p[spec.id] ?? OFF;
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [test, setTest] = useState<ServiceTestResult | null>(null);
  const [testing, setTesting] = useState(false);
  const [secretBusy, setSecretBusy] = useState(false);
  const saved = (k: string) => st?.secrets?.[spec.id]?.[k] === true;
  const value = (f: Field) => draft[f.key] ?? (f.secret ? "" : String((cfg as unknown as Record<string, unknown>)[f.key] ?? ""));
  const runTest = async () => {
    setTesting(true);
    try { setTest(await api.pluginServiceTest(spec.id)); }
    catch (e) { setTest({ ok: false, error: String(e) }); }
    finally { setTesting(false); }
  };
  const saveSecret = async (f: Field, v: string) => {
    setSecretBusy(true);
    try {
      await api.pluginSecretSet(spec.id, f.key, v);
      setDraft((d) => { const { [f.key]: _drop, ...rest } = d; void _drop; return rest; });
      flash(v ? `${f.label} saved` : `${f.label} removed`);
      setTest(null);
    } catch (e) { flash(String(e)); }
    finally { setSecretBusy(false); }
  };
  const saveField = async (f: Field) => {
    const v = (draft[f.key] ?? "").trim();
    if (await save({ ...p, [spec.id]: { ...cfg, [f.key]: v } })) {
      setDraft((d) => { const { [f.key]: _drop, ...rest } = d; void _drop; return rest; });
      flash("Saved"); setTest(null);
    }
  };
  const summary = test ? (test.ok ? `Connected · ${test.identity ?? ""}${test.source ? ` · from ${test.source}` : ""}` : `Not connected: ${test.error ?? "unknown error"}`)
    : spec.fields.some((f) => f.secret && saved(f.key)) ? "Token saved · test to check it" : `No token saved · ${spec.env} also work`;
  const id = `svc-${spec.id}`;
  // A self-hosted GitLab issues its tokens on its own host.
  const helpUrl = spec.id === "gitlab" && /^https?:\/\/[^/\s]+/i.test(cfg.base_url ?? "") ? `${(cfg.base_url ?? "").replace(/\/+$/, "")}/-/user_settings/personal_access_tokens` : spec.help.url;
  return <>
    <div className="srowx"><div style={{ flex: 1 }}>{spec.name}<div className="desc">{summary}</div></div><Switch label={`${spec.name} plugin`} checked={cfg.enabled} onChange={(enabled) => void save({ ...p, [spec.id]: { ...cfg, enabled } })} /></div>
    <div className="srowx" style={{ minHeight: 34 }}><Button aria-label={`${spec.name} Details`} aria-expanded={open} aria-controls={id} onClick={() => setOpen(!open)}>Details {open ? "▾" : "▸"}</Button></div>
    <div id={id} hidden={!open}>{open && <>
      <div className="srowx desc">{spec.blurb}</div>
      {spec.fields.map((f) => f.secret
        ? <div className="srowx" key={f.key}><input aria-label={f.label} className="input mono" type="password" style={{ flex: 1, minWidth: 0 }} value={value(f)} placeholder={saved(f.key) ? "Saved · paste to replace" : f.placeholder} onChange={(e) => { const v = e.currentTarget.value; setDraft((d) => ({ ...d, [f.key]: v })); }} onKeyDown={(e) => { if (e.key === "Enter" && value(f).trim()) void saveSecret(f, value(f).trim()); }} /><Button disabled={secretBusy || !value(f).trim()} onClick={() => void saveSecret(f, value(f).trim())}>Save</Button>{saved(f.key) && <Button disabled={secretBusy} onClick={() => void saveSecret(f, "")}>Remove</Button>}</div>
        : <div className="srowx" key={f.key}><input aria-label={f.label} className="input mono" style={{ flex: 1, minWidth: 0 }} value={value(f)} placeholder={f.placeholder} onChange={(e) => { const v = e.currentTarget.value; setDraft((d) => ({ ...d, [f.key]: v })); }} onKeyDown={(e) => { if (e.key === "Enter") void saveField(f); }} /><Button disabled={busy || draft[f.key] === undefined} onClick={() => void saveField(f)}>Save</Button></div>)}
      <div className="srowx"><div className="desc" style={{ flex: 1 }}>Saved tokens are stored in settings.json like other keys; {spec.env} are used when nothing is saved here.</div><Button onClick={() => void openUrl(helpUrl).catch((e) => flash(String(e)))}>{spec.help.label}</Button><Button disabled={testing} onClick={() => void runTest()}>{testing ? "Testing…" : "Test connection"}</Button></div>
    </>}</div>
  </>;
}
