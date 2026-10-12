import { Children, cloneElement, isValidElement, useId, useState, type ReactElement, type ReactNode } from "react";

type ElementProps = { children?: ReactNode; "aria-sort"?: "ascending" | "descending" | "none" };
const elements = (children: ReactNode) => Children.toArray(children).filter(isValidElement) as ReactElement<ElementProps>[];
const plain = (node: ReactNode): string => Children.toArray(node).map(child => isValidElement<ElementProps>(child) ? plain(child.props.children) : String(child)).join("");
const number = (text: string): number | null => {
  const trimmed = text.trim();
  if (!/^-?\d[\d,]*(\.\d+)?%?$/.test(trimmed)) return null;
  const value = Number(trimmed.replace(/[,％%]/g, ""));
  return Number.isFinite(value) ? value : null;
};

/** Controls only reorder/filter rendered rows; the transcript remains unchanged. */
export function ReplyTable({ children }: { children?: ReactNode }) {
  const id = useId();
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<{ column: number; descending: boolean } | null>(null);
  const parts = elements(children);
  const head = parts[0], body = parts[1];
  const rows = body ? elements(body.props.children) : [];
  // Small tables stay prose-sized; incomplete streaming tables keep normal rendering.
  if (!head || !body || rows.length < 6) return <table>{children}</table>;
  const headerRow = elements(head.props.children)[0];
  if (!headerRow) return <table>{children}</table>;
  const headers = elements(headerRow.props.children);
  const filtered = rows.map((row, index) => ({ row, index, cells: elements(row.props.children).map(cell => plain(cell.props.children)) }))
    .filter(({ cells }) => cells.join(" ").toLowerCase().includes(query.toLowerCase()));
  if (sort) filtered.sort((a, b) => {
    const left = a.cells[sort.column] ?? "", right = b.cells[sort.column] ?? "";
    const ln = number(left), rn = number(right);
    const order = ln !== null && rn !== null ? ln - rn : left.localeCompare(right);
    return (sort.descending ? -order : order) || a.index - b.index;
  });
  return <div className="reply-table">
    <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", margin: "8px 0" }}>
      <label htmlFor={id}>Filter rows</label>
      <input id={id} type="search" value={query} onChange={event => setQuery(event.target.value)} style={{ minWidth: 120, flex: 1 }} />
      <span role="status">{filtered.length} / {rows.length} rows</span>
      {(query || sort) && <button type="button" onClick={() => { setQuery(""); setSort(null); }}>Reset table</button>}
    </div>
    <div style={{ overflowX: "auto" }}><table>
      {cloneElement(head, {}, cloneElement(headerRow, {}, headers.map((cell, column) => cloneElement(cell, { key: column, "aria-sort": sort?.column === column ? sort.descending ? "descending" : "ascending" : "none" },
        <button type="button" onClick={() => setSort(current => ({ column, descending: current?.column === column ? !current.descending : false }))} style={{ font: "inherit", color: "inherit", background: "transparent", border: 0, cursor: "pointer", textAlign: "inherit" }}>
          {cell.props.children}{sort?.column === column ? sort.descending ? " ↓" : " ↑" : " ↕"}
        </button>))))}
      {cloneElement(body, {}, filtered.map(({ row, index }) => cloneElement(row, { key: index })))}
    </table></div>
    {!filtered.length && <p>No matching rows.</p>}
  </div>;
}
