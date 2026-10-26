// A crash while drawing one view must not take the window with it.
//
// Without a boundary anywhere, a single throw during render unmounts the whole
// tree: the user gets a blank window with no composer, no sidebar and no way
// back except killing the app, while the harness behind it is still running its
// turn. Everything the transcript draws — markdown, tool cards, artificts,
// charts — comes from model output, which is untrusted by design, so a render
// throw is a matter of when and not whether.
//
// `resetKey` is what makes the failure recoverable: navigating to another view
// changes it, and the next render gets a fresh attempt instead of a panel that
// outlives the screen it belonged to.
import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  children: ReactNode;
  /** Changing this clears a previous failure. A new view deserves a fresh try. */
  resetKey?: unknown;
  /** A second way out, e.g. "Back to chats". Kept a caller decision so this
   *  primitive stays free of store and router knowledge. */
  onBack?: () => void;
  backLabel?: string;
}

interface State { error: Error | null }

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // Console rather than a toast: this is a bug, and the component stack is the
    // part that makes it reportable. The panel keeps the message in front of the
    // user, which a toast would not.
    console.error("OpenLeash: a view failed to render", error, info.componentStack);
  }

  componentDidUpdate(prev: Props) {
    if (this.state.error && prev.resetKey !== this.props.resetKey) this.setState({ error: null });
  }

  private retry = () => this.setState({ error: null });

  private back = () => {
    const back = this.props.onBack;
    this.setState({ error: null });
    back?.();
  };

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    const { onBack, backLabel = "Back to chats" } = this.props;
    return (
      <div className="viewcrash" role="alert">
        <div className="viewcrash-title">This view stopped working</div>
        <div className="viewcrash-note">
          Only this screen failed to draw — the chat and its turn keep running.
        </div>
        <pre className="viewcrash-msg">{error.message || String(error)}</pre>
        <div className="viewcrash-actions">
          <button className="btn" onClick={this.retry}>Try again</button>
          {onBack && <button className="btn ghost" onClick={this.back}>{backLabel}</button>}
        </div>
      </div>
    );
  }
}
