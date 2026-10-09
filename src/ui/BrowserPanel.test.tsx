// @vitest-environment jsdom
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup } from "@testing-library/react";
import { browserPanel } from "../api";
import { BrowserPanel, browserPoint } from "./BrowserPanel";
vi.mock("../api", () => ({ browserPanel: vi.fn() }));
const frame = { url: "https://example.com", title: "Agent page", png: "abc", width: 1600, height: 1000, viewport_width: 800, viewport_height: 500 };
afterEach(() => { cleanup(); vi.clearAllMocks(); });
it("maps scaled screenshots to CSS viewport coordinates", () => {
  expect(browserPoint(210, 120, { left: 10, top: 20, width: 400, height: 250 }, frame)).toEqual({ x: 400, y: 200 });
});
it("opens the task's same browser, offers explicit navigation and closes only the panel", async () => {
  vi.mocked(browserPanel).mockResolvedValue(frame);
  const close = vi.fn();
  render(<BrowserPanel taskId="task-a" onClose={close} />);
  await waitFor(() => expect(screen.getByAltText(/Main agent browser page/)).toBeTruthy());
  expect(browserPanel).toHaveBeenCalledWith("task-a", { kind: "snapshot" });
  fireEvent.change(screen.getByLabelText("Browser URL"), { target: { value: "https://example.org" } });
  fireEvent.click(screen.getByText("Go"));
  await waitFor(() => expect(browserPanel).toHaveBeenCalledWith("task-a", { kind: "navigate", url: "https://example.org" }));
  fireEvent.click(screen.getByText("Close browser panel"));
  expect(close).toHaveBeenCalledOnce();
});
