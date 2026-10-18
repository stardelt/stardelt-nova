// Browser side of sign-in. Nova's session lives in an HttpOnly cookie the backend
// sets; this file never sees a token. On a 401 it sends the browser to the backend's
// login route, which runs the OIDC flow and comes back to `return_to`.

/**
 * Only a same-origin relative path may be a `return_to` (mirrors the backend's
 * `safe_return_to`): `/x`, never `//host`, `/\host`, `https://…` or `/api/auth/…`.
 */
export function safeReturnTo(path: string): string {
  const ok =
    path.startsWith('/') &&
    !path.startsWith('//') &&
    !path.includes('\\') &&
    !/[\u0000-\u001f\u007f]/.test(path) &&
    path.length <= 2048 &&
    !path.startsWith('/api/auth/')
  return ok ? path : '/'
}

let redirecting = false

/** Send the browser to sign in, then back to where it was. Runs at most once per page. */
export function redirectToLogin(): void {
  if (redirecting) return
  redirecting = true
  const here = `${window.location.pathname}${window.location.search}${window.location.hash}`
  window.location.assign(`/api/auth/login?return_to=${encodeURIComponent(safeReturnTo(here))}`)
}
