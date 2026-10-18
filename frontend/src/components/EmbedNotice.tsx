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
        <span className="badge shrink-0 border-amber-400/40 bg-amber-400/10 text-amber-300">
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
          : 'border-amber-400/40 bg-amber-400/10 text-amber-300',
      ].join(' ')}
      title={embed.checked ? `headers read from ${embed.checked}` : undefined}
    >
      {ok ? 'embeds ok' : 'framing blocked'}
    </span>
  )
}
