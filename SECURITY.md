# Security Policy

## Reporting a vulnerability

**Please do not open a public GitHub Issue for a security bug.**

Report it privately via GitHub's private vulnerability reporting:

> **Security → Report a vulnerability** on this repository
> (<https://github.com/DevLeashed/OpenLeash/security/advisories/new>)

That opens a private thread visible only to the maintainer. You will get an
acknowledgement within 7 days.

If the private advisory flow is unavailable to you, open a regular issue that
says only "security report available on request" with **no technical detail**,
and the maintainer will open a private channel with you.

### What to include

- Affected OpenLeash version (the `Settings → About` build number, and the
  `version` in `src-tauri/tauri.conf.json`).
- Your OS and version.
- Reproduction steps, or a proof of concept if you have one.
- The impact you observed.

### Please do NOT

- Paste a **live** API key, OAuth token, or credential file into the report.
  Redact it. We can reproduce with a dummy value.
- Attach real user data, `settings.json`, or a `~/.codex` / `~/.claude`
  credentials file. These contain live secrets.
- Run a proof of concept against a production account you do not own, or
  against anyone's machine without their explicit permission.

### Special note for bugs that use your own machine

OpenLeash is an agent harness: it executes shell commands and file edits
requested by a language model, and it can type and click on your desktop
(see `src-tauri/src/agent/`). If you find a bug where **OpenLeash's own
actions** escape the permission model, the PoC can be destructive by
definition. Please:
- Use a throwaway VM, a spare machine, or a dedicated test account.
- Set the agent to a scoped working directory, not your home directory.
- Do not point it at production credentials or a real repository.
- Tell us whether the escape needed the model's cooperation or fired on its
  own — that distinction matters a lot for severity.

We will treat a well-scoped, careful report that damages a test VM as a
**good-faith** report, full stop.

## What OpenLeash holds, and what a bug can reach

This is public so you can judge whether a finding matters. It is also a map
of the obvious attack surface, which we consider fair game to document.

OpenLeash, by design, is a high-privilege local application. Its security
boundary is **"which commands the agent is allowed to run"**, not "what the
agent can reach". A user who grants an agent shell access has handed it
shell access; that is the product, not a vulnerability.

Concretely:

- **Model-authored code is executed.** The model decides which shell
  commands run. The permission layer (`src-tauri/src/agent/permissions.rs`)
  gates them, and the guard layer (`src-tauri/src/agent/pcguard.rs`) blocks
  the Esc key. A bypass of either is a real finding.
- **API keys and OAuth tokens are stored in plaintext** in
  `settings.json` under the app's data directory, and in the CLI-owned
  credential files it imports from (`~/.codex/auth.json`,
  `~/.claude/.credentials.json`). They are not encrypted at rest, and are
  protected only by OS file permissions on the user account. **Any
  same-user process, including other unprivileged malware, can read them.**
  This is a known and accepted limitation of day-one design, not a hidden
  vulnerability — but a way to *exfiltrate* them from outside the user's
  own account (privilege escalation, remote read, IPC cross-webview) **is** a
  vulnerability.
- **Synthetic input and screen capture.** The app installs a global
  keyboard hook and can synthesise mouse/keyboard events
  (`enigo`) and capture the screen (`xcap`). On a shared or multi-user
  machine this is a local-privilege concern; a way to make it activate
  without the guard, or to read frames it should not, is a finding.
- **The webview renders remote content.** The agent loads provider
  responses, which can contain model-generated markdown/HTML. Anything that
  lets remote content reach the IPC surface or escape the sandbox is a
  finding.

### Out of scope

- "The agent ran a command I did not intend to allow" when the command was
  allowed by the permission model you configured.
- Plaintext storage of your own credentials on your own machine.
- Findings that require an attacker to already have code execution as your
  user account.
- Denial of service by resource exhaustion in the Rust host, absent a
  privilege or data boundary crossing.
- Missing hardening headers with no demonstrated impact.

## Supported versions

OpenLeash is pre-1.0. Only the latest released version receives security
fixes. There is no LTS branch and no backport policy.

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |
| < 0.1   | :x:                |
| main    | :x: (not a release) |

`main` is not a supported release. If your finding is only reproducible
against a `main` build, say so; we will still look, but it will not enter
the advisory as a released-version issue.

## Disclosure timeline

We aim for:

| Stage                                  | Target |
| -------------------------------------- | ------ |
| Acknowledgement of a valid report     | 7 days |
| Triage and severity assessment        | 14 days |
| Fix released for a **critical/high**  | 30 days from triage |
| Fix released for a **medium**         | 90 days from triage |
| Fix released for a **low**            | next scheduled release |
| Public advisory (CVE requested)       | on or shortly after fix |

If a fix is going to take longer than the target, we will say so in the
private thread rather than go quiet.

We will credit you in the advisory and the fix commit unless you ask us not
to. We will not pursue legal action over good-faith research that stays
within the guidance above.

## Reporting a security bug is not a licence to break other people

OpenLeash's whole threat model assumes the operator is running it on their
own machine. Please keep testing to that assumption. Do not target a third
party's OpenLeash install, do not persist on a machine you do not own, and
do not use a finding to access an account, token, or file that is not yours.
