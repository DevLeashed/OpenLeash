/** Searchable words from a chat summary for the command palette. */
import type { TaskSummary } from "../api";

/** Every query word must match somewhere in the task's searchable metadata. */
export function matchesTaskQuery(task: TaskSummary, query: string): boolean {
  const terms = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
  if (!terms.length) return true;
  const searchable = [
    task.title,
    task.id,
    task.project,
    task.cwd,
    task.branch,
    task.base_branch,
    task.model,
    task.status,
    task.waiting_kind ?? "",
    task.step,
  ].join(" ").toLocaleLowerCase();
  return terms.every((term) => searchable.includes(term));
}

/** Compact second line for a task result: location first, then useful live state. */
export function taskSearchDetail(task: TaskSummary): string {
  const location = task.project || task.cwd;
  const branch = task.branch;
  const status = task.waiting_kind === "question" ? "Needs an answer"
    : task.waiting_kind === "approval" ? "Needs approval"
      : task.status === "running" ? "Running"
        : task.status === "waiting" ? "Waiting"
          : task.status === "done" ? "Done"
            : task.status === "failed" ? "Failed"
              : task.status === "stopped" ? "Stopped" : "Idle";
  return [location, branch && branch !== task.base_branch ? branch : "", status].filter(Boolean).join(" · ");
}
