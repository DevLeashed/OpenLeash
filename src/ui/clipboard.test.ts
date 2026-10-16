// @vitest-environment jsdom
// The remote-control UI is served over plain `http://` on the LAN, where
// `navigator.clipboard` does not exist. The old form
// `navigator.clipboard?.writeText(t).then(...)` threw on the `.then`, so every
// copy button on a paired phone or laptop did nothing at all — no copy and no
// error message. These pin the fallback and, just as importantly, that a real
// failure is reported rather than silently swallowed.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { copyText } from "./clipboard";
import { get, set } from "../store";

const original = { clipboard: Object.getOwnPropertyDescriptor(navigator, "clipboard"), exec: (document as unknown as { execCommand?: unknown }).execCommand };

function setClipboard(value: unknown) {
  Object.defineProperty(navigator, "clipboard", { configurable: true, value });
}
function setExecCommand(fn: unknown) {
  Object.defineProperty(document, "execCommand", { configurable: true, writable: true, value: fn });
}

beforeEach(() => { set({ toast: "" }); });
afterEach(() => {
  if (original.clipboard) Object.defineProperty(navigator, "clipboard", original.clipboard);
  else delete (navigator as unknown as { clipboard?: unknown }).clipboard;
  setExecCommand(original.exec);
  vi.restoreAllMocks();
});

describe("copying on a device without the async clipboard", () => {
  it("falls back to a selection copy instead of doing nothing", () => {
    setClipboard(undefined);
    const exec = vi.fn(() => true);
    setExecCommand(exec);
    expect(copyText("Hello", { what: "Copied!" })).toBe(true);
    expect(exec).toHaveBeenCalledWith("copy");
    expect(get().toast).toBe("Copied!");
  });

  it("carries the exact text into the fallback field, and cleans up after itself", () => {
    setClipboard(undefined);
    let seen = "";
    setExecCommand(() => {
      seen = (document.querySelector("textarea") as HTMLTextAreaElement).value;
      return true;
    });
    copyText("tailscale serve --bg http://127.0.0.1:4411");
    expect(seen).toBe("tailscale serve --bg http://127.0.0.1:4411");
    // The temporary field must not be left in the document.
    expect(document.querySelector("textarea")).toBeNull();
  });

  it("says so when the device refuses the copy too, rather than staying silent", () => {
    setClipboard(undefined);
    setExecCommand(() => false);
    expect(copyText("Hello", { failure: "Couldn't copy" })).toBe(false);
    expect(get().toast).toBe("Couldn't copy");
  });

  it("survives a browser that throws from execCommand", () => {
    setClipboard(undefined);
    setExecCommand(() => { throw new Error("not allowed"); });
    expect(copyText("Hello")).toBe(false);
    expect(document.querySelector("textarea")).toBeNull();
  });
});

describe("copying with the async clipboard available", () => {
  it("prefers it, and reports success", async () => {
    const writeText = vi.fn(async () => {});
    setClipboard({ writeText });
    setExecCommand(() => { throw new Error("must not be needed"); });
    copyText("Hello", { what: "Copied" });
    expect(writeText).toHaveBeenCalledWith("Hello");
    await Promise.resolve();
    expect(get().toast).toBe("Copied");
  });

  it("still copies via the fallback when the API exists but rejects", async () => {
    // Safari exposes the API and then refuses without focus or permission.
    setClipboard({ writeText: vi.fn(async () => { throw new Error("denied"); }) });
    const exec = vi.fn(() => true);
    setExecCommand(exec);
    copyText("Hello");
    await vi.waitFor(() => expect(exec).toHaveBeenCalledWith("copy"));
    expect(get().toast).toBe("Copied");
  });

  it("reports the outcome to a button that shows its own state", async () => {
    setClipboard({ writeText: vi.fn(async () => {}) });
    const onResult = vi.fn();
    copyText("Hello", { onResult });
    await vi.waitFor(() => expect(onResult).toHaveBeenCalledWith(true));
    // A refused copy must not leave a button stuck on "Copied".
    setClipboard({ writeText: vi.fn(async () => { throw new Error("denied"); }) });
    setExecCommand(() => false);
    const failed = vi.fn();
    copyText("Hello", { onResult: failed });
    await vi.waitFor(() => expect(failed).toHaveBeenCalledWith(false));
  });
});
