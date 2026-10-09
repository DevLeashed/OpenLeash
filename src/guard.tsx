/**
 * The PC-control banner.
 *
 * A separate always-on-top window, not part of the main UI: while an agent is
 * driving the mouse and keyboard the app is not focused, so anything drawn
 * inside the main window would be invisible at exactly the moment the warning
 * matters. This window floats above whatever the agent is clicking.
 *
 * Two states:
 *   active   — an agent has the screen right now (violet, animated dot)
 *   objected — the user pressed Esc; the agent has been told to stop taking the
 *              screen (red, persists until the run ends, so the agent can't
 *              quietly carry on without the user noticing)
 */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";

type St = { active: boolean; objected: boolean; summary: string; task_id: string };

const OFF: St = { active: false, objected: false, summary: "", task_id: "" };

function Banner() {
  const [s, setS] = useState<St>(OFF);
  useEffect(() => {
    invoke<St>("pcguard_state").then(setS).catch(() => {});
    // The state event is the source of truth after this; the fetch above only
    // covers the gap between the window opening and the first change.
    const un = listen<St>("ol://pcguard", (e) => setS({ ...OFF, ...e.payload }));
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, []);

  const stop = () => {
    if (!s.task_id) return;
    invoke("pcguard_stop", { id: s.task_id }).catch(() => {});
  };

  // Idle: the window is hidden by the backend, but render nothing so a stray
  // show (e.g. a stale state right after a batch ends) is not a ghost banner.
  if (!s.active && !s.objected) return null;

  return (
    <div className={"bar" + (s.objected ? " objected" : "")} data-tauri-drag-region>
      <span className={"dot" + (s.active && !s.objected ? " pulse" : "")} />
      <div className="txt">
        <div className="t1">{s.objected ? "Screen control stopped" : "OpenLeash is controlling your PC"}</div>
        <div className="t2">
          {s.objected
            ? "You pressed Esc. The agent was told to stop."
            : s.summary || "Working on your screen…"}
        </div>
      </div>
      <button
        className="stop"
        onClick={stop}
        onMouseDown={(e) => e.stopPropagation()}
        title="Stop this run (Esc twice also stops it)"
      >
        Stop
      </button>
      <div className="esc">esc</div>
    </div>
  );
}

const css = `
  /* No webfont here. This window is a compact banner that appears over another
   * app mid-interaction, so it has to paint on the first frame; fetching Geist
   * first would show it in the fallback and then reflow it under the cursor.
   * The system stack below is what it renders with. */
  * { box-sizing: border-box; }
  html, body, #root { margin: 0; height: 100%; background: #18181b; overflow: hidden; }
  body {
    font-family: "Geist", system-ui, "Segoe UI", sans-serif;
    -webkit-font-smoothing: antialiased; user-select: none; cursor: default;
    /* The native window is opaque: paint its edges too, rather than exposing
     * WebView2's white background through transparent margins. */
  }
  .bar {
    margin: 0; height: 100%; display: flex; align-items: center; gap: 11px;
    padding: 0 10px 0 13px; border-radius: 8px;
    background: #18181b;
    border: 1px solid rgba(167, 139, 250, 0.38);
    color: #f4f4f5;
  }
  .bar.objected { border-color: rgba(233, 133, 133, 0.55); }
  .dot { width: 9px; height: 9px; border-radius: 50%; background: #a78bfa; flex: none; box-shadow: 0 0 9px rgba(167,139,250,0.8); }
  .dot.pulse { animation: p 1.15s ease-in-out infinite; }
  .bar.objected .dot { background: #e98585; box-shadow: 0 0 9px rgba(233,133,133,0.8); }
  @keyframes p { 0%,100% { opacity: 1; } 50% { opacity: 0.28; } }
  .txt { flex: 1; min-width: 0; }
  .t1 { font-size: 12.5px; font-weight: 600; letter-spacing: -0.01em; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .t2 { font-size: 10.5px; color: #a3a3ab; margin-top: 1px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .stop {
    flex: none; font: 500 11px/1 "Geist", system-ui, sans-serif; color: #f4f4f5;
    background: rgba(255,255,255,0.09); border: 1px solid rgba(255,255,255,0.12);
    padding: 6px 11px; border-radius: 8px; cursor: pointer;
  }
  .stop:hover { background: #e98585; border-color: #e98585; color: #161618; }
  .esc {
    flex: none; font: 500 9.5px/1 "Geist Mono", monospace; color: #d4d4d8;
    background: rgba(255,255,255,0.07); border: 1px solid rgba(255,255,255,0.1);
    padding: 4px 5px; border-radius: 5px; text-transform: uppercase;
  }
`;

const style = document.createElement("style");
style.textContent = css;
document.head.appendChild(style);

createRoot(document.getElementById("root")!).render(<Banner />);
