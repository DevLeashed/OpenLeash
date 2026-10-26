// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { mdComponents, remarkOl } from "./mdx";
afterEach(cleanup);
const rows = ["Alpha | 100", "Beta | 2", "Gamma | 30", "Delta | 4", "Epsilon | 50", "Zeta | 6"];
const markdown = (data = rows) => `| Area | Count |\n|---|---|\n${data.map(row => `| ${row} |`).join("\n")}`;
const tableRows = () => screen.getAllByRole("row").slice(1).map(row => within(row).getAllByRole("cell").map(cell => cell.textContent));
describe("reply tables", () => {
  it("filters large tables and reports empty results without mutating content", () => {
    render(<ReactMarkdown remarkPlugins={[remarkGfm, remarkOl]} components={mdComponents}>{markdown()}</ReactMarkdown>);
    fireEvent.change(screen.getByRole("searchbox"), { target: { value: "beta" } });
    expect(tableRows()).toEqual([["Beta", "2"]]);
    expect(screen.getByRole("status").textContent).toBe("1 / 6 rows");
    fireEvent.change(screen.getByRole("searchbox"), { target: { value: "missing" } });
    expect(screen.getByText("No matching rows.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Reset table" }));
    expect(tableRows()).toHaveLength(6);
  });
  it("sorts numbers numerically, toggles direction and restores source order", () => {
    render(<ReactMarkdown remarkPlugins={[remarkGfm]} components={mdComponents}>{markdown()}</ReactMarkdown>);
    fireEvent.click(screen.getByRole("button", { name: /Count/ }));
    expect(tableRows().map(row => row[1])).toEqual(["2", "4", "6", "30", "50", "100"]);
    expect(screen.getAllByRole("columnheader")[1]!.getAttribute("aria-sort")).toBe("ascending");
    fireEvent.click(screen.getByRole("button", { name: /Count/ }));
    expect(tableRows()[0]).toEqual(["Alpha", "100"]);
    fireEvent.click(screen.getByRole("button", { name: "Reset table" }));
    expect(tableRows().map(row => row[0])).toEqual(["Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta"]);
  });
  it("leaves small tables alone", () => {
    render(<ReactMarkdown remarkPlugins={[remarkGfm]} components={mdComponents}>{markdown(rows.slice(0, 2))}</ReactMarkdown>);
    expect(screen.queryByRole("searchbox")).toBeNull();
    expect(tableRows()).toHaveLength(2);
  });
  // The Progress column is drawn as a bar inside the cell, so the cell element
  // the table sorts by used to hold no text at all: every sort key was the empty
  // string, the comparator saw no difference, and clicking the column left the
  // rows exactly where they were.
  it("sorts the progress column by its percentage", () => {
    const data = [
      "one.md | 146 | 81",
      "two.md | 161 | 109",
      "three.md | 71 | 36",
      "four.md | 38 | 29",
      "five.md | 121 | 28",
      "six.md | 0 | 16",
    ];
    const md = `| File | done | open |\n|---|---|---|\n${data.map(row => `| ${row} |`).join("\n")}`;
    render(<ReactMarkdown remarkPlugins={[remarkGfm, remarkOl]} components={mdComponents}>{md}</ReactMarkdown>);
    fireEvent.click(screen.getByRole("button", { name: /Progress/ }));
    expect(tableRows().map(row => row[3])).toEqual(["0%", "57%", "60%", "64%", "66%", "81%"]);
    fireEvent.click(screen.getByRole("button", { name: /Progress/ }));
    expect(tableRows().map(row => row[3])).toEqual(["81%", "66%", "64%", "60%", "57%", "0%"]);
  });
});
