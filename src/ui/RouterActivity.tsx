// "Router activity": the switches a chat's router made, newest first. Opened
// from the composer's route menu; reads `route_log`, so it shows what the
// backend actually did rather than what the transcript happened to keep.
import { useEffect, useState } from "react";
import { api, EFFORTS, type RouteEvent } from "../api";
import { modelInfo, set, useStore } from "../store";
import { I } from "./icons";
import { IconButton, Loader, Modal } from "./primitives";

export const openRouterLog = (id: string) => set({ routerLog: id, menu: null, pal: false });

const ACTION: Record<string, string> = { use: "switched", escalate: "escalated", deescalate: "de-escalated", effort: "effort", stay: "stayed", hold: "held", revert: "reverted" };

function when(at: number) {
  const d = new Date(at * 1000);
  const today = new Date().toDateString() === d.toDateString();
  return today ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" }) : d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

const name = (id: string) => (id ? modelInfo(id).name : "—");

/** Newest first; the backend keeps them oldest first. */
export function newestFirst(list: readonly RouteEvent[]): RouteEvent[] {
  return [...list].sort((a, b) => b.at - a.at);
}

export function RouterActivity() {
  const id = useStore((s) => s.routerLog);
  const title = useStore((s) => (s.routerLog ? s.tasks[s.routerLog]?.title ?? "" : ""));
  const [events, setEvents] = useState<RouteEvent[] | null>(null);
  const [err, setErr] = useState("");
  useEffect(() => {
    if (!id) return;
    let alive = true;
    setEvents(null); setErr("");
    api.routeLog(id).then((l) => { if (alive) setEvents(newestFirst(l)); }).catch((e) => { if (alive) setErr(String(e)); });
    return () => { alive = false; };
  }, [id]);
  if (!id) return null;
  const close = () => set({ routerLog: null });
  return (
    <Modal onClose={close} ariaLabel="Router activity" style={{ width: "min(620px, 100%)" }}>
      <div className="mh"><span style={{ display: "flex", color: "var(--mut2)" }}>{I.route(15)}</span><span style={{ flex: 1 }}>Router activity{title ? ` · ${title}` : ""}</span><IconButton label="Close" onClick={close}>{I.close()}</IconButton></div>
      <div className="mb router-log">
        {err && <div className="router-error" role="alert">Couldn't read the router log · {err}</div>}
        {!err && !events && <Loader size={14} />}
        {events && !events.length && <div className="desc">The router hasn't switched anything in this chat yet.</div>}
        {events && events.length > 0 && (
          <ol className="router-log-list">
            {events.map((e, i) => (
              <li key={`${e.at}-${i}`} className="router-log-row">
                <span className="router-log-time mono">{when(e.at)}</span>
                <div className="router-log-main">
                  <div>
                    <span className="router-log-agent">{e.agent || e.agent_kind || "agent"}</span>{" "}
                    <span className="router-hint">{ACTION[e.action] ?? e.action}</span>{" "}
                    <span>{name(e.from)} → {name(e.to)}</span>
                    {e.effort_from !== e.effort_to && <span className="router-hint"> · {EFFORTS[e.effort_from] ?? e.effort_from} → {EFFORTS[e.effort_to] ?? e.effort_to}</span>}
                  </div>
                  {/* The backend's reason already leads with the rule's name
                      ("Stuck → escalate: …"), so the name only shows alone. */}
                  {(e.rule_name || e.reason) && <div className="desc">{e.reason && (!e.rule_name || e.reason.startsWith(e.rule_name)) ? e.reason : [e.rule_name, e.reason].filter(Boolean).join(" · ")}</div>}
                </div>
              </li>
            ))}
          </ol>
        )}
      </div>
    </Modal>
  );
}
