
import { createContext, memo, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { ContainerChip } from "./Workspaces";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import ReactMarkdown from "react-markdown";
import { copyText, mdComponents, MD_PLUGINS as MD_PLUGINS_SHARED, MdInline, replyUrlTransform } from "./mdx";
import { RecoverySummary } from "./RecoverySummary";
import { AssistantMarkdown } from "./Visualization";
import { ArtifictWorkspace, studioHandoff } from "./ArtifictWorkspace";
import { InlineArtifictCard, hasArtifictCardSlot, type ArtifictWorkspaceSelection } from "./InlineArtifictCard";
import { BrowserPanel } from "./BrowserPanel";
import { FindBar } from "./Find";
import { openRunTimeline } from "./RunTimeline";
import { api, ago, effortLabel, fmt$, Bg, FileRef, Item, ItemData, ToolAction, ToolInput, SubInfo, TaskSummary, xDepth, xOn } from "../api";
import { flash, get, go, isPhoneClient, loadPreviousHistory, loadSubTranscript, chatModel, modelColor, modelInfo, newerSummary, openTask, requestPause, set, subModel, useStore, useThrottled, zoom } from "../store";
import { Composer, humanBytes, MAX_FILES, MAX_IMAGE, PendingStar } from "./Composer";
import { openSwap } from "./ModelSwap";
import { Annotations, AttachmentChips, SentAnnotationMessage, splitContextBlocks, stripHarnessBlocks } from "./SelectionNote";
import { heldByGlobal, isPaused } from "./Paused";
import { enterSendsMessage, messageSendKey } from "../keys";
import { I, StatusIcon, chatStatus } from "./icons";
import { Question } from "./Question";
import { AskNonBlockingList } from "./AskNonBlocking";
import { AgentNoticeList } from "./AgentNotice";
import { AnchoredPanel, AgentColor, Tooltip, Loader, Button, IconButton, Input, MenuButton, MenuRow, MenuScrim, ctxPlace, menuLayer, Pressable, RoundButton, TextArea, Thumb, Kbd, useAnchoredPanel, cssUrl } from "./primitives";
import { TodoChip } from "./session/TodoPanel";
import { QueueList } from "./session/QueueList";
import { Rewind } from "./Rewind";
import { swarmCounts, swarmGlow, swarmState } from "./session/Swarm";
import { ContextRing } from "./session/ContextRing";
import { Bot, Brain, Check, CircleAlert, CircleCheckBig, CircleQuestionMark, Clock, CornerDownLeft, ExternalLink, FilePenLine, FileText, GitBranch, GitFork, Globe, Image, Info, Megaphone, MessagesSquare, Monitor, Network, Plug, Search, Send, Square, Terminal, Webhook, X } from "lucide-react";

const li = (C: typeof Search, size = 14) => <C aria-hidden="true" size={size} strokeWidth={1.7} />;

const G = "var(--mut)";
const NO_BG: never[] = [];
const NO_ITEMS: Item[] = [];

// Hoisted: rebuilding these objects on every render throws away react-markdown's
// own identity checks, and the streaming message re-renders on every token.
// Both come from `mdx` so every markdown surface in the app shares one stack.
const MD_PLUGINS = MD_PLUGINS_SHARED;
const MD_COMPONENTS = mdComponents;

/** How often a streaming reply's markdown is re-parsed. Parsing is the most
 *  expensive thing the chat does per frame, and the text only ever grows, so
 *  re-parsing on every delta means parsing the whole answer again ~60 times a
 *  second. At 120ms it still reads as typing, at a cost per second rather than
 *  per token. */
const MD_THROTTLE_MS = 120;

const Md = memo(function Md({ text, interactive = false, streaming = false }: { text: string; interactive?: boolean; streaming?: boolean }) {
  const shown = useThrottled(text, MD_THROTTLE_MS);
  // Raw deltas still render Md. Reuse the subtree until the throttled text (or
  // rendering mode) changes so those renders do not re-run the markdown parser.
  return useMemo(() => (
    <div className="md">
      {interactive ? <AssistantMarkdown text={shown} streaming={streaming} /> : <ReactMarkdown remarkPlugins={MD_PLUGINS} components={MD_COMPONENTS} urlTransform={replyUrlTransform}>{shown}</ReactMarkdown>}
    </div>
  ), [shown, interactive, streaming]);
});

/** An agent reply with a hover copy button (copies the markdown source); right-click shows which model wrote it. */
function Reply({ it, streaming = false }: { it: Item; streaming?: boolean }) {
  const [ctx, setCtx] = useState<{ x: number; y: number; sel: string } | null>(null);
  const text = it.text;
  return (
    <div className="reply" onContextMenu={(e) => { e.preventDefault(); setCtx({ x: e.clientX, y: e.clientY, sel: window.getSelection()?.toString() ?? "" }); }}>
      <Md text={text} interactive streaming={streaming} />
      <div className="msgacts"><span onClick={() => copyText(text)}>Copy</span></div>
      {ctx && <ReplyMenu it={it} x={ctx.x} y={ctx.y} sel={ctx.sel} close={() => setCtx(null)} />}
    </div>
  );
}

function ReplyMenu({ it, x, y, sel, close }: { it: Item; x: number; y: number; sel: string; close: () => void }) {
  const model: string | undefined = it.data?.model;
  const run = (f: () => void) => () => { close(); f(); };
  const ref = useRef<HTMLDivElement>(null);
  const [h, setH] = useState(76);
  useLayoutEffect(() => { setH(ref.current?.offsetHeight || 76); }, []);
  return (
    <div style={menuLayer(zoom())}>
      <MenuScrim layer={90} onClick={close} onContextMenu={(e) => { e.preventDefault(); close(); }} />
      {/* The chat is inside the zoomed app root, so this menu carries the same zoom:
          a right-click near the bottom of a reply has to open upwards, not off-screen. */}
      <div ref={ref} className="pop ctx" style={{ ...ctxPlace(x, y, 228, h, zoom()), overflowY: "auto" }}>
        <div className="mmodel" aria-disabled="true">
          <i className="tdotc" style={{ background: model ? modelColor(model) : "var(--dim)" }} />
          <span className="mm-name">{model ? modelInfo(model).name : "Unknown model"}</span>
          {it.data?.via && <span className="mm-via">{it.data.via}</span>}
        </div>
        <div className="msep" />
        {sel.trim() && <MenuRow onClick={run(() => copyText(sel, "Selection copied"))}><span className="cico">{I.copy(12)}</span><span style={{ flex: 1 }}>Copy selection</span></MenuRow>}
        <MenuRow onClick={run(() => copyText(it.text, "Message copied"))}><span className="cico">{I.copy(12)}</span><span style={{ flex: 1 }}>Copy message</span></MenuRow>
      </div>
    </div>
  );
}

function rel(p: string, cwd: string) {
  const n = (x: string) => x.replace(/\\/g, "/");
  const a = n(p), b = n(cwd).replace(/\/$/, "");
  return a.toLowerCase().startsWith(b.toLowerCase() + "/") ? a.slice(b.length + 1) : a;
}

/** Where an `http_request` went, short enough for a row: host and path, with
 *  a query string folded to `?…`. The backend has already redacted secrets in
 *  the stored url, but the query is still mostly noise in a one-line label —
 *  the full request is in the row's output. A url that does not parse (the
 *  model can send anything) is shown as it came. */
function httpTarget(url: string | undefined): string {
  const raw = String(url ?? "").trim();
  if (!raw) return "";
  try {
    const u = new URL(raw);
    const path = u.pathname === "/" ? "" : u.pathname;
    return `${u.host}${path}${u.search ? "?…" : ""}`;
  } catch {
    return raw;
  }
}

/** The status code an `http_request` result opens with (`HTTP 201 Created · …`). */
function httpStatus(d: ItemData): string | undefined {
  if (d.name !== "http_request") return undefined;
  return String(d.output ?? "").match(/^HTTP (\d{3})\b/)?.[1];
}

function toolView(d: ItemData, cwd: string) {
  const name: string = d.name ?? "";
  const inp: ToolInput = d.input ?? {};
  switch (name) {
    case "read_file": return { icon: li(FileText), color: G, verb: "Read", target: inp.path ? rel(inp.path, cwd) : "" };
    case "glob": return { icon: li(Search), color: G, verb: "Find", target: inp.pattern ?? "" };
    case "grep": return { icon: li(Search), color: G, verb: "Search", target: (inp.pattern ?? "") + (inp.glob ? `  in ${inp.glob}` : "") };
    // Code intelligence: the operation, then where (`src/a.rs:12 · foo`) or what (a workspace query).
    case "lsp": {
      const where = inp.path ? rel(String(inp.path), cwd) + (inp.line ? `:${String(inp.line)}` : "") : String(inp.query ?? "");
      return { icon: li(Network), color: G, verb: String(inp.operation ?? "lsp").replace(/_/g, " "), target: where + (inp.symbol ? ` · ${String(inp.symbol)}` : "") };
    }
    case "list_agents": return { icon: li(Bot), color: G, verb: "Agents", target: "list" };
    case "send_message": return { icon: li(Send), color: G, verb: "Message", target: `→ ${inp.to ?? ""}  ${String(inp.message ?? "").slice(0, 80)}` };
    // The two cross-chat tools. Kept as their own rows rather than folded into
    // the generic fallback, which would read the whole input blob as the target.
    case "list_chats": return { icon: li(MessagesSquare), color: G, verb: "Chats", target: inp.query ? `search “${inp.query}”` : "list" };
    case "message_chat": return { icon: li(Send), color: G, verb: "Message chat", target: `→ ${inp.to ?? ""}  ${String(inp.message ?? "").slice(0, 80)}` };
    case "wait": return { icon: li(Clock), color: G, verb: "Wait", target: (Array.isArray(inp.ids) && inp.ids.length ? inp.ids.join(", ") : "background jobs") + (inp.mode === "all" ? " · all" : " · any") };
    case "multi_edit": return { icon: li(FilePenLine), color: G, verb: "Edit", target: (inp.path ? rel(inp.path, cwd) : "") + (Array.isArray(inp.edits) ? `  ×${inp.edits.length}` : "") };
    case "edit_file": return { icon: li(FilePenLine), color: G, verb: "Edit", target: inp.path ? rel(inp.path, cwd) : "" };
    case "write_file": return { icon: li(FilePenLine), color: G, verb: "Write", target: inp.path ? rel(inp.path, cwd) : "" };
    case "bash": return { icon: li(Terminal), color: G, verb: inp.run_in_background ? "Start" : "Run", target: inp.command ?? "" };
    case "bash_output": return { icon: li(Terminal), color: G, verb: "Check", target: inp.id ?? "" };
    case "kill_bash": return { icon: I.close(13), color: G, verb: "Stop", target: inp.id ?? "" };
    case "web_fetch": return { icon: li(Webhook), color: G, verb: "Fetch", target: inp.url ?? "" };
    case "web_search": return { icon: li(Search), color: G, verb: "Search", target: Array.isArray(inp.queries) ? inp.queries.join(" · ") : (inp.query ?? "") };
    case "web_read_many": return { icon: li(Webhook), color: G, verb: "Read pages", target: (Array.isArray(inp.urls) ? `${inp.urls.length} links` : "") };
    case "http_request": return { icon: li(Globe), color: G, verb: (String(inp.method ?? "").trim() || "GET").toUpperCase(), target: httpTarget(inp.url) };
    case "memory": {
      const act = String(inp.action ?? "");
      if (act === "save") return { icon: li(Brain), color: G, verb: "Remember", target: inp.name ? `${inp.kind ?? ""} · ${inp.name}` : (inp.summary ?? "") };
      if (act === "forget") return { icon: li(Brain), color: G, verb: "Forget", target: inp.file ?? inp.name ?? "" };
      return { icon: li(Brain), color: G, verb: "Recall", target: inp.file ?? inp.query ?? "index" };
    }
    case "view_image": return { icon: li(Image), color: G, verb: "View", target: inp.path ? rel(inp.path, cwd) : "" };
    case "screenshot": return { icon: li(Monitor), color: G, verb: "Shot", target: inp.monitor != null ? `monitor ${inp.monitor}` : "screen" };
    case "browser": {
      const act = String(inp.action ?? "");
      const what: Record<string, string> = { navigate: "Open", read: "Read", screenshot: "Shot", click: "Click", type: "Type", js: "Run JS", back: "Back", info: "Look" };
      // The page the call ended on, which the result line carries, reads better
      // than repeating the input — and for a click it is the only place the
      // destination is visible at all.
      const landed = String(d.output ?? "").match(/Now at: (\S+)/)?.[1];
      const target = act === "navigate" ? (inp.url ?? "") : act === "click" ? (inp.selector ?? "") : act === "type" ? `“${String(inp.text ?? "").slice(0, 40)}”` : act === "js" ? String(inp.js ?? "").slice(0, 60) : (landed ?? "");
      return { icon: li(Globe), color: G, verb: what[act] ?? "Browser", target };
    }
    case "github": {
      const a = String(inp.action ?? "").replace(/_/g, " ");
      const ref = inp.number != null ? `#${inp.number}` : inp.id != null ? `run ${inp.id}` : inp.title ?? inp.query ?? inp.path ?? "";
      return { icon: li(GitBranch), color: G, verb: "GitHub", target: [a, ref, inp.repo ? `· ${inp.repo}` : ""].filter(Boolean).join(" ") };
    }
    case "linear": case "jira": case "slack": case "notion": case "gitlab": {
      // The service plugins: name the service, the action and whatever the
      // call is about (an issue key, a channel, a query, a page).
      const svc: Record<string, string> = { linear: "Linear", jira: "Jira", slack: "Slack", notion: "Notion", gitlab: "GitLab" };
      const a = String(inp.action ?? "").replace(/_/g, " ");
      const about = [inp.id, inp.key, inp.issue, inp.iid != null ? `!${String(inp.iid)}` : undefined, inp.channel, inp.page_id, inp.database_id, inp.query, inp.jql, inp.title, inp.path, inp.project]
        .find((v) => typeof v === "string" || typeof v === "number");
      return { icon: li(name === "slack" ? MessagesSquare : name === "gitlab" ? GitBranch : Plug), color: G, verb: svc[name] ?? name, target: [a, about != null ? String(about).slice(0, 80) : ""].filter(Boolean).join(" ") };
    }
    case "computer": {
      const acts: ToolAction[] = Array.isArray(inp.actions) ? inp.actions : [];
      const one = (a: ToolAction) => {
        const t = String(a.type ?? "");
        if (t === "type") return `type “${String(a.text ?? "").slice(0, 30)}”`;
        // A zoomed screenshot is its region, a plain one is the display it came
        // from: neither has the x/y a click has, so they read on their own.
        if (t === "screenshot") {
          return a.width != null ? `zoom ${a.width}×${a.height} @ ${a.x},${a.y}` : a.monitor != null ? `shot monitor ${a.monitor}` : "shot";
        }
        if (a.keys?.length) return `${t.replace(/_/g, " ")} ${a.keys.join("+")}`;
        if (a.x != null) return `${t.replace(/_/g, " ")} ${a.x},${a.y}`;
        return t.replace(/_/g, " ");
      };
      return { icon: li(Monitor), color: G, verb: "Computer", target: acts.map(one).join(" → ") };
    }
    default:
      if (name.startsWith("mcp__")) { const [, srv, tool = ""] = name.split("__"); return { icon: li(Plug), color: G, verb: srv ?? "", target: tool }; }
      return { icon: li(Info), color: G, verb: name, target: "" };
  }
}

/** Opens the chat's shared Browser view; `null` while the browser plugin is off. */
const OpenBrowserView = createContext<(() => void) | null>(null);

/** The page a `browser` call ended on, from the result line, or the one it was
 *  pointed at. Only real web urls: a `file://` is a path on this machine and
 *  belongs in a file manager, not a browser tab. */
function browserPage(d: ItemData): string | null {
  const landed = String(d.output ?? "").match(/Now at: (\S+)/)?.[1] ?? "";
  const asked = d.input?.action === "navigate" ? String(d.input?.url ?? "") : "";
  for (const u of [landed, asked]) {
    if (/^https?:\/\/\S+$/i.test(u)) return u;
  }
  return null;
}

/** One tool call. Memoized, and its body only exists while the fold is open:
 *  a collapsed fold is a 0fr grid, so a long transcript used to keep every
 *  call's output — up to 400 lines each — mounted and reconciled per token.
 *  Exported so a render test can pin the glyph, which `FeedRows` cannot reach:
 *  a folded group does not mount its rows at all. */
export const ToolItem = memo(function ToolItem({ it, cwd, held }: { it: Item; cwd: string; held?: boolean }) {
  const openBrowserView = useContext(OpenBrowserView);
  const d = it.data ?? {};
  const v = toolView(d, cwd);
  const hasDiff = Array.isArray(d.diff) && d.diff.length > 0;
  const images: string[] = Array.isArray(d.images) ? d.images.filter((x) => typeof x === "string") : [];
  const imgs = images.map((src, i) => ({ src, alt: `${v.verb} image ${i + 1}` }));
  // The backend stops keeping the blobs on older rows to bound what a long
  // computer-use session holds (see `prune_item_images`), and leaves the count
  // behind. Say so rather than showing a row that looks like it never took a
  // screenshot — the pictures are still on disk in the tool result, so this is
  // about the transcript, not about losing them.
  const dropped = typeof d.images_dropped === "number" ? d.images_dropped : 0;
  const [manual, setManual] = useState<boolean | null>(null);
  // Only screenshots open by themselves. A diff does not: an edit's body is
  // most of the row and a run that touches a dozen files unfurls a dozen slabs
  // of green and red into the middle of the story, which is the opposite of what
  // a transcript is for. The header already says what the call did and carries
  // the +/− counts, so the collapse loses nothing — one click brings it back.
  const autoOpen = images.length > 0;
  const open = manual ?? (autoOpen && d.status !== "denied");
  const status = d.status;
  const meta = status === "running" ? "" : status === "denied" ? "denied" : d.meta ?? httpStatus(d) ?? (status === "error" ? "error" : "");
  const diffCounts = hasDiff ? meta.match(/^\+(\d+)\s+−(\d+)$/) : null;
  // The page a browser call ended on, so the user can follow the agent into
  // whatever it just looked at. Their choice: nothing opens unless they click.
  const page = d.name === "browser" ? browserPage(d) : null;
  return (
    <div className="tool">
      <Pressable className="hd" onClick={() => setManual(!open)}>
        <span className="mono" style={{ width: 14, textAlign: "center", color: v.color, fontSize: 11 }}>{v.icon}</span>
        <span className="verb">{v.verb}</span>
        <Tooltip content={v.target} truncated><span className="tgt">{v.target || (status === "running" ? "…" : "")}</span></Tooltip>
        {images.length > 0 && <Thumb src={images[0]!} items={imgs} index={0} alt={`${v.verb} image`} style={{ height: 22, maxWidth: 44, objectFit: "cover", borderRadius: 4, border: "1px solid var(--line, rgba(255,255,255,0.12))" }} />}
        {images.length > 1 && <span className="meta">+{images.length - 1}</span>}
        {dropped > 0 && <Tooltip content="Older images on this row were dropped from the transcript to keep memory in check. The tool result still has them — open it in the review view."><span className="meta">+{dropped} dropped</span></Tooltip>}
        {/* A pause freezes the turn *between* calls, so a call still marked
            `running` is normally one the chat stopped waiting on — the trace would
            keep animating over a tool nothing is driving. The exception is a command
            that already started: the backend marks the row `exec` for exactly as long
            as the process is alive, because that one is still running and will run to
            completion whatever the pause says. Freezing its glyph claims work stopped
            that has not, which is the same lie as a spinner over a parked agent,
            pointed the other way. */}
        {status === "running" && <StatusIcon status={held && !d.exec ? "paused" : "running"} />}
        <span className="meta" style={{ color: status === "error" || status === "denied" ? "#ff8a8a" : undefined }}>{diffCounts ? <><span style={{ color: "var(--diff-add)" }}>+{diffCounts[1]}</span> <span style={{ color: "var(--diff-del)" }}>−{diffCounts[2]}</span></> : meta}</span>
        <span style={{ transform: `rotate(${open ? 180 : 0}deg)`, transition: "transform .22s cubic-bezier(.32,.72,0,1)", display: "flex" }}>{I.chev("var(--dim)")}</span>
      </Pressable>
      <div className="fold" style={{ gridTemplateRows: open ? "1fr" : "0fr" }}>
        <div>
          {open && (
            <div className={"bd" + (hasDiff ? "" : " plain")}>
              {/* A token, not a hex: this body is a *tint* over whatever panel it
                  lands on. Hardcoding the dark theme's white put near-white text
                  on that tint, so the command vanished in light mode — invisible
                  on a light surface, exactly the row you most need to read. */}
              {v.verb === "Run" && <div style={{ color: "var(--fg)" }}>$ {d.input?.command}</div>}
              {page && (
                <div style={{ display: "flex", alignItems: "center", gap: 8, margin: "2px 0 6px" }}>
                  <span className="pageurl mono" title="Open this page in your own browser">{page}</span>
                  <Tooltip content="Open in your browser · it opens there, not in the agent's headless one">
                    <IconButton label="Open this page" onClick={() => openUrl(page).catch(() => flash("Couldn't open the browser"))}><ExternalLink size={12} strokeWidth={1.7} /></IconButton>
                  </Tooltip>
                  {openBrowserView && <Tooltip content="Watch and take over the main agent's page beside the chat"><IconButton label="Open Browser view" onClick={openBrowserView}><Globe size={12} strokeWidth={1.7} /></IconButton></Tooltip>}
                </div>
              )}
              {images.map((src, i) => <Thumb key={i} src={src} items={imgs} index={i} alt={`${v.verb} image ${i + 1}`} style={{ maxWidth: "100%", maxHeight: 260, borderRadius: 8, margin: "4px 0", border: "1px solid rgba(255,255,255,0.1)", objectFit: "contain" }} />)}
              {hasDiff
                ? d.diff?.map((l, i) => <div key={i} className={"dline " + l.k}><span className="dsign">{l.k === "a" ? "+" : l.k === "d" ? "−" : ""}</span><span className="dtext" style={{ color: l.k === "a" ? "var(--diff-add)" : l.k === "d" ? "var(--diff-del)" : undefined }}>{l.t || " "}</span></div>)
                : String(d.output ?? "").split("\n").slice(0, 400).map((t: string, i: number) => <div key={i} style={{ color: status === "error" ? "var(--red2)" : undefined }}>{t || " "}</div>)}
            </div>
          )}
        </div>
      </div>
    </div>
  );
});

function Approval({ it, task, onOpenArtifict }: { it: Item; task: TaskSummary; onOpenArtifict?: (selection: ArtifictWorkspaceSelection) => void }) {
  const d = it.data ?? {};
  const [fb, setFb] = useState("");
  const [showFb, setShowFb] = useState(false);
  if (d.resolved) {
    const denied = d.resolved === "deny" || d.resolved === "cancelled";
    const label = d.kind === "plan"
      ? denied ? "Kept planning" : "Plan approved"
      : d.kind === "ultra" ? denied ? "Kept going solo" : d.resolved === "worktrees" ? "Switched to ultrathread · worktrees" : "Switched to ultrathread"
      : d.resolved === "cancelled" ? "Cancelled" : denied ? "Denied" : d.resolved === "reviewer" ? "Approved by reviewer" : d.resolved === "always" ? "Always allowed" : "Allowed once";
    return (
      <div className="resolved">
        <span style={{ color: denied ? "var(--red)" : "var(--mut2)", display: "flex" }}>{denied ? li(X, 12) : li(Check, 12)}</span>
        <span>{label} · <span className="mono" style={{ fontSize: 11.5 }}>{d.kind === "command" ? d.detail : d.kind === "plan" || d.kind === "ultra" ? "" : d.title}</span></span>
        {d.feedback && <span style={{ color: "var(--mut)" }}>&ldquo;{d.feedback}&rdquo;</span>}
      </div>
    );
  }
  const respond = (decision: string) => api.respond(task.id, it.id, { decision, feedback: fb }).catch((e) => flash(String(e)));
  const plan = d.kind === "plan";
  if (d.kind === "ultra") {
    // The agent's worktree call: true = for, false = against, null = no opinion. Older cards: suggest_wt.
    const rec: boolean | null = d.rec_wt ?? (d.suggest_wt ? true : null);
    const wtFirst = rec === true;
    const tag = <span style={{ fontSize: 10.5, opacity: 0.85, fontWeight: 500 }}>recommended</span>;
    return (
      <div className="card-a plan" data-pending-approval={it.id} data-enter={wtFirst ? "worktrees" : "once"}>
        <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap", fontWeight: 600, color: "var(--ultra2)" }}>
          <span className="ultraspark" aria-hidden="true" />{d.title}
          <span style={{ fontWeight: 400, color: "var(--mut2)" }}>{d.reason}</span>
        </div>
        {d.git && rec !== null && (
          <div className="ultrarec">
            <b>Agent recommends: {rec ? "worktrees" : "no worktrees"}</b>
            {d.rec_why && <span> · {d.rec_why}</span>}
          </div>
        )}
        <div style={{ background: "rgba(0,0,0,0.2)", borderRadius: 9, padding: "8px 12px", maxHeight: 320, overflow: "auto" }}><Md text={d.detail ?? ""} /></div>
        {showFb && <Input autoFocus value={fb} onChange={(e) => setFb(e.currentTarget.value)} placeholder="Anything the agent should know instead?" onKeyDown={(e) => { if (e.key === "Enter") respond("deny"); }} />}
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
          <Button variant={wtFirst ? undefined : "primary"} style={wtFirst ? undefined : { background: "var(--ultra)" }} title="Everyone works in the shared checkout" onClick={() => respond("once")}>Ultrathread{rec === false && tag}{!wtFirst && <Kbd>Enter</Kbd>}</Button>
          {d.git && <Button variant={wtFirst ? "primary" : undefined} style={wtFirst ? { background: "var(--ultra)" } : undefined} title="Each worker gets its own git worktree; a fuze agent merges them back" onClick={() => respond("worktrees")}>Ultrathread · worktrees{wtFirst && tag}{wtFirst && <Kbd>Enter</Kbd>}</Button>}
          <Button variant="ghost" onClick={() => (showFb ? respond("deny") : setShowFb(true))}>{showFb ? "Keep going solo" : "Keep going solo…"}<Kbd>Esc</Kbd></Button>
        </div>
      </div>
    );
  }
  return (
    <div className={"card-a" + (plan ? " plan" : "")} data-pending-approval={it.id}>
      <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap", fontWeight: 600, color: plan ? "var(--ultra2)" : "#ff8a8a" }}>
        {plan ? li(Info, 14) : li(CircleAlert, 14)}
        {plan ? "Plan ready for review" : "Needs your approval"}
        {d.sub && <span className="branchtag">{d.sub} subagent</span>}
        <span style={{ fontWeight: 400, color: "var(--mut2)" }}>{d.reason}</span>
      </div>
      {plan ? <div style={{ background: "rgba(0,0,0,0.2)", borderRadius: 9, padding: "8px 12px", maxHeight: 420, overflow: "auto" }}><Md text={d.detail ?? ""} /></div>
        : d.kind === "command" ? <div className="cmdbox">$ {d.detail}</div>
        : <div className="cmdbox">{d.title}{(d.kind === "mcp" || d.kind === "http") && d.detail ? "\n" + d.detail : ""}</div>}
      {/* Approving a plan is not the only thing the user does with it: it is
          filed in the project's artificts, so point at the record they will
          come back to instead of leaving it to be found in a list. */}
      {plan && d.plan_artifict?.id && d.plan_artifict.version_id && (
        <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap", fontSize: 11.5, color: "var(--mut2)" }}>
          <span>Filed in this project&rsquo;s artificts as <strong style={{ fontWeight: 600 }}>{d.plan_artifict.title || "this plan"}</strong>.</span>
          {onOpenArtifict && <Button variant="ghost" style={{ height: 24, padding: "0 8px", fontSize: 11 }} onClick={() => onOpenArtifict({ artifictId: d.plan_artifict!.id!, versionId: d.plan_artifict!.version_id! })}>Open in Artificts</Button>}
        </div>
      )}
      {showFb && (
        <Input autoFocus value={fb} onChange={(e) => setFb(e.currentTarget.value)} placeholder={plan ? "What should change in the plan?" : "Tell the agent what to do instead"} onKeyDown={(e) => { if (e.key === "Enter") respond("deny"); }} />
      )}
      <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
        <Button variant="primary" style={plan ? { background: "var(--ultra)" } : undefined} onClick={() => respond("once")}>{plan ? "Approve" : "Allow once"}<Kbd>Enter</Kbd></Button>
        {d.repeat ? null : plan ? <Button onClick={() => respond("always")}>Approve + auto-edit</Button>
          : d.rule ? <Button onClick={() => respond("always")}>Always allow <span className="mono" style={{ fontSize: 11 }}>{d.rule}</span></Button>
          : d.kind === "edit" ? <Button onClick={() => respond("always")}>Allow all edits</Button> : null}
        <Button variant="ghost" onClick={() => (showFb && fb.trim() ? respond("deny") : showFb ? respond("deny") : setShowFb(true))}>{plan ? "Keep planning" : showFb ? "Deny" : "Deny…"}<Kbd>Esc</Kbd></Button>
      </div>
    </div>
  );
}

function Thinking({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  // Measure the content and clip to exactly that height so the expand transition ends on the real size.
  const inner = useRef<HTMLSpanElement>(null);
  const [h, setH] = useState<number | null>(null);
  useLayoutEffect(() => {
    if (open && inner.current) setH(inner.current.scrollHeight + 4); // + vertical padding
  }, [open, text]);
  if (!text.trim()) return null;
  return (
    <Tooltip content={open ? undefined : "Click to expand"}>
      <Pressable className={"thinking" + (open ? " open" : "")} style={open && h ? { maxHeight: h } : undefined} onClick={() => setOpen(!open)}>
        <span ref={inner} style={{ display: "block" }}>{text.trim()}</span>
      </Pressable>
    </Tooltip>
  );
}

function Sub({ it, task, held }: { it: Item; task: TaskSummary; held: boolean }) {
  const d = it.data ?? {};
  const running = d.status === "running";
  // A parked agent still says `running`, and the pill animates on that word —
  // so a frozen swarm's transcript was full of subagent pills that looked in
  // flight. The chat decides, not the pill.
  const live = running && !held;
  const info = task.subs.find((s) => s.id === d.sub_id);
  // The colour is the model this agent is actually on, which for most agents is
  // the chat's — an empty `sub.model` is not "no model", it is "follows the chat".
  const m = info ? subModel(task, info) : chatModel(task);
  return (
    <Pressable className={"subpill" + (live ? " live" : "")} onClick={() => set({ subOpen: d.sub_id ?? null, details: true })}>
      <StatusIcon status={held && running ? "paused" : d.status ?? "running"} color={modelColor(m)} />
      <span className="mono" style={{ fontSize: 11, color: "var(--mut2)" }}>{d.name ?? d.role}</span>
      {d.background && <Tooltip content="Runs in the background; the main agent keeps working"><span className="bgtag">bg</span></Tooltip>}
      <span style={{ color: "#d4d4d8", fontWeight: 500, overflow: "hidden", textOverflow: "ellipsis" }}>{it.text}</span>
      <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{running ? (held ? `${info?.meta || "running"} · held` : info?.meta || "running") : `${d.status} · ${d.secs ?? 0}s`}</span>
      {I.chevR()}
    </Pressable>
  );
}

function GoalNotice({ it }: { it: Item }) {
  const d = it.data ?? {};
  const ok = d.level === "goal";
  // Reads like a normal final answer: a small status tag, then the summary as chat text.
  // `quiet`: the agent wrote its own summary after this, so only the tag shows.
  return (
    <div className="goalreply">
      <span className={"goaltag" + (ok ? "" : " blocked")}>{ok ? li(CircleCheckBig, 12) : li(CircleAlert, 12)}{it.text}</span>
      {d.summary && !d.quiet && <div className="reply"><Md text={d.summary} /></div>}
    </div>
  );
}

function BroadcastNotice({ text, replied }: { text: string; replied: string[] }) {
  const [open, setOpen] = useState(false);
  const body = text.replace(/^Broadcast from the user:\s*/, "");
  const took = replied.filter((x) => x !== "main");
  return (
    <div className="resolved" style={{ alignItems: "flex-start", cursor: "pointer" }} onClick={() => setOpen(!open)} title={open ? undefined : "Show message"}>
      <span style={{ color: "var(--mut)", display: "flex" }}>{li(Megaphone, 13)}</span>
      <span style={{ minWidth: 0 }}>
        <span style={{ color: "var(--mut)" }}>You broadcast to all agents</span>
        {/* Who's actually on it: the ones it applied to. The rest ignore it. */}
        {took.length > 0 && <span style={{ color: "var(--mut3)" }}>{" · "}{replied.join(", ")} {took.length === 1 ? "is" : "are"} on it</span>}
        {open && <div className="sel" style={{ whiteSpace: "pre-wrap", marginTop: 3 }} onClick={(e) => e.stopPropagation()}>{body}</div>}
      </span>
    </div>
  );
}

/** Agent-to-agent message: one quiet line ("main sent a message"); click for the full text.
 *
 *  Cross-chat messages (`list_chats` / `message_chat`) come through the same
 *  level, and carry `from`/`to` plus the other chat's id, so the line can name
 *  which chat it was and offer to open it. Older items have neither: they fall
 *  back to reading the line itself, exactly as before. */
function MsgNotice({ it }: { it: Item }) {
  const [open, setOpen] = useState(false);
  const chat = useStore((s) => (it.data?.chat_id ? s.tasks[it.data.chat_id as string] : null));
  const chatId = typeof it.data?.chat_id === "string" ? it.data.chat_id : "";
  const openChat = () => { if (chatId) { go("session", { task: chatId }); } };
  const m = it.text.match(/^(?:Message from )?([\w.-]+)\s*(?:→\s*([\w.-]+)\s*)?:\s*([\s\S]*)$/);
  const who = (it.data?.from as string | undefined) ?? m?.[1] ?? "An agent";
  const body = m ? m[3] : it.text;
  // `to` is set only on the sender's own copy, which is what makes the two
  // directions tell apart without reading the sentence.
  const to = it.data?.to as string | undefined;
  return (
    <div className="resolved" style={{ alignItems: "flex-start", cursor: "pointer" }} onClick={() => setOpen(!open)} title={open ? undefined : "Show message"}>
      <span style={{ color: "var(--mut)", display: "flex" }}>{li(Info, 13)}</span>
      <span style={{ minWidth: 0 }}>
        <span style={{ color: "var(--mut)" }}>
          {to ? `Sent a message to ${to}` : m?.[2] ? `${who} sent a message to ${m[2]}` : `Received a message from ${who}`}
          {chatId && (chat
            ? <button type="button" style={{ marginLeft: 6, padding: 0, border: 0, background: "none", color: "var(--accent)", font: "inherit", cursor: "pointer" }} onClick={(e) => { e.stopPropagation(); openChat(); }}>open chat</button>
            : <span style={{ color: "var(--mut3)" }}> · chat deleted</span>)}
        </span>
        {open && <div className="sel" style={{ whiteSpace: "pre-wrap", marginTop: 3 }} onClick={(e) => e.stopPropagation()}>{body}</div>}
      </span>
    </div>
  );
}

/** Where a forked chat came from. The link back is only offered while that chat
 *  still exists — once it's deleted the line is just history. */
function ForkNotice({ it }: { it: Item }) {
  const [open, setOpen] = useState(false);
  const from = it.data?.from as string | undefined;
  const parent = useStore((s) => (from ? s.tasks[from] : null));
  const summary = (it.data?.summary as string | undefined)?.trim();
  const openParent = () => { if (from) { go("session", { task: from }); } };
  return (
    <div className="resolved" style={{ alignItems: "flex-start", cursor: summary ? "pointer" : "default" }} onClick={() => summary && setOpen(!open)} title={summary ? (open ? undefined : "Show what carried over") : undefined}>
      <span style={{ color: "var(--mut2)", display: "flex" }}>{li(GitFork, 13)}</span>
      <span style={{ minWidth: 0 }}>
        <span style={{ color: "var(--mut)" }}>
          Forked from {it.data?.title ?? "an earlier chat"}
          {summary ? "" : " · full copy, whole conversation included"}
        </span>
        {parent ? (
          <button type="button" style={{ marginLeft: 6, padding: 0, border: 0, background: "none", color: "var(--accent)", font: "inherit", cursor: "pointer" }} onClick={(e) => { e.stopPropagation(); openParent(); }}>open it</button>
        ) : (
          <span style={{ color: "var(--mut3)" }}> · deleted</span>
        )}
        {open && summary && <div className="sel" style={{ whiteSpace: "pre-wrap", marginTop: 3 }} onClick={(e) => e.stopPropagation()}>{summary}</div>}
      </span>
    </div>
  );
}

/** A subagent's question to the main agent, and the answer it got. The answer is
 *  often a page of reasoning, so the whole exchange is one quiet line: the
 *  summary, with the question and the answer behind a click. */
function AskNotice({ it }: { it: Item }) {
  const [open, setOpen] = useState(false);
  const d = it.data ?? {};
  const asked = d.level === "ask" ? "An agent asked the main agent" : `${it.text.split(" asked:")[0] || "An agent"} asked the main agent`;
  const question = d.question ?? it.text.split(" asked: ").slice(1).join(" asked: ");
  const answer = typeof d.answer === "string" ? d.answer : "";
  // One line, however long the answer was: clip it, and read it as markdown so
  // the `**bold**` an agent typed shows as bold, not as asterisks.
  const summary = answer ? <span style={{ color: "var(--mut3)" }}> · <MdInline text={clip(answer)} /></span> : null;
  return (
    <div className="resolved askcard" style={{ alignItems: "flex-start" }} onClick={() => setOpen(!open)} title={open ? undefined : "Show the question and answer"}>
      <span style={{ color: "var(--mut)", display: "flex" }}>{li(CircleQuestionMark, 13)}</span>
      <span style={{ minWidth: 0 }}>
        <span style={{ color: "var(--mut)" }}>{asked}{answer ? "" : <span className="shimmer" style={{ fontSize: 12 }}> · waiting for an answer…</span>}{summary}</span>
        {open && <div className="ask-body">
          <div style={{ fontWeight: 500 }}>{question}</div>
          {answer ? <div className="ans"><span style={{ color: "var(--mut2)", display: "inline-flex" }}>{li(CornerDownLeft, 12)}</span> <span className="sel">{answer}</span></div> : <div className="shimmer" style={{ fontSize: 12 }}>waiting for an answer…</div>}
        </div>}
      </span>
    </div>
  );
}

/** `/btw` — a side question and the answer it got.
 *
 *  The question is always shown, because this row is the *only* record that it
 *  was ever asked: nothing goes into the chat's history, so an answer with no
 *  question beside it is unattributable a week later. The answer itself stays
 *  collapsed until asked for — the point of the command is something you read and
 *  move on from, not another turn sitting in the transcript. */
function AsideNotice({ it }: { it: Item }) {
  const [open, setOpen] = useState(false);
  const q = (it.data?.question as string | undefined)?.trim();
  return (
    <div className="resolved" style={{ alignItems: "flex-start", cursor: "pointer" }} onClick={() => setOpen(!open)} title={open ? undefined : "Show the answer"}>
      <span style={{ color: "var(--accent)", display: "flex" }}>{li(MessagesSquare, 13)}</span>
      <span style={{ minWidth: 0, display: "block" }}>
        <span className="sel" style={{ color: "var(--mut)" }}>
          {it.data?.working ? it.text : <MdInline text={q ? `/btw ${q}` : "Side question"} />}
        </span>
        {open && <div className="sel" onClick={(e) => e.stopPropagation()}><Md text={it.text} /></div>}
      </span>
    </div>
  );
}

export function WorktreeNotice({ it }: { it: Item }) {
  const [open, setOpen] = useState(false);
  const legacy = it.text.match(/^Created a separate worker worktree at (.+) \((.+)\)\.?$/);
  const path = it.data?.path ?? legacy?.[1];
  const branch = it.data?.branch ?? legacy?.[2];
  return (
    <div className="worktree-notice">
      <Pressable className="worktree-notice-toggle" aria-expanded={open} onClick={() => setOpen((value) => !value)} style={{ display: "inline-flex", alignItems: "center", gap: 5, padding: "1px 0", border: 0, background: "transparent", color: "var(--mut)", font: "inherit", cursor: "pointer" }}>
        <span>Created worktree</span>
        <span aria-hidden="true" className="tg-chev" style={{ transform: `rotate(${open ? 90 : 0}deg)` }}>›</span>
      </Pressable>
        {open && <div className="worktree-notice-details" style={{ margin: "4px 0 2px 12px", paddingLeft: 8, borderLeft: "1px solid var(--ov-70)", color: "var(--mut2)", fontSize: 11.5, lineHeight: 1.5, overflowWrap: "anywhere" }}>
        {path && <div>Path: <span className="sel">{path}</span></div>}
        {branch && <div>Branch: <span className="sel">{branch}</span></div>}
        {it.data?.detail && <div>{it.data.detail}</div>}
      </div>}
    </div>
  );
}

export function RouterNotice({ it }: { it: Item }) {
  const text = it.text.replace(/^Router:\s*/, "");
  return <div className="resolved route router-note"><span className="router-chip">{li(Network, 11)}Router</span><span className="sel">{text}</span></div>;
}

function Notice({ it }: { it: Item }) {
  const lv = it.data?.level;
  if (it.data?.goal) return <GoalNotice it={it} />;
  if (lv === "fork") return <ForkNotice it={it} />;
  if (lv === "btw") return <AsideNotice it={it} />;
  if (lv === "router") return <RouterNotice it={it} />;
  if (lv === "route" || lv === "event") return <div className="resolved route"><span style={{ color: "var(--mut2)", display: "flex" }}>{li(Network, 13)}</span><span className="sel">{it.text}</span></div>;
  if (lv === "subq" || lv === "ask") return <AskNotice it={it} />;
  if (lv === "msg") return it.data?.global ? <BroadcastNotice text={it.text} replied={it.data?.replied ?? []} /> : <MsgNotice it={it} />;
  if (lv === "worktree") return <WorktreeNotice it={it} />;
  const color = lv === "error" ? "var(--red)" : "var(--mut)";
  const glyph = lv === "error" ? li(CircleAlert, 13) : lv === "stopped" ? li(Square, 11) : li(Info, 13);
  return (
    <div className="resolved" style={{ color: lv === "error" ? "#ffb3b3" : undefined, alignItems: "flex-start" }}>
      <span style={{ color, display: "flex" }}>{glyph}</span>
      <span className={it.data?.working ? "shimmer" : "sel"} style={{ whiteSpace: "pre-wrap" }}>{it.text}</span>
    </div>
  );
}

/** Stop the agent (if running) and wait until it has actually wound down. */
async function stopAndSettle(id: string) {
  const t = get().tasks[id];
  if (!t || (t.status !== "running" && t.status !== "waiting")) return;
  await api.interrupt(id);
  for (let i = 0; i < 40; i++) {
    await new Promise((r) => setTimeout(r, 150));
    const s = get().tasks[id]?.status;
    if (s !== "running" && s !== "waiting") return;
  }
}

/** Pictures pasted into an editor become its own, local attachments. The
 *  composer's `addImages` parks them in shared state keyed by composer, which is
 *  wrong here: this editor belongs to one message, and the composer's chips must
 *  not gain rows because someone pasted a photo into a message being edited. */
function pastedImages(files: File[]): Promise<string[]> {
  const imgs = files.filter((f) => f.type.startsWith("image/"));
  if (!imgs.length) return Promise.resolve([]);
  const over = imgs.filter((f) => f.size > MAX_IMAGE);
  if (over.length) flash(`${over.length} image${over.length > 1 ? "s are" : " is"} over 5 MB`);
  return Promise.all(
    imgs.filter((f) => f.size <= MAX_IMAGE).map((f) => new Promise<string>((resolve) => {
      const r = new FileReader();
      r.onload = () => resolve(typeof r.result === "string" ? r.result : "");
      r.onerror = () => resolve("");
      r.readAsDataURL(f);
    })),
  ).then((out) => out.filter(Boolean));
}

/** Pick files for an edited message. The OS dialog, not a hidden <input>: the
 *  agent needs the real path, and a browser `File` deliberately has none. */
async function pickFiles(): Promise<string[]> {
  const picked = await open({ multiple: true, title: "Attach files" });
  return (Array.isArray(picked) ? picked : picked ? [picked] : []).filter((p): p is string => typeof p === "string");
}

/** The staged temp copies the backend made for a message, as pickable file refs,
 *  so an edited message can be resent with the attachments it went out with. */
function stagedFiles(data: ItemData | undefined): FileRef[] {
  const staged = Array.isArray(data?.attachments) ? data!.attachments! : [];
  return staged.map((a, i) => ({ path: a.path, name: a.name || `attachment-${i + 1}`, size: a.size, isDir: false, image: false }));
}

/** One user message: the bubble, its attachments, and the editor it becomes when
 *  the user rewrites it (text and attachments both). Exported for a render test.
 *
 *  Memoized on what it reads off the chat — its id and whether it is working —
 *  because the summary is replaced on every status, step and cost event, and a
 *  bubble re-rendered for each one re-ran its attachment and annotation scans. */
export const UserMsg = memo(function UserMsg({ it, task }: { it: Item; task: TaskSummary }) {
  const live = isLiveTask(task);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(it.text);
  const [busy, setBusy] = useState(false);
  // Opened by this message's own rewind button. Declared with the other hooks,
  // above the `editing` early-return below: a hook declared after a conditional
  // return changes the hook count between renders.
  const [rwOpen, setRwOpen] = useState(false);
  // Editing attachments is part of editing the message: the pictures and files
  // that went out with it are local to this one message (the composer keeps its
  // own), so the editor holds its own copies and sends them with the text.
  const [imgs, setImgs] = useState<string[]>(() => (Array.isArray(it.data?.images) ? (it.data.images as string[]) : []));
  const [files, setFiles] = useState<FileRef[]>(() => stagedFiles(it.data));
  // The agent-only context stays out of the editor and goes back on the message when it is resent.
  const agentContext = splitContextBlocks(it.text).context;
  const box = useRef<HTMLTextAreaElement>(null);
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 360) + "px";
  }, [draft, editing]);
  const startEdit = () => { setDraft(splitContextBlocks(it.text).body); setImgs(Array.isArray(it.data?.images) ? (it.data.images as string[]) : []); setFiles(stagedFiles(it.data)); setEditing(true); setTimeout(() => { const el = box.current; if (el) { el.focus(); el.setSelectionRange(el.value.length, el.value.length); } }, 20); };
  const addEditFiles = async (paths: string[]) => {
    if (!paths.length) return;
    const refs = await api.filesAttach(paths).catch((e) => { flash(String(e)); return [] as FileRef[]; });
    const fresh = refs.filter((r) => !files.some((x) => x.path === r.path));
    if (!fresh.length) return flash("Already attached");
    setFiles((f) => [...f, ...fresh].slice(0, MAX_FILES));
    // An image attached as a file is only visible to the model as an image block,
    // so it goes in beside the pasted ones rather than as a path alone.
    const shots = await Promise.all(fresh.filter((r) => r.image).map((r) => api.fileDataUrl(r.path).catch(() => "")));
    const inlined = shots.filter(Boolean);
    if (inlined.length) setImgs((x) => [...x, ...inlined]);
  };
  const resend = async () => {
    const body = draft.trim();
    if (!body || busy) return;
    const text = [body, agentContext].filter(Boolean).join("\n\n");
    setBusy(true);
    try {
      await stopAndSettle(task.id);
      await api.rewind(task.id, it.id);
      const r = await api.task(task.id);
      // The summary is folded in rather than assigned: the stop and the rewind
      // each published one, and a later `task` event can have landed while
      // `task_get` was in flight. See `newerSummary`.
      set((s) => ({ items: { ...s.items, [task.id]: r.items }, tasks: { ...s.tasks, [task.id]: newerSummary(s.tasks[task.id], r.summary) } }));
      // Editing a message re-sends its attachments: the backend keeps the staged
      // temp copies on the message itself, so fall back to those when the editor
      // holds no picks of its own, and the resend stages fresh copies of exactly
      // what went out the first time.
      const withStaged = files.length ? files : stagedFiles(it.data);
      const paths = withStaged.filter((f) => !f.image).map((f) => f.path);
      const imageNames = imgs.map((_, i) => withStaged.find((f) => f.image)?.name ?? `image-${i + 1}`);
      await api.send(task.id, text, false, imgs.length ? imgs : undefined, paths, imageNames);
      setEditing(false);
    } catch (e) {
      flash(String(e));
    } finally {
      setBusy(false);
    }
  };
  if (editing) {
    return (
      <div className="urow">
        <div className="ubub editing">
          {imgs.length > 0 && <div className="ubimgs">{imgs.map((src, i) => <div key={i} className="attach"><Thumb src={src} items={imgs.map((s: string, j: number) => ({ src: s, alt: `Attached image ${j + 1}` }))} index={i} alt={`Attached image ${i + 1}`} /><IconButton label="Remove image" className="attach-x" onClick={() => setImgs((x) => x.filter((_, j) => j !== i))}>{I.close(10)}</IconButton></div>)}</div>}
          {files.length > 0 && <div className="filerow editrow">{files.map((f, i) => (
            <Tooltip key={f.path} content={f.path}>
              <div className="filechip">
                {f.image && <span className="fileimg" style={{ backgroundImage: cssUrl(imgs[i]) }} />}
                <span className="filename">{f.name}</span>
                <span className="filesize">{humanBytes(f.size)}</span>
                <IconButton label={`Remove ${f.name}`} className="attach-x" onClick={() => setFiles((x) => x.filter((_, j) => j !== i))}>{I.close(10)}</IconButton>
              </div>
            </Tooltip>
          ))}</div>}
          <textarea ref={box} value={draft} onChange={(e) => setDraft(e.currentTarget.value)}
            onPaste={(e) => {
              // A photo pasted into the message being edited is an attachment
              // of that message, and lands in this editor's own list.
              const shots = Array.from(e.clipboardData.files);
              if (shots.some((f) => f.type.startsWith("image/"))) {
                e.preventDefault();
                void pastedImages(shots).then((add) => add.length && setImgs((x) => [...x, ...add]));
              }
            }}
            onKeyDown={(e) => {
              if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); setEditing(false); }
              else if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); void resend(); }
            }} />
          <div className="editbar">
            <Tooltip content="Attach files to this message · or paste images into the box">
              <span className="attachbtn">
                <IconButton label="Attach files to this message" onClick={async () => void addEditFiles(await pickFiles())}>{I.paperclip()}</IconButton>
              </span>
            </Tooltip>
            <div className="btn ghost" style={{ height: 24, marginLeft: "auto" }} onClick={() => setEditing(false)}>Cancel</div>
            <div className="btn primary" style={{ height: 24, opacity: draft.trim() && !busy ? 1 : 0.5 }} onClick={resend}>{busy ? "Sending…" : "Send"}</div>
          </div>
        </div>
      </div>
    );
  }
  // Rewind is a picker now, not an instant action: the three-way "files /
  // conversation / both" choice and the list of checkpoints live in one place,
  // and a plain click could not express it. The old instant path survives as
  // the "conversation" option inside the dialog. `Edit`/`startEdit` is
  // untouched — it is a different gesture (rewrite and resend this message).
  return (
    <div className="urow">
      {!live && <IconButton label="Rewind to here: pick a checkpoint and choose whether to put back the conversation, the files, or both." className="rw" onClick={() => setRwOpen(true)}>{I.rewind()}</IconButton>}
      {rwOpen && <Rewind taskId={task.id} itemId={it.id} onClose={() => setRwOpen(false)} />}
      <div className="ucol">
      <div className="ubub">{Array.isArray(it.data?.images) && it.data.images.length > 0 && <div className="ubimgs">{it.data.images.map((src: string, i: number) => <Thumb key={i} src={src} items={(it.data.images as string[]).map((s: string, j: number) => ({ src: s, alt: `Attached image ${j + 1}` }))} index={i} alt={`Attached image ${i + 1}`} />)}</div>}<AttachmentChips text={it.text} /><SentAnnotationMessage text={stripHarnessBlocks(it.text, it.data)} /></div>
      <div className="msgacts">
        <span onClick={() => copyText(stripHarnessBlocks(it.text, it.data))}>Copy</span>
        <span onClick={startEdit}>Edit</span>
      </div>
      </div>
    </div>
  );
}, (a, b) => a.it === b.it && a.task.id === b.task.id && isLiveTask(a.task) === isLiveTask(b.task));

type FeedEntry = { key: string; it: Item } | { key: string; tools: Item[] };

// ── What a transcript row actually reads off the chat ──
//
// The chat summary is a new object on every status, step, usage and subagent
// event, and every row is handed it. Compared by identity, that meant one step
// of the agent re-rendered every visible row of the transcript. These compare
// only the parts the rows read, so a row re-renders when something it shows
// changed. Item objects are replaced in the store only when they change, so
// those compare by identity.

/** Working: the agent is running, or waiting on the user. */
function isLiveTask(t: TaskSummary): boolean {
  return t.status === "running" || t.status === "waiting";
}

/** The summary fields every row reads: the chat it belongs to, its folder for
 *  relative paths, whether it is working (rewind/edit buttons), and the inputs
 *  of `isPaused` that come from the chat (the freeze glyphs). */
function rowTaskEqual(a: TaskSummary, b: TaskSummary): boolean {
  return a === b || (a.id === b.id && a.cwd === b.cwd && a.status === b.status && !!a.paused === !!b.paused && !!a.unpaused === !!b.unpaused);
}

/** What a subagent pill reads off the chat for one agent: its resolved model
 *  (the pill's colour) and its live step line. */
function subLook(t: TaskSummary, id: unknown): string {
  const info = (t.subs ?? []).find((s) => s.id === id);
  return `${info ? subModel(t, info) : chatModel(t)}\u0000${info?.meta ?? ""}`;
}

/** `rowTaskEqual`, plus the subagent fields any pill among `items` shows.
 *  Exported for the comparator tests. */
export function feedTaskEqual(a: TaskSummary, b: TaskSummary, items: readonly Item[]): boolean {
  if (a === b) return true;
  if (!rowTaskEqual(a, b)) return false;
  for (const it of items) if (it.kind === "sub" && subLook(a, it.data?.sub_id) !== subLook(b, it.data?.sub_id)) return false;
  return true;
}

/** Same length, same item objects. Exported for the comparator tests. */
export function sameItems(a: readonly Item[], b: readonly Item[]): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

/** The background-job fields a tool group shows for its own commands. The job
 *  list is replaced on every output line of any command, and only a group that
 *  started one of them has anything to redraw. */
function bgLook(bgs: Bg[], tools: readonly Item[]): string {
  let out = "";
  for (const t of tools) {
    const id = t.data?.bg_id;
    if (!id) continue;
    const j = bgs.find((b) => b.id === id);
    out += j ? `${j.id}\u0000${j.running}\u0000${j.exit ?? ""}\u0000${j.last_line}\u0001` : `${id}\u0001`;
  }
  return out;
}

/** Agent-to-agent messages, questions and route/router switches are background chatter: they ride along in tool groups. */
const isChatter = (it: Item) => it.kind === "notice" && (it.data?.level === "msg" || it.data?.level === "route" || it.data?.level === "router" || it.data?.level === "event" || it.data?.level === "ask" || it.data?.level === "subq") && !it.data?.goal;

/** Background work that has finished. A subagent settles when it reports back; a
 *  background command when its process exits. Both are read off the live job list,
 *  so a command only counts as done once the harness has actually seen it exit —
 *  a job it knows nothing about (an older chat, a job pruned long ago) stays
 *  inline rather than being guessed either way. */
function isSettledBg(it: Item, bgs: Bg[] | null): boolean {
  const d = it.data ?? {};
  if (it.kind === "sub") return !!d.status && d.status !== "running";
  if (it.kind !== "tool" || d.name !== "bash" || !d.input?.run_in_background || d.status === "running") return false;
  const j = bgs?.find((b) => b.id === d.bg_id);
  return !!j && !j.running;
}

/** Harness events that aren't model switches, as a summary phrase. Older chats tagged them "route". */
function eventSum(it: Item): string | null {
  if (it.data?.level === "event") return it.data?.sum ?? it.text;
  const m = it.text.match(/^Resuming (\d+) subagents?/);
  if (m) return `resumed ${m[1]} subagent${m[1] === "1" ? "" : "s"}`;
  if (it.text.startsWith("A stop hook failed")) return "a stop hook failed";
  if (it.text.startsWith("An account or key freed up")) return "resumed automatically";
  return null;
}

/** Merge back-to-back tool calls into one group; hidden thinking doesn't break a run.
 *  Commands and settled subagents stay in that group when they end, so completion
 *  doesn't split the surrounding actions or reset an open fold. A subagent that is
 *  still running keeps its own row: it is the one thing there still worth watching. */
function groupFeed(items: Item[], showThinking: boolean): FeedEntry[] {
  const out: FeedEntry[] = [];
  for (const it of items) {
    if (it.kind === "thinking" && !showThinking) continue;
    // A blank text/thinking block renders as nothing but would still end the run
    // before it: free routed models emit one between every parallel call, which
    // left each call in its own one-line group instead of one folded line. The
    // item stays in the transcript; only the grouping looks past it.
    if ((it.kind === "text" || it.kind === "thinking") && !it.text.trim()) continue;
    const prev = out[out.length - 1];
    // A finished subagent rides along like a call: its row folds into the chain
    // and still opens its own transcript, so the receipt cannot break the run.
    const settledSub = it.kind === "sub" && isSettledBg(it, null);
    if (hasArtifictCardSlot(it)) {
      // Artifict tools have a standalone transcript row: their building state
      // and complete preview must not be hidden inside a generic tool fold.
      out.push({ key: it.id, it });
    } else if (it.kind === "tool" || settledSub || isChatter(it)) {
      if (prev && "tools" in prev) prev.tools.push(it);
      else out.push({ key: it.id, tools: [it] });
    } else out.push({ key: it.id, it });
  }
  return out;
}

/** Past-tense phrase per verb: [singular, plural-noun]. */
const PAST: Record<string, [string, string]> = {
  Read: ["read", "files"], Find: ["searched", "patterns"], Search: ["searched", "patterns"], Edit: ["edited", "files"], Write: ["created", "files"],
  Run: ["ran", "commands"], Start: ["started", "commands"], Check: ["checked", "jobs"], Stop: ["stopped", "jobs"], Fetch: ["fetched", "pages"],
  View: ["viewed", "images"], Shot: ["took", "screenshots"], Message: ["messaged", "agents"], Wait: ["waited on", "jobs"], Agents: ["listed", "agents"],
  GitHub: ["used", "GitHub actions"], Computer: ["used", "computer actions"],
  Open: ["opened", "pages"], Click: ["clicked", "page elements"], Type: ["typed into", "fields"], Back: ["went back", "pages"],
  Look: ["looked at", "pages"], "Run JS": ["ran", "page scripts"],
};
const ING: Record<string, string> = { Read: "Reading", Find: "Searching", Search: "Searching", Edit: "Editing", Write: "Writing", Run: "Running", Start: "Starting", Fetch: "Fetching", View: "Viewing", Wait: "Waiting on", Open: "Opening", Click: "Clicking", Type: "Typing", Look: "Looking at", "Run JS": "Running" };
const clip = (s: string) => (s.length > 40 ? s.slice(0, 39) + "…" : s);
const short = (s: string) => { const b = s.split(/[\\/]/).pop() || s; return b.length > 40 ? b.slice(0, 39) + "…" : b; };

function summarize(tools: Item[], cwd: string, bgs: Bg[]) {
  const order: string[] = [];
  const by: Record<string, string[]> = {};
  let add = 0, del = 0, failed = 0;
  const from: string[] = [];
  let sent = 0;
  let routes = 0;
  let routed = 0;
  let asks = 0;
  let answered = 0;
  let subagents = 0;
  const events: string[] = [];
  // `http_request` rows are keyed by their method (GET, POST…), which is not a
  // word the PAST table can conjugate — and lowercasing it reads as "post items".
  const http = new Set<string>();
  for (const t of tools) {
    if (t.kind === "notice") {
      const ev = eventSum(t);
      if (ev) events.push(ev);
      else if (t.data?.level === "router") routed++;
      else if (t.data?.level === "ask") asks++;
      else if (t.data?.level === "subq") answered++;
      // A message to another chat is the one agent-message that went *out*:
      // counting it as "got a message" read as the opposite of what happened.
      else if (t.data?.level === "msg" && t.data?.to) sent++;
      // `data.from` first: a cross-chat message names a chat *title*, which can
      // contain spaces, and the text regex below only reads a single token out
      // of "Message from X:" — the older in-chat form, where X is an agent id.
      else from.push(String(t.data?.from ?? "").trim() || (t.text.match(/^(?:Message from )?([\w.-]+)\s*(?:→|:)/)?.[1] ?? "").trim());
      continue;
    }
    // A finished subagent is not one of the verb groups below: it is a whole run
    // of its own, so it reads as a count. A live one is never in here to count.
    if (t.kind === "sub") {
      subagents++;
      if (endedBad(t, bgs)) failed++;
      continue;
    }
    const d = t.data ?? {};
    const view = toolView(d, cwd);
    const settled = isSettledBg(t, bgs);
    const v = settled ? { ...view, verb: "Run" } : view;
    if (!by[v.verb]) { by[v.verb] = []; order.push(v.verb); }
    if (d.name === "http_request" && !settled) http.add(v.verb);
    // Seeded above, and only re-seeded here when this is the verb's first entry,
    // so the read below always finds the list it just pushed onto.
    by[v.verb]!.push(String(v.target || ""));
    if (Array.isArray(d.diff)) for (const l of d.diff) { if (l.k === "a") add++; else if (l.k === "d") del++; }
    if (d.status === "error" || d.status === "denied" || (settled && endedBad(t, bgs))) failed++;
  }
  // `order` only ever names verbs this loop pushed a list for, so the lookup
  // can't miss — and an empty list would have been a crash before, not a
  // shorter phrase, so there is no other reading to fall back to here.
  const parts = order.map((verb) => {
    const list = by[verb]!;
    if (http.has(verb)) return list.length === 1 ? `sent ${verb} to ${clip(list[0]!) || "a url"}` : `sent ${list.length} ${verb} requests`;
    const [past, noun] = PAST[verb] ?? [verb.toLowerCase(), "times"];
    if (list.length === 1) {
      if (verb === "Run" || verb === "Start") return `${past} a command`;
      const one = list[0]!;
      if (verb === "Find" || verb === "Search") return `${past} for ${clip(one) || "files"}`;
      return one ? `${past} ${short(one)}` : past;
    }
    return `${past} ${list.length} ${noun}`;
  });
  if (from.length) {
    const who = [...new Set(from.filter(Boolean))];
    parts.push(`got ${from.length === 1 ? "a message" : `${from.length} messages`}${who.length === 1 ? ` from ${who[0]}` : ""}`);
  }
  if (sent) parts.push(sent === 1 ? "sent a message to another chat" : `sent ${sent} messages to other chats`);
  if (routes) parts.push(routes === 1 ? "switched model" : `switched model ${routes} times`);
  if (routed) parts.push(routed === 1 ? "the router switched model" : `the router switched model ${routed} times`);
  if (asks) parts.push(asks === 1 ? "asked the main agent" : `asked the main agent ${asks} questions`);
  if (answered) parts.push(answered === 1 ? "answered a question" : `answered ${answered} questions`);
  if (subagents) parts.push(`${subagents} subagent${subagents === 1 ? "" : "s"} finished`);
  parts.push(...events);
  const text = parts.join(", ");
  return { text: text.charAt(0).toUpperCase() + text.slice(1), add, del, failed };
}

/** Whether a piece of settled work ended badly. A subagent says so in its own
 *  status; a command has to be read off its exit, because the tool call that
 *  started it succeeded — the process it launched is what came back non-zero. */
function endedBad(it: Item, bgs: Bg[]): boolean {
  const d = it.data ?? {};
  if (it.kind === "sub") return d.status === "failed" || d.status === "stopped";
  const ex = bgs?.find((b) => b.id === d.bg_id)?.exit;
  if (!ex) return false;
  if (ex.includes("killed")) return true;
  const code = ex.match(/^exit (-?\d+)/)?.[1];
  return code != null && code !== "0";
}

/** One finished piece of background work. A subagent keeps its pill, so the row
 *  still opens its transcript; a command gets the exit the transcript never got,
 *  because the tool call that started it only ever reported that it had started. */
const DoneRow = memo(function DoneRow({ it, task, bgs }: { it: Item; task: TaskSummary; bgs: Bg[] }) {

  if (it.kind === "sub") return <Sub it={it} task={task} held={false} />;
  const job = bgs?.find((b) => b.id === it.data?.bg_id);
  return (
    <div className="bgdone">
      <ToolItem it={it} cwd={task.cwd} />
      {job && <div className="mono bg-exit" title={job.last_line}>{job.exit ?? "stopped"}{job.last_line ? ` · ${job.last_line}` : ""}</div>}
    </div>
  );
});

/** A run of tool calls folded into one line. Memoized, and the calls themselves
 *  are only mounted once the fold is open — same reason as ToolItem. */
const ToolGroup = memo(function ToolGroup({ tools, task, held, bgs }: { tools: Item[]; task: TaskSummary; held: boolean; bgs: Bg[] }) {
  const [open, setOpen] = useState(false);
  // `groupFeed` only ever builds a group with at least one call in it, so the
  // tail is really there; before, an empty group would have thrown here.
  const last = tools[tools.length - 1]!;
  // Same distinction as the rows, and the same exception. A pause freezes the turn
  // between calls, so a tail that is merely waiting to be resumed is not running and
  // must not claim to be — "Reading foo.rs…" in a shimmer is the loudest thing on
  // screen saying work is under way. But a call that already started its process is
  // genuinely going, and this is the line most of the time the user is looking at,
  // since the group is folded by default.
  const running = last.kind === "tool" && last.data?.status === "running" && (!held || !!last.data?.exec);
  const cwd = task.cwd;
  // Only the tail can be running, so the summary is taken over the rest — and
  // recomputed when calls, job results or the folder change, not per streamed token.
  const s = useMemo(() => summarize(running ? tools.slice(0, -1) : tools, cwd, bgs), [tools, cwd, running, bgs]);
  const lv = toolView(last.data ?? {}, cwd);
  // `split("\n")` always yields at least one piece, so the head of a one-line
  // target is there to take.
  const tgt = String(lv.target || "").split("\n")[0]!;
  const ing = last.data?.name === "http_request" ? `Sending ${lv.verb} to` : ING[lv.verb] ?? lv.verb;
  const now = running ? `${ing} ${["Read", "Edit", "Write", "View"].includes(lv.verb) ? short(tgt) : clip(tgt)}…` : "";
  return (
    <div className="toolgroup">
      <Pressable className="tg-line" aria-expanded={open} onClick={() => setOpen(!open)}>
        {s.text && <span className="tg-text">{s.text}{running ? "," : ""}</span>}
        {running && <span className="shimmer tg-now">{s.text ? now.charAt(0).toLowerCase() + now.slice(1) : now}</span>}
        {(s.add > 0 || s.del > 0) && <span className="tg-diff"><span style={{ color: "var(--diff-add)" }}>+{s.add}</span> <span style={{ color: "var(--diff-del)" }}>−{s.del}</span></span>}
        {s.failed > 0 && <span className="tg-failed">· {s.failed} failed</span>}
        <span className="tg-chev" style={{ transform: `rotate(${open ? 90 : 0}deg)` }}><svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M3.5 2 L6.5 5 L3.5 8" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" /></svg></span>
      </Pressable>
      <div className="fold" style={{ gridTemplateRows: open ? "1fr" : "0fr" }}>
        <div>{open && <div className="tg-list">{tools.map((t) => t.kind === "notice" ? <div key={t.id} className="tg-note"><Notice it={t} /></div> : isSettledBg(t, bgs) ? <DoneRow key={t.id} it={t} task={task} bgs={bgs} /> : <ToolItem key={t.id} it={t} cwd={cwd} held={held} />)}</div>}</div>
      </div>
    </div>
  );
// `groupFeed` builds a fresh `tools` array on every pass, so the group is
// compared call by call: a streamed token elsewhere in the turn leaves every
// call the same object, and without this every group in the live turn re-ran
// `summarize` on every flush.
}, (a, b) => sameItems(a.tools, b.tools) && a.held === b.held && feedTaskEqual(a.task, b.task, a.tools) && (a.bgs === b.bgs || bgLook(a.bgs, a.tools) === bgLook(b.bgs, b.tools)));

/** Interactive items never fold away: the user may still need to act on them. */
const KEEP = new Set(["user", "approval", "question"]);
const WORK = new Set(["tool", "thinking", "sub"]);

function fmtDur(ms: number) {
  const s = Math.max(1, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

type Turn = { key: string; head: Item[]; work: Item[]; tail: Item[]; ms: number; actions: number };

/** The quiet copy of a goal notice, one per notice object: a fresh copy on every
 *  pass was a new item each time, so the row showing it never hit its memo. */
const quietCopies = new WeakMap<Item, Item>();
function quietCopy(i: Item): Item {
  let q = quietCopies.get(i);
  if (!q) quietCopies.set(i, (q = { ...i, data: { ...i.data, quiet: true } }));
  return q;
}

/** Split a finished turn into: the user's message, the work in between, and the final answer. */
function splitTurns(items: Item[], live: boolean): (Item | Turn)[] {
  const turns: Item[][] = [];
  for (const it of items) {
    if (it.kind === "user" || !turns.length) turns.push([it]);
    // The `else` only runs once `turns` holds something, and the branch above is the
    // only thing that adds to it, so the turn being extended is the last one.
    else turns[turns.length - 1]!.push(it);
  }
  const out: (Item | Turn)[] = [];
  turns.forEach((t, ti) => {
    const done = !live || ti < turns.length - 1;
    const headN = t[0]?.kind === "user" ? 1 : 0;
    let end = t.length;
    const isAnswer = (i: Item) => i.kind === "text" || (i.kind === "notice" && !!i.data?.goal);
    // `end > headN` is `headN >= end` false, and headN is 0 or 1, so this indexes
    // inside the turn: the item at headN when headN is 1, the first when 0.
    while (end > headN && isAnswer(t[end - 1]!)) end--;
    const mid = t.slice(headN, end);
    const actions = mid.filter((i) => i.kind === "tool" || i.kind === "sub").length;
    if (!done || end === t.length || !mid.some((i) => WORK.has(i.kind)) || mid.some((i) => KEEP.has(i.kind) || hasArtifictCardSlot(i))) { out.push(...t); return; }
    // A turn always has a head (the first item pushed into it is what made it), and
    // `t[headN] ?? t[0]` picks the user's message when there is one.
    const start = new Date((t[headN] ?? t[0]!).ts).getTime();
    const stop = new Date(t[t.length - 1]!.ts).getTime();
    const tail = t.slice(end);
    // Goal summary + the agent's own summary would say the same thing twice: keep the agent's.
    const hasText = tail.some((i) => i.kind === "text" && i.text.trim());
    const quietTail = tail.map((i) => (hasText && i.kind === "notice" && i.data?.goal ? quietCopy(i) : i));
    out.push({ key: "turn-" + t[0]!.id, head: t.slice(0, headN), work: mid, tail: quietTail, ms: stop - start, actions });
  });
  return out;
}

/** `splitTurns` rebuilds every turn object on every pass, so a folded turn is
 *  compared by what it shows and by the calls inside it. */
const Worked = memo(function Worked({ turn, task, showThinking, onOpenArtifict }: { turn: Turn; task: TaskSummary; showThinking: boolean; onOpenArtifict?: (selection: ArtifictWorkspaceSelection) => void }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="it">
      <Pressable className="tg-line worked" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span className="tg-text">Worked for {fmtDur(turn.ms)}{turn.actions ? ` · ${turn.actions} action${turn.actions === 1 ? "" : "s"}` : ""}</span>
        <span className="tg-chev" style={{ transform: `rotate(${open ? 90 : 0}deg)` }}><svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M3.5 2 L6.5 5 L3.5 8" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" /></svg></span>
      </Pressable>
      <div className="fold" style={{ gridTemplateRows: open ? "1fr" : "0fr" }}>
        <div>{open && <div className="worked-body"><FeedRows items={turn.work} task={task} showThinking={showThinking} onOpenArtifict={onOpenArtifict} /></div>}</div>
      </div>
    </div>
  );
}, (a, b) => a.turn.key === b.turn.key && a.turn.ms === b.turn.ms && a.turn.actions === b.turn.actions && sameItems(a.turn.work, b.turn.work) && a.showThinking === b.showThinking && a.onOpenArtifict === b.onOpenArtifict && feedTaskEqual(a.task, b.task, a.turn.work));

/** The store is read ONCE per transcript, up here, and handed down: a subscription
 *  per feed row meant every streamed delta re-ran one selector per row. */
const NO_BGS: Bg[] = [];
function isVisibleFeedItem(item: Item): boolean {
  return !(item.kind === "user" && item.data?.queued) && (item.kind !== "asklater" || !!item.data?.answers || !!item.data?.dismissed) && (item.kind !== "user_notice" || !!item.data?.dismissed);
}

/** Plain feed rows. Exported so a render test can pin what the transcript shows.
 *
 *  Memoized item by item: `Feed` slices a new array for every run and turn on
 *  every pass, and a finished turn's rows are the same item objects each time. */
export const FeedRows = memo(function FeedRows({ items, task, showThinking, bgs, streamingItemId, onOpenArtifict }: { items: Item[]; task: TaskSummary; showThinking: boolean; bgs?: Bg[]; streamingItemId?: string; onOpenArtifict?: (selection: ArtifictWorkspaceSelection) => void }) {
  // A queued message hasn't been sent yet, so it has no place in the transcript
  // — it lives in the list above the composer until the agent reads it, and
  // rejoins the feed as an ordinary message the moment that happens. An
  // `ask_nonblocking` questions stay above the composer while unanswered;
  // once dealt with, they rejoin the transcript as compact receipts, where
  // their answers remain visible without duplicating the live forms.
  const sent = useMemo(() => items.filter(isVisibleFeedItem), [items]);
  // Read once here, not per row: this is the same store, and a subscription per
  // feed row meant every streamed delta re-ran one selector per row. `bgs` lets a
  // caller (a render test) hand the list in instead of the store providing it.
  const stored = useStore((s) => s.bg[task.id] ?? NO_BGS);
  const jobs = bgs ?? stored;
  // Read once here for the same reason: the freeze is a property of the chat, and
  // a subagent pill or a tool row that asks for itself cannot know it.
  const held = isPaused(task, useStore((s) => s.settings?.paused_all ?? false));
  const rows = useMemo(() => groupFeed(sent, showThinking), [sent, showThinking]);
  return (
    <>
      {rows.map((e) =>
        "tools" in e
          ? <div key={e.key} className="it" data-item={e.key}><ToolGroup tools={e.tools} task={task} held={held} bgs={jobs} /></div>
          : <Row key={e.key} it={e.it} task={task} showThinking={showThinking} held={held} streaming={e.it.id === streamingItemId} onOpenArtifict={onOpenArtifict} />)}
    </>
  );
}, (a, b) => sameItems(a.items, b.items) && a.showThinking === b.showThinking && a.bgs === b.bgs && a.streamingItemId === b.streamingItemId && a.onOpenArtifict === b.onOpenArtifict && feedTaskEqual(a.task, b.task, a.items));

function Feed({ items, task, live, showThinking, onOpenArtifict }: { items: Item[]; task: TaskSummary; live: boolean; showThinking: boolean; onOpenArtifict?: (selection: ArtifictWorkspaceSelection) => void }) {
  let latest: Item | undefined;
  if (live) {
    for (let i = items.length - 1; i >= 0; i--) {
      const it = items[i]!;
      if (isVisibleFeedItem(it)) { latest = it; break; }
    }
  }
  const streamingItemId = latest?.kind === "text" ? latest.id : undefined;
  const parts = useMemo(() => splitTurns(items, live), [items, live]);
  const rows: React.ReactNode[] = [];
  let run: Item[] = [];
  const flush = () => { if (run.length) rows.push(<FeedRows key={"r-" + run[0]!.id} items={run} task={task} showThinking={showThinking} streamingItemId={streamingItemId} onOpenArtifict={onOpenArtifict} />); run = []; };
  for (const p of parts) {
    if ("work" in p) {
      flush();
      // A folded turn is a finished one, never the live turn the streaming
      // reply is in, so its rows are not handed the id: when a new reply starts
      // streaming, only the live run has anything to redraw.
      rows.push(<FeedRows key={"h-" + p.key} items={p.head} task={task} showThinking={showThinking} onOpenArtifict={onOpenArtifict} />);
      rows.push(<Worked key={p.key} turn={p} task={task} showThinking={showThinking} onOpenArtifict={onOpenArtifict} />);
      rows.push(<FeedRows key={"t-" + p.key} items={p.tail} task={task} showThinking={showThinking} onOpenArtifict={onOpenArtifict} />);
    } else run.push(p);
  }
  flush();
  return <>{rows}</>;
}

/** A feed row. Memoized, because the timeline re-renders on every streamed delta
 *  and most rows are already-finished text that cannot have changed. */
const Row = memo(function Row({ it, task, showThinking, held, streaming = false, onOpenArtifict }: { it: Item; task: TaskSummary; showThinking: boolean; held: boolean; streaming?: boolean; onOpenArtifict?: (selection: ArtifictWorkspaceSelection) => void }) {
  if (it.kind === "thinking" && !showThinking) return null;
  return (
    <div className="it" data-item={it.id}>
      {it.kind === "user" && (it.data?.brief ? <div className="brief"><div className="label" style={{ marginBottom: 4 }}>Brief from the main agent</div><Md text={it.text} /></div> : <UserMsg it={it} task={task} />)}
      {it.kind === "text" && <Reply it={it} streaming={streaming} />}
      {it.kind === "thinking" && <Thinking text={it.text} />}
      {it.kind === "tool" && (hasArtifictCardSlot(it)
        ? <InlineArtifictCard item={it} taskId={task.id} onOpenArtifict={onOpenArtifict} />
        : <ToolItem it={it} cwd={task.cwd} held={held} />)}
      {it.kind === "artifict" && <InlineArtifictCard item={it} taskId={task.id} onOpenArtifict={onOpenArtifict} />}
      {it.kind === "approval" && <Approval it={it} task={task} onOpenArtifict={onOpenArtifict} />}
      {it.kind === "question" && <Question it={it} task={task} />}
      {it.kind === "asklater" && (it.data?.answers || it.data?.dismissed) && <Question it={it} task={task} />}
      {it.kind === "sub" && <Sub it={it} task={task} held={held} />}
      {it.kind === "notice" && <Notice it={it} />}
      {it.kind === "user_notice" && it.data?.dismissed && <div className="agent-notice-receipt"><Info size={13} aria-hidden="true" /><div><strong>{it.data?.title || "Agent notice"}</strong> · dismissed<div>{it.text}</div></div></div>}
    </div>
  );
// The chat summary is a new object on every agent step; a row compares only the
// parts of it the row reads (see `feedTaskEqual`).
}, (a, b) => a.it === b.it && a.showThinking === b.showThinking && a.held === b.held && a.streaming === b.streaming && a.onOpenArtifict === b.onOpenArtifict && feedTaskEqual(a.task, b.task, [a.it]));

function useStick(dep: unknown) {
  const ref = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  // Only when the transcript grew: scrollHeight is a forced layout, and this
  // runs on every render of the panel otherwise.
  const tail = `${dep}`;
  useLayoutEffect(() => {
    const el = ref.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [tail]);
  useEffect(() => {
    stick.current = true;
    const el = ref.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [dep]);
  const onScroll = (e: React.UIEvent<HTMLDivElement>) => { const el = e.currentTarget; stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80; };
  return { ref, onScroll };
}

/** A subagent's own live transcript, model and status. Exported for its test: the
 *  brief it was launched with has to show in a chat reopened after launch. */
export function SubPanel({ task, sub }: { task: TaskSummary; sub: SubInfo }) {
  const items = useStore((s) => s.subItems[task.id]?.[sub.id] ?? NO_ITEMS);
  const agents = useStore((s) => s.agents);
  const showThinking = useStore((s) => s.settings?.show_thinking ?? false);
  const def = agents.find((a) => a.id === sub.role);
  const { ref, onScroll } = useStick(sub.id);
  const running = sub.status === "running";
  // A frozen chat freezes every agent in it, and this is where a subagent's own
  // state is read — so it asks the chat, not `sub.status`: a parked agent still
  // reads `running`, which is right on its own and wrong here.
  const held = isPaused(task, useStore((s) => s.settings?.paused_all ?? false));
  // Resolved the same way the backend resolves it, so the panel can never name a
  // model this agent isn't on. `sub.model` is empty unless somebody chose one.
  const model = subModel(task, sub);
  // With no model of its own, it is on the chat's — and the chat may have a swap
  // queued, so say which model that is rather than the one the chat is on today.
  const follows = !sub.model;
  // What the picker shows is the level this subagent's own model will send, so
  // a Max-only model doesn't claim "Medium".
  const eff = sub.effort ?? def?.effort ?? (def?.tools === "read_only" ? Math.max(3, task.effort) : task.effort);
  const effLabel = effortLabel(modelInfo(model), eff);
  // The live task event ships no reports, so ask for this one when the panel
  // opens. A report is read once, here, rather than re-sent on every status
  // change of every agent in the swarm.
  const [report, setReport] = useState(sub.report);
  useEffect(() => {
    let live = true;
    if (!sub.report) api.subReport(task.id, sub.id).then((r) => { if (live) setReport(r); }).catch(() => {});
    else setReport(sub.report);
    return () => { live = false; };
  }, [task.id, sub.id, sub.report]);
  // The transcript is read when the panel opens, not with the chat: `task_open`
  // sends only the main timeline. `loaded` stops the "no transcript" line from
  // flashing while the read is still in flight.
  const [loaded, setLoaded] = useState(false);
  useEffect(() => {
    let live = true;
    setLoaded(false);
    void loadSubTranscript(task.id, sub.id).finally(() => { if (live) setLoaded(true); });
    return () => { live = false; };
  }, [task.id, sub.id]);
  return (
    <AgentColor.Provider value={modelColor(model)}>
    <div className="subpanel">
      <div className="sph">
        <Bot aria-hidden="true" size={15} color="var(--mut2)" />
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ fontWeight: 600, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{sub.task}</div>
          <div style={{ fontSize: 11, color: "var(--mut3)" }}>{def?.name ?? sub.role} · {running ? sub.meta || "running" : sub.status}{sub.started ? ` · ${ago(sub.started)}` : ""}{sub.branch ? <Tooltip content={`Own worktree: ${sub.cwd}`}><span className="mono" style={{ marginLeft: 6, color: "var(--violet)" }}>{sub.branch}</span></Tooltip> : null}</div>
        </div>
        {running && (
          <Tooltip content="Stop this subagent · the rest of the chat keeps going">
            <IconButton label="Stop subagent" onClick={() => api.subStop(task.id, sub.id).catch((e) => flash(String(e)))}>{I.stop()}</IconButton>
          </Tooltip>
        )}
        <IconButton label="Close" onClick={() => set({ subOpen: null })}>{I.close()}</IconButton>
      </div>
      <div className="spmodel">
        <Tooltip content={running ? (follows ? "Following the chat's model · switch it, or pick a model of its own" : "Switch this subagent's model · takes effect on its next request") : "Model"}><MenuButton style={{ border: "1px solid rgba(255,255,255,0.08)", height: 26 }}
          onClick={(e) => set({ menu: "model", pickFor: { sub: sub.id }, menuAnchor: (e.currentTarget as HTMLElement).getBoundingClientRect() })}>
          {modelInfo(model).name}{follows && <span className="effl" style={{ opacity: 0.7 }}>from chat</span>}{effLabel && <span className="effl">{effLabel}</span>}{I.chev()}
        </MenuButton></Tooltip>
        {/* The way back: an agent with a model of its own is a one-way door
            otherwise, since picking a model is all the picker can do. */}
        {!follows && <Tooltip content="Go back to following the chat's model"><Button variant="ghost" style={{ height: 26, fontSize: 11 }} onClick={() => api.subModel(task.id, sub.id, "").catch((e) => flash(String(e)))}>Follow chat</Button></Tooltip>}
        {sub.serving && <Tooltip content="Serving its requests"><span className="mono" style={{ fontSize: 10.5, color: "var(--hint)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>via {sub.serving}</span></Tooltip>}
      </div>
      <div ref={ref} className="sptl" onScroll={onScroll}>
        <Feed items={items} task={task} live={running} showThinking={showThinking} />
        {running && (
          // A frozen chat freezes every agent in it at once, and this panel is
          // where a subagent's own state is read: the shimmer said "working" for
          // an agent parked mid-turn. The chat's pause decides, not the agent's
          // status — a parked agent still reads `running`, which is correct on
          // its own and wrong here.
          <div className="working">
            {/* The freeze comes from the chat, so the panel takes its colour from
                there: the shimmer says "working" for an agent parked mid-turn. */}
            {held ? <StatusIcon status="paused" /> : <StatusIcon status="running" color={modelColor(model)} />}
            <span className={held ? "" : "shimmer"}>{sub.meta || "Working"}</span>
          </div>
        )}
        {!running && report && <div className="report"><div className="label" style={{ marginBottom: 6 }}>Report to the main agent</div><Md text={report} /></div>}
        {!items.length && !running && loaded && <div className="empty">No transcript saved for this subagent.</div>}
      </div>
      <SubInput task={task} sub={sub} />
    </div>
    </AgentColor.Provider>
  );
}

/** Talk to one subagent directly: steers it if running, continues it if finished.
 *
 *  Exported for its test: this is the app's second message box, and the phone
 *  rule (`messageSendKey`) has to be shown to hold here too, not only in the
 *  composer. */
export function SubInput({ task, sub }: { task: TaskSummary; sub: SubInfo }) {
  const [text, setText] = useState("");
  const [attachedImages, setAttachedImages] = useState<string[]>([]);
  const running = sub.status === "running";
  const sendWith = useStore((s) => s.settings?.send_with);
  const phoneEnterSends = useStore((s) => s.settings?.phone_enter_sends);
  // The same rule the composer follows, for the same reason: a phone's on-screen
  // Return key cannot report a held `Shift`, so there Enter starts a new line and
  // only the button sends, unless `phone_enter_sends` is on. Left to "Enter sends
  // always", this box sent the messages someone was breaking into paragraphs.
  const sendKey = messageSendKey(isPhoneClient(), sendWith, phoneEnterSends);
  const send = () => {
    const t = text.trim();
    if (!t) return;
    setText("");
    void api.subSend(task.id, sub.id, t, attachedImages, attachedImages.map((_, i) => `image-${i + 1}`)).catch((e) => flash(String(e)));
  };
  return (
    <div className="subinput">
      <TextArea className="" rows={1} value={text} placeholder={running ? `Message ${sub.role} directly…` : `Follow up with ${sub.role}…`} onChange={(e) => setText(e.currentTarget.value)} onPaste={(e) => { const pasted = Array.from(e.clipboardData.files); if (pasted.some((f) => f.type.startsWith("image/"))) { e.preventDefault(); void pastedImages(pasted).then((images) => { if (images.length) setAttachedImages((old) => [...old, ...images]); }); } }}
        onKeyDown={(e) => { if (e.key !== "Enter" || e.nativeEvent.isComposing) return; if (enterSendsMessage(sendKey, e)) { e.preventDefault(); send(); } }} />
      {/* The label names the sending key only when a key sends: on a phone with
          Enter-to-send off, promising Enter would teach the opposite of what
          Enter does. */}
      <RoundButton label={`Send${sendKey ? ` · ${sendKey}` : ""}`} className={text.trim() ? "ready" : ""} onClick={send}>{I.up(11)}</RoundButton>
    </div>
  );
}

/** Floating composer; publishes its height as --cfloat-h so the timeline pads past it. */
function CFloat({ task }: { task: TaskSummary }) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    const pane = el?.parentElement;
    if (!el || !pane) return;
    const sync = () => pane.style.setProperty("--cfloat-h", `${el.offsetHeight}px`);
    sync();
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return <div ref={ref} className="cfloat"><PausedBar task={task} /><div className="composer-attention"><AgentNoticeList taskId={task.id} /><AskNonBlockingList task={task} /></div><QueueList task={task} /><Annotations chat={task.id} /><Composer mode="session" /></div>;
}

function PausedBar({ task }: { task: TaskSummary }) {
  const all = useStore((s) => s.settings?.paused_all ?? false);
  // The same rule as the rest of the app (`heldByGlobal`), status check included.
  // This used to be `all && !task.unpaused`, which the backend does not do: it only
  // freezes chats that are *working*. So an idle or finished chat opened while a
  // stranded flag was set got a pause bar claiming "Everything is paused" on a
  // chat that nothing was holding.
  const globally = heldByGlobal(task, all);
  const reason = task.paused?.reason ?? (globally ? "Paused by Pause all" : "");
  const [full, setFull] = useState(false);
  const [clipped, setClipped] = useState(false);
  const line = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = line.current;
    if (el && !full) setClipped(el.scrollHeight > el.clientHeight + 1);
    // Measured when the text to measure changes, not on every render: this is a
    // forced synchronous layout, and the pause banner re-renders on every task
    // event, so it used to measure once per agent status change.
  }, [reason, full]);
  if (!task.paused && !globally) return null;
  const exhausted = task.paused?.kind === "exhausted";
  // "Exhausted" = every model in the chain failed; only call it usage when it really is limits.
  const usage = exhausted && /429|rate.?limit|quota|usage|credits?|billing|insufficient|limit reached|too many requests|free tier/i.test(reason);
  const long = clipped || full;
  const draining = task.busy > 0;
  const title = draining ? `Pausing… waiting on ${task.busy} command${task.busy === 1 ? "" : "s"} to finish` : usage ? "Paused · out of usage" : exhausted ? "Paused · every model failed" : task.paused?.kind === "closed" ? "Paused · the app closed mid-run" : globally ? "Everything paused" : "Paused";
  return (
    <div className="pausebar">
      <span className="pz">{I.pause(12)}</span>
      <div className="pb-main">
        <div className="pb-title">{title}{task.paused ? <span className="pb-ago">{ago(task.paused.since)} ago</span> : null}</div>
        <div ref={line} className={`pb-reason${full ? " open sel" : ""}`} title={full ? undefined : reason}>{reason}. Agents are frozen mid-step; nothing was sent to them.</div>
        {long && (
          <div className="pb-links">
            <span onClick={() => setFull(!full)}>{full ? "Show less" : "Show full message"}</span>
            <span onClick={() => copyText(reason)}>Copy</span>
          </div>
        )}
        <RecoverySummary task={task} />
        <div className="pb-actions">
          <Button style={{ height: 28, background: "#fbbf24", color: "#1b1405", border: 0, fontWeight: 600 }} title={all ? "Resume this chat only — everything else stays frozen" : undefined} onClick={() => api.resume(task.id).catch((e) => flash(String(e)))}>{I.play(10)}Resume</Button>
          {draining && <Button variant="ghost" style={{ height: 28, color: "#fde68a" }} title="Kill running commands now; the agent is told they were cut off and reruns them after resuming" onClick={() => (all ? api.forcePauseAll() : api.forcePause(task.id)).catch((e) => flash(String(e)))}>Force pause</Button>}
          {exhausted && <Button variant="ghost" style={{ height: 28 }} onClick={() => go("settings", { settingsTab: "accounts" })}>Accounts</Button>}
          {exhausted && <Button variant="ghost" style={{ height: 28 }} title="Point this chat at a model you still have" onClick={() => openSwap()}>Swap model</Button>}
          <span style={{ flex: 1 }} />
          <Button variant="ghost" style={{ height: 28, color: "var(--mut3)" }} title="Turn this pause into a stop: frozen work is cancelled and the agent is told when you continue" onClick={() => api.dismissPause(task.id).catch((e) => flash(String(e)))}>Dismiss</Button>
        </div>
      </div>
    </div>
  );
}

/** Announces a reply to a screen reader without announcing it token by token.
 *
 *  The transcript itself is deliberately *not* a live region: a streaming reply
 *  re-renders on every token, so a live region on the text would read the answer
 *  out one fragment at a time. This says the two things that are actually useful
 *  — that a reply started, and what it said once it was finished — and only for
 *  a turn that happened while this chat was open, so opening an old chat does
 *  not read its last message aloud. */
function LiveReply({ live, text }: { live: boolean; text: string }) {
  const [say, setSay] = useState("");
  const wasLive = useRef(false);
  useEffect(() => {
    if (live) {
      wasLive.current = true;
      setSay("Assistant is replying");
      return;
    }
    if (wasLive.current && text.trim()) {
      wasLive.current = false;
      setSay(text);
    }
  }, [live, text]);
  return <div className="sr-only" role="status" aria-live="polite">{say}</div>;
}

/** How long the run has gone without a sign of progress.
 *
 *  A stalled run used to look exactly like a thinking one: the spinner kept
 *  spinning and the step line kept saying what it was doing. This counts the
 *  seconds since the transcript last grew, which is the difference between
 *  "working" and "stuck" — and past a minute it says so outright, because that
 *  is the point at which the user should be deciding whether to stop it.
 *  `tail` is the transcript's growth key, so any real progress resets the count. */
function Waiting({ tail }: { tail: string }) {
  const [secs, setSecs] = useState(0);
  useEffect(() => {
    setSecs(0);
    if (typeof window === "undefined") return;
    const id = window.setInterval(() => setSecs((n) => n + 1), 1000);
    return () => window.clearInterval(id);
  }, [tail]);
  if (secs < 5) return null;
  const mins = Math.floor(secs / 60);
  return (
    <span className="waiting-for" title="Time since the last sign of progress">
      {mins ? `${mins}m ${secs % 60}s` : `${secs}s`}
      {secs >= 60 ? " · still running" : ""}
    </span>
  );
}

/** How close to the end of the transcript counts as being back at it, so the
 *  view follows new output again. Small on purpose: anything bigger swallows the
 *  first steps of a scroll up, which is the bug `onScroll` explains. */
const FOLLOW_SLACK = 24;

/** Where each chat's transcript was left: whether the reader was following its
 *  end, and the rows at the top of the view if not. Leaving a chat and coming
 *  back used to start the view from whatever the previous chat left behind, so
 *  it landed somewhere up the transcript instead of where the reader had been.
 *  Bounded: one small entry per chat visited, oldest dropped first. */
const leftAt = new Map<string, { stick: boolean; anchors: { id: string; top: number }[] }>();
const LEFT_AT_CAP = 64;

/** Scroll so the first of `anchors` still on the page sits where it was. */
function restoreAnchors(el: HTMLElement, anchors: { id: string; top: number }[]): boolean {
  const viewTop = el.getBoundingClientRect().top;
  const rows = new Map<string, HTMLElement>();
  for (const row of el.querySelectorAll<HTMLElement>("[data-item]")) rows.set(row.dataset.item!, row);
  for (const anchor of anchors) {
    const row = rows.get(anchor.id);
    if (!row) continue;
    el.scrollTop += row.getBoundingClientRect().top - viewTop - anchor.top;
    return true;
  }
  return false;
}

/** Keep the view on the live end, and remember where that left it. */
function pinToEnd(el: HTMLElement, lastTop: { current: number }) {
  el.scrollTop = el.scrollHeight;
  lastTop.current = el.scrollTop;
}

/** Whether an upward scroll from `target` goes to a scrollable block inside the
 *  transcript (a long code block, a tool's output) rather than the transcript. A
 *  wheel over one of those must not stop the chat following its live end. */
function innerScrollTakes(target: EventTarget, scroller: HTMLElement) {
  for (let n = target instanceof Element ? target : null; n && n !== scroller; n = n.parentElement) {
    if (n.scrollTop > 0) return true;
  }
  return false;
}

/** The transcript. Exported so a render test can pin the scroll anchoring. */
export function Timeline({ task, onOpenArtifict }: { task: TaskSummary; onOpenArtifict?: (selection: ArtifictWorkspaceSelection) => void }) {
  const items = useStore((s) => s.items[task.id]);
  const pending = useStore((s) => s.pendingMessages[task.id] ?? NO_ITEMS);
  const visibleItems = useMemo(() => pending.length ? [...(items ?? []), ...pending] : items, [items, pending]);
  const showThinking = useStore((s) => s.settings?.show_thinking ?? false);
  const ref = useRef<HTMLDivElement>(null);
  // Whether the reader is following the live end. Only the reader turns it off,
  // by moving up, and only reaching the end again turns it back on. See onScroll.
  const stick = useRef(true);
  // Where the view was last seen, by the scroll handler or by our own writes: how
  // a scroll event tells which way the view moved.
  const lastTop = useRef(0);
  // The rows at the top of the view and where they sat (see the anchoring below).
  const anchors = useRef<{ id: string; top: number }[]>([]);
  const touchY = useRef<number | null>(null);
  const last = visibleItems?.[visibleItems.length - 1];
  // Pin to the bottom only when the transcript actually grew, not on every
  // render: reading scrollHeight forces a synchronous layout, and paying it per
  // streamed token is what made long chats crawl.
  const tail = `${items?.length ?? 0}:${last?.text.length ?? 0}:${pending.length}`;
  const lastReply = useMemo(() => {
    const list = visibleItems ?? [];
    for (let i = list.length - 1; i >= 0; i--) {
      const it = list[i];
      if (it && it.kind === "text" && it.text.trim()) return it.text;
    }
    return "";
  }, [visibleItems]);
  // Rows to put back at the top of the view once the chat being entered has
  // rows to put there (its transcript can still be loading).
  const restoring = useRef<{ id: string; top: number }[] | null>(null);
  // Entering a chat. A layout effect, and declared before the pin below so it
  // runs first: the pane is not remounted between chats, so until this runs the
  // follow flag and scroll offset are the previous chat's. As a passive effect
  // it ran after paint — one frame of the new chat at the old chat's offset,
  // and a pin that skipped the new rows because the old chat was scrolled up.
  useLayoutEffect(() => {
    const saved = leftAt.get(task.id);
    stick.current = saved?.stick ?? true;
    restoring.current = saved && !saved.stick ? saved.anchors : null;
    anchors.current = saved?.anchors ?? [];
    const el = ref.current;
    if (el && stick.current) pinToEnd(el, lastTop);
    return () => {
      leftAt.delete(task.id);
      leftAt.set(task.id, { stick: stick.current, anchors: anchors.current });
      if (leftAt.size > LEFT_AT_CAP) leftAt.delete(leftAt.keys().next().value!);
    };
  }, [task.id]);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (restoring.current) {
      if (!items) return; // still loading: keep the place to restore for later
      // The rows the reader was on may have been dropped while the chat was
      // closed (only its newest rows are kept); then the end is the place.
      if (restoreAnchors(el, restoring.current)) lastTop.current = el.scrollTop;
      else stick.current = true;
      restoring.current = null;
    }
    if (stick.current) pinToEnd(el, lastTop);
  }, [tail, task.id, items]);
  // Scroll anchoring, by hand. Rows change height above the reader all the
  // time — a turn that finishes folds its work into one "Worked" row, a
  // subagent card collapses when it is done, an off-screen row trades its
  // `content-visibility` estimate for its real height — and every such change
  // shifted what was on screen. The browser's own anchoring cannot help: the
  // node it anchors to is the row that just got folded away. So remember where
  // the rows at the top of the view sit; when the layout changes, find the
  // first of them still on the page and scroll by however far it moved. A
  // reader stuck to the bottom is simply kept there.
  const recordAnchors = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    const viewTop = el.getBoundingClientRect().top;
    const found: { id: string; top: number }[] = [];
    for (const row of el.querySelectorAll<HTMLElement>("[data-item]")) {
      const box = row.getBoundingClientRect();
      if (box.bottom <= viewTop) continue;
      found.push({ id: row.dataset.item!, top: box.top - viewTop });
      // Several, so a row folded into "Worked" still leaves one that survived.
      if (found.length >= 8) break;
    }
    anchors.current = found;
  }, []);
  useEffect(() => {
    const el = ref.current;
    const content = el?.querySelector(".timeline-in");
    if (!el || !content || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (stick.current) { pinToEnd(el, lastTop); return; }
      const viewTop = el.getBoundingClientRect().top;
      const rows = new Map<string, HTMLElement>();
      for (const row of el.querySelectorAll<HTMLElement>("[data-item]")) rows.set(row.dataset.item!, row);
      for (const anchor of anchors.current) {
        const row = rows.get(anchor.id);
        if (!row) continue;
        const moved = row.getBoundingClientRect().top - viewTop - anchor.top;
        if (Math.abs(moved) >= 1) { el.scrollTop += moved; lastTop.current = el.scrollTop; }
        break;
      }
      recordAnchors();
    });
    observer.observe(content);
    return () => observer.disconnect();
  }, [task.id, recordAnchors]);
  const live = task.status === "running";
  const streaming = live && last && (last.kind === "text" || (last.kind === "thinking" && showThinking)) && Date.now() - new Date(last.ts).getTime() < 60_000;
  // `isPaused`, not `task.paused`: a chat frozen by "Pause all" carries no pause
  // of its own, and the working line was drawn for it anyway — a shimmering step
  // over a turn that has not moved since the flag landed, which is the whole
  // reason opening a paused chat feels live.
  const paused = isPaused(task, useStore((s) => s.settings?.paused_all ?? false));
  // A question card is the tallest thing in the feed, and the transcript is
  // pinned to the bottom underneath a floating composer. A card with a long
  // title, intro or description is therefore taller than the space it has, so
  // the pin puts its *top* above the window and the first thing on screen is
  // the last option — with nothing on the card to say the text above it exists.
  // The pin isn't wrong, it is just the wrong anchor for a card this shape:
  // when the card's top has gone above the top of the pane, scroll back to it.
  // Only then: a card that already fits needs no help, and moving one would
  // throw away the position the reader was at. It runs after the pin, on the
  // same trigger, so it wins: the pin running last would drag the card's top
  // straight back off screen, and the question would be unreadable again for as
  // long as the agent kept talking.
  //
  // `unanswered` is the gate on the layout read. Without it this would run on
  // every render of every chat, and `offsetTop` is a forced synchronous layout —
  // the exact per-token cost the pin above is written to avoid. A chat with no
  // question pending, which is nearly all of them, never reads it at all.
  const unanswered = useMemo(
    () => (items ?? []).find((i) => i.kind === "question" && !i.data?.answers && !i.data?.dismissed)?.id ?? null,
    [items],
  );
  useLayoutEffect(() => {
    if (!unanswered) return;
    const el = ref.current;
    const card = el?.querySelector<HTMLElement>("[data-pending-question]");
    // offsetTop is relative to `.timeline`, the positioned ancestor, so it is
    // already in the same space as `scrollTop`: negative means the top of the
    // card is scrolled out of view above.
    if (el && card && card.offsetTop < 0) el.scrollTop = card.offsetTop - 12;
  }, [unanswered, tail]);
  return (
    <div ref={ref} className="timeline" onScroll={(e) => {
      const el = e.currentTarget;
      const top = el.scrollTop;
      const fromEnd = el.scrollHeight - top - el.clientHeight;
      // The view has not moved since it was last pinned, but the end has: the
      // content grew under a follower between the pin and this event — rows
      // trading their size estimate for their real height as a chat opens,
      // the newest page of a reopened chat landing, output streaming in. That
      // is not the reader leaving the end, and treating it as "far from the
      // end" below is what left a chat you had just come back to stranded up
      // the transcript while the agent kept working. Follow it instead.
      if (stick.current && Math.abs(top - lastTop.current) <= 1 && fromEnd > FOLLOW_SLACK) {
        pinToEnd(el, lastTop);
        recordAnchors();
        return;
      }
      // Any move up away from the end stops the follow, however small. This used
      // to be "within 80px of the bottom" on every event, and the first frames of
      // a wheel's smooth scroll, or each small touchpad step, are inside those
      // 80px: the follow came straight back on, the next streamed token pinned
      // the view to the bottom and cancelled the scroll, and the screen shook
      // in place. A view pulled up by the content shrinking under it is still
      // at the end (`fromEnd` is 0), so that does not count as the reader moving.
      // Far from the end is never following, whichever way the view got there.
      if ((top < lastTop.current - 1 && fromEnd > 2) || fromEnd > 80) stick.current = false;
      else if (fromEnd <= FOLLOW_SLACK) stick.current = true;
      lastTop.current = top;
      recordAnchors();
      if (top < 100) void loadPreviousHistory(task.id, el);
    }}
    // Also on the input, not only on the scroll event: that event is a frame
    // behind, and a token that streams in between would pin the view back.
    onWheel={(e) => {
      const el = e.currentTarget;
      if (e.deltaY < 0 && el.scrollTop > 0 && !innerScrollTakes(e.target, el)) stick.current = false;
      else if (e.deltaY > 0 && el.scrollHeight - el.scrollTop - el.clientHeight <= FOLLOW_SLACK) stick.current = true;
    }}
    onTouchStart={(e) => { touchY.current = e.touches[0]?.clientY ?? null; }}
    onTouchMove={(e) => {
      const y = e.touches[0]?.clientY ?? null;
      const previous = touchY.current;
      touchY.current = y;
      // A finger moving down the screen moves the content up.
      const el = e.currentTarget;
      if (y !== null && previous !== null && y > previous && el.scrollTop > 0 && !innerScrollTakes(e.target, el)) stick.current = false;
    }}>
      <LiveReply live={live} text={lastReply} />
      <div className="timeline-in">
        {!items && !pending.length && <Loader variant="agent" className="loading-state" size={13} label="Loading conversation…" />}
        {visibleItems && <Feed items={visibleItems} task={task} live={live} showThinking={showThinking} onOpenArtifict={onOpenArtifict} />}
        {live && !streaming && (
          <div className="working">
            {/* A frozen chat keeps its step line but loses both animations: the
                spinner is redrawn by an animation nothing is driving, and the
                shimmer says "happening" for a turn that has not moved. The one
                still shape in the pause colour replaces both. */}
            {paused
              ? <StatusIcon status="paused" />
              : <StatusIcon status="running" color={modelColor(chatModel(task))} />}
            <span className={paused ? "" : "shimmer"}>{task.step || (paused ? "Frozen" : "Working")}</span>
            {!paused && <Waiting tail={tail} />}
            {paused
              ? <span style={{ fontSize: 11, color: "var(--dim2)" }}>Resume to carry on</span>
              : <span className="working-keyhint" style={{ fontSize: 11, color: "var(--dim2)" }}>Esc to pause · Esc Esc to stop</span>}
          </div>
        )}
      </div>
    </div>
  );
}

/** Header chip for ultrathreads: live agent count; opens the swarm tree. */
function SwarmChip({ task, held }: { task: TaskSummary; held: boolean }) {
  const c = swarmCounts(task.subs);
  return (
    <Tooltip content="Open the agent tree"><Button variant="ghost" className="swarmchip" onClick={() => set({ details: true, subOpen: null })}>
      {/* The count is of agents that are *parked*, not working, once the chat is
          frozen, and says so — a chip counting twelve agents "live" over a chat
          that is not running anything is what made a paused swarm look busy. */}
      <span className={"tdot " + (held ? "paused" : c.running ? "running" : "done")} />
      <span><b>{c.running}</b> {held ? "held" : "live"}</span><span className="dim">/ {task.subs.length}</span>
    </Button></Tooltip>
  );
}

type TreeNode = { sub: SubInfo; kids: TreeNode[] };

/** Rows of the agent tree the panel mounts at once. */
const TREE_CAP = 80;

/** Settled agents. Everything else is in flight: still working, or waiting on
 *  the user, or held at a limit the tree has to keep visible. */
export const SETTLED = new Set(["done", "failed", "stopped"]);

/** Whether an agent is in flight, from its status alone. Shared with the Done
 *  line so what gets folded and what stays inline can never disagree. */
export const isLiveSub = (s: Pick<SubInfo, "status">): boolean => !SETTLED.has(s.status);

/** An agent that spawned more work than fits in the row budget: settled, but
 *  with a live subtree, so it belongs in the live tree and not behind Done. */
const busy = (n: TreeNode): boolean => isLiveSub(n.sub) || n.kids.some(busy);

/** Same agents, same nesting, split into the two things the panel shows: the
 *  live subtrees and the settled ones, each keeping parents ahead of their own
 *  children so the cap can only ever cut the quiet end of a list. A swarm
 *  spawns into insertion order, so without this a 300-agent ultrathread buries
 *  the twelve still working under everyone who already finished. */
export function buildTree(subs: SubInfo[], prev?: Map<string, TreeNode>): { live: TreeNode[]; quiet: TreeNode[]; nodes: Map<string, TreeNode> } {
  const byId = new Map(subs.map((s) => [s.id, { sub: s, kids: [] as TreeNode[] }]));
  const roots: TreeNode[] = [];
  for (const n of byId.values()) {
    const p = n.sub.parent ? byId.get(n.sub.parent) : undefined;
    (p ? p.kids : roots).push(n);
  }
  // Hand back last pass's node for every agent whose row would draw the same:
  // the backend republishes the whole list on every status event, so every
  // `sub` is a fresh object each time, and `TreeRow`'s memo on the node could
  // never hit. Bottom-up, so a parent is reused only when its children were.
  const nodes = new Map<string, TreeNode>();
  const reuse = (n: TreeNode, seen: Set<string>): TreeNode => {
    // A parent cycle in a malformed list would recurse for ever; the plain
    // build above already leaves a cycle out of `roots`, this only guards.
    if (seen.has(n.sub.id)) return n;
    seen.add(n.sub.id);
    const kids = n.kids.map((k) => reuse(k, seen));
    const old = prev?.get(n.sub.id);
    const same = !!old && sameSub(old.sub, n.sub) && old.kids.length === kids.length && kids.every((k, i) => k === old.kids[i]);
    const out = same ? old! : { sub: n.sub, kids };
    nodes.set(n.sub.id, out);
    return out;
  };
  const seen = new Set<string>();
  const top = roots.map((r) => reuse(r, seen));
  return { live: top.filter(busy), quiet: top.filter((r) => !busy(r)), nodes };
}

/** Every field of a `SubInfo` is a primitive, so a shallow pass is the whole
 *  comparison. */
function sameSub(a: SubInfo, b: SubInfo): boolean {
  if (a === b) return true;
  const ka = Object.keys(a) as (keyof SubInfo)[];
  if (ka.length !== Object.keys(b).length) return false;
  return ka.every((k) => a[k] === b[k]);
}

/** Every agent a tree shows, parents before children — what the cap spends. */
function rowCount(nodes: TreeNode[]): number {
  return nodes.reduce((n, x) => n + 1 + rowCount(x.kids), 0);
}

/** What the budget actually mounts, following the same rule as TreeRow: a node,
 *  its first child always, and the rest of them only while there's budget left. */
function mountedRows(nodes: TreeNode[], budget: number): number {
  return nodes.reduce((n, x) => n + 1 + (x.kids.length ? mountedRows(budget > 0 ? x.kids : x.kids.slice(0, 1), budget - 1) : 0), 0);
}

function elapsed(since: string | null) {
  if (!since) return "";
  const s = Math.max(0, Math.round((Date.now() - new Date(since).getTime()) / 1000));
  return s < 60 ? `${s}s` : s < 3600 ? `${Math.floor(s / 60)}m` : `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}

/** One agent's row. Memoized on the node: the backend republishes the whole
 *  subagent list on every status event, so `task.subs` is a new array each
 *  time. Without this, one agent taking a step re-rendered all 80 mounted
 *  rows; now only its own does, because `buildTree` hands back the same node
 *  for every agent that didn't change. The chat summary is a new object on
 *  every event too, so it is compared by `look` — the only part of it a row
 *  reads, the model an agent resolves to — rather than by identity. */
const TreeRow = memo(function TreeRow({ node, depth, last, budget, task, held, look }: { node: TreeNode; depth: number; last: boolean; budget: number; task: TaskSummary; held: boolean; look: string }) {
  const a = node.sub;
  const running = a.status === "running";
  // An agent's own status says `running` while it sits parked by a freeze, so
  // the row asks the chat whether anything is moving before it animates: the
  // pulsing dot and the shimmering step were the two places a frozen swarm still
  // looked like it was working.
  const live = running && !held;
  // Resolved, not raw: an agent with no model of its own is on the chat's, and a
  // queued swap moves it, so reading `a.model` here left a whole swarm tinted with
  // the colour of whatever the chat was on when the row was built.
  const m = subModel(task, a);
  return (
    <>
      <Pressable className={"trow" + (live ? " live" : "")} style={{ ["--d" as string]: depth, ["--line" as string]: running ? modelColor(m) : "rgba(255,255,255,0.1)" } as React.CSSProperties} data-last={last || undefined} data-depth={depth} onClick={() => set({ subOpen: a.id })}>
        <span className="tguide" aria-hidden="true" />
        {/* A parked agent keeps its own status colour: the dot says which agent
            this is, and the row's `live` class is what makes it pulse. Dropping
            the glow here too would have read as "stopped", which is a different
            thing — it is still mid-turn, it is just not going anywhere. */}
        <span className={"tdot " + a.status} style={live ? { background: modelColor(m), boxShadow: `0 0 7px ${modelColor(m)}` } : undefined} />
        <div className="tbody">
          <div className="thead"><span className="mono trole">{a.role}</span>{node.kids.length > 0 && <span className="tkids">{node.kids.length} sub{node.kids.length === 1 ? "" : "s"}</span>}{a.background && <span className="bgtag">bg</span>}<span className="ttime">{running ? elapsed(a.started) : a.status}</span></div>
          <div className="ttask">{a.task}</div>
          {running && a.meta && <div className={"tmeta" + (live ? " shimmer" : "")}>{a.meta}</div>}
        </div>
      </Pressable>
      {/* One long swarm can spawn hundreds of agents: once the budget runs out we stop
          descending rather than render every node, and the count below says so. */}
      {node.kids.map((k, i) => budget > 0 || i === 0 ? <TreeRow key={k.sub.id} node={k} depth={depth + 1} last={i === node.kids.length - 1} budget={budget - 1} task={task} held={held} look={look} /> : null)}
    </>
  );
}, treeRowEqual);

function treeRowEqual(a: { node: TreeNode; depth: number; last: boolean; budget: number; held: boolean; look: string }, b: typeof a): boolean {
  return a.node === b.node && a.depth === b.depth && a.last === b.last && a.budget === b.budget && a.held === b.held && a.look === b.look;
}

/** The part of the chat summary the tree's rows read: what `subModel` resolves
 *  an agent's model from. */
function treeLook(t: TaskSummary): string {
  return `${chatModel(t)}\u0000${t.ultra_x ? JSON.stringify(t.ultra_x) : ""}`;
}

/** The agents that finished, folded behind one line. A fold and not a group
 *  because the work really is over: every row inside has a final status, so
 *  nothing is hidden behind the summary that you might still want to watch.
 *  Its count comes straight off the label, so the line can only ever claim what
 *  clicking it actually opens. */
const DoneSection = memo(function DoneSection({ tree, budget, task, held, look }: { tree: TreeNode[]; budget: number; task: TaskSummary; held: boolean; look: string }) {
  const [open, setOpen] = useState(false);
  const n = rowCount(tree);
  const shown = mountedRows(tree, budget);
  if (!n) return null;
  return (
    <div className="tgroup">
      <Pressable className="tg-line" aria-expanded={open} onClick={() => setOpen(!open)} title={open ? undefined : "Show the agents that finished"}>
        <span className="tg-chev" style={{ transform: `rotate(${open ? 90 : 0}deg)` }}><svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M3.5 2 L6.5 5 L3.5 8" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" /></svg></span>
        <span className="tg-text">Done · {n} agent{n === 1 ? "" : "s"}</span>
      </Pressable>
      <div className="fold" style={{ gridTemplateRows: open ? "1fr" : "0fr" }}>
        <div>{open && <div className="tree">{tree.map((x, i) => <TreeRow key={x.sub.id} node={x} depth={0} last={i === tree.length - 1} budget={budget} task={task} held={held} look={look} />)}</div>}</div>
      </div>
      {shown < n && <div style={{ padding: "2px 6px", color: "var(--dim)" }}>Showing {shown} of {n}. Their reports are still on each agent's own page.</div>}
    </div>
  );
}, (a, b) => a.tree.length === b.tree.length && a.tree.every((n, i) => n === b.tree[i]) && a.budget === b.budget && a.held === b.held && a.look === b.look);

export function SwarmTree({ task }: { task: TaskSummary }) {
  const held = isPaused(task, useStore((s) => s.settings?.paused_all ?? false));
  // Last pass's nodes, so `buildTree` can hand back the ones that did not change.
  const prevNodes = useRef<Map<string, TreeNode> | undefined>(undefined);
  const tree = useMemo(() => {
    const t = buildTree(task.subs, prevNodes.current);
    prevNodes.current = t.nodes;
    return t;
  }, [task.subs]);
  const look = treeLook(task);
  const c = swarmCounts(task.subs);
  const total = task.subs.length || 1;
  // Same cap as the chat sidebar: enough rows to read, bounded so a 1000-agent
  // swarm doesn't mount a 1000th row on every status event. The live subtrees
  // get first refusal on it and only what's left over is the Done fold's to
  // spend, so a big swarm can't push the agents still working off the bottom.
  const liveRows = rowCount(tree.live);
  const shownLive = tree.live.length > TREE_CAP ? tree.live.slice(0, TREE_CAP) : tree.live;
  // Rows still unspent once the live tree is mounted, for nesting inside it and
  // for the Done fold. Nothing is left when the live roots alone filled the cap:
  // the swarm is past what the panel reads, and descending is what gives.
  const spare = shownLive.length === tree.live.length ? Math.max(0, TREE_CAP - liveRows) : 0;
  const maxDepth = task.subs.reduce((m, s) => Math.max(m, s.depth ?? 1), 0);
  return (
    <div className={"dsec swarm" + (task.ultra ? " ultra" : "")}>
      <div className="dh"><span style={{ flex: 1 }}>{task.ultra ? "Swarm" : "Subagents"}</span><span>{task.subs.length ? `${c.running} ${held ? "held" : "running"} · ${task.subs.length} total` : ""}</span></div>
      {task.subs.length > 0 && (
        <>
          <div className="swarmbar" aria-hidden="true">
            <span className="sb-done" style={{ flex: c.done / total }} />
            <span className="sb-run" style={{ flex: c.running / total }} />
            <span className="sb-fail" style={{ flex: c.failed / total }} />
            <span className="sb-stop" style={{ flex: c.stopped / total }} />
          </div>
          {task.ultra && <div className="swarmstats"><span><b>{c.done}</b> done</span><span><b>{c.running}</b> live</span>{c.failed > 0 && <span className="bad"><b>{c.failed}</b> failed</span>}<span><b>{maxDepth}</b> level{maxDepth === 1 ? "" : "s"} deep</span></div>}
        </>
      )}
      <div className="tree">{shownLive.map((n, i) => <TreeRow key={n.sub.id} node={n} depth={0} last={i === shownLive.length - 1} budget={spare} task={task} held={held} look={look} />)}</div>
      <DoneSection tree={tree.quiet} budget={spare} task={task} held={held} look={look} />
      {!task.subs.length && <div style={{ padding: "2px 6px", color: "var(--dim)" }}>{task.ultra ? "No subagents yet · the first wave launches after planning" : "None spawned"}</div>}
      {liveRows > shownLive.length && <div style={{ padding: "2px 6px", color: "var(--dim)" }}>{shownLive.length} live of {liveRows} shown · the rest are in the Done line below.</div>}
    </div>
  );
}

/** Whether a background command's process is gone. Read off the job list rather
 *  than off the tool call that started it: that call only ever reported that it
 *  had started, and the process it launched is what came back. */
const bgEnded = (b: Bg) => !b.running;

/** A background command that came back non-zero, so the fold can say so. A
 *  command killed by a force pause never ran to completion either, and that is
 *  the one that most needs saying: the agent was told to rerun it. */
function bgFailed(b: Bg): boolean {
  if (b.running) return false;
  if ((b.exit ?? "").includes("killed")) return true;
  const code = b.exit?.match(/^exit (-?\d+)/)?.[1];
  return code != null && code !== "0";
}

/** One background command row, running or finished. */
function BgRow({ task, b }: { task: TaskSummary; b: Bg }) {
  // Click to expand: the full command, wrapped and selectable. It used to be a
  // centred, non-interactive tooltip that vanished as the pointer moved toward
  // it, so a long command could be neither read in full nor copied.
  const [open, setOpen] = useState(false);
  const wrap = open ? { whiteSpace: "pre-wrap" as const, wordBreak: "break-all" as const, userSelect: "text" as const, cursor: "text" } : { whiteSpace: "nowrap" as const, overflow: "hidden", textOverflow: "ellipsis", cursor: "pointer" };
  return (
    <div className="drow">
      <span style={{ marginTop: 4 }}><StatusIcon status={b.running ? "running" : "done"} /></span>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div className={`mono${open ? " sel" : ""}`} style={{ fontSize: 11.5, color: "#e4e4e7", ...wrap }} title={open ? undefined : "Click to show the whole command"} onClick={() => { if (!window.getSelection()?.toString()) setOpen(!open); }}>{b.cmd}</div>
        <div className="mono" style={{ fontSize: 10.5, color: bgFailed(b) ? "var(--st-fail)" : "var(--mut3)", marginTop: 3, whiteSpace: open ? "pre-wrap" : "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{b.running ? b.last_line || "running" : `${b.exit} · ${b.last_line}`}</div>
        {open && <div style={{ display: "flex", gap: 12, marginTop: 3, fontSize: 11, color: "var(--mut3)" }}><span style={{ cursor: "pointer" }} onClick={() => copyText(b.cmd, "Command copied")}>Copy</span><span style={{ cursor: "pointer" }} onClick={() => setOpen(false)}>Show less</span></div>}
      </div>
      {b.running && <IconButton label="Stop" style={{ width: 22, height: 22 }} onClick={() => api.bgKill(task.id, b.id)}><StatusIcon status="stopped" size={11} /></IconButton>}
    </div>
  );
}

/**
 * The jobs this chat started, with the ones that have since exited folded behind
 * a Done line — the same shape the swarm tree and the transcript's Done fold use,
 * and for the same reason.
 *
 * A chat that runs tests, a dev server and a build leaves a long column of exited
 * processes behind it, all identical rows with a green tick. They are receipts,
 * not work in progress: nothing about them can change, and there is no Stop button
 * left to reach for. Keeping them inline meant the live jobs were pushed down a
 * column the user had to scroll past on every check, which is how a chat with one
 * running server reads as though it has thirty things going.
 */
export function BgCommands({ task, bgs }: { task: TaskSummary; bgs: Bg[] }) {
  const live = bgs.filter((b) => !bgEnded(b));
  const done = bgs.filter(bgEnded);
  const [open, setOpen] = useState(false);
  const bad = done.filter(bgFailed).length;
  return (
    <div className="dsec">
      <div className="dh"><span style={{ flex: 1 }}>Background commands</span><span>{live.length || ""}</span></div>
      {live.map((b) => <BgRow key={b.id} task={task} b={b} />)}
      {/* The count comes straight off the rows behind the line, so it can never
          claim something clicking it would not open. */}
      {done.length > 0 && (
        <div className="tgroup">
          <Pressable className="tg-line" aria-expanded={open} onClick={() => setOpen(!open)} title={open ? undefined : "Show the commands that have finished"}>
            <span className="tg-chev" style={{ transform: `rotate(${open ? 90 : 0}deg)` }}><svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M3.5 2 L6.5 5 L3.5 8" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" /></svg></span>
            <span className="tg-text">Done · {done.length} command{done.length === 1 ? "" : "s"}</span>
            {bad > 0 && <span className="tg-failed">· {bad} did not finish</span>}
          </Pressable>
          <div className="fold" style={{ gridTemplateRows: open ? "1fr" : "0fr" }}>
            <div>{open && done.map((b) => <BgRow key={b.id} task={task} b={b} />)}</div>
          </div>
        </div>
      )}
      {!bgs.length && <div style={{ padding: "2px 6px", color: "var(--dim)" }}>Nothing running</div>}
    </div>
  );
}

/** How long the details rail keeps its contents after closing: its width and
 *  opacity transitions (`.details`, `.details-in`) run for up to .28s, and
 *  emptying it at once would blank the panel mid-slide. */
const DETAILS_LINGER_MS = 320;

/** `on`, held true for `ms` after it turns false. */
function useLinger(on: boolean, ms: number): boolean {
  const [kept, setKept] = useState(on);
  useEffect(() => {
    if (on) { setKept(true); return; }
    const t = setTimeout(() => setKept(false), ms);
    return () => clearTimeout(t);
  }, [on, ms]);
  return on || kept;
}

function Details({ task }: { task: TaskSummary }) {
  const open = useStore((s) => s.details);
  // A closed rail is 0px wide and transparent, but its contents used to stay
  // mounted: the swarm tree, the job list and an open subagent's transcript
  // re-rendered on every chat event for a panel nobody could see. Only the
  // frame stays, so the open and close transitions are unchanged.
  const shown = useLinger(open, DETAILS_LINGER_MS);
  // Off by default, so the meter is the measured pair until the user asks for
  // the estimated split (Settings → General → Context breakdown).
  const breakdown = useStore((s) => s.settings?.ctx_breakdown ?? false);
  const subOpen = useStore((s) => s.subOpen);
  const bgs = useStore((s) => (shown ? s.bg[task.id] ?? NO_BG : NO_BG));
  const sub = subOpen ? task.subs.find((s) => s.id === subOpen) : null;
  if (sub) {
    return <div className="details" style={{ width: open ? 400 : 0, maxWidth: "calc(100% - 360px)" }}><div className="details-in wide" style={{ opacity: open ? 1 : 0, padding: 0 }}>{shown && <SubPanel key={sub.id} task={task} sub={sub} />}</div></div>;
  }
  return (
    <div className="details" style={{ width: open ? 292 : 0, maxWidth: "calc(100% - 380px)" }}>
      <div className="details-in" style={{ opacity: open ? 1 : 0 }}>
        {shown && task.goal && (
          <div className="dsec">
            <div className="dh"><span style={{ flex: 1 }}>Goal</span><span>{task.goal.status === "active" ? (task.goal.nudges ? `check ${task.goal.nudges}/25` : "active") : task.goal.status.replace("_", " ")}</span></div>
            <div style={{ padding: "0 6px", lineHeight: 1.5, color: task.goal.status === "achieved" ? "var(--mut2)" : "#e4e4e7" }}>{task.goal.text}</div>
            {task.goal.status === "active" && <Button variant="ghost" style={{ height: 22, padding: "0 6px", fontSize: 11, alignSelf: "flex-start", marginLeft: 2 }} onClick={() => api.send(task.id, "/goal clear")}>Clear goal</Button>}
          </div>
        )}
        {shown && <>
        <SwarmTree task={task} />
        <BgCommands task={task} bgs={bgs} />
        <div className="dsec">
          <div className="dh"><span style={{ flex: 1 }}>Context window</span><span>{fmt$(task.usage.cost)}</span></div>
          <ContextRing used={task.usage.last_context} limit={task.context_window} parts={task.usage.parts} breakdown={breakdown} />
          {task.serving && <div className="mono" style={{ padding: "0 6px", fontSize: 10.5, color: "var(--mut3)", wordBreak: "break-all" }}>via {task.serving}</div>}
          <div className="mono" style={{ fontSize: 10.5, color: "var(--dim2)", wordBreak: "break-all" }}>{task.cwd}</div>
        </div>
        </>}
      </div>
    </div>
  );
}

/** Last status report per chat, so reopening the panel is instant. Bounded to
 *  the most recent `STATUS_KEEP` chats: this was a plain object that only ever
 *  grew, holding a report's text for every chat ever opened. */
const STATUS_KEEP = 40;
const statusCache: Record<string, { text: string; model: string; via?: string; at: number }> = {};

function rememberStatus(id: string, entry: { text: string; model: string; via?: string; at: number }) {
  statusCache[id] = entry;
  const ids = Object.keys(statusCache);
  if (ids.length > STATUS_KEEP) for (const old of ids.slice(0, ids.length - STATUS_KEEP)) delete statusCache[old];
}

/** Ask for a summary of what the chat is up to — a side request, the run keeps going. */
function StatusButton({ task }: { task: TaskSummary }) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [, bump] = useState(0);
  const cur = statusCache[task.id];
  const ask = useCallback(() => {
    setBusy(true); setErr("");
    api.statusSummary(task.id)
      .then((r) => { rememberStatus(task.id, { ...r, at: Date.now() }); bump((n) => n + 1); })
      .catch((e) => setErr(String(e)))
      .finally(() => setBusy(false));
  }, [task.id]);
  // A cached report older than a minute is refetched as the panel opens.
  const onOpen = useCallback(() => {
    const c = statusCache[task.id];
    if (!busy && (!c || Date.now() - c.at > 60_000)) ask();
  }, [ask, busy, task.id]);
  const { anchor, toggle, close } = useAnchoredPanel(onOpen);
  const age = cur ? ago(new Date(cur.at).toISOString()) : "";
  return (
    <>
      <Tooltip content="What is it doing? · a summary from a side request; the run isn't interrupted"><Button className="shdr-control" variant="ghost" aria-label="Status" onClick={toggle}>{li(Info, 13)}</Button></Tooltip>
      <AnchoredPanel anchor={anchor} onClose={close} width={460}>
        <div className="sp-head">
          <span style={{ fontWeight: 600 }}>Status</span>
          {cur && !busy && <span className="sp-age">{age === "now" ? "just now" : `${age} ago`}</span>}
          <div style={{ flex: 1 }} />
          {cur && <Button variant="ghost" style={{ height: 24 }} onClick={() => copyText(cur.text, "Status copied")}>Copy</Button>}
          <Button variant="ghost" style={{ height: 24 }} disabled={busy} onClick={ask}>Refresh</Button>
          <IconButton label="Close" onClick={close}>{I.close()}</IconButton>
        </div>
        <div className="sp-body">
          {busy && <div className="sp-wait"><StatusIcon status="running" color={modelColor(chatModel(task))} /><span className="shimmer">Reading the chat…</span></div>}
          {!busy && err && <div style={{ color: "#ff8a8a" }}>{err}</div>}
          {cur && <div style={{ opacity: busy ? 0.45 : 1 }}><Md text={cur.text.replace(/^#{1,3}\s*status\s*\n+/i, "")} /></div>}
        </div>
        {cur?.model && <div className="sp-foot"><i className="tdotc" style={{ background: modelColor(cur.model) }} />{modelInfo(cur.model).name}{cur.via ? ` · ${cur.via}` : ""}</div>}
      </AnchoredPanel>
    </>
  );
}

export function Session() {
  const task = useStore((s) => (s.task ? s.tasks[s.task] : null));
  const openId = useStore((s) => s.task);
  const openError = useStore((s) => s.openError);
  const details = useStore((s) => s.details);
  const find = useStore((s) => s.find);
  // Subscribed, not read through get(): a global pause arrives as a settings
  // event, and this header has to re-render when it lands.
  const pausedAll = useStore((s) => s.settings?.paused_all ?? false);
  // On by default, as in Rust `BrowserCfg::default`.
  const browserEnabled = useStore((s) => s.settings?.plugins?.browser.enabled ?? true);
  // Whether this chat is on its Artificts surface is the chat's own answer,
  // saved with the chat like its name and its pin: switching to another chat
  // carries that chat's answer with it, so the record surface belongs to the
  // chat rather than to the window. The two rails read the same field, which is
  // how they stand down for whichever chat is showing a record.
  const workspaceOpen = useStore((s) => (s.task ? s.tasks[s.task]?.artifict_open ?? false : false));
  // One surface, one or two views. The browser plugin used to open a floating
  // dialog of its own; it is a view of this pane now, and the pane is the frame.
  const [workspaceTab, setWorkspaceTab] = useState<"artificts" | "browser">("artificts");
  const [workspaceExpanded, setWorkspaceExpanded] = useState(false);
  // Where the studio renders its toolbar: a slot in this header, so the
  // Artificts surface has one bar rather than the header plus its own.
  const [studioSlot, setStudioSlot] = useState<HTMLDivElement | null>(null);
  const setWorkspaceOpen = useCallback((open: boolean) => {
    const id = get().task;
    if (!id) return;
    // Optimistic, because the surface is the thing the click asked for; the
    // backend's own summary lands on the next event and settles it.
    set((s) => (s.tasks[id] ? { tasks: { ...s.tasks, [id]: { ...s.tasks[id]!, artifict_open: open } } } : {}));
    void api.update(id, { artifict_open: open }).catch(() => {});
  }, []);
  // A chat started from another chat's studio opens on the artifict it was
  // started from, so the studio stays put while the chat beside it changes.
  const [workspaceSelection, setWorkspaceSelection] = useState<ArtifictWorkspaceSelection | null>(null);
  const handoffId = task?.id;
  useEffect(() => {
    if (!handoffId) return;
    // A selection belongs to the chat it was made in: switching chats (or
    // opening a new one) must not carry the last chat's open artifict along.
    const handed = studioHandoff.get(handoffId) ?? null;
    studioHandoff.delete(handoffId);
    setWorkspaceSelection(handed);
  }, [handoffId]);
  // Stable identity on purpose. This is handed to `Timeline` and passed down to
  // every row: as an inline arrow it was a new prop on every Session render, and
  // Session re-renders on every summary event for this chat (status, step, cost,
  // transcript extent). A new function identity defeats `memo` on every row, so
  // the whole transcript re-rendered for one chat-level change. Every setter it
  // needs is stable — three state setters and the `useCallback` above — so this
  // never has to change.
  // A browser row can open the shared Browser view beside the chat. Stable for
  // the same reason as `openArtifict`: it reaches every row through context.
  const openBrowserView = useCallback(() => {
    setWorkspaceTab("browser");
    setWorkspaceOpen(true);
  }, [setWorkspaceOpen]);
  const openArtifict = useCallback((selection: ArtifictWorkspaceSelection) => {
    setWorkspaceSelection(selection);
    setWorkspaceTab("artificts");
    setWorkspaceOpen(true);
  }, [setWorkspaceOpen]);
  const bg = useStore((s) => (s.task ? s.bg[s.task] : undefined) ?? NO_BG);
  const [stats, setStats] = useState<{ add: number; del: number; n: number } | null>(null);
  const taskId = task?.id;
  const taskStatus = task?.status;
  useEffect(() => {
    setStats(null);
    if (!taskId || taskStatus === "running") return;
    let cancelled = false;
    // The header only needs three totals, not the full text of every file diff.
    void api.reviewStats(taskId).then((r) => {
      if (!cancelled) setStats({ n: r.files, add: r.add, del: r.del });
    }).catch(() => { if (!cancelled) setStats(null); });
    return () => { cancelled = true; };
  }, [taskId, taskStatus]);
  if (!task) {
    // A failure the store recorded for *this* chat. Shows why, and offers the
    // two things worth offering — retry, or get out. Without this the user got
    // "Task not found" for both "the harness errored" and "no such chat", which
    // reads as a dead end rather than a failure.
    const why = openError && openError.id === openId ? openError.why : null;
    return (
      <div className="empty chat-open-failed" role="alert">
        <div style={{ color: "var(--fg)" }}>{why ? "That chat could not be opened" : "Task not found"}</div>
        {why && <div className="mono" style={{ fontSize: 11, color: "var(--mut2)", maxWidth: 460, wordBreak: "break-word" }}>{why}</div>}
        <div style={{ display: "flex", gap: 8 }}>
          {why && openId && <Button variant="primary" style={{ height: 26 }} onClick={() => { void openTask(openId); }}>Try again</Button>}
          <Button variant="ghost" style={{ height: 26 }} onClick={() => go("home")}>Back to chats</Button>
        </div>
      </div>
    );
  }
  const paused = isPaused(task, pausedAll);
  // The agent colour follows `chatModel`, not `task.model`, so the header agrees
  // with the sidebar dot about a swap the user queued mid-turn (the `*` badge).
  const agentColor = modelColor(chatModel(task));
  const busy = task.subs.filter((s) => s.status === "running").length + bg.filter((b) => b.running).length;
  const live = task.status === "running" || task.status === "waiting";
  return (
    <AgentColor.Provider value={agentColor}>
      <div className={"shdr" + (task.ultra ? " ultra " + swarmState(task, heldByGlobal(task, pausedAll)) : "")}>
        {/* A parked chat's own quiet status: the header must not raise the red
            "needs you" alert for a wait on a trigger. See `chatStatus`. */}
        <StatusIcon status={paused ? "paused" : chatStatus(task)} />
        <div className="shdr-title" style={{ fontWeight: 600, fontSize: 13.5, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", minWidth: 0 }}>{task.title}</div>
        {task.ultra && (() => {
          const ux = task.ultra_x;
          const x = xOn(ux);
          const c = swarmCounts(task.subs);
          const total = task.subs.length;
          // One state for the badge, the sidebar dot and the header underline:
          // violet while the swarm works, then the colour that says how it went.
          const state = swarmState(task, heldByGlobal(task, pausedAll));
          const glowing = swarmGlow(state);
          const label = x && ux ? `Ultrathread X${ux.wt ? " · wt" : ""} · ${xDepth(ux)}` : task.ultra_wt ? "Ultrathread · wt" : "Ultrathread";
          const tip = x && ux
            ? `Ultrathread X · ${xDepth(ux)} layers, each with its own model, effort and fanout`
            : "Ultrathread · nested subagents, keeps going until the list is done";
          const tally = total > 0 ? (glowing ? `${c.running} running` : `${c.done} of ${total} done${c.failed ? `, ${c.failed} failed` : ""}`) : "";
          return (
            <Tooltip content={tally ? `${tip} — ${tally}` : tip}>
              <div className={"ultrabadge shdr-optional " + state + (x ? " ultrabadgex" : "")}>
                <span className="ultraspark" aria-hidden="true" />
                {label}
              </div>
            </Tooltip>
          );
        })()}
        {task.branch && !workspaceOpen && <div className="branchtag shdr-optional">{task.branch}</div>}
        {!workspaceOpen && <ContainerChip task={task} />}
        {task.goal && !workspaceOpen && <Tooltip content={task.goal.text}><div className={"goalchip shdr-optional " + task.goal.status}>{task.goal.status === "active" ? "Goal" : task.goal.status === "achieved" ? "Goal met" : task.goal.status === "blocked" ? "Goal blocked" : "Goal stopped"}</div></Tooltip>}
        {task.goal && !workspaceOpen && <Button className="shdr-optional" variant="ghost" title="Remove the current goal" aria-label="Remove current goal" style={{ width: 24, height: 22, padding: 0, fontSize: 11 }} onClick={() => api.send(task.id, "/goal clear").catch((e) => flash(String(e)))}>{I.close(11)}</Button>}
        {task.pending.model && !workspaceOpen && <Tooltip content="Switches with your next message"><div className="goalchip shdr-optional">{modelInfo(task.pending.model).name}<PendingStar /></div></Tooltip>}
        <div className="shdr-spacer" />
        <Button className="shdr-control" variant={workspaceOpen ? "ghost" : "default"} aria-pressed={!workspaceOpen} onClick={() => setWorkspaceOpen(false)}>Chat</Button>
        <Button className="shdr-control" variant={workspaceOpen ? "primary" : "ghost"} aria-pressed={workspaceOpen} onClick={() => { setWorkspaceSelection(null); setWorkspaceTab("artificts"); setWorkspaceOpen(true); }}>Artificts</Button>
        {/* On the Artificts surface the header is the studio's toolbar: the
            chat-only controls step aside and the studio renders its own tools
            into this slot, so the surface has one bar instead of two. The
            chat's status icon and title stay, and so does Resume on a paused
            chat — that one is never safe to hide. */}
        {workspaceOpen && workspaceTab === "artificts" && <div className="shdr-studio" ref={setStudioSlot} />}
        {!workspaceOpen && <div className="shdr-optional"><TodoChip todos={task.todos} /></div>}
        {task.ultra && task.subs.length > 0 && !workspaceOpen && <div className="shdr-optional"><SwarmChip task={task} held={paused} /></div>}
        {(live || paused) && (
          paused
            ? <Button className="shdr-optional" style={{ color: "#fbbf24", borderColor: "rgba(251,191,36,0.3)" }} title={pausedAll && !task.paused ? "Resume this chat only — everything else stays frozen" : undefined} onClick={() => api.resume(task.id).catch((e) => flash(String(e)))}>{I.play(10)}Resume</Button>
            : !workspaceOpen && <Tooltip content="Freeze this task and its subagents"><Button className="shdr-optional" variant="ghost" aria-label="Pause" onClick={() => requestPause(task.id)?.catch((e) => flash(String(e)))}>{I.pause(10)}</Button></Tooltip>
        )}
        {stats && stats.n > 0 && !workspaceOpen && (
          <Button className="shdr-optional" onClick={() => go("diff")}>Review<span className="mono" style={{ fontSize: 10.5, color: "var(--diff-add)" }}>+{stats.add}</span><span className="mono" style={{ fontSize: 10.5, color: "var(--diff-del)" }}>−{stats.del}</span></Button>
        )}
        {!workspaceOpen && <Tooltip content="Run timeline"><Button className="shdr-optional" variant="ghost" aria-label="Run timeline" onClick={() => openRunTimeline(task.id)}>{I.gantt(13)}</Button></Tooltip>}
        {!workspaceOpen && <StatusButton task={task} />}
        {!workspaceOpen && <Tooltip content="Details · Ctrl J"><Button className="shdr-control" aria-label="Details" style={{ background: details ? "rgba(255,255,255,0.08)" : "transparent", borderColor: "transparent", color: details ? "#f4f4f5" : "var(--mut)" }} onClick={() => set({ details: !details })}>
          {I.panel()}
          {busy > 0 && <span style={{ minWidth: 16, height: 16, padding: "0 4px", borderRadius: 8, display: "grid", placeItems: "center", background: "rgba(167,139,250,0.14)", color: "var(--violet)", fontSize: 10, fontWeight: 600 }}>{busy}</span>}
        </Button></Tooltip>}
      </div>
      <div className={workspaceOpen ? "session-collaboration-layout" : undefined} style={{ flex: 1, minHeight: 0, display: "flex" }}>
        {/* `data-task` is how the selection layer knows which chat a selection
            came from: the note it opens has to go to this chat's composer, and
            the pane is what identifies it. */}
        <div className={workspaceOpen ? "session-pane collaboration-chat" : "session-pane"} data-task={task.id} style={{ flex: workspaceOpen ? (workspaceExpanded ? "0 0 0%" : "0 0 26%") : 1, minWidth: workspaceOpen ? 280 : 300, position: "relative", display: workspaceOpen && workspaceExpanded ? "none" : undefined }}>
          <OpenBrowserView.Provider value={browserEnabled ? openBrowserView : null}><Timeline task={task} onOpenArtifict={openArtifict} /></OpenBrowserView.Provider>
          {find && <FindBar onClose={() => set({ find: false })} />}
          <CFloat task={task} />
        </div>
        {workspaceOpen && <div className="session-pane collaboration-workspace" style={{ flex: 1, minWidth: 380, position: "relative", display: "flex", flexDirection: "column" }}>{browserEnabled && <div className="workspace-tabs" aria-label="Workspace views"><Button variant={workspaceTab === "artificts" ? "primary" : "ghost"} aria-pressed={workspaceTab === "artificts"} onClick={() => setWorkspaceTab("artificts")}>Artificts</Button><Button variant={workspaceTab === "browser" ? "primary" : "ghost"} aria-pressed={workspaceTab === "browser"} onClick={() => setWorkspaceTab("browser")}><Globe size={13} aria-hidden="true" />Browser</Button></div>}<div className="workspace-notices"><AgentNoticeList taskId={task.id} /></div>{workspaceTab === "browser" && browserEnabled ? <BrowserPanel embedded key={task.id} taskId={task.id} onClose={() => setWorkspaceTab("artificts")} onExpandedChange={setWorkspaceExpanded} /> : <ArtifictWorkspace key={`${task.id}:${workspaceSelection?.artifictId ?? ""}:${workspaceSelection?.versionId ?? ""}`} taskId={task.id} taskTitle={task.title} initialSelection={workspaceSelection} onExpandedChange={setWorkspaceExpanded} toolbarSlot={studioSlot} />}</div>}
        {/* The record surface owns the width while it is open, so the
            subagent/background rail stands down with its own toggle. */}
        {!workspaceOpen && <Details task={task} />}
      </div>
    </AgentColor.Provider>
  );
}

