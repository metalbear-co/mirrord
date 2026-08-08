import type { ErrorInfo, ReactNode } from 'react'
import { Component } from 'react'
import { cn } from '@metalbear/ui'
import { TriangleAlert } from 'lucide-react'
import { emitUserBlocked } from '../analytics'
import { strings } from '../strings'

const STACK_TRACE_MAX_LEN = 500

interface Props {
  component: string
  resetKey?: string | null
  fallback?: ReactNode
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

  override componentDidUpdate(prev: Props): void {
    if (this.state.crashed && prev.resetKey !== this.props.resetKey) {
      this.setState({ crashed: false })
    }
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
        this.props.fallback ?? (
          <div style={{ padding: 24, fontFamily: 'system-ui' }}>
            <h2>{strings.errorBoundary.title}</h2>
            <p>{strings.errorBoundary.body}</p>
          </div>
        )
      )
    }
    return this.props.children
  }
}

export function PaneCrashNotice({ className }: { className?: string }) {
  return (
    <div
      className={cn(
        'text-muted-foreground flex h-full flex-col items-center justify-center gap-1 p-6 text-center',
        className,
      )}
    >
      <TriangleAlert className="text-muted-foreground/70 h-5 w-5" />
      <p className="text-body font-semibold">
        {strings.errorBoundary.paneTitle}
      </p>
      <p className="text-meta">{strings.errorBoundary.paneBody}</p>
    </div>
  )
}
