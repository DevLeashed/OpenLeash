# Workspaces

Every task works in a *workspace*. Choose it on the new-task screen, next to the project and branch. The default lives in **Settings → General** or **Settings → Workspaces**.

| Mode | Files | Commands | Good for |
|---|---|---|---|
| **Local** | The project folder itself | On this computer | Quick changes, one task at a time |
| **Worktree** | A git worktree on a new `ol/<slug>` branch | On this computer | Several tasks in parallel on one repo |
| **Docker** | A private copy of the project | In a container built from the project | Isolated, reproducible environments; Full Access with less worry |

## Docker workspaces

A docker task works like a Kilo Code cloud task, but on your own machine. It gets its own copy of the project and its own container, and you review the result when it is done.

### Opting a project in

Add a `.openleash_docker/` folder at the project root containing a `Dockerfile`. That folder is the image's whole build context, so anything the Dockerfile `COPY`s must be inside it.

```dockerfile
# .openleash_docker/Dockerfile
FROM node:22-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends git ripgrep && rm -rf /var/lib/apt/lists/*
```

The image needs `/bin/sh`. If it has `bash`, commands run in bash. You can also add an optional `.openleash_docker/openleash.json`:

```json
{
  "workdir": "/workspace",
  "env": { "NODE_ENV": "development" },
  "network": "bridge",
  "ports": [3000],
  "idle_minutes": 15,
  "user": "host"
}
```

| Key | Meaning |
|---|---|
| `workdir` | Where the workspace is mounted. Default `/workspace`. |
| `env` | Variables for every command. |
| `network` | `"bridge"` (default) or `"none"`. Host networking is not offered. |
| `ports` | Container ports published on `127.0.0.1` only. |
| `idle_minutes` | Overrides the idle timeout for this project. `0` never stops the container. |
| `user` | `"host"` (default: your uid:gid on Linux, so files stay yours), `"root"`, or a user from the image. Docker Desktop on Windows and macOS maps ownership itself. |

Unknown keys are rejected. In particular, raw `docker run` flags can't be passed: the file comes from the repository, and flags like `--privileged` or `-v /:/host` would give the repository the whole machine.

Docker mode is only offered for **trusted** folders, because building the image runs the project's own `RUN` steps.

### What happens

1. **Workspace.** A git project is cloned into `~/.openleash/workspaces/<task>` on a new `ol/<slug>` branch. A clone is used rather than a worktree so git also works inside the container. `.worktreeinclude` files and the post-setup hook are applied as they are for worktrees. A non-git project is copied, skipping whatever `.gitignore` and `.ignore` exclude, and skipping symlinks.
2. **Image.** Tagged with a hash of everything in `.openleash_docker/`. **Any change to that folder rebuilds the image** the next time a docker task needs it, and running containers move to the new image. **Settings → Workspaces → Rebuild / Clean rebuild** forces a rebuild.
3. **Container.** `openleash-<task>`, labelled with the task and project, with the workspace bind-mounted. Only the agent's shell commands run in it. File tools work on the host side of the mount, so read-before-write, staleness checks, checkpoints and the review view behave exactly as they do for a worktree. Paths the agent copies from command output (`/workspace/src/x.ts`) map back to the host file.
4. **Lifecycle.** The harness owns the container: it starts when you send the chat a message, stops after the idle timeout (default 15 minutes, counting commands and background jobs as activity), is stopped when OpenLeash next starts if it was left running, and is removed with the chat. Deleting a chat removes its workspace only if you choose to.
5. **Getting the work out.** The chat header has a container chip with start/stop and **Push branch**, which pushes `ol/<slug>` into the project repository so you can merge it like any other branch. The review view and commit work as usual inside the workspace.

OpenLeash runs no `docker` command at all unless you have docker chats or pick Docker, so it never wakes an engine you don't use. Docker Desktop (Windows, macOS) and Docker Engine (Linux) are supported, with bind mounts relabelled for SELinux on Linux. `OPENLEASH_DOCKER=podman` uses Podman instead.

## Manager

**Settings → Workspaces** lists every worktree and container OpenLeash created: the chat each belongs to, branch, state, and start/stop. It also finds what was **left behind**, meaning worktrees and docker workspaces whose chat is gone, and containers labelled by OpenLeash with no chat. Each can be removed from there. A leftover-container scan only runs when you ask for it, unless docker chats already exist.
