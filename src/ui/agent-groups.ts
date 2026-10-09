// Pure helpers for the Subagents editor's tool-restriction field.
//
// Kept out of the component so the parsing/formatting can be unit-tested without
// mounting a dialog. The wire shape is Roo Code's — a plain group name, or a
// `[name, {fileRegex, description}]` tuple — which the Rust side serializes as an
// object for the tuple half (see `ToolGroup` / `GroupOpts` in store.rs).
import type { ToolGroup } from "../api";

/** One group as the editor holds it. `fileRegex`/`description` empty means the
 *  group is unrestricted, which is the plain-string wire form. */
export interface GroupRow {
  name: string;
  fileRegex: string;
  description: string;
}

export const GROUP_NAMES = ["read", "edit", "command", "mcp"] as const;

export function groupsOf(d: { groups?: ToolGroup[] }): GroupRow[] {
  return (d.groups ?? []).map((g) =>
    typeof g === "string"
      ? { name: g, fileRegex: "", description: "" }
      : { name: g.name, fileRegex: g.file_regex ?? "", description: g.description ?? "" },
  );
}

/** Back to the wire form: a bare string unless the row actually restricts. */
export function toToolGroups(rows: GroupRow[]): ToolGroup[] {
  return rows
    .filter((r) => r.name.trim())
    .map((r) =>
      r.fileRegex.trim() || r.description.trim()
        ? { name: r.name, file_regex: r.fileRegex.trim(), description: r.description.trim() }
        : r.name,
    );
}

/** Whether `rows` already holds a restricted `edit` group. */
export function hasEditRestriction(rows: GroupRow[]): boolean {
  return rows.some((r) => r.name === "edit" && !!r.fileRegex.trim());
}

/**
 * Validate the rows the way the Rust side will. Returns an error to show the
 * user, or null. The regex check has to run in the browser too, because the save
 * path refuses a bad pattern with a Rust error and the dialog's own flash reads
 * better than a round-tripped message that arrives after the dialog closes.
 */
export function validateGroups(rows: GroupRow[]): string | null {
  const editWithRegex = rows.filter((r) => r.name === "edit" && r.fileRegex.trim());
  if (editWithRegex.length > 1) return "The edit group can only be listed once — put both patterns in one fileRegex (e.g. \\.(md|mdx)$).";
  for (const r of rows) {
    if (!r.name.trim()) return "Every tool group needs a name.";
    if (!(GROUP_NAMES as readonly string[]).includes(r.name)) return `Unknown tool group \`${r.name}\`. Use one of: ${GROUP_NAMES.join(", ")}.`;
    if (r.fileRegex.trim() && r.name !== "edit") return `A fileRegex only applies to the \`edit\` group, not \`${r.name}\`.`;
    if (r.fileRegex.trim()) {
      try {
        new RegExp(r.fileRegex);
      } catch (e) {
        return `fileRegex isn't a valid regular expression: ${String(e)}`;
      }
    }
  }
  return null;
}

/** The path a restriction promise is about, for the summary line. */
export function restrictionLabel(rows: GroupRow[]): string | null {
  const edit = rows.find((r) => r.name === "edit" && r.fileRegex.trim());
  if (!edit) return null;
  return edit.description.trim() ? `edits ${edit.description.trim()}` : `edits matching ${edit.fileRegex.trim()}`;
}

// The iteration ceiling. 0 means "no ceiling", which keeps agents saved before
// the field running exactly as they did.
export const NO_STEPS = 0;

/** Human line for the ceiling, shown under the field and on the agent row. */
export function stepsLabel(steps: number | undefined): string {
  const n = steps ?? NO_STEPS;
  return n > 0 ? `stops after ${n} step${n === 1 ? "" : "s"}` : "no step limit";
}
