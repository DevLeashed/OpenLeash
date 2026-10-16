# Remote connectivity

OpenLeash can be used from somewhere other than the window it runs in. There are three ways, and in all three the agent's commands run on one machine while you drive it from another.

| You want to… | Use |
|---|---|
| Approve, steer or start tasks from your phone or another computer | [Remote control](#remote-control) |
| Run agents on a dev box or server and drive them from this computer | [Other machines over SSH](#other-machines-over-ssh) |
| Work on a project that lives on a dev box or server, with this computer's agent | [Open over SSH](#open-over-ssh) |
| Work on a project that lives inside WSL | [WSL projects](#wsl-projects) |

For running a task in a container instead, see [Workspaces](workspaces.md).

## Remote control

**Settings → Remote → Allow remote control.** OpenLeash starts a small web server and serves its own interface to any browser that has been paired. The remote page is the full app, with no reduced mobile mode: chats, approvals, questions, plan review, settings, the review view and everything else.

### Pairing

Nothing can use the server until it has been paired, and that includes pages opened on the same computer.

- **QR code or link.** Click *Pair a device* and scan the code with your phone's camera. The link carries a long secret that is swapped for a session the first time it is used.
- **Six-digit code.** Open the address shown in Settings on the other device and type a code from *Create pairing code*. A code lasts five minutes, works once, and stops working after five wrong guesses. A device that keeps guessing wrong is locked out for ten minutes.

Each paired browser appears under **Paired devices** with its browser, OS, and when it was last used. You can revoke one device, or use **Sign out all devices**, which also rotates the QR secret. The titlebar shows a chip whenever a remote device is connected, so remote use is never silent.

### Reachability

| Setting | Listens on | Use it for |
|---|---|---|
| **This computer** (default) | `127.0.0.1` | SSH tunnels, Tailscale Serve, other local tooling |
| **Network** | every interface | Phones on the same Wi-Fi or a Tailscale network |

The default port is **17420** and can be changed. Network mode uses plain HTTP: anyone on the network who can watch traffic can see what you do and steal a session. Only use it on networks you trust. A private overlay such as Tailscale or WireGuard, or an SSH tunnel to a loopback-only server, is the better choice anywhere else.

### What works differently in a browser

- **Folder and file pickers** show the host computer's disk inside the page. A native dialog would open on the host, where nobody is looking.
- **Links** open on the device you are holding.
- **Window buttons** are hidden. Closing the browser tab does not close OpenLeash.
- **Dropping files onto the composer** only works in the desktop window.
- **The composer's `Enter`** follows the device, not the window it is driving. A phone's on-screen keyboard has no `Ctrl`, and its Return key cannot be relied on to report a held `Shift` — so in a phone browser `Enter` starts a new line and the send button sends, and **Settings → General → Enter sends on a phone** (off by default) turns Enter-to-send on for that device, with `Shift Enter` the newline. A browser on another computer is not a phone browser: there **Send message with** still decides, exactly as in the window.
- **Copy buttons** use the browser's clipboard when the page has one. A page on `http://` over the LAN is not a secure context, so most browsers hide it: the copy then goes through the selected-text fallback instead. Either way the button tells you it copied, or that it could not.
- If the connection drops, the page shows *Reconnecting…* and reloads by itself once the computer is reachable again.

### How it works

Settings → Remote state is kept in `~/.openleash/remote.json`, separate from `settings.json`. Only hashes of session secrets are stored there.

The page is the same frontend bundle as the desktop window, with a bridge script (`src-tauri/src/remote/shim.js`) loaded in front of it. Every call the page makes goes over a WebSocket to the desktop app, which hands it to the main window's own IPC entry point. So a remote call is checked against the same Tauri capability as a click in the window, with no second command list to drift out of date. Events reach the page through the same channel.

See [the security model](security-model.md#remote-control) for what this does and does not protect.

## Other machines over SSH

**Settings → Remote → Other machines over SSH.** Add a machine by its SSH alias or `user@host` and press **Connect**. OpenLeash then:

1. reads that machine's `~/.openleash/remote.json` over SSH to get its port and pairing secret,
2. starts `ssh -N -L` to forward a local port to it, and
3. opens the forwarded page in your browser, already paired.

The agents run on that machine, against its files and its toolchain, and keep running if your laptop goes to sleep.

Requirements:

- OpenLeash running on the other machine with remote control on. *This computer* is enough, because the tunnel arrives locally. On a headless Linux server, run it under a virtual display, for example `xvfb-run -a openleash`.
- Key-based or agent-based SSH login. OpenLeash uses your system `ssh` with `BatchMode=yes`: it reads your `~/.ssh/config`, keys, agent and `known_hosts`, never asks for a password, and never touches a host-key decision. Connect once from a terminal first to accept the host key.

The tunnel closes when you press **Disconnect** or quit OpenLeash. `OPENLEASH_SSH` overrides the `ssh` binary.

The remote session opens in your browser rather than an OpenLeash window. Tauri reserves the IPC slot in every page it displays, and the remote page needs that slot for its bridge.

### Send a chat to another machine

Right-click a chat in the sidebar (or use **+** in a chat) and choose **Send to another machine…**. Pick one of your saved SSH machines, then:

- **Copy** leaves this chat here and creates an independent copy there. **Move** stops the chat, sends it, and archives it here once the other machine confirms the import. If the import isn't confirmed (OpenLeash isn't running there, or its answer was lost), the chat stays here, stopped and not archived, with a notice that the bundle is waiting on the other machine; archive it yourself once it shows up there. If sending fails, the chat stays here, stopped, with an error in its transcript.
- **Project folder** is where the chat runs on the other machine. The dialog suggests the project there whose git `origin` is the same repository, and **Browse…** lists that machine's folders. Leave it empty to let the other machine choose: the project with the same `origin`, then the same path, then its home folder (with a warning to the agent and to you).
- **Continue running there** (only for a chat that is running) starts it again on the other machine right after the import. Off by default.
- The dialog warns when the project has uncommitted changes or unpushed commits. **Only the conversation travels, not the code.** Push or sync the code first if the other machine needs it.

How it travels: the chat is written to an `openleash-chats` v2 bundle (the export format plus where it came from), copied over `ssh` (streamed through its standard input, size-checked, then renamed into place) into the other machine's imports folder, `$OPENLEASH_HOME/imports` (by default `~/.openleash/imports/`, `%USERPROFILE%\.openleash\imports` on Windows), and the OpenLeash there is asked over an SSH tunnel and its remote-control WebSocket to import it. The WebSocket carries only the file name, never the chat. The session used for this is logged out when the dialog closes, so transfers don't use up paired-device slots. On Windows machines the copy runs through PowerShell.

If OpenLeash isn't running there (or remote control is off), the bundle waits in that imports folder and is imported, stopped, the next time OpenLeash starts there. **Continue running there** is not honoured in that case. A bundle that can't be imported is moved to `imports/failed/`.

After the import, the main agent and every subagent with a conversation are told which machines the chat moved between, where the project now is, that background commands, worktrees, spill files, checkpoints and unfinished tool calls stayed behind, that files must be re-read before editing, and which branch and commit the old machine was on. The imported chat starts with the other machine's own prompt, tools, plugins and trust decisions, and its permission level is capped at that machine's default.

## Open over SSH

**Open over SSH…** in the project menu (the folder chip on the home screen or the titlebar project selector), or *Open project over SSH…* in the command palette, opens a folder on another machine as a project, VS Code Remote-SSH style: the agent runs here, and its tools (files, commands, git) run on that machine. The other machine needs a POSIX shell (Linux, macOS or BSD) and the SFTP subsystem, but not OpenLeash.

1. **Pick a host.** Choose a saved SSH host, or add one in place with a name and `user@host` or an alias from `~/.ssh/config`. Hosts are the same list as [Other machines over SSH](#other-machines-over-ssh) (**Settings → Remote**).
2. **Test.** OpenLeash connects from scratch, runs one command and opens SFTP, then shows the OS and home folder, or the error with a hint: key login missing (password prompts aren't supported, because `ssh` runs with `BatchMode=yes`), host key not yet accepted, host not found or unreachable, a login shell that isn't POSIX, or SFTP not working. It also says if `git` or `rg` is missing there.
3. **Browse** the machine's folders, starting in its home folder. You can type an absolute path, `~/…`, or go to `/`.
4. **Choose** a folder. It is opened as `ssh://<host-id>/<path>`, where `<host-id>` is the saved host's id rather than its address, so editing a host's address in Settings keeps its projects, and no project path can choose what `ssh` connects to.

SSH projects are listed as `<folder> · <host name>` with a server icon. Saving or removing a host closes its pooled connection. Code: `remote/ssh_projects.rs`, `agent/ssh_session.rs`, `ui/OpenProject.tsx`.

### What runs where in an SSH project

| Runs on the remote host | Runs on this computer |
|---|---|
| `read_file`, `edit_file`, `multi_edit`, `write_file`, `view_image` (over one pooled SFTP session) | The agent itself, model requests, the permission gate and approvals |
| `glob` and `grep` (file lists and candidate files from `rg`, else `git ls-files`/`find`, else `grep -E`; matching and output are the same code as for a local project) | Your hooks (Settings → Hooks), started in your home folder with `OL_CWD`/`OL_PROJECT` set to the `ssh://` URI |
| `bash`, foreground and background, as `bash -lc` (or `sh -lc`) in the project folder | Chat history, memory (keyed by host and path), spill files, trust decisions |
| git: review, commit, revert, branch info | The browser and desktop tools |

- **Paths** are POSIX. The agent may use paths relative to the project, absolute paths on that host, or `ssh://` URIs on the *same* host. Windows paths, `~`, other hosts and `file://` URLs are refused.
- **Writes** are checked against where they really land: the target and the project root are both resolved with the server's `realpath` (following symlinks, including a dangling one), and the write goes to exactly that path. In Auto mode, `proj/link -> /etc` counts as outside the project and asks.
- **Read before write** is enforced like locally, with a content hash next to the mtime: SFTP reports whole seconds, so a change in the same second as the read is still caught.
- **Stopping** a command (timeout, Esc, a force pause, `kill_bash`) signals its whole remote process group, not just the local `ssh`.
- **Long output** is cut to the usual size but not saved to a local spill file (the agent couldn't read it from the remote host); the agent is told to redirect output to a file and read that instead.
- **Attachments:** images are attached as usual; other local files are refused for an SSH chat (copy them to the host). File links in replies open in VS Code Remote-SSH (`vscode://vscode-remote/ssh-remote+…`); reply images are read over SFTP with the usual extension, magic-byte and 8 MiB checks.

Not available for SSH projects yet (each refuses with "… not yet supported for SSH projects" instead of falling back to a local folder): checkpoints and rewind of files, worktree and docker workspaces, artificts, opening project files in the browser tool, automatic post-edit diagnostics, and the project's own skills, agents, hooks and MCP servers (`.openleash/…`, `.claude/…`). The project's instruction files (`AGENTS.md`, `CLAUDE.md`, …) are read from the host and gated by folder trust as usual. fish/csh login shells and Windows OpenSSH servers are not supported.

## WSL projects

Open a project at its WSL path, for example `\\wsl.localhost\Ubuntu\home\me\app`. The easiest way is **Open from WSL…** in the project menu (the folder chip on the home screen, or the titlebar project selector), shown when WSL has at least one distro: choose the distro and the folder dialog opens in its `/home/<you>` (or `/home` when there are several users). Docker Desktop's internal distros are not listed. You can also type the path into the folder picker or pick it from **Linux** in the Explorer sidebar. In a remote browser, the folder picker lists each distro as a root next to the drive letters. OpenLeash then:

- runs shell commands, background jobs, hooks and git inside that distro with `wsl.exe --exec bash -lc`, so the project's Linux toolchain is used,
- keeps file tools on the Windows UNC path, which Windows serves natively,
- maps Linux paths the agent copies from command output (`/home/me/app/src/main.rs`) back to the matching file, and
- tells the agent it is on Linux in WSL.

Worktrees for a WSL repository are created from inside the distro. Cancelling a command stops the `wsl.exe` process; a program that detaches from its terminal inside the distro can outlive it.
