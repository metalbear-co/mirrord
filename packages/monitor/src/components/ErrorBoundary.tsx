import { Component, ErrorInfo, ReactNode } from "react";
import { emitUserBlocked } from "../analytics";

type Props = {
  component: string;
  children: ReactNode;
};

type State = { crashed: boolean };

export class ErrorBoundary extends Component<Props, State> {
  state: State = { crashed: false };

  static getDerivedStateFromError(): State {
    return { crashed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    emitUserBlocked("ui_crashed", "user_action", {
      error: error.message,
      component: this.props.component,
      stack: info.componentStack?.slice(0, 500),
    });
  }

  render(): ReactNode {
    if (this.state.crashed) {
      return (
        <div style={{ padding: 24, fontFamily: "system-ui" }}>
          <h2>Session Monitor crashed.</h2>
          <p>Please reload the page.</p>
        </div>
      );
    }
    return this.props.children;
  }
}
