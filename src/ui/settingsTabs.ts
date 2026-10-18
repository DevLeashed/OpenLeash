import { Archive, Bot, Boxes, Cable, ChartNoAxesColumn, KeyRound, ListChecks, Network, Palette, PanelLeft, Puzzle, Settings, SlidersHorizontal, Smartphone, Sparkles, Terminal, UserRound, type LucideIcon } from "lucide-react";

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
 * and the "Shortcut button" choice in Settings → Tabs. Keeping the
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
  ["routing", "Routers", Network],
  ["agents", "Subagents", Bot],
  ["skills", "Skills", Sparkles],
  ["commands", "Commands", Terminal],
  ["chats", "Chats", Archive],
  ["tabs", "Tabs", PanelLeft],
  ["workspaces", "Workspaces", Boxes],
  ["perms", "Permissions", KeyRound],
  ["plugins", "Plugins", Puzzle],
  ["connectors", "Connectors", Cable],
  ["checks", "Checks & hooks", ListChecks],
  ["mcp", "MCP servers", Cable],
  ["remote", "Remote", Smartphone],
  ["usage", "Stats", ChartNoAxesColumn],
] as const;

export const SETTINGS_TABS: readonly SettingsTab[] = TABS.map((tab) => ({ id: tab[0], label: tab[1], Icon: tab[2] }));

/**
 * Pages with a branch in the Settings render switch but no nav button.
 *
 * Only the nav entry is dropped, so a page listed here stays reachable from
 * anywhere that sets `settingsTab` directly — the palette, the route menu and
 * the sidebar shortcut — and unhiding one is deleting its id here rather than
 * rewiring every entry point back. Empty since Routers came back: that page
 * keeps the id "routing" so saved `settingsTab` and shortcut values still land
 * on it.
 */
export const HIDDEN_SETTINGS_TABS: readonly string[] = [];

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
