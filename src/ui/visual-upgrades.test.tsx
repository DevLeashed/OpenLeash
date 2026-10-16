// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { commandReceipt } from "./CommandReceipt";
import { ImageExplanation, parseImageSpec } from "./ImageExplanation";
import { AssistantMarkdown, Visualization } from "./Visualization";
import { LightboxProvider, Thumb } from "./primitives/Lightbox";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn().mockResolvedValue("preview"), convertFileSrc: (id: string) => id }));
afterEach(cleanup);
describe("visual and receipt upgrades", () => {
  it("never treats an absent or interrupted exit as a passing receipt", () => {
    expect(commandReceipt({ name: "bash", status: "done" })?.label).toContain("No recorded");
    expect(commandReceipt({ name: "bash", status: "done", meta: "exit 0" })?.detail).toContain("not test acceptance");
    expect(commandReceipt({ name: "bash", meta: "timed out" })?.label).toBe("Command incomplete");
    expect(commandReceipt({ name: "bash", bg_id: "bg1" })).toBeNull();
    expect(commandReceipt({ name: "bash", status: "running", exec: true })?.label).toBe("Command running");
  });
  it("validates image explanations without accepting script or file URLs", () => {
    for (const src of ["javascript:alert(1)", "file:///secret", "http://localhost/image", "data:text/html,hello"]) expect(parseImageSpec(JSON.stringify({ src }))).toBeNull();
    expect(parseImageSpec(JSON.stringify({ src: "https://example.org/a.png", marks: [{ x: 101, y: 0, label: "bad" }] }))).toBeNull();
    expect(parseImageSpec(JSON.stringify({ src: "https://example.org/a.png", marks: [{ x: 50, y: 10, label: "Head" }] }))?.marks).toHaveLength(1);
    // Agents show their own screenshots by path rather than pasting base64.
    for (const src of ["out/shot.png", "C:/Users/me/AppData/Local/Temp/shot.jpg", "/tmp/a.webp", "file:///C:/x/b.png"]) expect(parseImageSpec(JSON.stringify({ src }))?.src).toBe(src);
  });
  it("renders annotations and keyboard-adjustable before/after split without an iframe", () => {
    const source = JSON.stringify({ title: "Comparison", src: "https://example.org/a.png", after: "https://example.org/b.png", marks: [{ x: 10, y: 20, label: "Changed area" }] });
    render(<AssistantMarkdown text={`before\n\n\`\`\`openleash-image\n${source}\n\`\`\`\n\nafter`} />);
    expect(screen.getByText("Changed area")).toBeTruthy();
    expect(document.querySelector("iframe")).toBeNull();
    fireEvent.change(screen.getByRole("slider"), { target: { value: "70" } });
    expect(screen.getByAltText("After").getAttribute("style")).toContain("70%");
    expect(screen.getByAltText("Before").getAttribute("referrerpolicy")).toBe("no-referrer");
  });
  it("reports invalid image JSON", () => {
    render(<ImageExplanation source="invalid" />);
    expect(screen.getByRole("status").textContent).toContain("Invalid image explanation");
  });
  it("resizes previews without replacing the iframe", async () => {
    render(<Visualization source="<p>Hi</p>" autoStart />);
    const frame = await screen.findByTitle("Interactive visualization preview");
    fireEvent.change(screen.getByRole("combobox", { name: "Preview height" }), { target: { value: "720" } });
    fireEvent.change(screen.getByRole("combobox", { name: "Preview width" }), { target: { value: "375px" } });
    expect(screen.getByTitle("Interactive visualization preview")).toBe(frame);
    expect(frame.getAttribute("style")).toContain("720px");
    expect(frame.getAttribute("style")).toContain("375px");
  });
  it("focuses and traps the lightbox then restores the opener", () => {
    render(<LightboxProvider><Thumb src="https://example.org/a.png" alt="Picture" /></LightboxProvider>);
    const opener = screen.getByRole("button", { name: "Picture" });
    opener.focus(); fireEvent.keyDown(opener, { key: "Enter" });
    const close = screen.getByRole("button", { name: "Close" });
    expect(document.activeElement).toBe(close);
    fireEvent.keyDown(close, { key: "Tab" }); expect(document.activeElement).toBe(close);
    fireEvent.keyDown(close, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull(); expect(document.activeElement).toBe(opener);
  });
});
