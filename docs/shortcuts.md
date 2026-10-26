# Keyboard shortcuts and commands

`Ctrl K` palette · `Ctrl N` new task · `Ctrl F` find in chat · `Ctrl ,` settings · `Shift Tab` plan mode · `Esc` deny / pause / close · `Enter` approve · `Ctrl J` details · `Ctrl B` sidebar · `Ctrl O` open folder · `Ctrl +` / `Ctrl -` / `Ctrl 0` zoom · `Alt Enter` queue a message to send after the current turn · review view: `A` accept · `X` revert · `↑`/`↓` or `J`/`K` walk files · `Ctrl Enter` commit. The review keys are plain letters; see [Review view](#review-view) for which modifiers they respect.

Question cards answer with the number keys, skip with `S`, page with `←`/`→`, and `Enter` moves to the next question (or accepts a confirmation). On a long list of options (more than eight), `/` focuses the filter; `Enter` then picks the first match, or sends what you typed as your own answer when nothing matches.

`Enter` in the composer sends the message and `Shift Enter` starts a new line — unless **Settings → General → Send message with** is *Ctrl Enter*, where the chord sends and plain `Enter` starts a new line. A phone browser is decided on its own, because its keyboard is a touch one: there is no `Ctrl` to press, and the on-screen Return key cannot be relied on to say whether `Shift` is held. So there `Enter` starts a new line and the send button sends, until **Settings → General → Enter sends on a phone** is turned on (off by default) — then `Enter` sends and `Shift Enter` starts the new line. Off is the default because Enter-to-send over a Return key with no trustworthy `Shift` sends the messages someone was breaking into paragraphs; the window you are reading this in is not affected by that switch. The rule is the box's, not the composer's: the one that talks to a single subagent from its panel sends by the same setting, and its send button names the key that sends (`messageSendKey` in `src/keys.ts` decides both).

`Esc` pauses a working chat: the current command is left to finish, then the turn freezes and Resume carries on as if nothing happened. The **Stop** button and `/stop` are a force pause: every command in flight is killed — **background ones included**, so a dev server or watcher the agent started with `run_in_background` does not survive — each agent is told which of **its own** commands went down, and the chat freezes where it stands. The turn is not cancelled, so every subagent keeps its own context and Resume carries on from there, mid-build if that is where it was. `Esc Esc` (the second press on a paused chat, or **Dismiss pause** on the paused list) is the outright stop: the agent, the model stream and every command die, cut tool calls are closed as errors, and the next message continues from history. Resuming a stopped chat offers three choices: **Continue** retries what got cut off and carries on, **Summarise and continue** reports where things stand and then carries on (cut-off subagents resume in the background and report back), and **Wrap up** reports and stops there (cut-off subagents stay stopped).

Modifier keys are written in words in the source (`Ctrl K`) and rendered per platform, so a macOS build shows `⌘K`. `chord()` in `src/keys.ts` does the swap and the `Kbd` primitive in `src/ui/primitives/Controls.tsx` applies it — note that this is a **display-only** transformation; it changes the label, not the binding. The global key handler accepts either `ctrlKey` or `metaKey`, so every `Ctrl` shortcut above works as `⌘` on macOS, but the review view's `A` / `X` / `J` / `K` keys check `ctrlKey` only and are unchanged on macOS.

The titlebar's folder button (left of the search box) switches project: click it, then ↑/↓ and `Enter`; `Esc` closes.

Slash commands (type `/` for autocomplete; custom commands from Settings → Commands are offered too): `/goal <what done looks like>`, `/plan`, `/ultra`, `/ultrawt`, `/race` (new task only), `/init`, `/compact [focus]`, `/btw <question>`, `/model [name]`, `/effort <level>`, `/assist <guide|default|necessary>`, `/perm <ask|auto|full>`, `/pause`, `/resume [message]`, `/all <message>`, `/review`, `/cost`, `/stop`, `/models`, `/details`, `/save`, `/saved`, `/paused`, `/skills`, `/accounts`, `/mcp`, `/settings`, `/clear`.

This list mirrors `SLASH` in `src/ui/Composer.tsx`, which is the source of truth — `/ultrax` is deliberately absent because that feature is still behind `ULTRA_X_SHOWN = false`. `/race` works from the new-task composer and the command palette, but it is not in the `/` menu, so type it in full. The composer also accepts `/new`, `/broadcast`, `/permissions` and `/agents` as aliases, which the menu does not show.

The pause banner on the new-task screen has a **View all** button (and the minimized pill a **List** one) that opens every paused chat in one place, with the reason each one is frozen and per-chat resume, discard or stop. **View all** shrinks the banner to a small pill on its way to the list. Tick rows to choose what to lift and **Resume N** thaws only those — under *Pause all* that is the only way to thaw several chats at once without thawing the rest, since dropping the global flag would thaw all of them. (The in-chat **Resume** button thaws a single chat, and stays frozen otherwise.) **Discard** cancels the frozen work and leaves the chat stopped (the transcript stays, and you continue from wherever you pick up next). `Ctrl K` → *Paused chats* and `/paused` get there too.

Messaging a chat that is paused resumes it, and the message joins the conversation *before* the thaw rather than after it — the model never gets a turn on the request the chat froze on, so it can't answer a question you have already answered.


## Review view

`A` accepts the selected file and `X` reverts it. `J`/`K` and `↑`/`↓` walk the file list, and `Ctrl Enter` commits. `A` is ignored while Ctrl (or ⌘ on macOS) is held. `X`, `J` and `K` do not check modifiers yet, so `Ctrl X` also reverts and `Ctrl J`/`Ctrl K` also walk the list (the global `Ctrl J` details and `Ctrl K` palette shortcuts fire too). Use the plain letters.

## Shortcuts by surface

These apply only while the named surface has focus.

| Surface | Keys |
|---|---|
| **Find in chat** (`Ctrl F`) | `Enter` next match · `Shift Enter` previous match · `Esc` close |
| **Composer, edit-message box** | `Enter` resends the edited message · `Esc` cancels the edit |
| **Comment boxes** (artifict studio, browser annotations, overlays) | `Ctrl Enter` sends the comment |
| **Model picker** | `↑`/`↓` move · `Enter` picks · `Shift Enter` picks without making it the default · `←`/`→` step effort while the search box is empty |
| **Rewind dialog** | `↑`/`↓` walk checkpoints · `Enter` confirms |
| **Artifict studio** | `C` comment mode · `E` edit mode (HTML only) · `[` / `]` step through versions · `Ctrl S` save source · `Esc` leaves comment or edit mode |
| **Browser view** | When the live view is focused, keys go to the page; `Ctrl`, `Alt` and `Meta` chords are not forwarded. The address bar uses `↑`/`↓`, `Enter` and `Esc`. |
| **Image lightbox** | `←`/`→` previous and next · `Esc` close |
| **Welcome tour** | `←`/`→` move between steps |

`Esc` in the main window, in order: denies a pending approval; clears a selection, the find bar or an open menu; otherwise pauses a working chat, and a second `Esc` stops it. While the artifict studio is open, `Esc` does nothing at the window level; use the studio's own controls above.

The zoom shortcuts also accept `=`, `+`, `-` and `0` without Ctrl.
