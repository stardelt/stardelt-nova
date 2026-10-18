import { Link, useLocation } from 'react-router-dom'

/**
 * Catch-all for an unknown URL (`App.tsx`'s `path="*"`). Without it nothing matched
 * inside `Shell`, so `<main>` rendered empty: no message, no navigation, no way back.
 *
 * It sits inside `Shell` and outside `RequireAdmin` on purpose — a mistyped path is not
 * an authorization question, so every signed-in user gets the same answer.
 */
export default function NotFound() {
  const { pathname } = useLocation()

  return (
    <div className="card p-6 max-w-2xl">
      <h1 className="text-xl font-semibold tracking-tight">This page does not exist</h1>
      <p className="text-text-dim text-sm mt-2">
        Nova has no route for <span className="font-mono break-all">{pathname}</span>.
      </p>
      <Link to="/" className="btn-ghost mt-4">
        Back to Overview
      </Link>
    </div>
  )
}
