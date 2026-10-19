// What each chat's Artifict studio was showing, kept while the app runs.
//
// The studio is keyed by chat so one chat's open artifict can never leak into
// another, which means it remounts on every chat switch and its local state is
// gone. This remembers the view per chat so coming back finds it as it was
// left. In memory on purpose: it is a view, not data, and the artifict itself
// is always re-read from the project.
export interface StudioView {
  artifictId: string;
  versionId: string;
  gallery: boolean;
  sbx: boolean;
  /** The sandbox open in the Sandboxes mode, if any. */
  sandboxId: string | null;
  mode: string;
  panel: string | null;
  tab: "artificts" | "browser";
  expanded: boolean;
}

const views = new Map<string, Partial<StudioView>>();

export const getStudioView = (taskId: string): Partial<StudioView> => views.get(taskId) ?? {};

export function patchStudioView(taskId: string, patch: Partial<StudioView>) {
  if (!taskId) return;
  views.set(taskId, { ...views.get(taskId), ...patch });
}

/** Tests only. */
export function resetStudioViews() {
  views.clear();
}
