import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Box, GitBranch, Hammer, Play, RefreshCw, Square, Trash2, Upload } from "lucide-react";
import type { TaskSummary } from "../api";
import { baseName } from "../api";
import { flash, go, saveSettings, set, useStore } from "../store";
import { CONTAINER_LABEL, defaultMode, workspacesApi, type ContainerState, type DockerProjectInfo, type WorkspaceMode, type WorkspacesList } from "../workspaces";
import { Button, ConfirmDialog, IconButton, Input, Segmented, Tooltip } from "./primitives";

/** Docker facts for a project. The engine is never asked here: opening a
 *  project must not run a docker command for someone who doesn't use docker. */
export function useDockerInfo(project: string) {
  const [info, setInfo] = useState<DockerProjectInfo | null>(null);
  useEffect(() => {
    setInfo(null);
    if (!project) return;
    let live = true;
    void workspacesApi.dockerInfo(project, false).then((i) => { if (live) setInfo(i); }).catch(() => {});
    return () => { live = false; };
  }, [project]);
  return [info, setInfo] as const;
}

/** Local / Worktree / Docker for the next new task. */
export function WorkspacePicker({ project, git }: { project: string; git: boolean }) {
  const home = useStore((s) => s.home);
  const [info, setInfo] = useDockerInfo(project);
  const mode: WorkspaceMode = home.docker ? "docker" : home.worktree && git ? "worktree" : "local";
  // A remembered "docker" for a project that can't do it falls back quietly.
  useEffect(() => {
    if (info && home.docker && (!info.has_config || !info.trusted)) set((s) => ({ home: { ...s.home, docker: false } }));
  }, [info, home.docker]);
  const [askTemplate, setAskTemplate] = useState(false);
  /** Add the template Dockerfile this project has none of. Split out of `choose`
   *  so the question can be asked in the app's own dialog: `window.confirm`
   *  paints browser chrome inside a native window. */
  const addTemplate = () => {
    void workspacesApi.addTemplate(project)
      .then(() => workspacesApi.dockerInfo(project, true))
      .then((i) => {
        setInfo(i);
        set((s) => ({ home: { ...s.home, worktree: false, docker: i.trusted && !i.config_error } }));
        flash(i.trusted ? "Added .openleash_docker/Dockerfile" : "Added .openleash_docker/Dockerfile. Trust this folder to use it.");
        if (i.engine && !i.engine.ok) flash(i.engine.error);
      })
      .catch((e) => flash(String(e)));
  };
  const choose = (m: WorkspaceMode) => {
    if (m === "docker" && info && !info.has_config) {
      setAskTemplate(true);
      return;
    }
    set((s) => ({ home: { ...s.home, worktree: m === "worktree", docker: m === "docker" } }));
    if (m === "docker") {
      // Only now, because the user asked for docker, is the engine checked.
      void workspacesApi.dockerInfo(project, true).then((i) => {
        setInfo(i);
        if (i.engine && !i.engine.ok) flash(i.engine.error);
        else if (i.config_error) flash(i.config_error);
      }).catch((e) => flash(String(e)));
    }
  };
  const dockerHint = !info?.has_config
    ? "No .openleash_docker folder yet: choose this to add a template Dockerfile"
    : !info.trusted
      ? "Trust this folder first: the image is built from the project's own files"
      : info.config_error ?? "Private copy of the project; commands run in a container built from .openleash_docker";
  return (
    <>
      <Segmented
        quiet
        label="Workspace for the new task"
        style={{ width: 300, height: 26 }}
        value={mode}
        onChange={choose}
        options={[
          { value: "local", label: "Local", hint: "Work directly in the project folder" },
          { value: "worktree", label: "Worktree", hint: git ? "Isolated git worktree on its own branch" : "Needs a git repository", disabled: !git },
          { value: "docker", label: "Docker", hint: dockerHint, disabled: !info || (info.has_config && (!info.trusted || !!info.config_error)) },
        ]}
      />
      {askTemplate && <ConfirmDialog
        title="Add a Dockerfile for this project?"
        body={<>This project has no <span className="mono">.openleash_docker</span> folder. OpenLeash can add a template Dockerfile there, which you can edit.</>}
        confirm="Add template"
        onConfirm={addTemplate}
        onClose={() => setAskTemplate(false)}
      />}
    </>
  );
}

const STATE_COLOR: Partial<Record<ContainerState, string>> = { running: "var(--green, #4ade80)", building: "var(--accent)", starting: "var(--accent)", error: "#ff8a8a" };

/** Chat-header chip for a docker chat: live container state plus controls. */
export function ContainerChip({ task }: { task: TaskSummary }) {
  const [busy, setBusy] = useState(false);
  if (!task.docker) return null;
  const st: ContainerState = task.container ?? "unknown";
  const working = st === "building" || st === "starting";
  const run = (f: () => Promise<unknown>, ok?: (v: unknown) => string) => {
    setBusy(true);
    void f().then((v) => { if (ok) flash(ok(v)); }).catch((e) => flash(String(e))).finally(() => setBusy(false));
  };
  return (
    <div className="container-chip shdr-optional" title={`${task.docker.container} · ${task.docker.host_root} → ${task.docker.guest_root}`}>
      <Box size={12} strokeWidth={1.8} aria-hidden="true" style={{ color: STATE_COLOR[st] }} />
      <span>{CONTAINER_LABEL[st]}</span>
      {st === "running"
        ? <IconButton label="Stop the container" disabled={busy || task.status === "running"} onClick={() => run(() => workspacesApi.stop(task.id))}><Square size={11} /></IconButton>
        : <IconButton label="Start the container" disabled={busy || working} onClick={() => run(() => workspacesApi.start(task.id))}><Play size={11} /></IconButton>}
      {task.branch && <IconButton label={`Push ${task.branch} into the project repository`} disabled={busy} onClick={() => run(() => workspacesApi.pushBranch(task.id), (b) => `Pushed ${String(b)} to ${baseName(task.docker!.project)}`)}><Upload size={11} /></IconButton>}
    </div>
  );
}

/** Settings → Workspaces: every worktree and container OpenLeash made. */
export function WorkspacesTab() {
  const s = useStore((st) => st.settings)!;
  const project = s.project;
  const [list, setList] = useState<WorkspacesList | null>(null);
  const [scan, setScan] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [build, setBuild] = useState<{ line: string; running: boolean } | null>(null);
  const [idle, setIdle] = useState(String(s.docker_idle_minutes ?? 15));
  const [info] = useDockerInfo(project);
  const refresh = useCallback((scanDocker: boolean) => {
    void workspacesApi.list(scanDocker).then(setList).catch((e) => flash(String(e)));
  }, []);
  useEffect(() => { refresh(scan); }, [refresh, scan]);
  useEffect(() => {
    let un: (() => void) | undefined;
    let dead = false;
    void listen<{ project: string; line?: string; done?: boolean; error?: string | null }>("ol://docker-build", ({ payload }) => {
      if (payload.done) { setBuild(null); if (payload.error) flash(payload.error); else flash("Image rebuilt · containers pick it up on their next start"); refresh(scan); }
      else if (payload.line) setBuild({ line: payload.line, running: true });
    }).then((u) => { if (dead) u(); else un = u; });
    return () => { dead = true; un?.(); };
  }, [refresh, scan]);
  // Container states arrive on task summaries; refresh when any change.
  const tasks = useStore((st) => st.tasks);
  const states = Object.values(tasks).map((t) => t.container ?? "").join();
  useEffect(() => { if (list?.docker.length) refresh(scan); }, [states]); // eslint-disable-line react-hooks/exhaustive-deps
  const act = (key: string, f: () => Promise<unknown>) => {
    setBusy(key);
    void f().then(() => refresh(scan)).catch((e) => flash(String(e))).finally(() => setBusy(null));
  };
  const openChat = (id: string) => go("session", { task: id });
  const mode = defaultMode(s);
  return (
    <>
      <div className="settings-head"><div className="settings-head-main"><div className="settings-title">Workspaces</div><div className="settings-lead">Where tasks work: the project folder, a git worktree, or a container. Everything OpenLeash created is listed here.</div></div></div>
      <div className="sgroup">
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>New tasks use</div><div className="desc">The default on the new-task screen. Docker needs a <span className="mono">.openleash_docker/Dockerfile</span> in the project.</div></div>
          <Segmented label="New tasks use" value={mode} onChange={(v) => { void saveSettings({ workspace: v, worktree: v === "worktree" }); set((st) => ({ home: { ...st.home, worktree: v === "worktree", docker: v === "docker" } })); }}
            options={[{ value: "local", label: "Local" }, { value: "worktree", label: "Worktree" }, { value: "docker", label: "Docker" }]} /></div>
        <div className="srowx"><div style={{ flex: 1 }}><div style={{ fontWeight: 500 }}>Stop idle containers after</div><div className="desc">Minutes without a command or a background job. They start again when you send the chat a message. 0 keeps them running. A project's <span className="mono">openleash.json</span> can override this.</div></div>
          <Input aria-label="Idle minutes" className="input mono" style={{ width: 72 }} inputMode="numeric" value={idle} onChange={(e) => setIdle(e.currentTarget.value.replace(/\D/g, "").slice(0, 4))} onBlur={() => { const n = Number(idle || 0); if (n !== s.docker_idle_minutes) void saveSettings({ docker_idle_minutes: n }); }} /></div>
        {project && info?.has_config && <div className="srowx"><div style={{ flex: 1, minWidth: 0 }}><div style={{ fontWeight: 500 }}>Image for {baseName(project)}</div><div className="desc" style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{build ? build.line : "Rebuilt automatically whenever anything in .openleash_docker changes."}</div></div>
          <Button disabled={!!build || !info.trusted} onClick={() => { setBuild({ line: "Starting the build…", running: true }); void workspacesApi.rebuild(project, false).catch(() => {}); }}><Hammer size={13} /> Rebuild</Button>
          <Button variant="ghost" disabled={!!build || !info.trusted} onClick={() => { setBuild({ line: "Starting a clean build…", running: true }); void workspacesApi.rebuild(project, true).catch(() => {}); }}>Clean rebuild</Button></div>}
      </div>

      <div className="settings-section-label">Containers</div>
      <div className="sgroup">
        {!list?.docker.length && <div className="srowx desc">No chats run in docker.</div>}
        {list?.docker.map((w) => {
          const st: ContainerState = (w.state as ContainerState) ?? "unknown";
          const working = w.status === "running";
          return (
            <div key={w.task_id} className="srowx">
              <Box size={14} strokeWidth={1.7} aria-hidden="true" style={{ color: STATE_COLOR[st] }} />
              <div style={{ flex: 1, minWidth: 0 }}>
                <button type="button" className="linkish" onClick={() => openChat(w.task_id)} style={{ fontWeight: 500 }}>{w.title}</button>
                <div className="desc mono" style={{ fontSize: 11 }}>{w.container} · {CONTAINER_LABEL[st]} · {baseName(w.project)}{w.branch ? ` · ${w.branch}` : ""}{w.exists ? "" : " · workspace missing"}</div>
              </div>
              {st === "running"
                ? <IconButton label="Stop" disabled={busy === w.task_id || working} onClick={() => act(w.task_id, () => workspacesApi.stop(w.task_id))}><Square size={12} /></IconButton>
                : <IconButton label="Start" disabled={busy === w.task_id} onClick={() => act(w.task_id, () => workspacesApi.start(w.task_id))}><Play size={12} /></IconButton>}
              <ConfirmRemove label={`Remove ${w.container ?? "container"}`} disabled={busy === w.task_id || working} tooltip="Removes only the container; its workspace files stay on disk." onConfirm={() => act(w.task_id, () => workspacesApi.removeContainer(w.task_id))} />
              <Tooltip content={working ? "Stop the chat before changing its folder" : "Leave existing files where they are; only future work switches folders"}>
                <Button variant="ghost" disabled={busy === w.task_id || working} style={{ height: 26 }} onClick={() => { void workspacesApi.pickFolder().then((folder) => folder && act(w.task_id, () => workspacesApi.changeFolder(w.task_id, folder))).catch((e) => flash(String(e))); }}>Change folder…</Button>
              </Tooltip>
            </div>
          );
        })}
        {list?.docker_error && <div className="srowx desc" style={{ color: "#ff8a8a" }}>{list.docker_error}</div>}
        {!list?.containers && !list?.docker.length && <div className="srowx"><div className="desc" style={{ flex: 1 }}>Docker isn't asked about anything until you use it.</div><Button variant="ghost" onClick={() => setScan(true)}>Scan for leftover containers</Button></div>}
        {list?.orphan_containers.map((c) => (
          <div key={c.name} className="srowx">
            <Box size={14} strokeWidth={1.7} aria-hidden="true" style={{ opacity: 0.5 }} />
            <div style={{ flex: 1, minWidth: 0 }}><div className="mono" style={{ fontSize: 12 }}>{c.name}</div><div className="desc">Leftover · its chat was deleted · {c.status}</div></div>
            <IconButton label={`Remove ${c.name}`} disabled={busy === c.name} onClick={() => act(c.name, () => workspacesApi.prune("container", c.name))}><Trash2 size={12} /></IconButton>
          </div>
        ))}
      </div>

      <div className="settings-section-label">Worktrees</div>
      <div className="sgroup">
        {!list?.worktrees.length && <div className="srowx desc">No chats use a worktree.</div>}
        {list?.worktrees.map((w) => (
          <div key={w.task_id} className="srowx">
            <GitBranch size={14} strokeWidth={1.7} aria-hidden="true" />
            <div style={{ flex: 1, minWidth: 0 }}>
              <button type="button" className="linkish" onClick={() => openChat(w.task_id)} style={{ fontWeight: 500 }}>{w.title}</button>
              <div className="desc mono" style={{ fontSize: 11, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{w.branch} · {baseName(w.project)}{w.exists ? "" : " · folder missing"}{w.archived ? " · archived" : ""}</div>
            </div>
          </div>
        ))}
      </div>

      {!!list?.worker_worktrees.length && <>
        <div className="settings-section-label">Worker worktrees</div>
        <div className="settings-note">Each top-level worker in an ultrathread worktrees run gets its own checkout on its own branch; a worker's own subagents share it. The fuze agent merges the branch back and removes the checkout, and deleting the chat removes any that are left.</div>
        <div className="sgroup">
          {list.worker_worktrees.map((w) => (
            <div key={`${w.task_id}:${w.sub_id}`} className="srowx">
              <GitBranch size={14} strokeWidth={1.7} aria-hidden="true" style={{ opacity: 0.6 }} />
              <div style={{ flex: 1, minWidth: 0 }}>
                <button type="button" className="linkish" onClick={() => openChat(w.task_id)} style={{ fontWeight: 500 }}>{w.title}</button>
                <div className="desc mono" style={{ fontSize: 11, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{w.branch}{w.branch ? " · " : ""}worker {w.sub_id}{w.exists ? "" : " · folder missing"}</div>
              </div>
            </div>
          ))}
        </div>
      </>}

      {(!!list?.orphan_worktrees.length || !!list?.orphan_workspaces.length) && <>
        <div className="settings-section-label">Left behind</div>
        <div className="settings-note">Folders whose chat is gone, usually from deleting a chat while keeping its files. Removing one deletes it, including uncommitted changes in it.</div>
        <div className="sgroup">
          {[...list.orphan_worktrees.map((p) => ["worktree", p] as const), ...list.orphan_workspaces.map((p) => ["workspace", p] as const)].map(([kind, p]) => (
            <div key={p} className="srowx">
              {kind === "worktree" ? <GitBranch size={14} strokeWidth={1.7} aria-hidden="true" style={{ opacity: 0.5 }} /> : <Box size={14} strokeWidth={1.7} aria-hidden="true" style={{ opacity: 0.5 }} />}
              <div style={{ flex: 1, minWidth: 0 }}><div className="mono sel" style={{ fontSize: 11.5, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{p}</div><div className="desc">{kind === "worktree" ? "Git worktree" : "Docker workspace"}</div></div>
              <ConfirmRemove label={`Remove ${baseName(p)}`} disabled={busy === p} onConfirm={() => act(p, () => workspacesApi.prune(kind, p))} />
            </div>
          ))}
        </div>
      </>}
      <div><Button variant="ghost" onClick={() => refresh(scan)}><RefreshCw size={13} /> Refresh</Button></div>
    </>
  );
}

function ConfirmRemove({ label, disabled, tooltip, onConfirm }: { label: string; disabled: boolean; tooltip?: string; onConfirm: () => void }) {
  const [armed, setArmed] = useState(false);
  if (!armed) return <IconButton label={label} disabled={disabled} onClick={() => setArmed(true)}><Trash2 size={12} /></IconButton>;
  return <div style={{ display: "flex", gap: 6 }}>
    <Button variant="ghost" onClick={() => setArmed(false)}>Keep</Button>
    <Tooltip content={tooltip ?? "Deletes the folder and anything uncommitted in it"}><Button onClick={() => { setArmed(false); onConfirm(); }}>Delete</Button></Tooltip>
  </div>;
}
