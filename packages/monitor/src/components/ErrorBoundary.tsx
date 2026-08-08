import type { ErrorInfo, ReactNode } from 'react'
import { Component } from 'react'
import { emitUserBlocked } from '../analytics'
import { strings } from '../strings'

const STACK_TRACE_MAX_LEN = 500

interface Props {
  component: string
  children: ReactNode
}

interface State {
  crashed: boolean
}

export class ErrorBoundary extends Component<Props, State> {
  override state: State = { crashed: false }

  static getDerivedStateFromError(): State {
    return { crashed: true }
  }

  override componentDidCatch(error: Error, info: ErrorInfo): void {
    emitUserBlocked(
      'ui_crashed',
      'user_action',
      {
        error: error.message,
        component: this.props.component,
        stack: info.componentStack?.slice(0, STACK_TRACE_MAX_LEN),
      },
      error,
    )
  }

  override render(): ReactNode {
    if (this.state.crashed) {
      return (
        <div style={{ padding: 24, fontFamily: 'system-ui' }}>
          <h2>{strings.errorBoundary.title}</h2>
          <p>{strings.errorBoundary.body}</p>
        </div>
      )
    }
    return this.props.children
  }
}
