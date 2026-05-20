import { NavLink, Outlet } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'

type Me = { user: string; auth_mode: string }

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
    queryFn: () => fetch('/api/me').then(r => r.json()),
  })

  return (
    <div className="h-full grid grid-cols-[240px_1fr]">
      <aside className="border-r border-border bg-bg-elev/60 backdrop-blur p-5 flex flex-col gap-6">
        <div className="flex items-center gap-3">
          <div className="w-8 h-8 rounded-[14px] bg-gradient-to-b from-accent to-accent-deep" />
          <div className="font-bold tracking-tight text-text">Stardelt Nova</div>
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
              signed in as <span className="text-text-dim">{me.data.user}</span>
              <div>auth: {me.data.auth_mode}</div>
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
