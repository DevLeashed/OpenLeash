import { Archive, Bot, Boxes, Cable, ChartNoAxesColumn, KeyRound, ListChecks, Network, Palette, Puzzle, Settings, SlidersHorizontal, Smartphone, Sparkles, Terminal, UserRound, type LucideIcon } from "lucide-react";

/** One Settings page: the id `settingsTab` navigates to, its label in the nav
 *  and in pickers, and the glyph both places draw for it. */
export interface SettingsTab {
  id: string;
  label: string;
  Icon: LucideIcon;
}

/**
 * Every Settings page, in nav order.
 *
 * One list, two readers: the Settings nav (which drops the hidden pages below)
 * and the "Sidebar shortcut button" choice in Settings → General. Keeping the
 * tuple form makes the label and the icon sit next to the id they belong to, so
 * a page added here is reachable from both places without a second list to keep
 * in step — and a page renamed here cannot leave the shortcut pointing at a
 * settings screen that no longer exists.
 */
const TABS = [
  ["general", "General", Settings],
  ["theme", "Theme", Palette],
  ["models", "Models", SlidersHorizontal],
  ["accounts", "Accounts", UserRound],
  ["routing", "Routing", Network],
  ["agents", "Subagents", Bot],
  ["skills", "Skills", Sparkles],
  ["commands", "Commands", Terminal],
  ["chats", "Chats", Archive],
  ["workspaces", "Workspaces", Boxes],
  ["perms", "Permissions", KeyRound],
  ["plugins", "Plugins", Puzzle],
  ["connectors", "Connectors", Cable],
  ["checks", "Checks & hooks", ListChecks],
  ["mcp", "MCP servers", Cable],
  ["remote", "Remote", Smartphone],
  ["usage", "Stats", ChartNoAxesColumn],
] as const;

export const SETTINGS_TABS: readonly SettingsTab[] = TABS.map(([id, label, Icon]) => ({ id, label, Icon }));

/**
 * Pages with a branch in the Settings render switch but no nav button.
 *
 * Only the nav entry is dropped, so a page listed here stays reachable from
 * anywhere that sets `settingsTab` directly — the palette's "Routing and
 * fallbacks", the route menu's "Edit routes…", and now the sidebar shortcut —
 * and unhiding one is deleting its id here rather than rewiring every entry
 * point back. Routing is the only one: it is a page worth reaching on purpose
 * and not worth a permanent slot in the nav.
 */
export const HIDDEN_SETTINGS_TABS: readonly string[] = ["routing"];

/** Where the sidebar footer's shortcut button goes when the user has not chosen
 *  a page: Accounts, which is where that button has always gone. */
export const SIDEBAR_SETTINGS_TAB_DEFAULT = "accounts";

/**
 * The page a stored tab id names, falling back to the default.
 *
 * The value is a plain string in `settings.json`, so it can name a page this
 * build does not have — one removed since, one from a newer build, or one typed
 * by hand. Resolving it here means the shortcut always opens a real settings
 * page instead of a blank one with nothing selected in the nav.
 */
export function settingsTabDef(id: string | null | undefined): SettingsTab {
  return SETTINGS_TABS.find((t) => t.id === id)
    ?? SETTINGS_TABS.find((t) => t.id === SIDEBAR_SETTINGS_TAB_DEFAULT)
    ?? SETTINGS_TABS[0]!;
}
