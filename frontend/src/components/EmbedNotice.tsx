import type { AppEmbed } from '../api'

/**
 * What the app itself says about being embedded.
 *
 * JupyterHub ships `content-security-policy: frame-ancestors 'none'` and Superset
 * `x-frame-options: SAMEORIGIN`; browsers honour those and render an empty frame,
 * with the reason only in the console. Nova reads the app's headers (`/api/apps/:id`
 * → `embed`) and says so here, with the header that has to change.
 */
export function EmbedNotice({ app, embed }: { app: string; embed?: AppEmbed }) {
  if (!embed || embed.state !== 'blocked') return null
  const header = embed.blocked_by ?? 'headers'
  return (
    <div className="card border-amber-400/40 bg-amber-400/5 px-4 py-3 text-sm">
      <div className="flex items-start gap-3">
        <span className="badge shrink-0 border-amber-400/40 bg-amber-400/10 text-warn">
          framing blocked
        </span>
        <div className="min-w-0">
          <div>
            {app} refuses to be embedded. The frame below stays empty — browsers block it before
            the page loads.
          </div>
          <div className="mt-2 text-[11px] font-mono text-text-dim break-all">
            {header}: {embed.detail}
          </div>
          <div className="text-text-mute mt-2">
            To embed it, the app has to permit Nova's origin — for JupyterHub that is a
            tornado setting (<span className="font-mono">frame-ancestors 'self' {typeof window !== 'undefined' ? window.location.origin : ''}</span>),
            for Superset the <span className="font-mono">X-Frame-Options</span> config. Until then,
            use “Open in new tab”.
          </div>
          {embed.checked && (
            <div className="text-[11px] text-text-mute mt-2">
              Headers read from <span className="font-mono">{embed.checked}</span>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

/** A one-word status for a page header: is the embed going to work? */
export function EmbedBadge({ embed }: { embed?: AppEmbed }) {
  if (!embed || embed.state === 'unknown') return null
  const ok = embed.state === 'allowed'
  return (
    <span
      className={[
        'badge',
        ok
          ? 'border-success/40 bg-success/10 text-success'
          : 'border-amber-400/40 bg-amber-400/10 text-warn',
      ].join(' ')}
      title={embed.checked ? `headers read from ${embed.checked}` : undefined}
    >
      {ok ? 'embeds ok' : 'framing blocked'}
    </span>
  )
}

/**
 * The app is frameable, but a visitor without a session is sent to a sign-in
 * service on another host first — and that page (Keycloak) must never be
 * framed. The browser shows nothing until the visitor has a session, so give
 * the obvious action — open the app top-level, sign in, come back — instead of
 * leaving a blank rectangle that looks like a broken notebook.
 */
export function SignInNotice({ app, embed, url }: { app: string; embed?: AppEmbed; url?: string }) {
  if (!embed?.sign_in_host || !url) return null
  return (
    <div className="card border-accent/40 bg-accent/5 px-4 py-3 text-sm">
      <div className="flex items-start gap-3">
        <span className="badge shrink-0 border-accent/40 bg-accent/15 text-accent">sign in first</span>
        <div className="min-w-0">
          <div>
            {app} sends everyone without a session to{' '}
            <span className="font-mono">{embed.sign_in_host}</span> to sign in — deliberately not
            frameable, so the frame below stays empty until you have signed in once.
          </div>
          <a href={url} target="_blank" rel="noreferrer" className="btn-primary mt-3">
            Open {app} in a new tab and sign in ↗
          </a>
        </div>
      </div>
    </div>
  )
}
