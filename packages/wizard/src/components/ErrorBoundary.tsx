import React from 'react'

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
  }

  override render() {
    if (this.state.hasError) {
      if (this.props.fallback) {
        return this.props.fallback
      }
      return (
        <div className="bg-destructive/10 border-destructive rounded-lg border p-4">
          <h3 className="text-destructive font-semibold">Something went wrong</h3>
          <p className="text-muted-foreground mt-1 text-sm">{this.state.error?.message}</p>
        </div>
      )
    }

    return this.props.children
  }
}

export default ErrorBoundary
