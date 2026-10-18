import { Component, type ErrorInfo, type ReactNode } from 'react'

/**
 * Keeps a render error in one route from blanking the whole shell. Shell keys it by
 * pathname, so navigating away clears the error.
 */
export default class ErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state: { error: Error | null } = { error: null }

  static getDerivedStateFromError(error: Error) {
    return { error }
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error('route render failed', error, info.componentStack)
  }

  render() {
    if (!this.state.error) return this.props.children
    return (
      <div className="card p-6 max-w-2xl" role="alert">
        <div className="font-semibold">This page failed to render.</div>
        <p className="text-text-dim text-sm mt-1">
          Navigate elsewhere in the sidebar or reload. The rest of Nova is unaffected.
        </p>
        <div className="text-xs text-text-mute mt-3 font-mono break-words">{this.state.error.message}</div>
      </div>
    )
  }
}
