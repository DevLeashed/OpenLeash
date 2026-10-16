import { CSSProperties, ReactNode } from "react";
import { Modal } from "./Modal";
import { Button } from "./Controls";

/** A yes/no the app asks in its own window.
 *
 *  Replaces `window.confirm`, which in a native app paints browser chrome: it
 *  ignores the app's theme, is unreachable by the app's own keyboard model, and
 *  reads as a web page rather than a desktop tool. Uses the same `.mh`/`.mb`/
 *  `.mf` frame as the other dialogs and inherits the focus trap from `Modal`,
 *  so Cancel — the first focusable — is what a keyboard lands on, not the
 *  destructive button. */
export function ConfirmDialog({
  title,
  body,
  confirm,
  cancel = "Cancel",
  danger = false,
  onConfirm,
  onClose,
  style,
}: {
  title: string;
  body?: ReactNode;
  confirm: string;
  cancel?: string;
  danger?: boolean;
  onConfirm: () => void;
  onClose: () => void;
  style?: CSSProperties;
}) {
  return (
    <Modal onClose={onClose} ariaLabel={title} style={{ width: "min(420px, 100%)", ...style }}>
      <div className="mh"><span style={{ flex: 1 }}>{title}</span></div>
      {body !== undefined && <div className="mb">{typeof body === "string" ? <div className="desc">{body}</div> : body}</div>}
      <div className="mf">
        <div style={{ flex: 1 }} />
        <Button variant="ghost" onClick={onClose}>{cancel}</Button>
        <Button variant="primary" style={danger ? { background: "#ff8a8a" } : undefined} onClick={() => { onClose(); onConfirm(); }}>{confirm}</Button>
      </div>
    </Modal>
  );
}
