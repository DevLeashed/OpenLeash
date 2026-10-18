import { useEffect, useMemo, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ago, api, baseName, effortLabel, normPath, oneLine, PERMS, SavedPrompt } from "../api";
import { dropPrompt, flash, get, go, loadPrompt, modelInfo, saveSettings, useStore } from "../store";
import { I } from "./icons";
import { Button, Dropdown, Opt, Pressable, Tooltip } from "./primitives";

const NO_SAVED: SavedPrompt[] = [];

/** Model + effort, the two options worth showing on a row. */
function ModelChip({ p }: { p: SavedPrompt }) {
  if (!p.model) return null;
  // Read against the model this prompt would run on: its own levels decide
  // which effort names mean anything.
  const effort = p.effort === null ? null : effortLabel(modelInfo(p.model), p.effort);
  return (
    <Tooltip content={`${p.model}${effort ? ` · ${effort} effort` : ""}`}>
      <span className="mono saved-model">{p.model.split("/").pop()}{effort ? ` · ${effort}` : ""}</span>
    </Tooltip>
  );
}

/** The options that don't fit on the row itself. */
function More({ p }: { p: SavedPrompt }) {
  const bits = [
    p.route ? `route: ${p.route}` : null,
    PERMS.find((x) => x.id === p.perm)?.name,
    p.assist,
    p.plan ? "plan" : null,
    p.ultra ? (p.ultra_wt ? "ultrathread · worktrees" : "ultrathread") : null,
    p.worktree ? "new worktree" : null,
    p.branch ? `branch: ${p.branch}` : null,
    (p.agents.length ? p.agents : []).length ? `${p.agents.length} subagents` : null,
    p.images.length ? `${p.images.length} image${p.images.length === 1 ? "" : "s"}` : null,
  ].filter(Boolean);
  return <Tooltip content={bits.join(" · ") || "Default options"}>{I.chev()}</Tooltip>;
}

/** How the list is ordered. `""` is the order the prompts are stored in: they are
 *  appended as they are parked, so it reads oldest first. */
type SavedSort = "" | "newest" | "folder" | "az";

export const SAVED_SORT_OPTIONS: Opt[] = [
  { value: "", label: "As saved", hint: "oldest first" },
  { value: "newest", label: "Newest first" },
  { value: "folder", label: "By folder", hint: "this project first" },
  { value: "az", label: "A–Z", hint: "by prompt text" },
];

/** Reads a stored order, tolerating a settings file that was hand-edited or
 *  written by another version: anything unrecognised is the stored order. */
export function savedSortOf(v: string | undefined): SavedSort {
  return v === "newest" || v === "folder" || v === "az" ? v : "";
}

/**
 * Whether a prompt belongs to the folder the app is on. A prompt with no folder
 * of its own belongs wherever you are — `loadPrompt` reads it the same way — and
 * so does everything while no folder is open, matching the sidebar's rule that a
 * chat with nothing to disagree with is not elsewhere. Spelled paths are compared
 * normalized, so `D:\work\app` and `d:/work/app/` are one folder.
 */
export function savedPromptIsHere(p: SavedPrompt, project: string): boolean {
  return !p.project || !project || normPath(p.project) === normPath(project);
}

/**
 * The list as it should read. With "show all chats" on, every prompt is there and
 * the ones parked for another folder are the caller's to dim; with it off they are
 * not listed at all — that is the whole point of the switch. `folder` deliberately
 * pushes the other folders' prompts to the bottom, keeping the current folder's own
 * prompts above them; `Array.sort` is stable, so prompts keep their saved order
 * within a group.
 */
export function savedList(list: SavedPrompt[], project: string, allProjects: boolean, sort: SavedSort): SavedPrompt[] {
  const shown = allProjects ? list : list.filter((p) => savedPromptIsHere(p, project));
  if (sort === "newest") return [...shown].sort((a, b) => (Date.parse(b.created_at) || 0) - (Date.parse(a.created_at) || 0));
  if (sort === "az") return [...shown].sort((a, b) => oneLine(a.text).localeCompare(oneLine(b.text), undefined, { sensitivity: "base" }));
  if (sort === "folder") {
    const away = (p: SavedPrompt) => (savedPromptIsHere(p, project) ? "" : normPath(p.project));
    return [...shown].sort((a, b) => {
      const ka = away(a), kb = away(b);
      if (!ka !== !kb) return ka ? 1 : -1;
      return ka.localeCompare(kb);
    });
  }
  return shown;
}

export function Saved() {
  const list = useStore((s) => s.settings?.saved_prompts ?? NO_SAVED);
  const [arm, setArm] = useState<string | null>(null);
  const [prs, setPrs] = useState<Awaited<ReturnType<typeof api.githubPrs>> | null>(null);
  const [prState, setPrState] = useState<"open" | "closed" | "all">("open");
  const [prLoading, setPrLoading] = useState(false);
  const [prError, setPrError] = useState("");
  const project = useStore((s) => s.settings?.project ?? "");
  const showPrs = useStore((s) => s.settings?.show_pull_requests ?? false);
  // The same switch as the sidebar's "show all chats", read here as "list prompts
  // from every folder too" — one setting, so the two screens can't disagree.
  const allProjects = useStore((s) => s.settings?.all_projects ?? false);
  const sort = savedSortOf(useStore((s) => s.settings?.saved_sort));
  const shown = useMemo(() => savedList(list, project, allProjects, sort), [list, project, allProjects, sort]);
  const hidden = list.length - shown.length;
  // Loading a prompt parked in another folder opens that folder, so say so:
  // the composer appearing in a different project is otherwise unexplained.
  const load = (p: SavedPrompt) => {
    const before = get().settings?.project ?? "";
    void loadPrompt(p).then((ok) => {
      if (!ok) return;
      const after = get().settings?.project ?? "";
      flash(after && after !== before
        ? `Opened ${baseName(after)} · prompt loaded, Enter to start it`
        : "Loaded into the composer · Enter to start it");
    });
  };
  const isPulls = useStore((s) => s.view === "pulls");
  const isSaved = useStore((s) => s.view === "saved");
  const loadPrs = async () => {
    if (!project || prLoading) return;
    setPrLoading(true); setPrError("");
    try { setPrs(await api.githubPrs(project, prState)); }
    catch (e) { setPrError(String(e)); setPrs(null); }
    finally { setPrLoading(false); }
  };
  useEffect(() => { if (isPulls && showPrs) void api.githubPrs(project, prState).then(setPrs).catch((e) => { setPrError(String(e)); setPrs(null); }); }, [isPulls, showPrs, project, prState]);
  useEffect(() => { setPrs(null); setPrError(""); }, [prState, project]);
  return (
    <div style={{ flex: 1, overflow: "auto", padding: "24px 28px" }}>
      <div style={{ maxWidth: 860, margin: "0 auto", display: "flex", flexDirection: "column", gap: 14 }}>
        {showPrs && <div style={{ display: "flex", gap: 8, borderBottom: "1px solid var(--border)", paddingBottom: 10 }}>
          <Button variant={isSaved ? "primary" : "ghost"} onClick={() => go("saved")}>Saved prompts</Button>
          <Button variant="primary" onClick={() => go("pulls")}>Pull requests</Button>
        </div>}
        {isPulls ? <>
          <div style={{ display: "flex", alignItems: "center", gap: 12 }}><div style={{ flex: 1 }}><div style={{ fontSize: 20, fontWeight: 600 }}>Pull requests</div><div className="secondary-text">{project ? baseName(project) : "Open a project to see its pull requests"}</div></div>
            <Dropdown value={prState} options={[{ value: "open", label: "Open" }, { value: "closed", label: "Closed" }, { value: "all", label: "All" }]} onChange={(v) => { setPrState(v as typeof prState); setPrs(null); }} search={false} style={{ width: 130 }} />
            <Button disabled={!project || prLoading} onClick={() => void loadPrs()}>{prLoading ? "Loading…" : "Refresh"}</Button>
          </div>
          {prError && <div role="alert" className="settings-note">{prError}</div>}
          {prs && <div className="secondary-text">{prs.repo} · {prs.prs.length} pull request{prs.prs.length === 1 ? "" : "s"}</div>}
          {prs?.prs.map((pr) => <div key={pr.number} className="sgroup"><div className="srowx">
            <div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 550 }}>#{pr.number} {pr.title}</div><div className="desc">{pr.draft ? "Draft · " : ""}{pr.head} → {pr.base} · @{pr.user} · updated {ago(pr.updated_at)}</div></div>
            <Button variant="ghost" onClick={() => void openUrl(pr.html_url).catch((e) => flash(String(e)))}>Open on GitHub</Button>
          </div></div>)}
          {!prs && !prError && !prLoading && <div className="empty">{project ? "Refresh to load pull requests for this project's GitHub repository." : "Open a GitHub repository project first."}</div>}
        </> : <>
        <div style={{ display: "flex", alignItems: "flex-end", gap: 24, flexWrap: "wrap", animation: "olIn .28s cubic-bezier(.22,1,.36,1) both" }}>
          <div style={{ flex: 1, minWidth: 200 }}>
            <div style={{ fontSize: 20, fontWeight: 600, letterSpacing: "-0.025em" }}>Saved prompts</div>
            <div className="secondary-text" style={{ marginTop: 3 }}>
              {shown.length ? `${shown.length} waiting to start · click one to load it and its options into the composer` : "Prompts you park from the new-task screen land here"}
            </div>
          </div>
          <Button variant="primary" onClick={() => go("home")}>New task</Button>
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          {shown.map((p, i) => {
            // A prompt parked for another folder is still listed — and still one
            // click from the composer — but steps back a shade so this folder's
            // own prompts are the ones that read as solid. The row keeps its
            // opacity: the entry animation owns that property.
            const here = savedPromptIsHere(p, project);
            return (
              <div key={p.id} className={"savedrow" + (here ? "" : " elsewhere")} style={{ animationDelay: Math.min(i, 12) * 16 + "ms" }}
                onMouseLeave={() => setArm(null)}
                onClick={() => load(p)}
                onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); load(p); } }}
                role="button" tabIndex={0}
              >
                <span className="savedglyph">{I.bookmark(13)}</span>
                <div style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 3 }}>
                  <span className="savedtext">{oneLine(p.text, 200)}</span>
                  <span className="savedmeta">
                    <ModelChip p={p} />
                    <More p={p} />
                    <span style={{ fontVariantNumeric: "tabular-nums" }}>{ago(p.created_at)}</span>
                  </span>
                </div>
                {!here && (
                  <Tooltip content={`Saved from ${p.project} · loading this switches to that folder`}><span className="savedtag">{baseName(p.project)}</span></Tooltip>
                )}
                {arm === p.id ? (
                  <Pressable className="saveddel armed" onClick={(e) => { e.stopPropagation(); setArm(null); void dropPrompt(p).then(() => flash("Deleted")); }}>Delete</Pressable>
                ) : (
                  <Pressable className="saveddel" onClick={(e) => { e.stopPropagation(); setArm(p.id); }} onFocus={() => setArm(p.id)} aria-label={`Delete saved prompt: ${oneLine(p.text, 40)}`}>{I.trash(12)}</Pressable>
                )}
              </div>
            );
          })}
        </div>
        {!shown.length && (
          <div className="empty">
            {list.length
              ? "Nothing parked for this folder. Prompts saved for other folders are hidden."
              : "Nothing saved yet. Write a prompt on the new-task screen and press the bookmark button next to Send to park it here."}
          </div>
        )}
        {/* Pushed under the list rather than up in the header: the order only
            matters once you can see prompts from more than one folder. */}
        {allProjects && list.length > 0 && (
          <div className="savedsort">
            <span className="savedsort-label">Sort</span>
            <Dropdown value={sort} options={SAVED_SORT_OPTIONS} onChange={(v) => void saveSettings({ saved_sort: v })} search={false} placeholder="Sort saved prompts" style={{ width: 232 }} />
          </div>
        )}
        {/* The switch lives in the sidebar, so the way to the hidden ones has to be
            reachable from the screen that hid them. */}
        {project && !allProjects && hidden > 0 && (
          <Tooltip content="Prompts parked for other folders are hidden">
            <Pressable className="projfilter" onClick={() => void saveSettings({ all_projects: true })}>
              {I.folder()}
              <span className="t">{`${hidden} in other folder${hidden === 1 ? "" : "s"} · show`}</span>
            </Pressable>
          </Tooltip>
        )}
        </>}
      </div>
    </div>
  );
}
