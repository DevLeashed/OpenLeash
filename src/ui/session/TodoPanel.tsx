import { useId } from "react";
import { ChevronDown, Circle, CircleCheck } from "lucide-react";
import type { Todo } from "../../api";
import { AnchoredPanel, Button, IconButton, Loader, useAnchoredPanel } from "../primitives";
import { I } from "../icons";

export function todoProgress(todos: Todo[]) {
  const completed = todos.filter((todo) => todo.status === "completed").length;
  const active = todos.find((todo) => todo.status === "in_progress");
  return { completed, active, total: todos.length };
}

function TodoIcon({ todo, size = 14 }: { todo: Todo; size?: number }) {
  return todo.status === "completed" ? <CircleCheck size={size} /> : todo.status === "in_progress" ? <Loader variant="agent" size={size} /> : <Circle size={size} />;
}

export function TodoPanel({ todos, expanded, onToggle }: { todos: Todo[]; expanded: boolean; onToggle: () => void }) {
  const listId = useId();
  if (!todos.length) return null;

  const { completed, active, total } = todoProgress(todos);
  return (
    <section className="todo-panel" aria-label="Task plan">
      <Button variant="ghost" className="todo-panel-trigger" aria-expanded={expanded} aria-controls={expanded ? listId : undefined} onClick={onToggle}>
        <span className="todo-panel-heading">Tasks</span>
        <span className="todo-panel-count">{completed}/{total}</span>
        <span className="todo-panel-current" aria-live="polite">{active ? active.activeForm || active.content : completed === total ? "Complete" : "Ready"}</span>
        <ChevronDown size={15} className={expanded ? "todo-panel-chevron expanded" : "todo-panel-chevron"} aria-hidden="true" />
      </Button>
      {expanded && <ol id={listId} className="todo-panel-list">
        {todos.map((todo, index) => (
          <li key={`${index}:${todo.content}`} className={`todo-panel-item ${todo.status}`}>
            <span className="todo-panel-status" aria-hidden="true"><TodoIcon todo={todo} size={15} /></span>
            <span>{todo.status === "in_progress" ? todo.activeForm || todo.content : todo.content}</span>
          </li>
        ))}
      </ol>}
    </section>
  );
}

/** Header chip: progress at a glance, the whole plan in a panel like Status. */
export function TodoChip({ todos }: { todos: Todo[] }) {
  const listId = useId();
  const { anchor, open, toggle, close } = useAnchoredPanel();
  if (!todos.length) return null;
  const { completed, active, total } = todoProgress(todos);
  const done = completed === total;
  // The chip shows the run's own state: spinning while a step runs, ticked
  // once the list is done, hollow while everything is still pending.
  const mark = done ? <CircleCheck size={13} /> : active ? <Loader variant="agent" size={13} /> : <Circle size={13} />;
  return (
    <>
      <Button variant="ghost" className={"todo-chip" + (open ? " on" : "")} aria-expanded={open} aria-controls={open ? listId : undefined} onClick={toggle}>
        <span className="todo-chip-mark" aria-hidden="true">{mark}</span>
        <span className="todo-chip-count">{completed}/{total}</span>
        {active && <span className="todo-chip-now">{active.activeForm || active.content}</span>}
        <ChevronDown size={13} className={open ? "todo-panel-chevron expanded" : "todo-panel-chevron"} aria-hidden="true" />
      </Button>
      <AnchoredPanel anchor={anchor} onClose={close} width={340}>
        <div className="sp-head">
          <span style={{ fontWeight: 600 }}>Tasks</span>
          <span className="sp-age">{done ? "complete" : active ? "in progress" : "ready"}</span>
          <div style={{ flex: 1 }} />
          <span className="sp-age">{completed}/{total}</span>
          <IconButton label="Close" onClick={close}>{I.close()}</IconButton>
        </div>
        <ol id={listId} className="todo-panel-list sp-list">
          {todos.map((todo, index) => (
            <li key={`${index}:${todo.content}`} className={`todo-panel-item ${todo.status}`}>
              <span className="todo-panel-status" aria-hidden="true"><TodoIcon todo={todo} /></span>
              <span>{todo.status === "in_progress" ? todo.activeForm || todo.content : todo.content}</span>
            </li>
          ))}
        </ol>
        <div className="sp-foot">
          {active ? <Loader variant="agent" size={11} /> : done ? <CircleCheck size={11} /> : <Circle size={11} />}
          <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{active ? active.activeForm || active.content : done ? "Everything on the list is done" : "Nothing in progress"}</span>
        </div>
      </AnchoredPanel>
    </>
  );
}
