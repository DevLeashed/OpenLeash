import type { ItemData } from "../api";

/** Exit metadata is a process receipt, never proof that all tests executed. */
export function commandReceipt(data: ItemData): { label: string; detail: string } | null {
  if (data.name !== "bash" || data.bg_id) return null;
  if (data.status === "running") return { label: data.exec ? "Command running" : "Command pending", detail: "No exit verdict yet" };
  if (data.status === "denied") return { label: "Command denied", detail: "Not executed" };
  const exit = /^exit (-?\d+)$/.exec(data.meta ?? "");
  if (exit) return { label: Number(exit[1]) === 0 ? "Command exited successfully" : "Command failed", detail: `Exit ${exit[1]} · process result, not test acceptance` };
  if (data.meta === "timed out" || data.meta === "interrupted" || data.meta === "force paused") return { label: "Command incomplete", detail: data.meta };
  return { label: "No recorded exit verdict", detail: "Do not infer success from tool completion" };
}
export function CommandReceipt({ data, cwd }: { data: ItemData; cwd: string }) {
  const receipt = commandReceipt(data);
  if (!receipt) return null;
  return <details style={{ margin: "4px 0 8px 22px", fontSize: 12, color: "var(--mut)" }}>
    <summary>{receipt.label} · {receipt.detail}</summary>
    <div>Working directory: <code>{cwd}</code></div>
    <pre style={{ whiteSpace: "pre-wrap" }}>{String(data.input?.command ?? "")}</pre>
    <div>Counts and scope must be checked in command output; this receipt does not parse arbitrary test logs.</div>
  </details>;
}
