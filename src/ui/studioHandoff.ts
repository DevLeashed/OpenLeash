/** A chat started from a studio opens on the artifict it was started from: the
 *  new chat's Session reads (and drops) this entry for its first selection, so
 *  the design does not move while the chat beside it is replaced. Kept in a
 *  module of its own so the composer can fill it without importing the studio. */
export const studioHandoff = new Map<string, { artifictId: string; versionId: string }>();
