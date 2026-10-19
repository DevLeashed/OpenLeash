import { afterEach, expect, it } from "vitest";
import { getStudioView, patchStudioView, resetStudioViews } from "./studioViewState";

afterEach(resetStudioViews);

it("remembers each chat's studio view separately and merges patches", () => {
  patchStudioView("a", { sbx: true, gallery: true });
  patchStudioView("b", { artifictId: "x", versionId: "v1" });
  patchStudioView("a", { gallery: false, sandboxId: "s1" });
  expect(getStudioView("a")).toEqual({ sbx: true, gallery: false, sandboxId: "s1" });
  expect(getStudioView("b")).toEqual({ artifictId: "x", versionId: "v1" });
  expect(getStudioView("c")).toEqual({});
});

it("ignores an empty chat id", () => {
  patchStudioView("", { sbx: true });
  expect(getStudioView("")).toEqual({});
});
