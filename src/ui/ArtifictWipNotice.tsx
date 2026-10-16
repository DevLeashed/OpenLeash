import { CircleAlert } from "lucide-react";
import type { ArtifictFormat } from "./artifictApi";
import { parseCollaborationRecord } from "./ArtifictRecord";

/** The stage a record claims about itself, when it is a collaboration record
 *  that declares a valid one. Every other format has no stage of its own, and
 *  unreadable content must not be allowed to supply one: the notice names a
 *  recorded stage or nothing, never a stage it inferred. */
export function artifictRecordStage(format: ArtifictFormat, content: string): string | undefined {
  return format === "json" ? parseCollaborationRecord(content)?.stage : undefined;
}

/** Every artifict is a working draft — the agent can append a revision, and a
 *  request can still change it — so the record surface and the inline card say
 *  so above the content instead of leaving the reader to infer it from a version
 *  bar. A declared stage is named as its author's claim, which is what keeps the
 *  warning honest: behind it, `verified` is a recorded word, not a verdict. */
export function ArtifictWipNotice({ format, content, version }: { format: ArtifictFormat; content: string; version?: number }) {
  const stage = artifictRecordStage(format, content);
  return <p className="artifict-wip" role="note">
    <CircleAlert size={13} aria-hidden="true" />
    <span><b>Work in progress</b> — a working draft, not a finished deliverable or an approval.{stage ? ` ${version ? `v${version} · ` : ""}Recorded stage: ${stage} · author-reported, not independently certified.` : null}</span>
  </p>;
}
