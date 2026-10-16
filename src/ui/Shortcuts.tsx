import { Modal, Kbd } from "./primitives";

/** The app's keyboard surface, in one place.
 *
 *  Mirrors the key handling in `App.tsx` (global), `Composer.tsx`, `Review.tsx`
 *  and `Question.tsx`; `docs/shortcuts.md` is the prose version. It lives in the
 *  app because a palette-driven tool with this many chords had no way to show
 *  them — the only listing was a single row in Settings.
 *
 *  `Ctrl` is written in words: `Kbd` renders it per platform, so a macOS build
 *  shows `⌘K`. */
export const SHORTCUT_GROUPS: { group: string; items: { keys: string; what: string }[] }[] = [
  {
    group: "Anywhere",
    items: [
      { keys: "Ctrl K", what: "Command palette — every action, chat and setting" },
      { keys: "Ctrl N", what: "New task" },
      { keys: "Ctrl F", what: "Find in this chat" },
      { keys: "Ctrl ,", what: "Settings" },
      { keys: "Ctrl J", what: "Toggle the details panel" },
      { keys: "Ctrl B", what: "Toggle the sidebar" },
      { keys: "Ctrl O", what: "Open a project folder" },
      { keys: "Ctrl +", what: "Zoom in" },
      { keys: "Ctrl -", what: "Zoom out" },
      { keys: "Ctrl 0", what: "Reset the zoom" },
      { keys: "Shift Tab", what: "Turn plan mode on or off" },
    ],
  },
  {
    group: "While the agent is working",
    items: [
      { keys: "Esc", what: "Pause — the turn freezes, background commands keep running" },
      { keys: "Esc Esc", what: "Hard stop — the agent, the stream and its commands all die" },
      { keys: "Alt Enter", what: "Steer the agent without stopping its turn" },
      { keys: "Enter", what: "Approve the card the agent is waiting on" },
    ],
  },
  {
    group: "Composer",
    items: [
      { keys: "Enter", what: "Send — or start a new line, if Settings says Ctrl Enter sends" },
      { keys: "Shift Enter", what: "The other one" },
      { keys: "/", what: "Slash commands, with autocomplete" },
    ],
  },
  {
    group: "Reviewing changes",
    items: [
      { keys: "A", what: "Accept the file you are looking at" },
      { keys: "X", what: "Revert it" },
      { keys: "↑ ↓ / J K", what: "Walk the changed files" },
      { keys: "Ctrl Enter", what: "Commit" },
    ],
  },
  {
    group: "Question cards",
    items: [
      { keys: "1-9", what: "Choose an option" },
      { keys: "S", what: "Skip — a refusal to decide, not a blank answer" },
      { keys: "← →", what: "Page through the questions" },
    ],
  },
];

/** The shortcut reference, as a dialog. */
export function ShortcutSheet({ onClose }: { onClose: () => void }) {
  return (
    <Modal onClose={onClose} ariaLabel="Keyboard shortcuts" style={{ width: "min(560px, 100%)", maxHeight: "80vh", display: "flex", flexDirection: "column" }}>
      <div className="mh"><span style={{ flex: 1 }}>Keyboard shortcuts</span><Kbd>Esc</Kbd></div>
      <div className="mb" style={{ overflowY: "auto" }}>
        {SHORTCUT_GROUPS.map((section) => (
          <div key={section.group} style={{ marginBottom: 14 }}>
            <div className="settings-section-label" style={{ marginTop: 0 }}>{section.group}</div>
            {section.items.map((item) => (
              <div key={item.keys} style={{ display: "flex", alignItems: "baseline", gap: 10, padding: "3px 0" }}>
                <span style={{ flex: "none", width: 96, textAlign: "right" }}><Kbd>{item.keys}</Kbd></span>
                <span className="desc" style={{ flex: 1 }}>{item.what}</span>
              </div>
            ))}
          </div>
        ))}
      </div>
    </Modal>
  );
}
