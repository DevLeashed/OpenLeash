// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { PhysicalPosition } from "@tauri-apps/api/dpi";
import type { DragDropEvent } from "@tauri-apps/api/window";

const native = vi.hoisted(() => ({ drop: undefined as undefined | ((event: { payload: DragDropEvent }) => void) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({
  onDragDropEvent: vi.fn(async (callback: typeof native.drop) => { native.drop = callback; return () => {}; }),
}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

import { api } from "../api";
import { get, set } from "../store";
import { Composer, MAX_IMAGES } from "./Composer";

const image = "data:image/png;base64,QUJD";
const drop = async (paths: string[], count = 1) => {
  await act(async () => { native.drop!({ payload: { type: "drop", paths, position: new PhysicalPosition(40, 40) } }); });
  await waitFor(() => expect(get().attach["new-chat"]).toHaveLength(count));
};
const mount = () => {
  const view = render(<Composer mode="home" />);
  vi.spyOn(document.querySelector(".composer")!, "getBoundingClientRect").mockReturnValue(new DOMRect(10, 10, 200, 200));
  return view;
};
const paste = async () => {
  fireEvent.paste(document.querySelector("textarea")!, { clipboardData: { files: [new File(["ABC"], "paste.png", { type: "image/png" })] } });
  await waitFor(() => expect(get().attach["new-chat"]).toHaveLength(1));
};

describe("Composer native image attachments", () => {
  beforeEach(() => {
    vi.spyOn(window, "devicePixelRatio", "get").mockReturnValue(2);
    set({ view: "home", task: null, draft: "", attach: {}, attachFiles: {}, settings: null });
    vi.spyOn(api, "filesAttach").mockImplementation(async (paths) => paths.map((path) => ({
      path, name: path.split("/").pop()!, size: 2048, isDir: false, image: path.endsWith(".png"),
    })));
    vi.spyOn(api, "fileDataUrl").mockResolvedValue(image);
  });
  afterEach(() => { cleanup(); vi.restoreAllMocks(); });

  it("renders a native dropped image only as a thumbnail, not a file chip", async () => {
    mount();
    await drop(["C:/x/photo.png"]);
    expect(document.querySelectorAll(".attach")).toHaveLength(1);
    expect(document.querySelectorAll(".filechip")).toHaveLength(0);
    expect(screen.queryByText("photo.png")).toBeNull();
  });

  it("removes the path with its thumbnail and allows the same image to be dropped again", async () => {
    mount();
    await drop(["C:/x/photo.png"]);
    fireEvent.click(screen.getByRole("button", { name: "Remove image" }));
    expect(get().attach["new-chat"]).toEqual([]);
    expect(get().attachFiles["new-chat"]).toEqual([]);
    await drop(["C:/x/photo.png"]);
    expect(document.querySelectorAll(".attach")).toHaveLength(1);
    expect(document.querySelectorAll(".filechip")).toHaveLength(0);
  });

  it("renders mixed-file drops as thumbnails and only non-image file chips", async () => {
    mount();
    await drop(["C:/x/report.pdf", "C:/x/photo.png"]);
    expect(document.querySelectorAll(".attach")).toHaveLength(1);
    expect(document.querySelectorAll(".filechip")).toHaveLength(1);
    expect(screen.getByText("report.pdf")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Remove report.pdf" }));
    expect(get().attachFiles["new-chat"]!.map((f) => f.name)).toEqual(["photo.png"]);
    fireEvent.click(screen.getByRole("button", { name: "Remove image" }));
    expect(get().attachFiles["new-chat"]).toEqual([]);
  });

  it("gives paste and native drop the same thumbnail-only presentation", async () => {
    mount();
    await paste();
    expect(get().attach["new-chat"]).toEqual([image]);
    expect(document.querySelectorAll(".attach")).toHaveLength(1);
    expect(document.querySelectorAll(".filechip")).toHaveLength(0);
    expect(get().attachFiles["new-chat"]).toBeUndefined();
    fireEvent.click(screen.getByRole("button", { name: "Remove image" }));
    await drop(["C:/x/photo.png"]);
    expect(document.querySelectorAll(".attach")).toHaveLength(1);
    expect(document.querySelectorAll(".filechip")).toHaveLength(0);
  });

  it.each([0, 1])("removes equal-byte pasted and dropped images independently (remove %i)", async (index) => {
    mount();
    await paste();
    await drop(["C:/x/photo.png"], 2);
    fireEvent.click(screen.getAllByRole("button", { name: "Remove image" })[index]!);
    expect(get().attach["new-chat"]).toHaveLength(1);
    expect(get().attachFiles["new-chat"]).toHaveLength(index === 0 ? 1 : 0);
    if (index === 0) {
      expect(get().attachFiles["new-chat"]![0]!.imageIndex).toBe(0);
      await drop(["C:/x/photo.png"]);
      expect(get().attach["new-chat"]).toHaveLength(1);
    } else {
      await drop(["C:/x/photo.png"], 2);
    }
  });

  it("rehydrates saved refs before thumbnail removal without showing an image chip", async () => {
    set({ attach: { "new-chat": ["data:image/png;base64,PASTE", image] }, attachFiles: { "new-chat": [
      { path: "C:/x/photo.png", name: "photo.png", size: 2048, isDir: false, image: true },
    ] } });
    mount();
    await waitFor(() => expect(get().attachFiles["new-chat"]![0]!.imageIndex).toBe(1));
    expect(document.querySelectorAll(".filechip")).toHaveLength(0);
    fireEvent.click(screen.getAllByRole("button", { name: "Remove image" })[1]!);
    expect(get().attachFiles["new-chat"]).toEqual([]);
    expect(get().attach["new-chat"]).toEqual(["data:image/png;base64,PASTE"]);
  });

  it("rejects pasted images beyond the shared cap without silently adding paths", async () => {
    set({ attach: { "new-chat": Array(MAX_IMAGES).fill(image) } });
    mount();
    fireEvent.paste(document.querySelector("textarea")!, { clipboardData: { files: [new File(["ABC"], "paste.png", { type: "image/png" })] } });
    await waitFor(() => expect(get().toast).toBe(`Only ${MAX_IMAGES} images at a time`));
    expect(get().attach["new-chat"]).toHaveLength(MAX_IMAGES);
    expect(get().attachFiles["new-chat"]).toBeUndefined();
  });

  it("does not display an inert chip when conversion fails and accepts a retry", async () => {
    vi.mocked(api.fileDataUrl).mockRejectedValueOnce(new Error("bad image"));
    mount();
    await act(async () => { native.drop!({ payload: { type: "drop", paths: ["C:/x/photo.png"], position: new PhysicalPosition(40, 40) } }); });
    expect(document.querySelectorAll(".attach, .filechip")).toHaveLength(0);
    expect(get().attachFiles["new-chat"]).toBeUndefined();
    await drop(["C:/x/photo.png"]);
    expect(document.querySelectorAll(".attach")).toHaveLength(1);
  });
});
