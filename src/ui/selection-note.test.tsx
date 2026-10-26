// @vitest-environment jsdom
// Selecting text in a chat has to offer a note, and the note has to arrive
// somewhere the agent will actually read it — as an annotation sent alongside the
// next message, never as text in the composer. These pin the two halves that are
// easy to get subtly wrong: that the pill waits for a deliberate selection rather
// than firing on every drag that happens to end over a word, and that a note
// reaches the model as a quote of the user's own words with their comment, rather
// than as a bare paste or as an item id the model cannot resolve.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MockInstance } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { api } from "../api";
import { get, set } from "../store";
import { submit } from "./Composer";
import { ANNOTATION_MARKER, ANNOTATION_PREFIX, annotationBlock, Annotations, AttachmentChips, contextBlock, parseAnnotationBlock, parseAttachments, pillPlace, SelectionNoteLayer, SentAnnotationMessage, splitContextBlocks, stagedAttachments, stripHarnessBlocks } from "./SelectionNote";
import type { Note } from "../store";
import type { Item, TaskSummary } from "../api";
import { UserMsg } from "./Session";

/** A selection the browser would leave behind: collapsed, so nothing is picked
 *  up by a stray `getSelection` from another test. */
function clearSelection() {
  window.getSelection()?.removeAllRanges();
}

/** Stand in for the transcript row a selection is made in. Only the two things
 *  the layer reads off it matter: that it is inside a chat, and which chat. */
function inChat(task: string | null, rect: Partial<DOMRect> = {}) {
  const pane = document.createElement("div");
  pane.className = "session-pane";
  if (task) pane.dataset.task = task;
  const row = document.createElement("div");
  row.className = "it";
  row.dataset.item = "i1";
  const box = document.createElement("div");
  row.appendChild(box);
  pane.appendChild(row);
  document.body.append(pane);
  const r = { top: 100, bottom: 140, left: 200, right: 500, width: 300, height: 40, ...rect } as DOMRect;
  const range = {
    rangeCount: 1,
    toString: () => "the guard is a literal-character denylist",
    removeAllRanges: () => {},
    getBoundingClientRect: () => r,
    getRangeAt: () => range,
    commonAncestorContainer: box,
  };
  vi.spyOn(window, "getSelection").mockReturnValue(range as unknown as Selection);
  return { pane, range };
}

/** The pill appears only once the pointer has been still for a beat. */
const settle = () => act(() => { vi.advanceTimersByTime(300); });

describe("a note on selected text", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    // The store is a module singleton, so a note saved by one test is still on the
    // next one's chat — and a "no comment" note would then read as the previous
    // test's commented one.
    set({ ready: true, notes: {}, sessionDrafts: {} });
    render(<div className="app"><SelectionNoteLayer /></div>);
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.restoreAllMocks();
    clearSelection();
  });

  it("offers the note once the selection settles, and not on the way there", () => {
    inChat("t1");

    fireEvent.mouseUp(document.body);
    expect(screen.queryByText("Add to chat"), "a drag that ends mid-word must not offer a note").toBeNull();

    settle();
    expect(screen.getByText("Add to chat")).toBeTruthy();
  });

  it("stays down for a selection outside a chat, which has no composer to add to", () => {
    inChat(null);

    fireEvent.mouseUp(document.body);
    settle();

    expect(screen.queryByText("Add to chat")).toBeNull();
  });

  it("puts the selection and the comment in the chat's annotations, not in the draft", () => {
    inChat("t1");

    fireEvent.mouseUp(document.body);
    settle();
    fireEvent.click(screen.getByText("Add to chat"));
    fireEvent.change(screen.getByPlaceholderText(/optional comment/i), { target: { value: "check the chained case" } });
    fireEvent.click(screen.getByText("Save"));

    expect(get().notes["t1"]).toEqual([
      { id: expect.any(String), item: "i1", text: "the guard is a literal-character denylist", body: "check the chained case" },
    ]);
    // The composer is where the user writes the next message; a note they did not
    // type does not belong in it, and would be sent on its own.
    expect(get().sessionDrafts["t1"] ?? "", "the box must stay the user's own words").toBe("");
  });

  it("keeps a comment-less selection as a bare quote", () => {
    inChat("t1");

    fireEvent.mouseUp(document.body);
    settle();
    fireEvent.click(screen.getByText("Add to chat"));
    fireEvent.click(screen.getByText("Save"));

    expect(get().notes["t1"]?.[0]?.body).toBe("");
    expect(get().notes["t1"]?.[0]?.text).toBe("the guard is a literal-character denylist");
  });

  it("closes on Escape, so the note is a note and not a mode", () => {
    inChat("t1");

    fireEvent.mouseUp(document.body);
    settle();
    expect(screen.getByText("Add to chat")).toBeTruthy();

    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByText("Add to chat")).toBeNull();
  });
});

describe("how annotations reach the agent", () => {
  const note = (over: Partial<Note> = {}): Note => ({ id: "n1", item: "", text: "the guard is a literal denylist", body: "", ...over });

  it("quotes the selection and attributes the comment to it", () => {
    const block = annotationBlock([note({ body: "why is `&&` not on the list" })]);
    expect(block).toContain("the guard is a literal denylist");
    expect(block).toContain("The user says about it: why is `&&` not on the list");
  });

  it("numbers the notes, so one can be talked about without re-quoting it", () => {
    const block = annotationBlock([note({ id: "a" }), note({ id: "b" })]);
    expect(block).toContain("### Note 1");
    expect(block).toContain("### Note 2");
  });

  it("sends the quote verbatim, not as markdown the agent would have to re-read", () => {
    // A selection out of this very app is full of backticks and asterisks; read
    // as markdown they would come back mangled, and quoting exactly is the point.
    const block = annotationBlock([note({ text: "`&&` and **chained**" })]);
    expect(block).toContain("`&&` and **chained**");
  });

  it("says nothing at all when there are no annotations", () => {
    expect(annotationBlock([])).toBe("");
  });
});

/**
 * The send path, which is where a note either becomes useful or quietly vanishes.
 * A note that is never sent is a note the agent cannot see, and one that is not
 * cleared is one that goes out again with the next message — both failures are
 * silent, so both are pinned here.
 */
describe("sending a chat that has annotations", () => {
  // Spied per test, not once at module scope: the pill's own describe restores
  // all mocks between its tests, which would silently unhook a shared spy and
  // leave every send assertion reading a spy nobody called.
  let send: MockInstance<typeof api.send>;
  const withChat = (notes: Note[], draft = "make it lazy") => {
    set((s) => ({
      view: "session",
      task: "t1",
      tasks: { ...s.tasks, t1: { id: "t1", status: "done", model: "anthropic/claude-opus-5" } as never },
      notes: { ...s.notes, t1: notes },
      sessionDrafts: { ...s.sessionDrafts, t1: draft },
      attach: {},
      attachFiles: {},
    }));
  };
  const note = (over: Partial<Note> = {}): Note => ({ id: "n1", item: "", text: "load_tasks reads all 2.4 GB", body: "", ...over });

  beforeEach(() => { send = vi.spyOn(api, "send").mockResolvedValue(undefined); });
  afterEach(() => { send.mockRestore(); });

  it("shows the message immediately while the backend send is still pending", async () => {
    let finish!: () => void;
    send.mockImplementation(() => new Promise<void>((resolve) => { finish = resolve; }));
    withChat([]);

    let sending!: Promise<void>;
    await act(async () => { sending = submit("session"); });
    expect(get().pendingMessages.t1?.map((item) => item.text)).toEqual(["make it lazy"]);
    expect(get().sessionDrafts.t1).toBe("");

    await act(async () => { finish(); await sending; });
    expect(get().pendingMessages.t1).toBeUndefined();
  });

  it("is cleared by a successful send from an existing chat", async () => {
    withChat([]);

    await act(async () => { await submit("session"); });

    expect(get().sessionDrafts.t1).toBe("");
  });

  it("keeps the draft when the send fails", async () => {
    send.mockRejectedValueOnce(new Error("offline"));
    withChat([]);

    await act(async () => { await submit("session"); });

    expect(get().sessionDrafts.t1).toBe("make it lazy");
  });

  it("does not clear a newer draft typed while sending", async () => {
    let finish!: () => void;
    send.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
    withChat([]);

    let sending!: Promise<void>;
    await act(async () => { sending = submit("session"); });
    set((s) => ({ sessionDrafts: { ...s.sessionDrafts, t1: "next question" } }));
    await act(async () => { finish(); await sending; });

    expect(get().sessionDrafts.t1).toBe("next question");
  });

  it.each([false, true])("clears screenshots immediately, including image-only and deferred sends (later=%s)", async (later) => {
    let finish!: () => void;
    send.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
    withChat([], "");
    const image = "data:image/png;base64,AAA";
    set({ attach: { t1: [image] } });

    let sending!: Promise<void>;
    await act(async () => { sending = submit("session", later); });
    expect(send).toHaveBeenCalledWith("t1", "", later, [image], [], ["image-1"]);
    expect(get().attach.t1).toEqual([]);
    expect(get().pendingMessages.t1?.[0]?.data.images).toEqual([image]);

    set({ attach: { t1: ["data:image/png;base64,BBB"] } });
    await act(async () => { finish(); await sending; });
    expect(get().attach.t1).toEqual(["data:image/png;base64,BBB"]);
  });

  it("clears attached files immediately and restores all consumed content on failure", async () => {
    let fail!: (error: Error) => void;
    send.mockImplementationOnce(() => new Promise<void>((_resolve, reject) => { fail = reject; }));
    withChat([]);
    const file = { path: "D:/dummy/notes.txt", name: "notes.txt", size: 10, image: false, isDir: false };
    const image = "data:image/png;base64,AAA";
    set({ attach: { t1: [image] }, attachFiles: { t1: [file] } });

    let sending!: Promise<void>;
    await act(async () => { sending = submit("session"); });
    expect(get().sessionDrafts.t1).toBe("");
    expect(get().attach.t1).toEqual([]);
    expect(get().attachFiles.t1).toEqual([]);

    await act(async () => { fail(new Error("offline")); await sending; });
    expect(get().sessionDrafts.t1).toBe("make it lazy");
    expect(get().attach.t1).toEqual([image]);
    expect(get().attachFiles.t1).toEqual([file]);
  });

  it("preserves a new draft, attachments, and another chat when sending fails", async () => {
    let fail!: (error: Error) => void;
    send.mockImplementationOnce(() => new Promise<void>((_resolve, reject) => { fail = reject; }));
    withChat([]);
    const image = "data:image/png;base64,AAA";
    const nextImage = "data:image/png;base64,BBB";
    const file = { path: "D:/dummy/notes.txt", name: "notes.txt", size: 10, image: false, isDir: false };
    const nextFile = { ...file, path: "D:/dummy/next.txt", name: "next.txt" };
    set({ attach: { t1: [image] }, attachFiles: { t1: [file] } });

    let sending!: Promise<void>;
    await act(async () => { sending = submit("session"); });
    set({ task: "t2", sessionDrafts: { t1: "next question", t2: "other chat" }, attach: { t1: [nextImage] }, attachFiles: { t1: [nextFile] } });
    await act(async () => { fail(new Error("offline")); await sending; });

    expect(get().sessionDrafts).toEqual({ t1: "next question", t2: "other chat" });
    expect(get().attach.t1).toEqual([image, nextImage]);
    expect(get().attachFiles.t1).toEqual([file, nextFile]);
  });

  it("goes out with the message, ahead of it, and the message still reads as the message", async () => {
    withChat([note({ body: "SO THATS WITH THE SLOW STARTUP TIMES?" })]);

    await act(async () => { await submit("session"); });

    const sent = send.mock.calls[0]?.[1] ?? "";
    expect(sent).toContain("load_tasks reads all 2.4 GB");
    expect(sent).toContain("SO THATS WITH THE SLOW STARTUP TIMES?");
    // The typed text has to stay the last word, or the agent reads the annotation
    // as the instruction and the instruction as a footnote.
    expect(sent.endsWith("make it lazy")).toBe(true);
  });

  it("is cleared by the send, so it cannot go out again with the next message", async () => {
    withChat([note()]);

    await act(async () => { await submit("session"); });
    expect(get().notes["t1"]).toEqual([]);

    set((s) => ({ sessionDrafts: { ...s.sessionDrafts, t1: "next question" } }));
    await act(async () => { await submit("session"); });

    expect(send.mock.calls[1]?.[1], "a sent annotation must not be sent twice").toBe("next question");
  });

  it("goes back when the send fails, because it was never sent", async () => {
    send.mockRejectedValueOnce(new Error("offline"));
    withChat([note({ body: "does this matter?" })]);

    await act(async () => { await submit("session"); });

    expect(send).toHaveBeenCalled();
    // Everything the failed send consumed is put back, so a note is not lost to
    // an error it had nothing to do with.
    expect(get().notes["t1"], "a failed send consumed nothing, so the note is still pending").toEqual([
      { id: "n1", item: "", text: "load_tasks reads all 2.4 GB", body: "does this matter?" },
    ]);
    expect(get().pendingMessages.t1).toBeUndefined();
  });

  it("sends annotations without typed text, including queued sends", async () => {
    withChat([note({ body: "check this" })], "   ");
    await act(async () => { await submit("session", true); });
    expect(send).toHaveBeenCalledWith("t1", annotationBlock([note({ body: "check this" })]), true, undefined, [], []);
    expect(get().notes["t1"]).toEqual([]);
  });

  it("leaves a message with no annotations exactly as it was", async () => {
    withChat([]);

    await act(async () => { await submit("session"); });

    expect(send.mock.calls[0]?.[1]).toBe("make it lazy");
  });
});

describe("sent annotations", () => {
  afterEach(cleanup);
  it("hides annotation text until its button is pressed, keeping the message visible", () => {
    const block = annotationBlock([{ id: "n1", item: "", text: "quoted answer", body: "check this" }]);
    render(<SentAnnotationMessage text={`${block}\n\nPlease fix it`} />);
    expect(screen.queryByText(/quoted answer/)).toBeNull();
    expect(screen.getByText("Please fix it")).toBeTruthy();
    const button = screen.getByRole("button", { name: "1 annotation" });
    expect(button.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(button);
    expect(screen.getByText(/quoted answer/)).toBeTruthy();
    fireEvent.click(button);
    expect(screen.queryByText(/quoted answer/)).toBeNull();
  });

  it("renders an annotation-only message as a button", () => {
    render(<SentAnnotationMessage text={annotationBlock([{ id: "n1", item: "", text: "quote", body: "" }])} />);
    expect(screen.getByRole("button", { name: "1 annotation" })).toBeTruthy();
    expect(screen.queryByText("quote")).toBeNull();
  });

  // The field case: expanded, the bubble showed the model-facing markdown —
  // "### Note 1 (on your message)" and "The user says about it:" — verbatim.
  it("shows sent notes as quote and comment cards, not the markdown handed to the model", () => {
    const block = annotationBlock([
      { id: "n1", item: "u1", text: "Still hidden: Comment mode", body: "make those start a new chat" },
      { id: "n2", item: "", text: "second quote", body: "" },
    ]);
    const { container } = render(<SentAnnotationMessage text={`${block}\n\nthanks`} />);
    fireEvent.click(screen.getByRole("button", { name: "2 annotations" }));
    expect(container.textContent).not.toContain("###");
    expect(container.textContent).not.toContain("The user says about it");
    const cards = container.querySelectorAll(".anrow");
    expect(cards.length).toBe(2);
    expect(cards[0]!.querySelector(".anquote")!.textContent).toBe("Still hidden: Comment mode");
    expect(cards[0]!.querySelector(".anbody")!.textContent).toBe("make those start a new chat");
    expect(cards[0]!.querySelector(".anfrom")).toBeTruthy();
    expect(cards[1]!.querySelector(".anbody")).toBeNull();
    expect(cards[1]!.querySelector(".anfrom")).toBeNull();
  });

  it("parses notes back exactly, including quotes that look like headings", () => {
    const notes = [
      { id: "a", item: "", text: "line one\n\n### Note 7\nstill quoted", body: "multi\n\nline comment" },
      { id: "b", item: "x", text: "plain", body: "" },
    ];
    const block = annotationBlock(notes);
    const inner = block.slice(ANNOTATION_PREFIX.length, block.indexOf(ANNOTATION_MARKER));
    expect(parseAnnotationBlock(inner)).toEqual([
      { text: "line one\n\n### Note 7\nstill quoted", body: "multi\n\nline comment", mine: false },
      { text: "plain", body: "", mine: true },
    ]);
    expect(parseAnnotationBlock("not a note block")).toBeNull();
  });
});

// A staged attachment note is written for the model: it names a temp copy in
// OpenLeash's hidden data folder, which is not a path the user chose and tells
// them nothing they can act on. The user's own words are all that should show.
describe("stripHarnessBlocks", () => {
  const note = "- `C:\\Users\\me\\AppData\\Local\\Temp\\OpenLeash\\proj-abc\\task-1\\batch\\image.png`";
  const wrapper = `\n\n<openleash-attachments>\nThe user sent these attachments. OpenLeash saved copies in its temporary attachment folder.\n${note}\n</openleash-attachments>`;

  it("removes the staged-path wrapper wherever it sits", () => {
    expect(stripHarnessBlocks(`look at this${wrapper}`)).toBe("look at this");
    expect(stripHarnessBlocks(`${wrapper.slice(2)}\n\nand then this`)).toBe("and then this");
    expect(stripHarnessBlocks(`before${wrapper}after`)).toBe("beforeafter");
    expect(stripHarnessBlocks("no wrapper here")).toBe("no wrapper here");
    // Unterminated: not a block, so leave it alone rather than eating text.
    expect(stripHarnessBlocks("<openleash-attachments>trailing")).toBe("<openleash-attachments>trailing");
  });

  it("removes the attached-files list only when those files are staged", () => {
    const list = "Attached files (read these with your tools, they are not inlined):\n- a.rs\n- b.rs";
    const data = { attachments: [{ path: "x", name: "a.rs", size: 1 }] };
    expect(stripHarnessBlocks(`here\n\n${list}`, data)).toBe("here");
    // Written inline with nothing staged stays as the message it is.
    expect(stripHarnessBlocks(`here\n\n${list}`)).toBe(`here\n\n${list}`);
  });
});

// The staged block is scaffolding the *agent* reads, not something the user
// should ever see as raw tags. It was rendered verbatim, which looked like a
// stray piece of markup rather than part of the app.
describe("the staged block becomes a chip list", () => {
  const note = "- `C:\\Users\\me\\AppData\\Local\\Temp\\OpenLeash\\proj-abc\\task-1\\batch\\01-image-1.png`";
  const wrapper = `\n\n<openleash-attachments>\nThe user sent these attachments. OpenLeash saved copies in its temporary attachment folder.\n${note}\n</openleash-attachments>`;

  it("shows the name the user chose, not the temp path it was copied to", () => {
    const files = parseAttachments(wrapper);
    expect(files).toHaveLength(1);
    // `01-image-1.png`, never the whole `.../Temp/OpenLeash/...` path.
    expect(files[0]!.name).toBe("01-image-1.png");
    expect(files[0]!.name).not.toContain("Temp");
    expect(files[0]!.name).not.toContain("OpenLeash");
  });

  it("takes every entry, so a multi-file message lists them all", () => {
    const many = `<openleash-attachments>\n- \`/tmp/a/one.rs\`\n- \`/tmp/a/two.rs\`\n</openleash-attachments>`;
    expect(parseAttachments(many).map(f => f.name)).toEqual(["one.rs", "two.rs"]);
  });

  it("renders nothing when there is no block at all", () => {
    expect(parseAttachments("just a plain message")).toEqual([]);
    // Unterminated is not a block, so it must not render a half-parsed card.
    expect(parseAttachments("<openleash-attachments>trailing")).toEqual([]);
    const { container } = render(<AttachmentChips text="plain" />);
    expect(container.querySelector(".ubatchip")).toBeNull();
  });

  it("renders one chip per file, with the stripped message beside it", () => {
    const { container } = render(<AttachmentChips text={`look at this${wrapper}`} />);
    const chips = container.querySelectorAll(".ubatchip");
    expect(chips).toHaveLength(1);
    expect(chips[0]!.textContent).toContain("image-1.png");
  });
});

// The backend keeps the staged-path note out of the visible text (it goes to the
// model only) and records the copies on the item's `attachments` field instead:
// images first, then files. A bubble that only parsed the text showed no files.
describe("a sent message's attachments come from the item", () => {
  const png = "data:image/png;base64,AAA";
  const staged = [
    { path: "/tmp/ol/b/01-shot.png", name: "shot.png", size: 3 },
    { path: "/tmp/ol/b/02-report.pdf", name: "report.pdf", size: 2048 },
  ];
  const task = { id: "t1", status: "idle" } as unknown as TaskSummary;
  const item = (data: Record<string, unknown>) => ({ id: "u1", kind: "user", text: "look at these", data, ts: new Date().toISOString() }) as Item;

  it("shows a chip for each file, and none for a picture already shown as a thumbnail", () => {
    const { container } = render(<UserMsg it={item({ images: [png], attachments: staged })} task={task} />);
    const chips = [...container.querySelectorAll(".ubatchip")].map((c) => c.textContent);
    expect(chips).toHaveLength(1);
    expect(chips[0]).toContain("report.pdf");
    expect(chips[0]).toContain("2 KB");
    expect(container.querySelector(".ubimgs img, .ubimgs [style]"), "the picture stays a thumbnail").toBeTruthy();
    // The bubble never shows the temp path, only the name the user chose.
    expect(container.textContent).not.toContain("/tmp/ol");
  });

  it("names a picture the timeline has dropped, so the bubble still says it went out", async () => {
    // The copy cannot be read here (the backend refuses it), so the picture is
    // named rather than drawn — but it is still a picture, not a file.
    vi.spyOn(api, "stagedImageUrl").mockRejectedValue(new Error("gone"));
    const only = [
      { path: "/tmp/ol/gone/01-shot.png", name: "shot.png", size: 3 },
      { path: "/tmp/ol/gone/02-report.pdf", name: "report.pdf", size: 2048 },
    ];
    const { container } = render(<UserMsg it={item({ images: [], images_dropped: 1, attachments: only })} task={task} />);
    // The read settles before the picture can be named, so the chip comes later.
    await waitFor(() => expect(container.querySelectorAll(".ubatchip").length).toBe(2));
    const chips = [...container.querySelectorAll(".ubatchip")].map((c) => c.textContent ?? "");
    expect(chips.map((c) => c.replace(/\d.*$/, ""))).toEqual(["shot.png", "report.pdf"]);
  });

  it("draws a picture the timeline has dropped, by reading its staged copy back", async () => {
    // The blob was dropped to bound memory, but the copy the backend staged is
    // still on disk: the picture the user sent is read back and shown as a
    // picture, not as a paperclip chip with a size.
    const url = "data:image/png;base64,BBB";
    vi.spyOn(api, "stagedImageUrl").mockResolvedValue(url);
    const only = [
      { path: "/tmp/ol/read/01-shot.png", name: "shot.png", size: 3 },
      { path: "/tmp/ol/read/02-report.pdf", name: "report.pdf", size: 2048 },
    ];
    const { container } = render(<UserMsg it={item({ images: [], images_dropped: 1, attachments: only })} task={task} />);
    await waitFor(() => expect(container.querySelector(".ubimgs img")).toBeTruthy());
    const img = container.querySelector<HTMLImageElement>(".ubimgs img")!;
    expect(img.getAttribute("src")).toBe(url);
    // Only the real file is a chip now; the picture is not.
    const chips = [...container.querySelectorAll(".ubatchip")].map((c) => c.textContent ?? "");
    expect(chips.map((c) => c.replace(/\d.*$/, ""))).toEqual(["report.pdf"]);
  });

  it("draws a dropped picture from the recorded count, even with no blob count at all", async () => {
    // A message saved after the count existed records how many of its copies
    // are pictures, so the split does not depend on the blobs being there.
    const url = "data:image/png;base64,CCC";
    vi.spyOn(api, "stagedImageUrl").mockResolvedValue(url);
    const only = [{ path: "/tmp/ol/count/01-shot.png", name: "shot.png", size: 3 }];
    const { container } = render(<UserMsg it={item({ images: [], image_attachments: 1, attachments: only })} task={task} />);
    await waitFor(() => expect(container.querySelector(".ubimgs img")).toBeTruthy());
    expect(container.querySelector(".ubatchip"), "the picture is not a file chip").toBeNull();
  });

  it("still reads the old in-text note for messages saved before the field existed", () => {
    const old = `look\n\n<openleash-attachments>\nnote\n- \`/tmp/a/old.rs\`\n</openleash-attachments>`;
    const { container } = render(<AttachmentChips text={old} data={{}} />);
    expect(container.querySelector(".ubatchip")?.textContent).toContain("old.rs");
  });

  it("an edit resends each picture once, as an image, and the files by their staged path", async () => {
    const send = vi.spyOn(api, "send").mockResolvedValue(undefined);
    vi.spyOn(api, "rewind").mockResolvedValue("");
    vi.spyOn(api, "task").mockResolvedValue({ summary: task, items: [] } as never);
    const { container } = render(<UserMsg it={item({ images: [png], attachments: staged })} task={task} />);
    const view = within(container);
    fireEvent.click(view.getByText("Edit"));
    // The editor lists the file, not the picture's staged copy a second time.
    expect(container.querySelectorAll(".filechip")).toHaveLength(1);
    await act(async () => { fireEvent.click(view.getByText("Send")); });
    expect(send).toHaveBeenCalledWith("t1", "look at these", false, [png], ["/tmp/ol/b/02-report.pdf"], ["shot.png"]);
  });
});

// The split between a message's pictures and its other files is what decides
// whether a picture is drawn or shown as a paperclip chip. It used to be
// guessed from the blobs still on the item, which is zero once the transcript
// has dropped them — and a picture the user sent became a file with a size.
describe("splitting a message's staged copies into pictures and files", () => {
  const staged = [
    { path: "/tmp/ol/b/01-shot.png", name: "shot.png", size: 3 },
    { path: "/tmp/ol/b/02-report.pdf", name: "report.pdf", size: 2048 },
  ];

  it("uses the recorded count, so a dropped picture is still a picture", () => {
    const { images, files } = stagedAttachments({ images: [], image_attachments: 1, attachments: staged });
    expect(images.map((a) => a.name)).toEqual(["shot.png"]);
    expect(files.map((a) => a.name)).toEqual(["report.pdf"]);
  });

  it("falls back to the blobs plus the ones that went, for messages saved before the count existed", () => {
    const { images, files } = stagedAttachments({ images: [], images_dropped: 1, attachments: staged });
    expect(images.map((a) => a.name)).toEqual(["shot.png"]);
    expect(files.map((a) => a.name)).toEqual(["report.pdf"]);
  });

  it("counts a message that kept its blobs as they are", () => {
    const { images, files } = stagedAttachments({ images: ["data:image/png;base64,AAA"], attachments: staged });
    expect(images.map((a) => a.name)).toEqual(["shot.png"]);
    expect(files.map((a) => a.name)).toEqual(["report.pdf"]);
  });

  it("treats every copy as a file when the message carried no pictures", () => {
    const { images, files } = stagedAttachments({ attachments: staged });
    expect(images).toEqual([]);
    expect(files.map((a) => a.name)).toEqual(["shot.png", "report.pdf"]);
  });
});

describe("where the annotations sit", () => {
  it("gives the chip its own surface, so the transcript cannot read through it", () => {
    // `.cfloat` paints a transparent-to-opaque gradient over its whole height and
    // its top is still see-through. A chip with no background of its own sat on
    // the agent's own words and both were readable at once, which is what made it
    // look like a wall of text. This is the regression, pinned in the markup.
    set((s) => ({ notes: { ...s.notes, t1: [{ id: "n1", item: "", text: "load_tasks reads 2.4 GB", body: "" }] } }));
    const { container } = render(<div className="cfloat"><Annotations chat="t1" /></div>);

    const chip = container.querySelector(".anchip");
    expect(chip, "the collapsed chip is what floats over the transcript").toBeTruthy();
    expect(chip?.closest(".annots"), "the chip must sit inside the padded surface").toBeTruthy();
    // The surface is `.cfloat > .annots` — a direct child, so it inherits the
    // gradient's own padding and width rather than escaping it.
    expect(chip?.closest(".annots")?.parentElement?.classList.contains("cfloat")).toBe(true);
  });

  it("says how many there are, so the row is a count and not a label", () => {
    set((s) => ({ notes: { ...s.notes, t1: [
      { id: "n1", item: "", text: "one", body: "" },
      { id: "n2", item: "", text: "two", body: "" },
      { id: "n3", item: "", text: "three", body: "" },
    ] } }));
    const { container } = render(<div className="cfloat"><Annotations chat="t1" /></div>);

    expect(container.querySelector(".anchip")?.textContent).toContain("3 annotations");
  });

  it("drops one annotation without touching the others", () => {
    set((s) => ({ notes: { ...s.notes, t1: [
      { id: "n1", item: "", text: "one", body: "" },
      { id: "n2", item: "", text: "two", body: "why" },
    ] } }));
    const { container } = render(<div className="cfloat"><Annotations chat="t1" /></div>);

    fireEvent.click(container.querySelector(".anchip")!);
    expect(container.querySelectorAll(".anrow")).toHaveLength(2);

    fireEvent.click(container.querySelectorAll(".an-x")[0]!.closest("button")!);
    expect(get().notes["t1"]!.map((n) => n.id)).toEqual(["n2"]);
  });

  it("shows nothing at all for a chat with no annotations", () => {
    set({ notes: {} });
    const { container } = render(<div className="cfloat"><Annotations chat="t1" /></div>);
    expect(container.querySelector(".anchip")).toBeNull();
  });
});

describe("where the pill sits", () => {
  it("puts the pill under a selection with room below it", () => {
    const at = pillPlace({ top: 100, bottom: 140, left: 200, width: 300 }, 150, 1);
    expect(at).toEqual({ top: 148, left: 275 });
  });

  it("turns the pill above a selection at the bottom of the window", () => {
    const at = pillPlace({ top: 880, bottom: 900, left: 200, width: 300 }, 150, 1);
    expect(at.top, "a pill under the last line of a chat is a pill nobody sees").toBeLessThan(880);
  });

  it("keeps the pill on screen at the right edge", () => {
    const at = pillPlace({ top: 100, bottom: 140, left: 1400, width: 200 }, 150, 1);
    expect(at.left).toBe(window.innerWidth - 150 - 8);
  });

  it("converts the selection's pixels through the app zoom, since the layer carries it", () => {
    // At 200% a selection spanning x 400-600 and ending at y 600 sits in the
    // zoomed layer at a centre of x 250 and a bottom of y 300 — and the pill
    // hangs 150 wide, so its left edge is 175.
    const at = pillPlace({ top: 560, bottom: 600, left: 400, width: 200 }, 150, 2);
    expect(at.top).toBe(308);
    expect(at.left).toBe(175);
  });
});

describe("context the app adds for the agent alone", () => {
  afterEach(() => { cleanup(); vi.restoreAllMocks(); });
  it("stays out of the user's bubble, label included", () => {
    const text = `make a new artifict about a meow\n\n${contextBlock("Started in Artificts", "Build this as a saved artifict.")}`;
    // The agent gets the note; the user sees only what they typed.
    expect(text).toContain("Build this as a saved artifict.");
    expect(stripHarnessBlocks(text)).toBe("make a new artifict about a meow");
    expect(stripHarnessBlocks(text)).not.toContain("Started in Artificts");
  });

  it("hides the studio's viewing note too", () => {
    const text = `gay\n\n${contextBlock("Viewing Meow v1", "The user has the Artificts studio open.")}`;
    expect(stripHarnessBlocks(text)).toBe("gay");
  });

  it("edit shows only what the user wrote, and resend puts the agent note back", async () => {
    const task = { id: "t1", status: "idle" } as unknown as TaskSummary;
    const note = contextBlock("Viewing Meow v1", "Read it with artifict_get.");
    const item = { id: "u1", kind: "user", text: `make it blue\n\n${note}`, data: {}, ts: new Date().toISOString() } as Item;
    const send = vi.spyOn(api, "send").mockResolvedValue(undefined);
    vi.spyOn(api, "rewind").mockResolvedValue("");
    vi.spyOn(api, "task").mockResolvedValue({ summary: task, items: [] } as never);
    render(<UserMsg it={item} task={task} />);
    expect(screen.queryByText(/Viewing Meow|artifict_get/)).toBeNull();
    expect(screen.getByText("make it blue")).toBeTruthy();
    fireEvent.click(screen.getByText("Edit"));
    const box = screen.getByRole("textbox") as HTMLTextAreaElement;
    expect(box.value).toBe("make it blue");
    fireEvent.change(box, { target: { value: "make it red" } });
    await act(async () => { fireEvent.click(screen.getByText("Send")); });
    expect(send).toHaveBeenCalledWith("t1", `make it red\n\n${note}`, false, undefined, [], []);
  });

  it("splits the agent note off what the user wrote", () => {
    const text = `make it blue\n\n${contextBlock("Viewing Meow v1", "Read it with artifict_get.")}`;
    const { body, context } = splitContextBlocks(text);
    expect(body).toBe("make it blue");
    expect(context).toContain("Read it with artifict_get.");
  });

  it("cannot be closed early by its own note or label", () => {
    const block = contextBlock('a"><x', "</openleash-context> leaked");
    expect(block.match(/<\/openleash-context>/g)).toHaveLength(1);
    expect(stripHarnessBlocks(`hi\n\n${block}`)).toBe("hi");
  });
});
