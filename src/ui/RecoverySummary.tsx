import type { TaskSummary } from "../api";
import { useStore } from "../store";
export function RecoverySummary({ task }: { task: TaskSummary }) {
  const jobs = useStore(s => s.bg[task.id]);
  const active = (jobs ?? []).filter(job => job.running);
  const remaining = task.todos.filter(todo => todo.status !== "completed");
  return <details className="pb-recovery">
    <summary>Recovery context · {remaining.length} unfinished tasks · {active.length} live background jobs</summary>
    <div>Last agent step: {task.step || "No step recorded"}</div>
    {active.map(job => <div key={job.id}><code>{job.id}</code> — still running</div>)}
    {remaining.slice(0, 8).map((todo, i) => <div key={i}>{todo.content} — {todo.status}</div>)}
    <div>Todo status is not verification evidence. Check command receipts/output before claiming completion; jobs absent from the live list are not known to be running.</div>
  </details>;
}
