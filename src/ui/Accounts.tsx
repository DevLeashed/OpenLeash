// Settings: Accounts (subscription logins + live usage), Routing (fallback
// chains) and Subagents (custom agent definitions).
import { Fragment, useEffect, useLayoutEffect, useRef, useState } from "react";
import { AccountProviderInfo, AccountView, AgentDef, AntigravityStatus, api, effortLadder, isRequiredAgent, ProviderView, Route, until, UsageCap, Window, withRequiredAgents } from "../api";
import { flash, get, isConnected, modelInfo, saveSettings, set, shownProviders, useNow, useStore, zoom } from "../store";
import { copyText } from "./clipboard";
import { Dropdown, Opt } from "./primitives";
import { I } from "./icons";
import { Modal, MorphText } from "./primitives";
import { ProviderIcon } from "./ProviderIcon";
import { Tooltip } from "./primitives";
import { ctxPlace, MenuRow, MenuScrim, menuLayer } from "./primitives";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Bot, Check, ChevronDown, ChevronLeft, ChevronRight, ChevronUp, CircleAlert, ExternalLink, RefreshCw } from "lucide-react";
import { Loader, Button, ColorPicker, IconButton, Input, Pressable, RemovableChip, Segmented, Switch, TextArea, TextButton } from "./primitives";
import { GROUP_NAMES, GroupRow, groupsOf, restrictionLabel, toToolGroups, validateGroups } from "./agent-groups";

// Stable fallbacks for selectors (a fresh [] each render loops useSyncExternalStore).
const NO_ROUTES: Route[] = [];
const NO_IDS: string[] = [];

const tone = (used: number) => (used >= 95 ? "var(--red)" : "var(--mut2)");

/** What a cap is drawn in: the app's own amber for work that is being held
 *  back, so a capped account reads as held on purpose rather than as broken. */
const CAP_COLOR = "var(--st-pause)";

/** The caps the backend reports for an account. Absent on a backend that
 *  predates them, since a phone browser drives this same backend. */
export function capsOf(a: AccountView): UsageCap[] {
  return a.caps ?? [];
}

/** The cap standing on one window of one account, if there is one. A temporary
 *  cap whose time has run out holds nothing, so no line draws one. */
export function liveCap(a: AccountView, window: string): UsageCap | null {
  const c = capsOf(a).find((x) => x.window === window);
  return c && (c.until === 0 || c.until * 1000 > Date.now()) ? c : null;
}

/** What a provider's “All …” line can say about the caps on one window: the cap
 *  every account carries when they all carry the same one, and how many carry
 *  any at all when they do not. */
export function capSummary(list: AccountView[], window: string): { shared: UsageCap | null; n: number } {
  const caps: UsageCap[] = [];
  let missing = false;
  for (const a of list) {
    const c = liveCap(a, window);
    if (c) caps.push(c);
    else missing = true;
  }
  const first = caps[0];
  const same = !!first && !missing && caps.every((c) => c.used === first.used && c.until === first.until);
  // The aggregated line only reads "capped" when every account carrying the cap
  // has reached it: one account held while another still works is not the pool
  // being capped, and the row bars already say which one is.
  return { shared: same ? { ...first, holding: caps.every((c) => c.holding) } : null, n: caps.length };
}

/** A cap in words, for a tooltip: where it holds, and what lifts it. */
export function capText(c: UsageCap) {
  const at = `${c.used.toFixed(0)}% of the ${c.window} window`;
  const when = c.until === 0 ? "until you remove it" : `lifts in ${until(c.until)}`;
  return c.holding
    ? `Capped at ${at} — reached, so the harness has stopped using this account (${when})`
    : `Capped at ${at} (${when})`;
}

/** What the badge beside a capped line says: the cap itself while it is still
 *  ahead of the usage, “capped” once it is what holds the account back. */
function capBadge(c: UsageCap) {
  const when = c.until === 0 ? "" : ` · ${until(c.until)}`;
  return c.holding ? `capped${when}` : `cap ${c.used.toFixed(0)}%${when}`;
}

export function UsageBar({ w, cap, onCap, hint, compact, style }: { w: Window; cap?: UsageCap | null; onCap?: () => void; hint?: string; compact?: boolean; style?: React.CSSProperties }) {
  const used = Math.max(0, Math.min(100, w.used));
  const left = 100 - used;
  // A reached cap is drawn as the account being held, not as an error: amber
  // fill, amber figure, and a badge that names the cap rather than the
  // percentage. The marker sits where the fill stops when the cap is reached,
  // so the line reads as a floor the bar is not allowed to shrink past.
  const held = !!cap?.holding;
  const color = held ? CAP_COLOR : tone(used);
  const tip = [
    `${w.label}: ${left.toFixed(0)}% left`,
    w.resets_at ? `resets in ${until(w.resets_at)}` : "",
    cap ? capText(cap) : "",
    onCap ? hint ?? "Double-click to cap this window" : "",
  ].filter(Boolean).join(" · ");
  return (
    <Tooltip content={tip}><div className="ubar" style={style} onDoubleClick={onCap}>
      <span className="ublabel">{w.label}</span>
      <span className="minibar">
        <span style={{ width: left + "%", background: color }} />
        {cap && <span className={"capmark" + (held ? " held" : "")} style={{ left: 100 - Math.max(0, Math.min(100, cap.used)) + "%" }} />}
      </span>
      <MorphText className="ubpct" style={{ color }}>{left.toFixed(0)}%</MorphText>
      {cap && <span className={"capbadge" + (held ? " held" : "")}>{capBadge(cap)}</span>}
      {!compact && w.resets_at > 0 && <span className="ubreset">{until(w.resets_at)}</span>}
    </div></Tooltip>
  );
}

/** Combined picture across accounts of a kind: how much headroom is left overall. */
export function netWindows(list: AccountView[]): Window[] {
  const on = list.filter((a) => a.enabled && !a.disabled_reason && a.usage);
  if (!on.length) return [];
  const labels = Array.from(new Set(on.flatMap((a) => a.usage!.windows.map((w) => w.label)))).slice(0, 2);
  return labels.map((label) => {
    const ws = on.map((a) => a.usage!.windows.find((w) => w.label === label)).filter(Boolean) as Window[];
    // Keep each aggregate tied to its own provider window. A full weekly
    // allowance does not mean the separate 5h window is full (and vice versa).
    // Missing readings are unknown, not unused allowance. Only accounts that
    // actually report this window belong in its average.
    const used = ws.reduce((sum, w) => sum + w.used, 0) / ws.length;
    const next = ws.filter((w) => w.resets_at > 0).map((w) => w.resets_at).sort()[0] ?? 0;
    return { label, used, resets_at: next };
  });
}

function AccountRow({ a, i, n, revealed, onCap }: { a: AccountView; i: number; n: number; revealed: boolean; onCap: (a: AccountView, w: { label: string; used: number | null; resets_at: number }) => void }) {
  const [edit, setEdit] = useState(false);
  const [label, setLabel] = useState(a.label);
  const [del, setDel] = useState(false);
  // Right-click menu: the cursor position, or null when closed.
  const [ctx, setCtx] = useState<{ x: number; y: number } | null>(null);
  const upd = async (patch: Parameters<typeof api.accountUpdate>[1]) => set({ accounts: await api.accountUpdate(a.id, patch) });
  const cooling = a.cooldown_until * 1000 > Date.now();
  // The cap the harness is acting on, if one is. It is shown ahead of a
  // cooldown because it is what the *user* did: a bench the provider reported
  // and a bench of their own making look identical from the row otherwise, and
  // only one of the two is theirs to lift.
  const capped = capsOf(a).find((c) => c.holding) ?? null;
  const state = a.disabled_reason ? { t: a.disabled_reason, c: "#ff6363", tip: undefined as string | undefined }
    : !a.enabled ? { t: "Off", c: "var(--dim)", tip: undefined }
    : capped ? { t: capped.until === 0 ? "Capped" : `Capped · ${until(capped.until)}`, c: CAP_COLOR, tip: `${capText(capped)}${cooling ? ` · also benched: ${a.cooldown_reason || "back shortly"} until ${until(a.cooldown_until)}` : ""}` }
    : cooling ? { t: until(a.cooldown_until), c: "var(--mut2)", tip: `${a.cooldown_reason || "Usage limit (per live usage)"} · back in ${until(a.cooldown_until)}` }
    : a.active ? { t: "Serving now", c: "var(--fg3)", tip: undefined } : { t: "Ready", c: "var(--mut2)", tip: undefined };
  return (
    <div className="acct" style={{ animationDelay: i * 40 + "ms", opacity: a.enabled ? 1 : 0.55 }} onContextMenu={(e) => { e.preventDefault(); setCtx({ x: e.clientX, y: e.clientY }); }}>
      <div className="prio">
        <IconButton label="Higher priority" disabled={i === 0} onClick={() => upd({ move: -1 })}><ChevronUp size={12} /></IconButton>
        <span>{i + 1}</span>
        <IconButton label="Lower priority" disabled={i === n - 1} onClick={() => upd({ move: 1 })}><ChevronDown size={12} /></IconButton>
      </div>
      <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 7 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
          {edit ? (
            <Input className="input" autoFocus style={{ height: 24, width: 180 }} value={label} onChange={(e) => setLabel(e.currentTarget.value)} onBlur={() => { setEdit(false); void upd({ label }); }} onKeyDown={(e) => e.key === "Enter" && (e.currentTarget as HTMLInputElement).blur()} />
          ) : (
            <Tooltip content="Double-click to rename"><span style={{ fontWeight: 600 }} onDoubleClick={() => setEdit(true)}>{a.label}</span></Tooltip>
          )}
          {revealed && <span className="mono" style={{ fontSize: 11.5, color: "var(--mut3)", overflowWrap: "anywhere" }}>
            {/* Key-login accounts identify by key fragment, OAuth ones by
                address; a Kimi account falls back to its email when the
                gateway reported one and the key fragment is somehow absent. */}
            {a.key_hint || a.email.trim() || "Nothing recorded"}
          </span>}
          <span style={{ fontSize: 11.5, color: "var(--mut3)", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
            {a.usage?.plan || ""}
          </span>
          <div style={{ flex: 1 }} />
          <Tooltip content={state.tip}><MorphText className="account-state" style={{ color: state.c }}>{state.t}</MorphText></Tooltip>
        </div>
        {a.usage?.windows.length ? (
          <div style={{ display: "flex", flexWrap: "wrap", gap: "3px 16px" }}>
            {a.usage.windows.map((w) => <UsageBar key={w.label} w={w} cap={liveCap(a, w.label)} onCap={() => onCap(a, { label: w.label, used: w.used, resets_at: w.resets_at })} />)}
          </div>
        ) : (
          <div style={{ fontSize: 11, color: a.usage?.error ? "#ff8a8a" : "var(--dim)", display: "flex", alignItems: "center", flexWrap: "wrap", gap: 6 }}>
            <span>{a.usage?.error || "Usage not read yet"}</span>
            {/* A cap stands on the account rather than on a reading, so it
                outlives a provider that stopped reporting the window it names.
                Without this it would be a cap with no line to be lifted from —
                one the user can neither see nor remove. */}
            {capsOf(a).filter((c) => c.until === 0 || c.until * 1000 > Date.now()).map((c) => (
              <Pressable key={c.window} className={"capbadge" + (c.holding ? " held" : "")} aria-label={`Cap on the ${c.window} window — change or lift it`}
                onClick={() => onCap(a, { label: c.window, used: null, resets_at: 0 })}>
                {c.window} · {capBadge(c)}
              </Pressable>
            ))}
          </div>
        )}
      </div>
      <div style={{ display: "flex", flexDirection: "column", alignItems: "flex-end", gap: 8 }}>
        <Switch label={`${a.enabled ? "Disable" : "Enable"} ${a.label}`} checked={a.enabled} onChange={(checked) => void upd({ enabled: checked })} />
        {del ? <Button variant="primary" style={{ height: 24, fontSize: 11 }} onClick={async () => set({ accounts: await api.accountRemove(a.id) })}>Remove</Button>
          : <IconButton label="Remove account" style={{ width: 24, height: 24 }} onClick={() => setDel(true)}>{I.trash()}</IconButton>}
      </div>
      {ctx && <AccountMenu a={a} x={ctx.x} y={ctx.y} close={() => setCtx(null)} />}
    </div>
  );
}

/**
 * What the email row shows before it is revealed: enough to tell two accounts
 * apart at a glance, not enough to read the address. Short local parts are
 * masked entirely rather than half-shown, since "a@…" identifies nobody.
 */
function maskEmail(email: string) {
  const at = email.indexOf("@");
  if (at < 0) return email ? "•••" : "No email recorded";
  const local = email.slice(0, at), domain = email.slice(at);
  if (local.length <= 2) return "•••" + domain;
  return local[0] + "•".repeat(local.length - 2) + local[local.length - 1] + domain;
}

/**
 * Right-click menu on an account.
 *
 * "Reveal" is why it exists: the sign-in address is the account's identity, and
 * it is the one thing that tells two pooled accounts of the same kind apart
 * when the labels have been renamed. It is deliberately a click rather than a
 * visible row — `reviewer_privacy.rs` pins the address out of everything that
 * gets screenshotted or pasted, so the UI must not put it back on the page by
 * default. It is also deliberately not a copy button: a password field that
 * unmasks in place is what people expect here, and it keeps the address inside
 * the app instead of on the clipboard.
 *
 * The address is masked by default (first and last character, middle elided) so
 * the reveal itself does not turn right-click into a leak. With no address
 * recorded — an API-key account, or a login file that carried none — the row
 * still reads "Reveal" but says so, since a row that silently does nothing
 * looks broken.
 */
function AccountMenu({ a, x, y, close }: { a: AccountView; x: number; y: number; close: () => void }) {
  const [shown, setShown] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const [h, setH] = useState(96);
  useLayoutEffect(() => { setH(ref.current?.offsetHeight || 96); }, [shown]);
  const email = a.email.trim();
  const copy = (what: string) => { copyText(what, { what: what === email ? "Email copied" : "Copied" }); };
  return (
    <div style={menuLayer(zoom())}>
      <MenuScrim layer={90} onClick={close} onContextMenu={(e) => { e.preventDefault(); close(); }} />
      {/* The accounts page lives inside the zoomed app root, so this menu
          carries the same zoom as the one in `ReplyMenu`. */}
      <div ref={ref} className="pop ctx" style={{ ...ctxPlace(x, y, 252, h, zoom()) }}>
        <div className="mmodel" aria-disabled="true">
          <span className="cico">{I.key(12)}</span>
          <span className="mm-name">{a.label}</span>
        </div>
        <div className="msep" />
        <MenuRow onClick={() => setShown(!shown)}>
          <span className="cico">{shown ? I.eyeOff(12) : I.eye(12)}</span>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ fontWeight: 500 }}>{shown ? "Hide email" : "Reveal"}</div>
            <div className="mono" style={{ fontSize: 11, color: "var(--mut3)", lineHeight: 1.4, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{shown && email ? email : maskEmail(email)}</div>
          </div>
        </MenuRow>
        {shown && email && <MenuRow onClick={() => copy(email)}><span className="cico">{I.copy(12)}</span><span style={{ flex: 1 }}>Copy email</span></MenuRow>}
      </div>
    </div>
  );
}

function Copy({ text }: { text: string }) {
  const [done, setDone] = useState(false);
  return (
    <Pressable className="copybox" onClick={() => copyText(text, { onResult: (ok) => { if (!ok) return; setDone(true); setTimeout(() => setDone(false), 1400); } })}>
      <span className="mono">{text}</span>
      <span className={"copyhint" + (done ? " done" : "")}>{done && <Check size={11} />}<MorphText>{done ? "Copied" : "Copy"}</MorphText></span>
    </Pressable>
  );
}

/** Setup-token guide for account providers that support one. */
function TokenGuide({ provider, onBack, onClose, onToken, busy }: { provider: ProviderView; onBack: () => void; onClose: () => void; onToken: (t: string) => void; busy: boolean }) {
  const meta = provider.account!;
  const [tok, setTok] = useState("");
  const ok = tok.trim().startsWith(meta.token_prefix);
  const steps: { title: string; body: React.ReactNode }[] = [
    { title: "Run in a terminal", body: <><Copy text={meta.setup_command} /><div className="gnote">Sign in to the account you want to connect.</div></> },
    { title: "Paste the token", body: (
      <div style={{ display: "flex", gap: 6 }}>
        <Input className={"input mono" + (tok && !ok ? " bad" : "")} autoFocus style={{ flex: 1, height: 34, fontSize: 11.5 }} type="password" placeholder={`${meta.token_prefix}…`} value={tok} onChange={(e) => setTok(e.currentTarget.value)} onKeyDown={(e) => e.key === "Enter" && ok && onToken(tok.trim())} />
        <Button variant="primary" disabled={!ok || busy} style={{ height: 34 }} onClick={() => onToken(tok.trim())}>{busy && <Loader size={13} />}Connect</Button>
      </div>
    ) },
  ];
  return (
    <Modal onClose={onClose} style={{ width: "min(540px, 100%)" }}>
        <div className="mh"><ProviderIcon provider={provider.id} size={18} /><span style={{ flex: 1 }}>Connect {meta.display_name} with a token</span><IconButton label="Close" onClick={onClose}>{I.close()}</IconButton></div>
        <div className="mb">
          <div className="gwhy">The CLI login file may not contain a token on Windows. Use a setup token instead.</div>
          <div className="gsteps">
            {steps.map((st, i) => (
              <div key={i} className="gstep" style={{ animationDelay: 80 + i * 70 + "ms" }}>
                <div className={"gnum" + (i === 1 && ok ? " ok" : "")}>{i === 1 && ok ? <Check size={12} /> : i + 1}</div>
                <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 6 }}><div style={{ fontWeight: 600 }}>{st.title}</div><div style={{ color: "var(--mut)", lineHeight: 1.55 }}>{st.body}</div></div>
              </div>
            ))}
          </div>
          {tok && !ok && <div style={{ fontSize: 11.5, color: "#ff8a8a" }}>Token must start with {meta.token_prefix}.</div>}
          {meta.warning && <div className="secondary-text">{meta.warning}</div>}
        </div>
        <div className="mf"><Button variant="ghost" onClick={onBack}><ChevronLeft size={13} />Back</Button><div style={{ flex: 1 }} /><span style={{ fontSize: 11, color: "var(--dim)" }}>Stored only in ~/.openleash</span></div>
    </Modal>
  );
}

/**
 * The terms notice a user has to acknowledge before an account of a gated
 * provider can be added. A separate final warning follows it, so this first
 * acknowledgement does not immediately open the connect form.
 *
 * Deliberately its own modal rather than a banner inside `ImportDialog`: the
 * gate exists to be the thing you cannot miss, and a strip at the top of the
 * form next to the Import button is exactly what somebody adds an account
 * without reading. It takes the whole screen, and there is no path from here to
 * the next step except the button that says you understood.
 *
 * Cancelling drops the whole Add flow, not just this step: leaving the gate up
 * would mean "Back" from the form lands somewhere the user did not ask to be,
 * and both warnings have to be re-acknowledged per Add.
 */
function TermsGate({ gate, provider, onAccept, onClose }: { gate: NonNullable<AccountProviderInfo["terms_gate"]>; provider: string; onAccept: () => void; onClose: () => void }) {
  return (
    <Modal onClose={onClose} style={{ width: "min(560px, 100%)" }}>
      <div className="mh"><ProviderIcon provider={provider} size={18} /><span style={{ flex: 1 }}>{gate.title}</span><IconButton label="Close" onClick={onClose}>{I.close()}</IconButton></div>
      <div className="mb">
        <div className="termsgate-lede">{gate.lede}</div>
        <ul className="termsgate-points">
          {gate.points.map((p, i) => <li key={i}>{p}</li>)}
        </ul>
        <div className="termsgate">
          <CircleAlert aria-hidden="true" size={13} strokeWidth={1.9} />
          <span>If Anthropic decides this counts as a breach, the realistic outcomes are your account being suspended and this subscription becoming unusable. Nobody here can undo that.</span>
        </div>
        <TextButton link onClick={() => openUrl(gate.terms_url).catch(() => flash("Couldn't open the browser"))}>
          Read {meta_url_label(gate.terms_url)}
          <ExternalLink size={10} strokeWidth={1.7} />
        </TextButton>
      </div>
      <div className="mf">
        <div style={{ flex: 1 }} />
        <Button variant="ghost" onClick={onClose}>Cancel</Button>
        <Button variant="primary" onClick={onAccept}>{gate.accept}</Button>
      </div>
    </Modal>
  );
}

function BanWarningGate({ onAccept, onClose }: { onAccept: () => void; onClose: () => void }) {
  return (
    <Modal onClose={onClose} style={{ width: "min(480px, 100%)" }}>
      <div className="mh"><CircleAlert aria-hidden="true" size={16} color="var(--st-pause)" /><span style={{ flex: 1 }}>Final warning</span><IconButton label="Close" onClick={onClose}>{I.close()}</IconButton></div>
      <div className="mb">
        <div className="termsgate-lede">This will most likely get your account banned. You have been warned.</div>
      </div>
      <div className="mf">
        <div style={{ flex: 1 }} />
        <Button variant="ghost" onClick={onClose}>Cancel</Button>
        <Button variant="primary" onClick={onAccept}>I understand</Button>
      </div>
    </Modal>
  );
}

/** "Anthropic's consumer terms", not the raw URL — the host is noise in prose. */
const meta_url_label = (url: string) => {
  const host = url.split("/")[2] ?? "the provider's";
  return host.replace(/^www\./, "") + "'s terms";
};

/** What a double-click on a usage line opens.
 *
 * A cap is an instruction the user gives, not a bench the provider reported.
 * Nothing that clears a bench — a resume, a retry, a token refresh — touches it,
 * and it rides in settings beside the account, so it survives a restart. That is
 * the distinction this dialog has to keep legible, and it is what the two
 * choices mean: Temporary is *this cap lifts by itself at a time you pick*, never
 * "this cap is a suggestion".
 */
export type CapTarget = {
  ids: string[];
  kind: string;
  /** Who the cap lands on, in words: an account label, or "each of the 4 …". */
  who: string;
  window: string;
  /** The cap already standing on this line, if any. */
  cap: UsageCap | null;
  /** The highest usage on this line right now, or null when nothing was read. */
  used: number | null;
  /** When the window rolls over; 0 = the provider did not say. */
  resets_at: number;
};

const CAP_HOURS = { "1h": 1, "4h": 4, "24h": 24 } as const;
type CapDur = keyof typeof CAP_HOURS | "window";

function CapDialog({ target, onClose }: { target: CapTarget; onClose: () => void }) {
  // Opens on the cap already standing, so double-clicking a line is also how one
  // is adjusted; a fresh cap starts at 80% used, which is headroom without
  // parking most of an account.
  const [at, setAt] = useState(target.cap ? Math.round(target.cap.used) : 80);
  // Permanent is the default. A cap that lifts on its own is the one that can
  // quietly stop holding a window while the user still believes it does.
  const [span, setSpan] = useState<"perm" | "temp">(target.cap && target.cap.until > 0 ? "temp" : "perm");
  const [dur, setDur] = useState<CapDur>("1h");
  const [busy, setBusy] = useState(false);
  const resetIn = Math.max(0, (target.resets_at || 0) - Math.floor(Date.now() / 1000));
  const secs = span === "perm" ? 0 : dur === "window" ? resetIn : CAP_HOURS[dur] * 3600;
  const nowAt = target.used;
  const run = async (f: () => Promise<AccountView[]>) => {
    setBusy(true);
    try { set({ accounts: await f() }); onClose(); } catch (e) { flash(String(e)); }
    setBusy(false);
  };
  return (
    <Modal onClose={onClose} style={{ width: "min(500px, 100%)" }}>
      <div className="mh"><ProviderIcon provider={target.kind} size={18} /><span style={{ flex: 1 }}>Cap usage · {target.window}</span><IconButton label="Close" onClick={onClose}>{I.close()}</IconButton></div>
      <div className="mb">
        <div className="field">
          <label>Stop at {at}% used <span style={{ color: "var(--dim)" }}>· {100 - at}% of the window left</span></label>
          <Input className="capslider" type="range" min={5} max={95} step={5} value={at} aria-label="Cap" aria-valuetext={`${at}% used`} onChange={(e) => setAt(Number(e.currentTarget.value))} />
          <div className="desc">{nowAt != null ? `Now at ${nowAt.toFixed(0)}% used. ` : ""}{nowAt != null && nowAt >= at ? "The cap is at or below that already, so it takes effect immediately." : ""}</div>
        </div>
        <div className="field"><label>How long it stands</label>
          <Segmented label="How long the cap stands" value={span} onChange={setSpan} style={{ width: "100%" }} options={[
            { value: "perm", label: "Permanent", hint: "Stands until you lift it from this line" },
            { value: "temp", label: "Temporary", hint: "Lifts by itself, without you coming back" },
          ]} />
        </div>
        {span === "temp" && <div className="field"><label>Lifts after</label>
          <Segmented label="When a temporary cap lifts" value={dur} onChange={setDur} style={{ width: "100%" }} options={[
            { value: "1h", label: "1 hour" },
            { value: "4h", label: "4 hours" },
            { value: "24h", label: "24 hours" },
            { value: "window", label: "This window", hint: resetIn > 0 ? `When the window rolls over, in ${until(target.resets_at)}` : "This provider does not say when the window rolls over", disabled: resetIn <= 0 },
          ]} />
          <div className="desc">A temporary cap can be renewed from the same line before it lifts.</div>
        </div>}
      </div>
      <div className="mf">
        {target.cap && <Button variant="ghost" style={{ color: "#ff8a8a" }} disabled={busy} onClick={() => void run(() => api.accountCapClear(target.ids, target.window))}>Remove cap</Button>}
        <div style={{ flex: 1 }} />
        <Button variant="ghost" onClick={onClose}>Cancel</Button>
        <Button variant="primary" disabled={busy} onClick={() => void run(() => api.accountCapSet(target.ids, target.window, at, secs))}>{busy && <Loader size={13} />}{target.cap ? "Update cap" : "Set cap"}</Button>
      </div>
    </Modal>
  );
}

function ImportDialog({ provider, onClose }: { provider: ProviderView; onClose: () => void }) {
  const kind = provider.id;
  const meta = provider.account!;
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [guide, setGuide] = useState(false);
  const run = async (f: () => Promise<AccountView[]>) => {
    setBusy(true);
    try {
      set({ accounts: await f() });
      set({ models: await api.models() });
      onClose();
    } catch (e) {
      // An empty Claude login file: walk them through setup-token instead of a wall of text.
      if (meta.setup_command && String(e).includes(meta.setup_command.split(" ").at(-1)!)) setGuide(true);
      else flash(String(e));
    }
    setBusy(false);
  };
  if (guide) return <TokenGuide provider={provider} onBack={() => setGuide(false)} onClose={onClose} onToken={(t) => run(() => api.accountImport(kind, t))} busy={busy} />;
  return (
    <Modal onClose={onClose} style={{ width: "min(520px, 100%)" }}>
        <div className="mh"><ProviderIcon provider={kind} size={18} /><span style={{ flex: 1 }}>Connect {meta.display_name}</span><IconButton label="Close" onClick={onClose}>{I.close()}</IconButton></div>
        <div className="mb">
          {/* A key-login provider with no local login file (Kimi Code) offers
              the paste box only: its credential is created in a web console,
              not stored by a CLI this app could read. */}
          {meta.login_file && (
            <div className="sgroup">
              <div className="srowx" style={{ alignItems: "flex-start" }}>
                <div style={{ flex: 1 }}>
                  <div style={{ fontWeight: 500 }}>{meta.key_login ? `Import ${meta.short_name} key` : "Import CLI login"}</div>
                  <div className="desc"><span className="mono">{meta.login_file}</span>{meta.key_login ? ` · API key created with your ${meta.display_name} subscription` : <> · run <span className="mono">{meta.login_command}</span> first</>}</div>
                  {meta.setup_command && <div className="desc"><TextButton link onClick={() => setGuide(true)}>Use a setup token instead<ChevronRight size={11} /></TextButton></div>}
                </div>
                <Button variant="primary" disabled={busy} onClick={() => run(() => api.accountImportLocal(kind))}>{busy && <Loader size={13} />}Import</Button>
              </div>
            </div>
          )}
          <div className="field">
            <label>{meta.key_login ? `Paste your ${meta.short_name} API key` : "Or paste credentials"}</label>
            <TextArea className="input mono" style={{ height: 88, padding: 10, fontSize: 11, resize: "vertical" }} value={text} placeholder={meta.paste_hint} onChange={(e) => setText(e.currentTarget.value)} />
          </div>
          {meta.warning && <div className="secondary-text">{meta.warning}</div>}
        </div>
        <div className="mf">
          <div style={{ flex: 1 }} />
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={!text.trim() || busy} onClick={() => run(() => api.accountImport(kind, text))}>{busy && <Loader size={13} />}{meta.key_login ? "Connect key" : "Connect"}</Button>
        </div>
    </Modal>
  );
}

function AntigravityAccountCard() {
  const [status, setStatus] = useState<AntigravityStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const refreshStatus = async () => {
    setBusy(true);
    try {
      const next = await api.antigravityStatus();
      setStatus(next);
      if (next.found && next.authed) set({ models: await api.models() });
    }
    catch (e) { flash(String(e)); }
    finally { setBusy(false); }
  };
  useEffect(() => { void refreshStatus(); }, []);
  return (
    <div className="sgroup account-group">
      <div className="srowx">
        <ProviderIcon provider="antigravity" size={18} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ fontWeight: 600 }}>Google Antigravity (agy)</div>
          <div className="desc">{status === null ? "Checking CLI login…" : !status.found ? "agy is not installed or not on PATH" : status.authed ? `Signed in · ${status.models} models available` : "Not signed in — sign in with agy in a terminal"}</div>
        </div>
        <Button disabled={busy} onClick={() => void refreshStatus()}>{busy && <Loader size={13} />}Refresh status</Button>
      </div>
      {status?.error && <div style={{ padding: "0 10px 8px", fontSize: 11.5, color: "#ff8a8a" }}>{status.error}</div>}
    </div>
  );
}

export function AccountsTab() {
  const accts = useStore((s) => s.accounts);
  const providers = useStore((s) => s.providers);
  // `null` = closed. Otherwise: which provider we are adding and which
  // acknowledgement, if any, still stands between the user and the form.
  const [dialog, setDialog] = useState<null | { id: string; gate: "terms" | "ban" | null }>(null);
  const activeProvider = providers.find((p) => p.id === dialog?.id && p.account);
  const [busy, setBusy] = useState(false);
  const [revealed, setRevealed] = useState(false);
  const [revealWarning, setRevealWarning] = useState(false);
  // The cap editor: which lines a double-click opened it on, or null when shut.
  const [cap, setCap] = useState<CapTarget | null>(null);
  // Just the relative "resets in" strings, on this tab's own clock.
  useNow(15_000);
  const refresh = async () => {
    setBusy(true);
    try {
      set({ accounts: await api.accountsRefresh() });
      // Publish the refreshed readings before waiting for model discovery. A
      // newer accounts event during that wait must not be overwritten here.
      set({ models: await api.poolModels() });
    } catch (e) { flash(String(e)); }
    setBusy(false);
  };
  const close = () => setDialog(null);
  // One account's own line: the cap lands on it alone. `used` is null when the
  // line is a cap standing on a window the provider is not reporting, where
  // there is no reading to show or compare against.
  const capAccount = (a: AccountView, w: { label: string; used: number | null; resets_at: number }) => setCap({
    ids: [a.id], kind: a.kind, who: a.label, window: w.label,
    cap: liveCap(a, w.label), used: w.used, resets_at: w.resets_at,
  });
  // A provider's "All …" line: the same ceiling on every account of the kind,
  // which is what the aggregated bar is made of — an average of accounts each
  // holding at the cap is an average that cannot exceed it.
  const capPool = (p: ProviderView, list: AccountView[], w: Window) => setCap({
    ids: list.map((a) => a.id), kind: p.id,
    who: list.length === 1 ? "this account" : `each of the ${list.length} ${p.account!.display_name} accounts`,
    window: w.label, cap: capSummary(list, w.label).shared,
    used: Math.max(0, ...list.map((a) => a.usage?.windows.find((x) => x.label === w.label)?.used ?? 0)),
    resets_at: w.resets_at,
  });
  return (
    <>
      <div className="settings-head">
        <div className="settings-head-main settings-title">Accounts</div>
        <Button onClick={() => revealed ? setRevealed(false) : setRevealWarning(true)}>{revealed ? I.eyeOff(13) : I.eye(13)}{revealed ? "Hide" : "Reveal"}</Button>
        <Button disabled={busy} onClick={refresh}>{busy ? <Loader size={13} /> : <RefreshCw size={13} />}Refresh</Button>
      </div>
      {shownProviders(providers).filter((p) => p.account).map((p) => {
        const list = accts.filter((a) => a.kind === p.id).sort((a, b) => b.priority - a.priority);
        const net = netWindows(list);
        const avail = list.filter((a) => a.available).length;
        return (
          <div key={p.id} className="sgroup account-group">
            <div className="srowx">
              <ProviderIcon provider={p.id} size={18} />
              <div style={{ flex: 1 }}>
                <div style={{ fontWeight: 600 }}>{p.account!.display_name}</div>
                <div className="desc">{list.length ? `${avail} of ${list.length} available` : "No accounts yet"}</div>
              </div>
              {net.length > 0 && net.map((w) => {
                const caps = capSummary(list, w.label);
                return (
                  <Fragment key={w.label}>
                    <UsageBar w={{ ...w, label: "All " + w.label }} compact cap={caps.shared} onCap={() => capPool(p, list, w)}
                      hint={`Double-click to cap every ${p.account!.short_name} account's ${w.label} window`} />
                    {/* Caps that do not agree cannot be drawn as one line, so the
                        header says how many accounts carry one instead. */}
                    {!caps.shared && caps.n > 0 && <Tooltip content={`${caps.n} of ${list.length} accounts have a cap on this window · double-click a row's line to see it`}><span className="capnote">{caps.n} capped</span></Tooltip>}
                  </Fragment>
                );
              })}
              {/* The terms gate makes the extra step clear before the connect
                  dialog, while keeping the account action itself labelled Add. */}
              <Button onClick={() => setDialog({ id: p.id, gate: p.account!.terms_gate ? "terms" : null })}>{I.plus(12)}Add</Button>
            </div>
            {list.map((a, i) => <AccountRow key={a.id} a={a} i={i} n={list.length} revealed={revealed} onCap={capAccount} />)}
          </div>
        );
      })}
      {shownProviders(providers).some((p) => p.id === "antigravity") && <AntigravityAccountCard />}
      {revealWarning && <Modal onClose={() => setRevealWarning(false)} style={{ width: "min(480px, 100%)" }}>
        <div className="mh"><CircleAlert size={16} /><span style={{ flex: 1 }}>Reveal account details?</span><IconButton label="Close" onClick={() => setRevealWarning(false)}>{I.close()}</IconButton></div>
        <div className="mb">This will display account emails and partial API keys beside account names. Anyone viewing your screen, screenshots, or screen sharing will be able to see them. Full keys are never displayed. Emails are only available when recorded by the login.</div>
        <div className="mf"><div style={{ flex: 1 }} /><Button variant="ghost" onClick={() => setRevealWarning(false)}>Cancel</Button><Button variant="primary" onClick={() => { setRevealWarning(false); setRevealed(true); }}>Reveal details</Button></div>
      </Modal>}
      {activeProvider && (dialog!.gate === "terms"
        ? <TermsGate
            gate={activeProvider.account!.terms_gate!}
            provider={activeProvider.id}
            onAccept={() => setDialog({ id: activeProvider.id, gate: "ban" })}
            onClose={close}
          />
        : dialog!.gate === "ban"
          ? <BanWarningGate
              onAccept={() => setDialog({ id: activeProvider.id, gate: null })}
              onClose={close}
            />
          : <ImportDialog provider={activeProvider} onClose={close} />)}
      {cap && <CapDialog target={cap} onClose={() => setCap(null)} />}
    </>
  );
}

// routing

export function ModelSelect({ value, onChange, placeholder, none, style = { flex: 1, minWidth: 0 } }: { value: string; onChange: (v: string) => void; placeholder?: string; none?: string; style?: React.CSSProperties }) {
  const models = useStore((s) => s.models);
  const provs = useStore((s) => s.providers);
  useStore((s) => s.accounts);
  const live = shownProviders(provs).filter((p) => p.enabled && isConnected(p));
  const opts: Opt[] = [];
  if (none !== undefined) opts.push({ value: "", label: none });
  opts.push(...live.flatMap((p) => models.filter((m) => m.provider === p.id && m.enabled !== false).map((m) => ({
    value: m.id, label: m.name, group: none === undefined ? p.name : undefined,
    hint: p.account ? "all accounts" : m.input_price || m.output_price ? `$${m.input_price}/${m.output_price}` : undefined,
  }))));
  if (value && !opts.some((o) => o.value === value)) {
    const known = models.find((x) => x.id === value);
    opts.unshift({ value, label: known?.name ?? modelInfo(value).name, hint: provs.some((p) => p.id === value.split("/")[0] && (isConnected(p) || p.custom)) ? "custom id" : "not connected" });
  }
  return <Dropdown value={value} options={opts} onChange={onChange} placeholder={placeholder} style={style} />;
}

function RouteCard({ r, onChange, onDelete }: { r: Route; onChange: (r: Route) => void; onDelete: () => void }) {
  const [del, setDel] = useState(false);
  const setStep = (i: number, v: string) => onChange({ ...r, steps: r.steps.map((s, j) => (j === i ? v : s)) });
  const move = (i: number, d: number) => {
    const s = [...r.steps];
    const j = i + d;
    if (j < 0 || j >= s.length) return;
    // Both ends are in range by the line above, so the swap can't drop an entry.
    [s[i], s[j]] = [s[j]!, s[i]!];
    onChange({ ...r, steps: s });
  };
  return (
    <div className="sgroup routecard">
      <div className="srowx">
        <Input className="input" style={{ flex: 1, height: 28, fontWeight: 600, background: "transparent", border: 0, padding: 0 }} value={r.name} onChange={(e) => onChange({ ...r, name: e.currentTarget.value })} />
        <span className="mono" style={{ fontSize: 10.5, color: "var(--dim)" }}>route/{r.id}</span>
        {del ? <Button variant="primary" style={{ height: 24 }} onClick={onDelete}>Delete</Button> : <IconButton label="Delete route" style={{ width: 24, height: 24 }} onClick={() => setDel(true)}>{I.trash()}</IconButton>}
      </div>
      <div className="srowx route-row">
        <span className="route-label">Applies to</span>
        <div className="route-targets">
          <Segmented
            label="Which models use this route"
            options={[
              { value: "all", label: "All models", hint: "Every model uses this route unless it has a more specific one" },
              { value: "heads", label: "Specific models", hint: "Only the models listed here" },
            ]}
            value={r.all ? "all" : "heads"}
            onChange={(v) => onChange({ ...r, all: v === "all" })}
            style={{ width: 220, flex: "none" }}
          />
          {!r.all && (
            <>
              {(r.heads ?? []).map((m) => (
                <RemovableChip key={m} label={modelInfo(m).name} onRemove={() => onChange({ ...r, heads: r.heads.filter((x) => x !== m) })}>{modelInfo(m).name}</RemovableChip>
              ))}
              <div className="route-add-model"><ModelSelect value="" placeholder="Add model" onChange={(v) => v && !r.heads?.includes(v) && onChange({ ...r, heads: [...(r.heads ?? []), v] })} /></div>
              {!r.heads?.length && <span className="desc">Add a model to use this route.</span>}
            </>
          )}
        </div>
      </div>
      <div className="srowx route-row">
        <span className="route-label">Try in order</span>
        <div className="route-chain">
          <div className="route-origin">Selected model</div>
          {r.steps.map((s, i) => (
            <div key={i} className="route-step">
              <span className="route-step-number">{i + 1}.</span>
              <ModelSelect value={s} onChange={(v) => setStep(i, v)} />
              <IconButton label="Move fallback up" disabled={i === 0} style={{ width: 24, height: 24 }} onClick={() => move(i, -1)}><ChevronUp size={12} /></IconButton>
              <IconButton label="Move fallback down" disabled={i === r.steps.length - 1} style={{ width: 24, height: 24 }} onClick={() => move(i, 1)}><ChevronDown size={12} /></IconButton>
              <IconButton label="Remove fallback" style={{ width: 24, height: 24 }} onClick={() => onChange({ ...r, steps: r.steps.filter((_, j) => j !== i) })}>{I.trash()}</IconButton>
            </div>
          ))}
          <div className="route-add-fallback" style={{ marginLeft: r.steps.length ? 24 : 0 }}><ModelSelect value="" placeholder="Add fallback model…" onChange={(v) => v && !r.steps.includes(v) && onChange({ ...r, steps: [...r.steps, v] })} /></div>
        </div>
      </div>
      <div className="srowx route-row route-outcome">
        <span className="route-label">If none work</span>
        <Segmented label="When all models are unavailable" options={[{ value: "pause", label: "Pause", hint: "Wait for a model to become available" }, { value: "fail", label: "Fail", hint: "Mark the task as failed" }]} value={r.on_exhausted} onChange={(v) => onChange({ ...r, on_exhausted: v })} style={{ width: 170 }} />
      </div>
    </div>
  );
}

export function RoutingTab() {
  const routes = useStore((s) => s.settings?.routes ?? NO_ROUTES);
  const save = (next: Route[]) => saveSettings({ routes: next });
  const add = () => {
    let id = "chain";
    let n = 1;
    while (routes.some((r) => r.id === id)) id = "chain-" + ++n;
    save([...routes, { id, name: n > 1 ? `Route ${n}` : "New route", heads: [], all: routes.length === 0, steps: [], on_exhausted: "pause" }]);
  };
  return (
    <>
      <div className="settings-head">
        <div className="settings-head-main">
          <div className="settings-title">Routing</div>
          {!!routes.length && <div className="settings-lead">Try backup models after the selected model.</div>}
        </div>
        <Button onClick={add}>{I.plus(12)}New route</Button>
      </div>
      {routes.map((r, i) => (
        <RouteCard key={r.id} r={r} onChange={(nr) => save(routes.map((x, j) => (j === i ? nr : x)))} onDelete={() => save(routes.filter((_, j) => j !== i))} />
      ))}
      {!routes.length && <div className="sgroup routing-empty"><div>No fallback routes</div><div className="desc">Chats pause if their model is unavailable.</div></div>}
    </>
  );
}

// subagents

const TOOLSETS: [AgentDef["tools"], string, string][] = [["all", "All tools", "Edit files and run commands"], ["no_shell", "No shell", "Can edit files, can't run commands"], ["read_only", "Read-only", "Search and read only"]];

function AgentDialog({ init, onClose }: { init: AgentDef | null; onClose: () => void }) {
  const [d, setD] = useState<AgentDef>(init ?? { id: "", name: "", description: "", prompt: "", model: "", tools: "all", color: "#a78bfa", builtin: false, source: "", inject_instructions: true });
  // Tool restrictions, held as editable rows and folded back to the wire shape
  // on save. Empty means this agent keeps whatever `tools` above says.
  const [rows, setRows] = useState<GroupRow[]>(() => groupsOf(init ?? {}));
  // This subagent may run on a model of its own, and a different model takes a
  // different set of levels: an empty pick means the chat's model decides.
  const effMi = modelInfo(d.model || get().home.model);
  // `effortLadder` returns the rungs in drawn order — Low first, Max last.
  const effortOpts = effortLadder(effMi);
  const save = async () => {
    const s = get().settings!;
    const id = d.id || d.name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
    if (!id || !d.description.trim()) return flash("Give it a name and a description");
    // `explore` + `general` are built in: edit them from their own row instead.
    if (!init && isRequiredAgent(id)) return flash("Explore and General are built in — edit them from their own row");
    // Catch a bad fileRegex here so the user sees it in the dialog, rather than
    // the settings write failing with a message after the dialog has closed.
    const gerr = validateGroups(rows);
    if (gerr) return flash(gerr);
    const next = { ...d, id, name: d.name.trim() || id, groups: toToolGroups(rows) };
    await saveSettings({ agents: [...s.agents.filter((a) => a.id !== id), next] });
    set({ agents: await api.agents() });
    if (!init) {
      const def = withRequiredAgents(s.default_agents.includes(id) ? s.default_agents : [...s.default_agents, id]);
      await saveSettings({ default_agents: def });
      set((st) => ({ home: { ...st.home, agents: def } }));
    }
    onClose();
  };
  return (
    <Modal onClose={onClose} style={{ width: "min(600px, 100%)" }}>
        <div className="mh"><Bot aria-hidden="true" size={16} color="var(--mut2)" /><span style={{ flex: 1 }}>{init ? `Edit ${init.name}` : "New subagent"}</span><IconButton label="Close" onClick={onClose}>{I.close()}</IconButton></div>
        <div className="mb">
          <div style={{ display: "flex", gap: 10 }}>
            <div className="field" style={{ flex: 1 }}><label>Name</label><Input autoFocus value={d.name} placeholder="Code reviewer" onChange={(e) => setD({ ...d, name: e.currentTarget.value })} disabled={!!init?.builtin} /></div>
            <div className="field" style={{ flex: "none" }}><label>Color</label><ColorPicker value={d.color} onChange={(color) => setD({ ...d, color })} style={{ maxWidth: 228 }} /></div>
          </div>
          <div className="field"><label>When to use it <span style={{ color: "var(--dim)" }}>the main agent reads this to decide</span></label><Input value={d.description} placeholder="Reviews a diff for bugs and risky changes before commit" onChange={(e) => setD({ ...d, description: e.currentTarget.value })} /></div>
          <div className="field"><label>Instructions</label><TextArea style={{ height: 130, padding: 10, resize: "vertical", lineHeight: 1.5 }} value={d.prompt} placeholder="You are a meticulous reviewer. Read the changed files, look for…" onChange={(e) => setD({ ...d, prompt: e.currentTarget.value })} /></div>
          <div className="field"><label>Tools</label>
            <Segmented label="Subagent tools" options={TOOLSETS.map(([value, label, hint]) => ({ value, label, hint }))} value={d.tools} onChange={(tools) => setD({ ...d, tools })} style={{ width: "100%" }} />
          </div>
          <div className="field"><label>File restrictions <span style={{ color: "var(--dim)" }}>optional — limit this agent's edits to matching paths</span></label>
            {rows.map((r, i) => (
              <div key={i} style={{ display: "flex", gap: 6, alignItems: "center", marginBottom: 6 }}>
                <Dropdown value={r.name} options={GROUP_NAMES.map((n) => ({ value: n, label: n }))} onChange={(name) => setRows(rows.map((x, j) => (j === i ? { ...x, name } : x)))} style={{ width: 108, flex: "none" }} />
                {r.name === "edit"
                  ? <Input className="input" aria-label="fileRegex" value={r.fileRegex} placeholder="\\.(md|mdx)$" onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, fileRegex: e.currentTarget.value } : x)))} style={{ flex: 1, minWidth: 0 }} />
                  : <span style={{ flex: 1, fontSize: 11.5, color: "var(--dim)" }}>unrestricted</span>}
                <Input className="input" aria-label="description" value={r.description} placeholder="docs only" onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, description: e.currentTarget.value } : x)))} style={{ width: 130, flex: "none" }} />
                <IconButton label="Remove restriction" style={{ width: 24, height: 24 }} onClick={() => setRows(rows.filter((_, j) => j !== i))}>{I.close()}</IconButton>
              </div>
            ))}
            <TextButton onClick={() => setRows([...rows, { name: "edit", fileRegex: "", description: "" }])}>+ Add file restriction</TextButton>
            <div className="desc">A pattern is matched against the file's path relative to the project root. Only the <span className="mono">edit</span> group can carry one — e.g. a docs writer that may only touch <span className="mono">*.md</span>. Invalid patterns are refused when you save.</div>
          </div>
          <div className="srowx" style={{ padding: "2px 0", minHeight: 0 }}>
            <div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Inject project instructions</div><div className="desc">Include project instructions in this agent's context.</div></div>
            <Switch label="Inject project instructions" checked={d.inject_instructions !== false} onChange={(checked) => setD({ ...d, inject_instructions: checked })} />
          </div>
          <div className="field"><label>Reasoning effort</label>
            {/* This subagent may run on a different model than the chat, and a
                different model takes a different set of levels. */}
            <Segmented label="Subagent reasoning effort" options={[{ value: -1, label: "Same as chat", hint: d.tools === "read_only" ? "The chat's effort, at most Medium" : "Whatever the chat uses" }, ...effortOpts]}
              value={d.effort ?? -1} onChange={(v) => setD({ ...d, effort: v < 0 ? null : v })} style={{ width: "100%" }} />
          </div>
          <div className="field"><label>Model</label>
            <ModelSelect value={d.model} none="Same as task" onChange={(v) => setD({ ...d, model: v })} style={{ width: "100%" }} />
          </div>
        </div>
        <div className="mf">
          {init && !init.builtin && <Button variant="ghost" style={{ color: "#ff8a8a" }} onClick={async () => {
            const s = get().settings!;
            const defaults = withRequiredAgents(s.default_agents.filter((id) => id !== init.id));
            if (await saveSettings({ agents: s.agents.filter((a) => a.id !== init.id), default_agents: defaults })) {
              set((st) => ({ agents: st.agents.filter((a) => a.id !== init.id), home: { ...st.home, agents: withRequiredAgents(st.home.agents.filter((id) => id !== init.id)) } }));
              set({ agents: await api.agents() });
              onClose();
            }
          }}>Delete</Button>}
          {init?.builtin && get().settings!.agents.some((a) => a.id === init.id) && (
            <Button variant="ghost" onClick={async () => {
              await saveSettings({ agents: get().settings!.agents.filter((a) => a.id !== init.id) });
              set({ agents: await api.agents() });
              onClose();
            }}>Reset to built-in</Button>
          )}
          <div style={{ flex: 1 }} />
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" onClick={save}>Save</Button>
        </div>
    </Modal>
  );
}

export function AgentsTab() {
  const agents = useStore((s) => s.agents);
  const defaults = useStore((s) => s.settings?.default_agents ?? NO_IDS);
  // The built-ins are always present, so a failure to list the project's own
  // agents used to look like "no subagents here" — which is a different claim.
  const catalogError = useStore((s) => s.catalogError);
  const [dialog, setDialog] = useState<null | { init: AgentDef | null }>(null);
  const toggleDefault = async (id: string) => {
    // `explore` + `general` are always on: ignore attempts to toggle them off.
    if (isRequiredAgent(id)) {
      if (!defaults.includes(id)) {
        const repaired = withRequiredAgents(defaults);
        await saveSettings({ default_agents: repaired });
        set((st) => ({ home: { ...st.home, agents: repaired } }));
      } else {
        flash("Explore and General are always on");
      }
      return;
    }
    const next = withRequiredAgents(defaults.includes(id) ? defaults.filter((x) => x !== id) : [...defaults, id]);
    await saveSettings({ default_agents: next });
    set((st) => ({ home: { ...st.home, agents: next } }));
  };
  return (
    <>
      <div className="settings-head">
        <div className="settings-head-main">
          <div className="settings-title">Subagents</div>
        </div>
        <Button onClick={() => setDialog({ init: null })}>{I.plus(12)}New subagent</Button>
      </div>
      <div className="sgroup">
        {agents.map((d) => (
          <Pressable key={d.id} className="srowx agentrow" aria-disabled={d.source === "project"} onClick={() => d.source !== "project" && setDialog({ init: d })}>
            <div style={{ flex: 1, minWidth: 0 }}>
              <div style={{ fontWeight: 500, display: "flex", gap: 7, alignItems: "center" }}>
                {d.name}<span className="mono" style={{ fontSize: 10.5, color: "var(--dim)" }}>{d.id}</span>
                {d.builtin && <span className="branchtag" style={{ height: 16, fontSize: 9.5 }}>built-in</span>}
                {d.source === "project" && <span className="branchtag" style={{ height: 16, fontSize: 9.5 }}>project</span>}
              </div>
              <div className="desc" style={{ whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{d.description}{d.model && <> · {modelInfo(d.model).name}</>}</div>
              {(() => {
                // Keep file restrictions discoverable without changing the
                // description, which is user-authored prose.
                const rest = restrictionLabel(groupsOf(d));
                if (!rest) return null;
                return <div className="desc" style={{ display: "flex", gap: 8, color: "var(--mut3)", marginTop: 2 }}>
                  <span title="This agent can only edit these paths">✎ {rest}</span>
                </div>;
              })()}
            </div>
            {isRequiredAgent(d.id)
              ? <Switch label={`${d.name} is always on`} hint="Always on · built-in" small checked />
              : <Switch label={`${defaults.includes(d.id) ? "Disable" : "Enable"} ${d.name} by default`} hint="On by default in new chats" small checked={defaults.includes(d.id)} onChange={(_, e) => { e.stopPropagation(); void toggleDefault(d.id); }} />}
          </Pressable>
        ))}
        {catalogError && (
          <div className="empty" role="alert" style={{ color: "#ff8a8a" }}>Couldn't list this project's subagents · {catalogError}</div>
        )}
      </div>
      <div style={{ fontSize: 11.5, color: "var(--mut3)" }}>The switch sets which subagents new chats start with. Explore and General are built in and always on. Change a chat's set any time from the composer.</div>
      {dialog && <AgentDialog init={dialog.init} onClose={() => setDialog(null)} />}
    </>
  );
}
