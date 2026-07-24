import React from 'react'
import { emitWizardBlocked } from '../analytics'
import { strings } from '../strings'

const STACK_TRACE_MAX_LEN = 500

interface Props {
  children: React.ReactNode
  fallback?: React.ReactNode
}

interface State {
  hasError: boolean
  error?: Error
}

class ErrorBoundary extends React.Component<Props, State> {
  constructor(props: Props) {
    super(props)
    this.state = { hasError: false }
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error }
  }

  override componentDidCatch(error: Error, errorInfo: React.ErrorInfo) {
    console.error('ErrorBoundary caught an error:', error, errorInfo)
    emitWizardBlocked(
      'ui_crashed',
      'user_action',
      {
        error: error.message,
        stack: errorInfo.componentStack?.slice(0, STACK_TRACE_MAX_LEN),
      },
      error,
    )
  }

  override render() {
    if (this.state.hasError) {
      if (this.props.fallback) {
        return this.props.fallback
      }
      return (
        <div className="bg-destructive/10 border-destructive rounded-lg border p-4">
          <h3 className="text-destructive font-semibold">
            {strings.errorBoundary.title}
          </h3>
          <p className="text-muted-foreground mt-1 text-sm">
            {this.state.error?.message}
          </p>
        </div>
      )
    }

    return this.props.children
  }
}

export default ErrorBoundary
