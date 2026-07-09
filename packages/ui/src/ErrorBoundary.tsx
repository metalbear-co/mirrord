import { Component, ErrorInfo, ReactNode } from 'react'

type Props = { children: ReactNode }

type State = { crashed: boolean }

/**
 * Outer crash guard for the whole site. Each feature (monitor, wizard) keeps its own inner
 * boundary with feature-specific telemetry; this one is the last resort and has no dependency on
 * either feature, so it can wrap both.
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { crashed: false }

  static getDerivedStateFromError(): State {
    return { crashed: true }
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error('mirrord UI crashed:', error, info.componentStack)
  }

  render(): ReactNode {
    if (this.state.crashed) {
      return (
        <div style={{ padding: 24, fontFamily: 'system-ui' }}>
          <h2>mirrord UI crashed.</h2>
          <p>Please reload the page.</p>
        </div>
      )
    }
    return this.props.children
  }
}
