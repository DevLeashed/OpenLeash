// A race's attempts in the sidebar: one foldable header naming the race, its
// state and a way into the comparison view, with the attempt rows under it.
// The rows themselves are the sidebar's own, passed in as children.
import type { ReactNode } from "react";
import type { Race } from "../api";
import { archiveRace, attemptsFinished, openRace, RACE_STATE, setRaces, useRaces } from "../race";
import { useStore } from "../store";
import { I } from "./icons";
import { Pressable, Tooltip } from "./primitives";
import "./Race.css";

export function RaceGroup({ race, children }: { race: Race; children: ReactNode }) {
  const folded = useRaces((s) => !!s.collapsed[race.id]);
  // "2/3" while attempts are still going: the group reads as progress, not just "Running".
  const progress = useStore((s) => { const n = attemptsFinished(race, s.tasks); return n < race.attempts.length ? `${n}/${race.attempts.length}` : null; });
  const st = RACE_STATE[race.state] ?? RACE_STATE.running;
  const toggle = () => setRaces((s) => {
    const collapsed = { ...s.collapsed };
    if (collapsed[race.id]) delete collapsed[race.id]; else collapsed[race.id] = true;
    return { collapsed };
  });
  return (
    <div className="race-group" data-race={race.id}>
      <Pressable className="srow race-head" aria-expanded={!folded} onClick={toggle}>
        <span className={"race-chev" + (folded ? "" : " open")}>{I.chevR()}</span>
        <span className="t">Race · {race.prompt}</span>
        <span className="race-pill small" style={{ ["--race-c" as string]: st.color }}>{race.winner ? (race.merged ? "Merged" : "Picked") : race.state === "running" && progress ? `Running ${progress}` : st.label}</span>
        <Tooltip content="Compare the attempts">
          <button type="button" className="race-compare" onClick={(e) => { e.stopPropagation(); openRace(race.id); }}>Compare</button>
        </Tooltip>
        <Tooltip content="Archive the race and its chats">
          <button type="button" className="race-compare" onClick={(e) => { e.stopPropagation(); void archiveRace(race.id); }}>Archive</button>
        </Tooltip>
      </Pressable>
      {!folded && <div className="race-group-rows">{children}</div>}
    </div>
  );
}
