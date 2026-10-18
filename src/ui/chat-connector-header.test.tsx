// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MessageCircle } from "lucide-react";
import { ChatConnectorHeader, connectorPill } from "./ChatConnectorHeader";

afterEach(cleanup);

it("derives the pill from backend state, red for anything that needs the user", () => {
  expect(connectorPill(null, false)).toEqual({ label: "Checking", tone: "off" });
  expect(connectorPill({ enabled: false, state: "off" }, false).label).toBe("Not set up");
  expect(connectorPill({ enabled: false, state: "error" }, true)).toEqual({ label: "Off", tone: "off" });
  expect(connectorPill({ enabled: true, state: "connected" }, true)).toEqual({ label: "Connected", tone: "ok" });
  expect(connectorPill({ enabled: true, state: "reconnecting" }, true).tone).toBe("busy");
  expect(connectorPill({ enabled: true, state: "error" }, true)).toEqual({ label: "Error", tone: "bad" });
  expect(connectorPill({ enabled: true, state: "incomplete" }, true)).toEqual({ label: "Setup needed", tone: "bad" });
});

it("is a real button that toggles, separate from the enable switch", () => {
  const toggle = vi.fn();
  const enable = vi.fn();
  render(<><ChatConnectorHeader icon={MessageCircle} name="Discord" pill={{ label: "Error", tone: "bad" }} summary="Token rejected" open={false} onToggle={toggle} controls="d" enabled={false} onEnable={enable} /><div id="d" /></>);
  const btn = screen.getByRole("button", { name: "Discord details" });
  // A native <button> gets Enter/Space activation from the browser for free.
  expect(btn.tagName).toBe("BUTTON");
  expect(btn.getAttribute("type")).toBe("button");
  expect(btn.getAttribute("aria-expanded")).toBe("false");
  expect(btn.getAttribute("aria-controls")).toBe("d");
  expect(document.getElementById(btn.getAttribute("aria-describedby")!)?.textContent).toBe("Error");
  expect(screen.getByRole("status").textContent).toBe("Token rejected");
  fireEvent.click(btn);
  expect(toggle).toHaveBeenCalledTimes(1);
  expect(enable).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("switch", { name: "Discord bot" }));
  expect(enable).toHaveBeenCalledWith(true, expect.anything());
  expect(toggle).toHaveBeenCalledTimes(1);
});
