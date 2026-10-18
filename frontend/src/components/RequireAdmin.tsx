import type { ReactNode } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api'
import { isAdmin } from '../roles'

/**
 * Wrapper for admin-only routes: `apps`, `apps/:id`, `health`, `settings`.
 *
 * A signed-in non-admin who types the URL gets an explanation and a way forward, not a
 * blank page or a half-rendered table. The honest part is *why*: the role comes from the
 * identity provider's group membership, so "ask an administrator to add you to the group"
 * is the real answer — there is nothing to click in Nova.
 */
export default function RequireAdmin({ children }: { children: ReactNode }) {
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })

  if (me.isLoading) return <div className="text-text-mute text-sm">loading…</div>

  if (!isAdmin(me.data)) {
    return (
      <div className="card p-6 max-w-2xl">
        <h1 className="text-xl font-semibold tracking-tight">Platform administration</h1>
        <p className="text-text-dim text-sm mt-2">
          This view is for platform administrators. You are signed in as{' '}
          <span className="font-mono">{me.data?.user ?? 'unknown'}</span>
          {me.data?.auth_mode === 'oidc' ? ', without the administrator group.' : '.'}
        </p>
        <p className="text-text-mute text-xs mt-3">
          Membership is decided by the identity provider, not by Nova — an administrator has to add
          you to the group, then you sign in again.
        </p>
      </div>
    )
  }

  return <>{children}</>
}
