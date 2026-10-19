# Sandboxes

A sandbox is a computer of an agent's own: a Linux desktop (XFCE, Firefox, a terminal, a file manager) running in a Docker container. It belongs to you, not to a chat. Open **Artificts → Sandboxes** to see them.

## What you can do

- **Cards with live thumbnails** show every sandbox, whether it is running, and who is driving it (an agent or you).
- **Open one** for a live view of its screen. **Take over** gives you the mouse and keyboard; the agent's actions are refused until you **Hand back**.
- **Start / Stop**, rename, add notes, set **memory** and **CPU** caps (applied live), and choose whether an agent may use the **screen** and/or run **commands** in this sandbox.
- **Snapshots** freeze the disk; **Restore** goes back to one. **Clone** makes a new sandbox from the current disk and shared files. **Reset** starts again from a fresh desktop (and can change the screen size). **Delete** removes the container, snapshots and files.
- **Files**: the shared folder is `/home/agent/shared` inside the sandbox. Add files or folders from your computer, save copies out, delete.
- **Use in this chat / Detach** points the chat at that sandbox. Swap it any time; it applies from the chat's next tool call.
- The camera button saves what the screen shows as an artifict.

## What agents can do

The built-in `sandbox` tool: `list` your sandboxes, `create` a new one (asks first), `use` an existing one (prefer reusing), `start`/`stop`/`release`, `delete` (asks first), `screen` (a batch of click/type/key/scroll/drag actions, returning one screenshot, like `computer` for your own screen), `exec` (a bash command as user `agent`, with `sudo`), and `publish` (a screenshot, or an .html/.svg/.md/.json/.txt/.csv/.log/image file from the sandbox, into the Artificts tab).

The host `computer` tool is unchanged and still means your own screen; `screen` always means the sandbox.

## Requirements and limits

Docker Desktop or Docker Engine. The first sandbox builds a ~1 GB desktop image (once, a few minutes); `OPENLEASH_DOCKER=podman` is honoured. Resolution is fixed when the machine is created (change it with Reset). Snapshots keep the disk, not running programs. Sandboxes stay running until stopped. See `docs/security-model.md` for what a sandbox does and does not protect.
