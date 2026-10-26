// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";
import { useState } from "react";
import { ErrorBoundary } from "./ErrorBoundary";

function Boom({ crash }: { crash: boolean }) {
  if (crash) throw new Error("markdown blew up");
  return <div>transcript</div>;
}

let logged: unknown[][] = [];
beforeEach(() => {
  // React reports a caught error through console.error as well; the boundary
  // must not turn a handled crash into test noise.
  logged = [];
  vi.spyOn(console, "error").mockImplementation((...args: unknown[]) => { logged.push(args); });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("a view that throws", () => {
  it("shows a panel instead of leaving the window blank", () => {
    const view = render(<ErrorBoundary><Boom crash /></ErrorBoundary>);
    expect(view.container.textContent).toContain("This view stopped working");
    expect(view.container.textContent).toContain("markdown blew up");
    expect(view.getByRole("alert")).toBeTruthy();
    // The stack goes to the console, where a bug report can pick it up.
    expect(logged.some((args) => String(args[0]).includes("failed to render"))).toBe(true);
  });

  it("tries again when the user asks", () => {
    const view = render(<ErrorBoundary><Boom crash /></ErrorBoundary>);
    fireEvent.click(view.getByText("Try again"));
    // Still broken: the same throw comes straight back, which is honest.
    expect(view.container.textContent).toContain("This view stopped working");
  });

  it("recovers once the child can render, and after the view changes", () => {
    function Host({ crash }: { crash: boolean }) {
      return <ErrorBoundary resetKey="session"><Boom crash={crash} /></ErrorBoundary>;
    }
    const view = render(<Host crash />);
    expect(view.container.textContent).toContain("This view stopped working");
    view.rerender(<Host crash={false} />);
    // Same resetKey, so the failure stands until the user asks again — a view
    // that keeps re-rendering must not flicker between panel and content.
    expect(view.container.textContent).toContain("This view stopped working");
    fireEvent.click(view.getByText("Try again"));
    expect(view.container.textContent).toBe("transcript");

    function Nav() {
      const [view, setView] = useState("broken");
      return (
        <ErrorBoundary resetKey={view} onBack={() => setView("home")} backLabel="Back to chats">
          <Boom crash={view === "broken"} />
        </ErrorBoundary>
      );
    }
    const nav = render(<Nav />);
    expect(nav.container.textContent).toContain("This view stopped working");
    fireEvent.click(nav.getByText("Back to chats"));
    expect(nav.container.textContent).toBe("transcript");
  });

  it("keeps the rest of the window alive", () => {
    const view = render(
      <div>
        <div>sidebar</div>
        <ErrorBoundary><Boom crash /></ErrorBoundary>
      </div>,
    );
    expect(view.container.textContent).toContain("sidebar");
    expect(view.container.textContent).toContain("This view stopped working");
  });
});
