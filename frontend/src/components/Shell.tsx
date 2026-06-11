import { NavLink, Outlet } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'

type Me = { sub?: string; name?: string; user?: string; auth_mode?: string; login?: string }

/** Fetch the current user. On 401 the backend returns `{ login: "/auth/login" }`
 *  when SSO is enabled; redirect the whole window to Keycloak so the UI is never
 *  shown to an unauthenticated user. */
async function fetchMe(): Promise<Me> {
  const r = await fetch('/api/me')
  if (r.status === 401) {
    const body = await r.json().catch(() => ({}))
    window.location.href = body?.login ?? '/auth/login'
    // Never resolves meaningfully — the redirect navigates away.
    return new Promise<Me>(() => {})
  }
  return r.json()
}

function navClass({ isActive }: { isActive: boolean }) {
  return [
    'block px-4 py-2 rounded-[14px] transition-colors',
    isActive
      ? 'bg-accent/15 text-text border border-accent/30'
      : 'text-text-dim hover:text-text hover:bg-bg-elev',
  ].join(' ')
}

export default function Shell() {
  const me = useQuery<Me>({
    queryKey: ['me'],
    queryFn: fetchMe,
    retry: false,
  })

  // While the auth check is in flight (and possibly redirecting), don't render
  // the app shell — avoids briefly flashing the full UI to an unauthenticated user.
  if (me.isLoading) {
    return <div className="h-full grid place-items-center text-text-mute">loading…</div>
  }

  return (
    <div className="h-full grid grid-cols-[240px_1fr]">
      <aside className="border-r border-border bg-bg-elev/60 backdrop-blur p-5 flex flex-col gap-6">
        <div className="flex items-center gap-3">
          <div className="w-8 h-8 rounded-[14px] bg-gradient-to-b from-accent to-accent-deep" />
          <div className="font-bold tracking-tight text-text">stardelt Nova</div>
        </div>
        <nav className="flex flex-col gap-1">
          <NavLink to="/"           className={navClass} end>Overview</NavLink>
          <NavLink to="/catalog"    className={navClass}>Catalog</NavLink>
          <NavLink to="/query"      className={navClass}>Query</NavLink>
          <NavLink to="/dashboards" className={navClass}>Dashboards</NavLink>
          <NavLink to="/health"     className={navClass}>Health</NavLink>
        </nav>
        <div className="mt-auto text-xs text-text-mute">
          {me.data ? (
            <>
              signed in as{' '}
              <span className="text-text-dim">
                {me.data.name ?? me.data.user ?? me.data.sub ?? 'unknown'}
              </span>
              {me.data.auth_mode && <div>auth: {me.data.auth_mode}</div>}
            </>
          ) : 'loading…'}
        </div>
      </aside>
      <main className="p-8 overflow-auto">
        <Outlet />
      </main>
    </div>
  )
}
