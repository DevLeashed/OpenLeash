// Workspaces: where a task's files live and its commands run. "local" is the
// project folder itself, "worktree" a git worktree on its own branch, "docker"
// a private copy of the project with commands run in a container built from
// the project's `.openleash_docker/`. The Rust half is
// src-tauri/src/agent/docker.rs and the workspace commands in lib.rs.
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

export type WorkspaceMode = "local" | "worktree" | "docker";
export type ContainerState = "unknown" | "building" | "starting" | "running" | "stopped" | "missing" | "error";
export interface DockerEnv { container: string; project: string; host_root: string; guest_root: string }

export interface DockerProjectInfo {
  has_config: boolean;
  trusted: boolean;
  config_error: string | null;
  /** Only present when the engine was asked (`checkEngine`). */
  engine: { ok: true; version: string } | { ok: false; error: string } | null;
}

export interface WorkspaceEntry { task_id: string; title: string; project: string; path: string; branch: string; status: string; archived: boolean; exists: boolean; container?: string; state?: ContainerState }
/** One ultrathread worker's own checkout. Several agents can share a path (a
 *  coordinator's nested workers), so entries are per agent, not per folder. */
export interface WorkerWorktree { task_id: string; title: string; sub_id: string; branch: string; path: string; status: string; exists: boolean }
export interface ContainerInfo { name: string; task_id: string; project: string; state: string; status: string; image: string; created: string }
export interface WorkspacesList {
  worktrees: WorkspaceEntry[];
  /** Ultrathread worker worktrees, per agent, with the chat that owns them. */
  worker_worktrees: WorkerWorktree[];
  docker: WorkspaceEntry[];
  /** Null unless docker was asked (a docker chat exists, or a scan was requested). */
  containers: ContainerInfo[] | null;
  docker_error: string | null;
  orphan_worktrees: string[];
  orphan_workspaces: string[];
  orphan_containers: ContainerInfo[];
  idle_minutes: number;
}

export const workspacesApi = {
  dockerInfo: (project: string, checkEngine: boolean) => invoke<DockerProjectInfo>("docker_project_info", { project, checkEngine }),
  addTemplate: (project: string) => invoke<void>("docker_add_template", { project }),
  start: (id: string) => invoke<void>("container_start", { id }),
  stop: (id: string) => invoke<void>("container_stop", { id }),
  rebuild: (project: string, noCache: boolean) => invoke<string>("docker_rebuild", { project, noCache }),
  pushBranch: (id: string) => invoke<string>("docker_push_branch", { id }),
  list: (scanDocker: boolean) => invoke<WorkspacesList>("workspaces_list", { scanDocker }),
  prune: (kind: "container" | "workspace" | "worktree", target: string) => invoke<void>("workspace_prune", { kind, target }),
  removeContainer: (id: string) => invoke<void>("container_remove", { id }),
  changeFolder: (id: string, folder: string) => invoke<void>("task_change_folder", { id, folder }),
  pickFolder: async () => {
    const folder = await open({ directory: true, multiple: false, title: "Choose chat work folder" });
    return typeof folder === "string" ? folder : null;
  },
};

/** The mode new tasks default to, from settings old and new. */
export function defaultMode(s: { workspace?: string; worktree: boolean } | null | undefined): WorkspaceMode {
  if (s?.workspace === "local" || s?.workspace === "worktree" || s?.workspace === "docker") return s.workspace;
  return s?.worktree ? "worktree" : "local";
}

export const CONTAINER_LABEL: Record<ContainerState, string> = {
  unknown: "Container idle", building: "Building image…", starting: "Starting container…", running: "Container running",
  stopped: "Container stopped", missing: "No container yet", error: "Container error",
};
