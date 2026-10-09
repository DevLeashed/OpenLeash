// Where an ultrathread stands, for the places that show it at a glance: the
// title-bar badge, the sidebar dot and the header underline. A live swarm
// streams the violet gradient; once it settles the colour says how it went, so
// an idle/paused/failed ultrathread never looks like it's still working.
import type { SubInfo, TaskSummary } from "../../api";

export interface SwarmCounts { running: number; done: number; failed: number; stopped: number }

export function swarmCounts(subs: SubInfo[]): SwarmCounts {
  const c: SwarmCounts = { running: 0, done: 0, failed: 0, stopped: 0 };
  for (const s of subs) {
    if (s.status === "running") c.running++;
    else if (s.status === "done") c.done++;
    else if (s.status === "failed") c.failed++;
    else c.stopped++;
  }
  return c;
}

export type SwarmState = "live" | "done" | "failed" | "paused" | "stopped" | "waiting";

/**
 * The swarm's state, worst-news-first: a failure anywhere outranks a clean
 * finish, a pause or an approval beats the chat being merely idle, and a
 * paused chat outranks a stopped one. A swarm with no subagents yet is only
 * live while the chat itself is — before the first wave it is still planning.
 *
 * `heldByGlobal` is the global pause's contribution, which the swarm cannot see
 * for itself: a chat frozen by "Pause all" carries no `paused` of its own, so
 * asking the task alone said "live" for a swarm that was frozen solid. That
 * overrode the pause everywhere the swarm's state picks the colour — the sidebar
 * dot went to the running blue, the header underline kept breathing, and the
 * badge counted live agents that no longer were. Callers pass
 * `heldByGlobal(task, pausedAll)`, the same predicate the rest of the app uses,
 * so one rule decides both.
 */
export function swarmState(task: Pick<TaskSummary, "paused" | "status" | "subs">, heldByGlobal = false): SwarmState {
  if (task.paused || heldByGlobal) return "paused";
  const c = swarmCounts(task.subs);
  if (task.status === "waiting") return "waiting";
  if (c.running > 0) return "live";
  if (c.failed > 0) return "failed";
  // Agents that were all stopped read as stopped, not a clean finish.
  if (c.stopped > 0 && c.done === 0) return "stopped";
  if (c.done > 0) return "done";
  // No agents at all: the chat decides.
  return task.status === "running" ? "live" : "stopped";
}

/** Only a live swarm glows. Everything else uses a flat status colour. */
export function swarmGlow(state: SwarmState): boolean {
  return state === "live";
}
