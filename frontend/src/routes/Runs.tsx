import { useQuery } from '@tanstack/react-query'
import { Link, useSearchParams } from 'react-router-dom'
import { api, type RunsListRow } from '../api'
import { EmptyState } from '../components/EmptyState'
import ErrorBox from '../components/ErrorBox'
import { RunStateBadge, formatWhen, runDuration, runStateWord } from './Pipelines'

/**
 * Runs: every run of the caller's notebooks and pipelines in one table, filterable by
 * kind, state and group (stardelt-nova#621). The list is flat — a row carries the
 * pipeline's `namespace`, `name` and `ownerGroup` beside the run's own fields.
 *
 * The filters live in the address, so a reload keeps them and a link can carry them.
 */

/** The choices of `?kind=`: the shapes a scheduled thing can have. */
const KIND_OPTIONS: { value: string; label: string }[] = [
  { value: '', label: 'All kinds' },
  { value: 'sql', label: 'SQL' },
  { value: 'notebook', label: 'Notebook' },
  { value: 'pipeline', label: 'Pipeline' },
]

/** The states worth filtering on, shown through `runStateWord` so the words cannot drift. */
const STATE_OPTIONS = ['running', 'queued', 'success', 'failed', 'skipped', 'upstream_failed']

/** `sql` -> `SQL`, `notebook` -> `Notebook`; anything else prints as it came. */
function kindWord(kind: string): string {
  if (kind === 'sql') return 'SQL'
  return kind ? kind.charAt(0).toUpperCase() + kind.slice(1) : '—'
}

export default function Runs() {
  const [params, setParams] = useSearchParams()
  const kind = params.get('kind') ?? ''
  const state = params.get('state') ?? ''
  const group = params.get('group') ?? ''

  const set = (key: string, value: string) => {
    const next = new URLSearchParams(params)
    if (value) next.set(key, value)
    else next.delete(key)
    setParams(next, { replace: true })
  }

  const runs = useQuery({
    queryKey: ['runs', kind, state, group],
    queryFn: () =>
      api.runs({
        kind: kind || undefined,
        state: state || undefined,
        group: group || undefined,
        limit: 100,
      }),
    refetchInterval: (query) =>
      query.state.data?.runs.some((row) => row.state === 'running' || row.state === 'queued')
        ? 5_000
        : false,
  })

  const rows: RunsListRow[] = runs.data?.runs ?? []
  // The groups that actually appear in the answer; `null` is administrators, not a choice.
  const groups = [...new Set(rows.map((row) => row.ownerGroup).filter((g): g is string => g != null))]
  const empty = rows.length === 0

  return (
    <div className="max-w-6xl space-y-5">
      <header className="min-w-0">
        <h1 className="text-2xl font-bold tracking-tight">Runs</h1>
        <p className="text-text-dim text-sm mt-1">
          Every run of the notebooks and pipelines your groups own, newest first.
        </p>
      </header>

      <div className="flex flex-wrap items-center gap-2">
        <select
          aria-label="Kind"
          className="input py-1 w-auto text-xs"
          value={kind}
          onChange={(e) => set('kind', e.target.value)}
        >
          {KIND_OPTIONS.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>

        <select
          aria-label="State"
          className="input py-1 w-auto text-xs"
          value={state}
          onChange={(e) => set('state', e.target.value)}
        >
          <option value="">All states</option>
          {STATE_OPTIONS.map((option) => (
            <option key={option} value={option}>
              {runStateWord(option)}
            </option>
          ))}
        </select>

        <select
          aria-label="Group"
          className="input py-1 w-auto text-xs"
          value={group}
          onChange={(e) => set('group', e.target.value)}
        >
          <option value="">All groups</option>
          {groups.map((g) => (
            <option key={g} value={g}>
              {g}
            </option>
          ))}
        </select>
      </div>

      <div className="card overflow-x-auto">
        <table className="w-full text-sm">
          <caption className="sr-only">Every run of the notebooks and pipelines your groups own.</caption>
          <thead className="bg-bg-soft/50">
            <tr>
              <th scope="col" className="th px-3 py-2.5">Pipeline</th>
              <th scope="col" className="th px-3 py-2.5">Kind</th>
              <th scope="col" className="th px-3 py-2.5">State</th>
              <th scope="col" className="th px-3 py-2.5">Started</th>
              <th scope="col" className="th px-3 py-2.5">Duration</th>
              <th scope="col" className="th px-3 py-2.5">Trigger</th>
            </tr>
          </thead>
          <tbody>
            {runs.isLoading &&
              [0, 1, 2].map((i) => (
                <tr key={i}>
                  {[0, 1, 2, 3, 4, 5].map((cell) => (
                    <td key={cell} className="td px-3">
                      <div className="h-4 animate-pulse rounded bg-bg-soft" />
                    </td>
                  ))}
                </tr>
              ))}

            {runs.isError && !runs.data && (
              <tr>
                <td colSpan={6} className="td px-3 py-3">
                  <ErrorBox error={runs.error} onRetry={() => void runs.refetch()} />
                </td>
              </tr>
            )}

            {runs.data && empty && (
              <tr>
                <td colSpan={6} className="px-3 py-3">
                  <EmptyState icon="list" title="No runs yet">
                    Schedule a notebook or a pipeline and its runs appear here.
                  </EmptyState>
                </td>
              </tr>
            )}

            {rows.map((row) => {
              const when = formatWhen(row.startedAt)
              return (
                <tr key={`${row.namespace}/${row.name}/${row.runId}`}>
                  <td className="td px-3">
                    <Link
                      to={`/pipelines/${encodeURIComponent(row.namespace)}/${encodeURIComponent(row.name)}`}
                      className="font-mono text-xs break-all underline"
                    >
                      {row.name}
                    </Link>
                    <span className="block text-xs text-text-mute">
                      {row.ownerGroup ?? 'administrators'}
                    </span>
                  </td>
                  <td className="td px-3">{kindWord(row.kind)}</td>
                  <td className="td px-3">
                    <RunStateBadge state={row.state} />
                  </td>
                  <td className="td px-3 whitespace-nowrap" title={when.title}>
                    {when.text}
                  </td>
                  <td className="td px-3 tabular-nums">{runDuration(row.durationSeconds)}</td>
                  <td className="td px-3">{row.triggeredBy ?? row.type}</td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>
    </div>
  )
}
