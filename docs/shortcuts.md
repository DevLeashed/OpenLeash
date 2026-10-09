# Keyboard shortcuts and commands

`Ctrl K` palette · `Ctrl N` new task · `Ctrl F` find in chat · `Ctrl ,` settings · `Shift Tab` plan mode · `Esc` deny / pause / close · `Enter` approve · `Ctrl J` details · `Ctrl B` sidebar · `Ctrl O` open folder · `Ctrl +` / `Ctrl -` / `Ctrl 0` zoom · `Alt Enter` steer without stopping the turn · review view: `A` accept · `X` revert · `↑`/`↓` or `J`/`K` walk files · `Ctrl Enter` commit.

Question cards answer with the number keys, skip with `S`, page with `←`/`→`, and `Enter` accepts a confirmation.

`Esc` pauses a working chat: the current command is left to finish, then the turn freezes and Resume carries on as if nothing happened. `Esc Esc` (or Stop) is the hard stop — the agent, the model stream and every command die, **background ones included**: a dev server or watcher the agent started with `run_in_background` does not survive the stop, and the agent is told which ones were killed so it restarts what it still needs. A pause alone leaves background commands running; "Force pause" is what kills them.

Modifier keys are written in words in the source (`Ctrl K`) and rendered per platform, so a macOS build shows `⌘K`. `chord()` in `src/keys.ts` does the swap and the `Kbd` primitive in `src/ui/primitives/Controls.tsx` applies it — note that this is a **display-only** transformation; it changes the label, not the binding. The global key handler accepts either `ctrlKey` or `metaKey`, so every `Ctrl` shortcut above works as `⌘` on macOS, but the review view's `A` / `X` / `J` / `K` keys check `ctrlKey` only and are unchanged on macOS.

The titlebar's folder button (left of the search box) switches project: click it, then ↑/↓ and `Enter`; `Esc` closes.

Slash commands (type `/` for autocomplete): `/goal <what done looks like>`, `/plan`, `/ultra`, `/ultrawt`, `/init`, `/compact [focus]`, `/btw <question>`, `/model [name]`, `/effort <level>`, `/assist <guide|default|necessary>`, `/perm <ask|auto|full>`, `/pause`, `/resume [message]`, `/all <message>`, `/review`, `/cost`, `/stop`, `/models`, `/details`, `/save`, `/saved`, `/paused`, `/skills`, `/accounts`, `/mcp`, `/settings`, `/clear`.

This list mirrors `SLASH` in `src/ui/Composer.tsx`, which is the source of truth — `/ultrax` is deliberately absent because that feature is still behind `ULTRA_X_SHOWN = false`.

The pause banner on the new-task screen has a **View all** button (and the minimized pill a **List** one) that opens every paused chat in one place, with the reason each one is frozen and per-chat resume, discard or stop. **View all** shrinks the banner to a small pill on its way to the list. Tick rows to choose what to lift and **Resume N** thaws only those — under *Pause all* that is the only way to thaw several chats at once without thawing the rest, since dropping the global flag would thaw all of them. (The in-chat **Resume** button thaws a single chat, and stays frozen otherwise.) **Discard** cancels the frozen work and leaves the chat stopped (the transcript stays, and you continue from wherever you pick up next). `Ctrl K` → *Paused chats* and `/paused` get there too.

Messaging a chat that is paused resumes it, and the message joins the conversation *before* the thaw rather than after it — the model never gets a turn on the request the chat froze on, so it can't answer a question you have already answered.
