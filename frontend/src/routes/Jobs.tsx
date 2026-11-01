import { useQuery } from '@tanstack/react-query'
import { api } from '../api'
import { formatTime } from '../format'
import ErrorBox from '../components/ErrorBox'
import { Icon } from '../components/Icon'

export default function Jobs() {
  const jobs = useQuery({ queryKey: ['jobs'], queryFn: api.jobs, refetchInterval: 5_000 })

  return (
    <div className="max-w-6xl space-y-6">
      <header className="flex items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Activity</h1>
          <p className="text-text-dim text-sm mt-1">Every query, load and transfer, newest first.</p>
        </div>
        <div className="text-xs text-text-mute flex items-center gap-2">
          <span className="w-1.5 h-1.5 rounded-full bg-success animate-pulse" />
          live
        </div>
      </header>

      <div className="card overflow-x-auto">
        <table className="w-full text-sm">
          <thead className="bg-bg-soft/50">
            <tr>
              <th className="th px-4 py-2.5">Kind</th>
              <th className="th px-4 py-2.5">Title</th>
              <th className="th px-4 py-2.5">User</th>
              <th className="th px-4 py-2.5">Status</th>
              <th className="th px-4 py-2.5 text-right">Rows</th>
              <th className="th px-4 py-2.5 text-right">Started</th>
            </tr>
          </thead>
          <tbody>
            {jobs.isLoading && (
              <tr><td className="px-4 py-5 text-text-mute" colSpan={6}>loading…</td></tr>
            )}
            {jobs.isError && !jobs.data && (
              <tr><td className="px-4 py-5" colSpan={6}><ErrorBox error={jobs.error} onRetry={() => jobs.refetch()} /></td></tr>
            )}
            {jobs.data && jobs.data.length === 0 && (
              <tr><td className="px-4 py-5 text-text-mute" colSpan={6}>No activity yet.</td></tr>
            )}
            {(jobs.data ?? []).map(j => (
              <tr key={j.id} className="border-t border-border/70 hover:bg-bg-soft/30">
                <td className="td px-4"><Kind kind={j.kind} /></td>
                <td className="td px-4">
                  <div className="font-mono text-xs truncate max-w-[520px]" title={j.title}>{j.title}</div>
                  {j.detail && <div className="text-[11px] text-text-mute mt-0.5 truncate max-w-[520px]">{j.detail}</div>}
                </td>
                <td className="td px-4 text-text-dim">{j.user ?? ''}</td>
                <td className="td px-4"><Status status={j.status} /></td>
                <td className="td px-4 text-right text-text-dim">{j.rows != null ? j.rows : '—'}</td>
                <td className="td px-4 text-right text-text-mute text-xs whitespace-nowrap">{formatTime(j.started_at)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <div className="flex items-center gap-2 text-xs text-text-mute">
        <Icon name="clock" className="w-3.5 h-3.5" />
        Refreshes every 5 s. This list lives in memory and starts empty when Nova restarts; the Query log keeps older queries.
      </div>
    </div>
  )
}

function Kind({ kind }: { kind: string }) {
  const map: Record<string, string> = {
    query: 'badge border-sky-400/40 bg-sky-400/10 text-info',
    load: 'badge border-emerald-400/40 bg-emerald-400/10 text-ok',
    transfer: 'badge border-violet-400/40 bg-violet-400/10 text-xfer',
  }
  return <span className={map[kind] ?? 'badge border-border bg-bg-soft text-text-dim'}>{kind}</span>
}

function Status({ status }: { status: string }) {
  const ok = status === 'succeeded'
  return (
    <span className={ok
      ? 'badge border-emerald-400/40 bg-emerald-400/10 text-ok'
      : 'badge border-red-400/40 bg-red-400/10 text-bad'}>
      {status}
    </span>
  )
}
