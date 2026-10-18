import type { Me } from './api'

/**
 * Whether this session gets the admin surface: the platform apps, the health view and
 * the settings page.
 *
 * The IdP is the authority — Nova's `/api/me` reports `is_admin`, derived from the
 * configured admin group of the OIDC claims. In `static-dev` (no login configured)
 * there is no IdP and no second user, so the person running it is the operator and
 * gets the admin surface; that is also how CI, `make demo` and the kind gate behave.
 *
 * This decides what the UI *shows*, and it mirrors the API rather than inventing a rule:
 * the same claim drives `auth::require_admin` on the operator routes (`/api/apps`,
 * `/api/workloads`, `/api/trino/cluster`), which answer 403 for a signed-in non-admin.
 * Hiding the navigation is courtesy; the refusal is the boundary.
 */
export function isAdmin(me?: Me): boolean {
  if (!me) return false
  if (typeof me.is_admin === 'boolean') return me.is_admin
  return me.auth_mode === 'static-dev'
}

/** Short label for the role badge, or `null` when there is no session to speak of. */
export function roleLabel(me?: Me): string | null {
  if (!me) return null
  return isAdmin(me) ? 'admin' : 'user'
}
