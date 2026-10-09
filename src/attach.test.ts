/// <reference types="node" />
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";

// The attach flow talks to the backend for every path it is given: the browser
// `File` object has no path in it, so the real one only exists on the Rust side.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args: { paths?: string[] }) => {
    if (cmd === "files_attach") {
      // The backend decides what is an image from the extension, and the
      // frontend takes its word for it.
      return (args.paths ?? []).map((p) => ({ path: p, name: p.split("/").pop()!, size: 2048, isDir: false, image: p.endsWith(".png") }));
    }
    if (cmd === "file_data_url") return "data:image/png;base64,QUJD";
    return null;
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { get, set } from "./store";
import { api, type Settings } from "./api";
import { addFiles, humanBytes, MAX_FILES, MAX_IMAGE, MAX_IMAGES, removeImage, resolveImageRefs, submit } from "./ui/Composer";

describe("the size on an attachment chip", () => {
  // One helper for both the composer and the message editor, so the boundaries
  // are pinned here: a chip that read "1023 B" next to one reading "1024 KB"
  // would mean the two copies had drifted.
  it("steps up exactly where the unit does", () => {
    expect(humanBytes(0)).toBe("0 B");
    expect(humanBytes(1023)).toBe("1023 B");
    expect(humanBytes(1024)).toBe("1 KB");
    expect(humanBytes(1024 * 1024)).toBe("1.0 MB");
    expect(humanBytes(1024 * 1024 * 1024 + 512 * 1024 * 1024)).toBe("1.5 GB");
  });
  it("rounds KB to a whole number and keeps one decimal above it", () => {
    expect(humanBytes(1536)).toBe("2 KB");
    expect(humanBytes(1536 * 1024)).toBe("1.5 MB");
  });
});

describe("attaching files for the agent", () => {
  beforeEach(() => {
    // No DOM in this suite, and `flash` (how these paths report a problem)
    // only needs a timer to schedule the toast clearing.
    vi.stubGlobal("window", { setTimeout: () => 0, clearTimeout: () => {} });
    vi.clearAllMocks();
    set({ attachFiles: {}, attach: {}, toast: null, sessionDrafts: {}, pendingMessages: {} });
  });
  afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });

  it("keeps the real path, not the browser's empty one", async () => {
    await addFiles("new-chat", ["C:/x/report.pdf"]);
    expect(get().attachFiles["new-chat"]).toEqual([
      { path: "C:/x/report.pdf", name: "report.pdf", size: 2048, isDir: false, image: false },
    ]);
    // A non-image is never inlined: the agent gets a path and reads it itself.
    expect(get().attach["new-chat"]).toBeUndefined();
  });

  it("inlines an image too, so the model can actually see it", async () => {
    await addFiles("new-chat", ["C:/x/a.png"]);
    expect(get().attach["new-chat"]).toEqual(["data:image/png;base64,QUJD"]);
    expect(get().attachFiles["new-chat"]![0]!.image).toBe(true);
  });

  it("refuses the same file twice, however the path is spelled", async () => {
    await addFiles("new-chat", ["C:/x/report.pdf"]);
    await addFiles("new-chat", ["c:\\X\\REPORT.PDF"]);
    expect(get().attachFiles["new-chat"]).toHaveLength(1);
  });

  it("caps the list instead of growing without bound", async () => {
    const many = Array.from({ length: MAX_FILES + 5 }, (_, i) => `C:/x/${i}.pdf`);
    await addFiles("new-chat", many);
    expect(get().attachFiles["new-chat"]).toHaveLength(MAX_FILES);
  });

  it("rejects a failed conversion without retaining an unsendable image ref", async () => {
    vi.spyOn(api, "fileDataUrl").mockRejectedValueOnce(new Error("Unreadable image"));
    await addFiles("new-chat", ["C:/x/a.png", "C:/x/report.pdf"]);
    expect(get().attach["new-chat"]).toBeUndefined();
    expect(get().attachFiles["new-chat"]!.map((f) => f.path)).toEqual(["C:/x/report.pdf"]);
    expect(get().toast).toContain("Unreadable image");
    await addFiles("new-chat", ["C:/x/a.png"]);
    expect(get().attach["new-chat"]).toHaveLength(1);
  });

  it("deduplicates repeated paths within a batch and across concurrent drops", async () => {
    await Promise.all([
      addFiles("new-chat", ["C:/x/a.png", "c:\\X\\A.PNG"]),
      addFiles("new-chat", ["C:/x/a.png"]),
    ]);
    expect(get().attachFiles["new-chat"]).toHaveLength(1);
    expect(get().attach["new-chat"]).toHaveLength(1);
  });

  it("does not retain image refs when the shared image cap is reached", async () => {
    set({ attach: { "new-chat": Array(MAX_IMAGES).fill("data:image/png;base64,OLD") } });
    await addFiles("new-chat", ["C:/x/a.png", "C:/x/report.pdf"]);
    expect(get().attach["new-chat"]).toHaveLength(MAX_IMAGES);
    expect(get().attachFiles["new-chat"]!.map((f) => f.name)).toEqual(["report.pdf"]);
    removeImage("new-chat", 0);
    await addFiles("new-chat", ["C:/x/a.png"]);
    expect(get().attachFiles["new-chat"]).toHaveLength(2);
    expect(get().attach["new-chat"]).toHaveLength(MAX_IMAGES);
  });

  it("rejects oversized dropped images like pasted images", async () => {
    vi.spyOn(api, "filesAttach").mockResolvedValueOnce([
      { path: "C:/x/a.png", name: "a.png", size: MAX_IMAGE + 1, image: true, isDir: false },
    ]);
    const read = vi.spyOn(api, "fileDataUrl");
    await addFiles("new-chat", ["C:/x/a.png"]);
    expect(read).not.toHaveBeenCalled();
    expect(get().attachFiles["new-chat"]).toBeUndefined();
    expect(get().toast).toContain("over 5 MB");
  });

  it("reconnects saved image refs by bytes rather than the mixed-file order", async () => {
    set({ attach: { "new-chat": ["data:image/png;base64,PASTE", "data:image/png;base64,QUJD"] }, attachFiles: { "new-chat": [
      { path: "C:/x/report.pdf", name: "report.pdf", size: 1, image: false, isDir: false },
      { path: "C:/x/a.png", name: "a.png", size: 1, image: true, isDir: false },
    ] } });
    await resolveImageRefs("new-chat");
    expect(get().attachFiles["new-chat"]![1]!.imageIndex).toBe(1);
    removeImage("new-chat", 0);
    expect(get().attachFiles["new-chat"]![1]!.imageIndex).toBe(0);
    removeImage("new-chat", 0);
    expect(get().attachFiles["new-chat"]!.map((f) => f.name)).toEqual(["report.pdf"]);
  });

  it("keeps saved image bytes when its original path no longer matches", async () => {
    set({ attach: { "new-chat": ["data:image/png;base64,SAVED"] }, attachFiles: { "new-chat": [
      { path: "C:/x/a.png", name: "a.png", size: 1, image: true, isDir: false },
    ] } });
    await resolveImageRefs("new-chat");
    expect(get().attach["new-chat"]).toEqual(["data:image/png;base64,SAVED"]);
    expect(get().attachFiles["new-chat"]).toEqual([]);
  });

  it("submits images once and only non-image paths in the text for home and session", async () => {
    const send = vi.spyOn(api, "send").mockResolvedValueOnce(undefined);
    const create = vi.spyOn(api, "create").mockRejectedValueOnce(new Error("dummy failure"));
    set({ task: "t1", settings: { project: "C:/x" } as Settings });
    await addFiles("t1", ["C:/x/a.png", "C:/x/report.pdf"]);
    await submit("session");
    expect(send).toHaveBeenCalledWith("t1", expect.stringContaining("- C:/x/report.pdf"), false, ["data:image/png;base64,QUJD"]);
    expect(send.mock.calls[0]![1]).not.toContain("a.png");
    await addFiles("new-chat", ["C:/x/a.png", "C:/x/report.pdf"]);
    await submit("home");
    expect(create).toHaveBeenCalledWith(expect.objectContaining({ images: ["data:image/png;base64,QUJD"], prompt: expect.stringContaining("- C:/x/report.pdf") }));
    expect(create.mock.calls[0]![0].prompt).not.toContain("a.png");
    expect(get().attachFiles["new-chat"]![0]!.imageIndex).toBe(0);
    removeImage("new-chat", 0);
    expect(get().attachFiles["new-chat"]!.map((f) => f.name)).toEqual(["report.pdf"]);
  });

  it("offsets newer image refs when a failed session send restores older images", async () => {
    let reject!: (e: Error) => void;
    vi.spyOn(api, "send").mockImplementationOnce(() => new Promise<void>((_, fail) => { reject = fail; }));
    set({ task: "t1" });
    await addFiles("t1", ["C:/x/old.png"]);
    const pending = submit("session");
    await addFiles("t1", ["C:/x/new.png"]);
    reject(new Error("dummy failure"));
    await pending;
    expect(get().attachFiles.t1!.map((f) => f.imageIndex)).toEqual([0, 1]);
    removeImage("t1", 0);
    expect(get().attachFiles.t1!.map((f) => f.name)).toEqual(["new.png"]);
    expect(get().attachFiles.t1![0]!.imageIndex).toBe(0);
    await addFiles("t1", ["C:/x/old.png"]);
    expect(get().attachFiles.t1).toHaveLength(2);
  });

  it("does nothing for an empty selection", async () => {
    await addFiles("new-chat", []);
    expect(get().attachFiles["new-chat"]).toBeUndefined();
  });
});
