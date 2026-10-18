// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import { browserAnnotate, browserPanel, type BrowserPanelAction, type BrowserPanelFrame, type Item } from "../api";
import { set } from "../store";
import { BrowserPanel, browserPoint, PREVIEW_DEBOUNCE_MS } from "./BrowserPanel";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("../api", async (importOriginal) => ({ ...(await importOriginal<typeof import("../api")>()), browserPanel: vi.fn(), browserAnnotate: vi.fn() }));

const frame: BrowserPanelFrame = { url: "https://example.com/", title: "Agent page", png: "abc", width: 1600, height: 1000, viewport_width: 800, viewport_height: 500, tabs: [], can_back: false, can_forward: false, history: [], dialog: null, element: null };
const element = { ref: "e12", selector: "main > h1", tag: "H1", role: "heading", name: "Welcome", bbox: { x: 100, y: 40, width: 300, height: 60 }, styles: { "font-size": "32px", "font-weight": "700", color: "rgb(0, 0, 0)" } };

beforeAll(() => {
  // jsdom lays nothing out: give the screenshot a size (400×250 on screen for
  // an 800×500 viewport, so every screen pixel is two CSS pixels).
  HTMLImageElement.prototype.getBoundingClientRect = () => ({ left: 0, top: 0, width: 400, height: 250, right: 400, bottom: 250, x: 0, y: 0, toJSON: () => ({}) });
  if (!("PointerEvent" in window)) {
    class PointerEventShim extends MouseEvent { pointerId: number; constructor(type: string, init: PointerEventInit = {}) { super(type, init); this.pointerId = init.pointerId ?? 1; } }
    (window as unknown as { PointerEvent: unknown }).PointerEvent = PointerEventShim;
  }
});
afterEach(() => { cleanup(); vi.clearAllMocks(); set({ items: {} }); });

const calls = () => vi.mocked(browserPanel).mock.calls.map(([, a]) => a);
const actions = (kind: BrowserPanelAction["kind"]) => calls().filter((a) => a.kind === kind);
function serve(f: Partial<BrowserPanelFrame> = {}) {
  vi.mocked(browserPanel).mockImplementation(async (_task, action) => (action.kind === "inspect" ? { ...frame, ...f, element } : { ...frame, ...f }));
}
async function mount(f: Partial<BrowserPanelFrame> = {}, props: Partial<Parameters<typeof BrowserPanel>[0]> = {}) {
  serve(f);
  const r = render(<BrowserPanel taskId="task-a" onClose={() => {}} embedded {...props} />);
  await screen.findByAltText(/Main agent browser page/);
  return r;
}

it("maps scaled screenshots to CSS viewport coordinates", () => {
  expect(browserPoint(210, 120, { left: 10, top: 20, width: 400, height: 250 }, frame)).toEqual({ x: 400, y: 200 });
});

it("opens the task's same browser, navigates from the address bar and closes only the modal", async () => {
  serve();
  const close = vi.fn();
  render(<BrowserPanel taskId="task-a" onClose={close} />);
  await screen.findByAltText(/Main agent browser page/);
  expect(browserPanel).toHaveBeenCalledWith("task-a", { kind: "snapshot" });
  const bar = screen.getByLabelText("Browser URL");
  fireEvent.focus(bar);
  fireEvent.change(bar, { target: { value: "example.org" } });
  fireEvent.keyDown(bar, { key: "Enter" });
  await waitFor(() => expect(browserPanel).toHaveBeenCalledWith("task-a", { kind: "navigate", url: "https://example.org/" }));
  fireEvent.click(screen.getByText("Close browser panel"));
  expect(close).toHaveBeenCalledOnce();
});

it("refuses a non-address instead of searching, and refuses other schemes", async () => {
  await mount();
  const bar = screen.getByLabelText("Browser URL");
  for (const typed of ["hello", "javascript:alert(1)"]) {
    fireEvent.focus(bar);
    fireEvent.change(bar, { target: { value: typed } });
    fireEvent.keyDown(bar, { key: "Enter" });
    expect(screen.getByRole("alert").textContent).toMatch(/isn't an address|Only http and https/);
  }
  expect(actions("navigate")).toHaveLength(0);
});

it("suggests visited pages, filtered and keyboard-selectable", async () => {
  await mount({ history: ["https://a.test/docs", "http://localhost:3000/", "http://localhost:5173/"] });
  const bar = screen.getByLabelText("Browser URL");
  fireEvent.focus(bar);
  fireEvent.change(bar, { target: { value: "local" } });
  const list = screen.getByRole("listbox", { name: "Visited pages" });
  expect(within(list).getAllByRole("option").map((o) => o.textContent)).toEqual(["http://localhost:3000/", "http://localhost:5173/"]);
  fireEvent.keyDown(bar, { key: "ArrowDown" });
  fireEvent.keyDown(bar, { key: "ArrowDown" });
  expect(within(list).getAllByRole("option")[1]!.getAttribute("aria-selected")).toBe("true");
  fireEvent.keyDown(bar, { key: "ArrowDown" });
  expect(within(list).getAllByRole("option")[0]!.getAttribute("aria-selected")).toBe("true");
  fireEvent.keyDown(bar, { key: "Enter" });
  await waitFor(() => expect(actions("navigate")).toEqual([{ kind: "navigate", url: "http://localhost:3000/" }]));
  // Escape closes the list first, then gives the address back.
  fireEvent.focus(bar);
  fireEvent.change(bar, { target: { value: "a.test" } });
  fireEvent.keyDown(bar, { key: "Escape" });
  expect(screen.queryByRole("listbox")).toBeNull();
  fireEvent.keyDown(bar, { key: "Escape" });
  expect((bar as HTMLInputElement).value).toBe("https://example.com/");
});

it("drives tabs and history from the strip and toolbar", async () => {
  await mount({ can_back: true, can_forward: false, tabs: [{ id: "t1", url: "https://example.com/", title: "Example", active: true }, { id: "t2", url: "https://docs.test/", title: "Docs", active: false }] });
  const strip = screen.getByRole("tablist", { name: "Browser tabs" });
  const [first, second] = within(strip).getAllByRole("tab");
  expect(first!.getAttribute("aria-selected")).toBe("true");
  fireEvent.click(first!);
  fireEvent.click(second!);
  fireEvent.click(screen.getByRole("button", { name: "Close tab Example" }));
  fireEvent.click(screen.getByRole("button", { name: "New tab" }));
  expect((screen.getByRole("button", { name: "Forward" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Back" }));
  fireEvent.click(screen.getByRole("button", { name: "Reload page" }));
  await waitFor(() => expect(calls().filter((a) => a.kind !== "snapshot")).toEqual([
    { kind: "tab_switch", id: "t2" }, { kind: "tab_close", id: "t1" }, { kind: "tab_new" }, { kind: "back" }, { kind: "refresh" },
  ]));
});

it("forwards clicks, batched typing, special keys and modifiers from the focused view", async () => {
  await mount();
  const view = screen.getByRole("application");
  fireEvent.click(view, { clientX: 100, clientY: 50 });
  view.focus();
  fireEvent.keyDown(view, { key: "h" });
  fireEvent.keyDown(view, { key: "i" });
  fireEvent.keyDown(view, { key: " " });
  fireEvent.keyDown(view, { key: "x" });
  fireEvent.keyDown(view, { key: "Enter" });
  fireEvent.keyDown(view, { key: "ArrowLeft", ctrlKey: true, shiftKey: true });
  fireEvent.keyDown(view, { key: "f", ctrlKey: true });
  await waitFor(() => expect(calls().filter((a) => a.kind !== "snapshot")).toEqual([
    { kind: "click", x: 200, y: 100 },
    { kind: "type", text: "hi x" },
    { kind: "key", key: "Enter" },
    { kind: "key", key: "ArrowLeft", modifiers: { ctrl: true, shift: true } },
  ]));
});

it("shows a pending page dialog and answers it", async () => {
  await mount({ dialog: { kind: "prompt", message: "Your name?", default_prompt: "Ada" } });
  const box = screen.getByRole("group", { name: "Page dialog" });
  expect(box.textContent).toContain("Your name?");
  fireEvent.change(within(box).getByLabelText("Dialog answer"), { target: { value: "Grace" } });
  fireEvent.click(within(box).getByText("Accept"));
  fireEvent.click(within(box).getByText("Dismiss"));
  await waitFor(() => expect(actions("dialog")).toEqual([{ kind: "dialog", accept: true, text: "Grace" }, { kind: "dialog", accept: false }]));
});

it("keeps the last picture while a dialog blocks the page", async () => {
  vi.mocked(browserPanel).mockResolvedValueOnce(frame).mockResolvedValue({ ...frame, png: "", dialog: { kind: "alert", message: "Hi", default_prompt: "" } });
  render(<BrowserPanel taskId="task-a" onClose={() => {}} embedded />);
  await screen.findByAltText(/Main agent browser page/);
  fireEvent.click(screen.getByRole("button", { name: "Reload page" }));
  await screen.findByRole("group", { name: "Page dialog" });
  expect((screen.getByAltText(/Main agent browser page/) as HTMLImageElement).src).toContain("base64,abc");
});

it("says when the agent is using the browser", async () => {
  await mount();
  expect(screen.getByText("You and the agent share this page")).toBeTruthy();
  const tool = { id: "t", kind: "tool", text: "", ts: "", data: { name: "browser", status: "running", input: { action: "click" } } } as unknown as Item;
  act(() => set({ items: { "task-a": [tool] } }));
  expect(screen.getByText(/Agent is using the browser · clicking/)).toBeTruthy();
});

it("surfaces backend errors inline", async () => {
  vi.mocked(browserPanel).mockRejectedValue("The browser plugin is turned off (Settings → Plugins).");
  render(<BrowserPanel taskId="task-a" onClose={() => {}} embedded />);
  expect((await screen.findByRole("alert")).textContent).toContain("turned off");
});

it("reports expansion to the workspace and resets it on leave", async () => {
  const onExpanded = vi.fn();
  const r = await mount({}, { onExpandedChange: onExpanded });
  fireEvent.click(screen.getByRole("button", { name: "Expand browser" }));
  expect(onExpanded).toHaveBeenLastCalledWith(true);
  r.unmount();
  expect(onExpanded).toHaveBeenLastCalledWith(false);
});

it("annotates an element with previewed styles and a region, then sends both", async () => {
  vi.mocked(browserAnnotate).mockResolvedValue();
  await mount();
  fireEvent.click(screen.getByRole("button", { name: "Annotate" }));
  const view = screen.getByRole("application");
  // A click picks the element under the pointer.
  fireEvent.pointerDown(view, { clientX: 100, clientY: 50, button: 0, pointerId: 1 });
  fireEvent.pointerUp(view, { clientX: 100, clientY: 50, button: 0, pointerId: 1 });
  await waitFor(() => expect(actions("inspect")).toEqual([{ kind: "inspect", x: 200, y: 100 }]));
  const note = await screen.findByRole("group", { name: "New annotation" });
  expect(note.textContent).toContain("<h1>");
  expect(note.textContent).toContain("e12");
  fireEvent.change(within(note).getByLabelText("Annotation comment"), { target: { value: "Make the `title`\nbigger" } });
  fireEvent.click(within(note).getByText(/Adjust styles/));
  const size = within(note).getByLabelText("Font size") as HTMLInputElement;
  expect(size.value).toBe("32px");
  // Two quick edits preview once, with the last value.
  fireEvent.change(size, { target: { value: "36px" } });
  fireEvent.change(size, { target: { value: "40px" } });
  await waitFor(() => expect(actions("preview_style")).toEqual([{ kind: "preview_style", selector: "main > h1", styles: [{ property: "font-size", value: "40px" }] }]), { timeout: PREVIEW_DEBOUNCE_MS + 1500 });
  fireEvent.click(within(note).getByText("Add note"));
  // A drag selects a region instead.
  fireEvent.pointerDown(view, { clientX: 10, clientY: 10, button: 0, pointerId: 1 });
  fireEvent.pointerMove(view, { clientX: 110, clientY: 60, pointerId: 1 });
  fireEvent.pointerUp(view, { clientX: 110, clientY: 60, pointerId: 1 });
  const region = await screen.findByRole("group", { name: "New annotation" });
  expect(region.textContent).toContain("Region · 200×100");
  fireEvent.change(within(region).getByLabelText("Annotation comment"), { target: { value: "Too much space" } });
  fireEvent.click(within(region).getByText("Add note"));
  const list = screen.getByRole("group", { name: "Pending annotations" });
  expect(within(list).getAllByRole("listitem")).toHaveLength(2);
  fireEvent.click(within(list).getByText("Send to agent (2)"));
  await waitFor(() => expect(browserAnnotate).toHaveBeenCalledWith("task-a", [
    { comment: "Make the `title`\nbigger", bbox: element.bbox, element: { ref: "e12", selector: "main > h1", tag: "H1", role: "heading", name: "Welcome" }, styles: [{ property: "font-size", before: "32px", after: "40px" }] },
    { comment: "Too much space", bbox: { x: 20, y: 20, width: 200, height: 100 } },
  ]));
  expect((await screen.findByRole("status")).textContent).toBe("Sent · the reply arrives in the chat");
  await waitFor(() => expect(actions("clear_preview")).toHaveLength(1));
  expect(screen.queryByRole("group", { name: "Pending annotations" })).toBeNull();
});

it("keeps annotations when sending fails, and discard clears previews", async () => {
  vi.mocked(browserAnnotate).mockRejectedValue("Annotation 1: it is no longer inside the visible page");
  await mount();
  fireEvent.click(screen.getByRole("button", { name: "Annotate" }));
  const view = screen.getByRole("application");
  fireEvent.pointerDown(view, { clientX: 100, clientY: 50, button: 0, pointerId: 1 });
  fireEvent.pointerUp(view, { clientX: 100, clientY: 50, button: 0, pointerId: 1 });
  const note = await screen.findByRole("group", { name: "New annotation" });
  expect((within(note).getByText("Add note") as HTMLButtonElement).disabled).toBe(true);
  fireEvent.change(within(note).getByLabelText("Annotation comment"), { target: { value: "Bolder" } });
  fireEvent.click(within(note).getByText(/Adjust styles/));
  fireEvent.change(within(note).getByLabelText("Font weight"), { target: { value: "900" } });
  fireEvent.click(within(note).getByText("Add note"));
  await waitFor(() => expect(actions("preview_style")).toEqual([{ kind: "preview_style", selector: "main > h1", styles: [{ property: "font-weight", value: "900" }] }]));
  fireEvent.click(screen.getByText("Send to agent (1)"));
  expect((await screen.findByRole("alert")).textContent).toContain("no longer inside");
  expect(screen.getByRole("group", { name: "Pending annotations" })).toBeTruthy();
  fireEvent.click(screen.getByText("Discard"));
  await waitFor(() => expect(actions("clear_preview")).toHaveLength(1));
  expect(screen.queryByRole("group", { name: "Pending annotations" })).toBeNull();
});
