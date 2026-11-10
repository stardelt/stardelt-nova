import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link, useParams } from 'react-router-dom'
import { ApiError, api, type PipelineView } from '../api'
import ErrorBox from '../components/ErrorBox'
import ScheduleDialog from '../components/ScheduleDialog'
import { formatBytes, formatTime, shortVersion } from '../format'

/**
 * One published notebook (stardelt-nova#517): its versions, newest first, the parameters of the
 * version the person picked, and a schedule for the newest or any older version. The notebook's
 * address is `/notebooks/<group>/<name>`, so a single notebook can be linked and an older
 * version read on its own.
 *
 * The versions route is read first; the parameters route is asked only once a version is known,
 * so an empty list never calls it. A missing notebook store answers 503 — a fact about the
 * installation, not something a Retry could fix.
 */
export default function NotebookDetail() {
  const { group = '', name = '' } = useParams()
  const versions = useQuery({
    queryKey: ['notebook-versions', group, name],
    queryFn: () => api.notebookVersions(group, name),
  })
  const [picked, setPicked] = useState<string | null>(null)
  const latest = versions.data?.[0]
  const selected: string | undefined = picked ?? latest?.objectVersion
  const parameters = useQuery({
    queryKey: ['notebook-parameters', group, name, selected ?? 'newest'],
    queryFn: () => api.notebookParameters(group, name, selected),
    enabled: selected !== undefined,
  })
  const usage = useQuery({
    queryKey: ['notebook-usage', group, name],
    queryFn: () => api.notebookUsage(group, name),
  })
  const [scheduling, setScheduling] = useState<{ objectVersion: string } | null>(null)
  const [scheduled, setScheduled] = useState<PipelineView | null>(null)

  const failed = versions.isError && !versions.data
  // Without a notebook store the route answers 503: a fact about the installation, not a failure
  // a Retry could fix.
  const storeMissing = failed && versions.error instanceof ApiError && versions.error.status === 503
  const rows = versions.data ?? []

  return (
    <div className="max-w-6xl space-y-5">
      <header className="flex flex-wrap items-start justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-2xl font-bold tracking-tight font-mono break-all">{name}</h1>
          {latest && (
            <p className="text-text-dim text-sm mt-1">
              {group} · published {formatTime(latest.uploadedAt)} by {latest.uploadedBy} ·{' '}
              {shortVersion(latest.objectVersion)}
            </p>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            className="btn-primary py-1.5"
            disabled={versions.isLoading || !latest}
            onClick={() => {
              if (!latest) return
              setPicked(latest.objectVersion)
              setScheduling({ objectVersion: latest.objectVersion })
            }}
          >
            Schedule latest
          </button>
          <Link to="/notebooks" className="btn-ghost py-1.5">Back to notebooks</Link>
        </div>
      </header>

      {scheduled && (
        <p role="status" className="text-sm">
          Scheduled {scheduled.name}.{' '}
          <Link
            to={`/pipelines/${encodeURIComponent(scheduled.namespace)}/${encodeURIComponent(scheduled.name)}`}
            className="underline"
          >
            Open it
          </Link>
        </p>
      )}

      {versions.isLoading && <p className="text-sm text-text-mute">Loading versions…</p>}
      {failed && (
        <ErrorBox error={versions.error} onRetry={storeMissing ? undefined : () => void versions.refetch()} />
      )}
      {versions.data && rows.length === 0 && (
        <p className="text-sm text-text-mute">No published version of this notebook is left.</p>
      )}

      {rows.length > 0 && (
        <section className="card relative overflow-x-auto">
          <div className="px-5 pt-5 pb-3">
            <h2 className="text-lg font-semibold">Versions</h2>
          </div>
          <table className="w-full text-sm">
            <caption className="sr-only">Published versions of {name}, newest first.</caption>
            <thead className="bg-bg-soft/50">
              <tr>
                <th scope="col" className="th px-5 py-2.5">Version</th>
                <th scope="col" className="th px-3 py-2.5">Size</th>
                <th scope="col" className="th px-3 py-2.5">Published</th>
                <th scope="col" className="th px-3 py-2.5">Published by</th>
                <th scope="col" className="th px-3 py-2.5">sha256</th>
                <th scope="col" className="th px-3 py-2.5"><span className="sr-only">Actions</span></th>
              </tr>
            </thead>
            <tbody>
              {rows.map((version) => {
                const current = version.objectVersion === selected
                return (
                  <tr
                    key={version.objectVersion}
                    aria-current={current ? 'true' : undefined}
                    className={`border-t border-border/70 ${current ? 'bg-bg-soft/50' : ''}`}
                  >
                    <td className="td px-5">
                      <button
                        type="button"
                        className="font-mono text-xs underline"
                        title={version.objectVersion}
                        onClick={() => setPicked(version.objectVersion)}
                      >
                        {shortVersion(version.objectVersion)}
                      </button>
                    </td>
                    <td className="td px-3 tabular-nums">{formatBytes(version.size)}</td>
                    <td className="td px-3 whitespace-nowrap">{formatTime(version.uploadedAt)}</td>
                    <td className="td px-3">{version.uploadedBy}</td>
                    <td className="td px-3"><span className="font-mono text-xs">{version.sha256.slice(0, 12)}</span></td>
                    <td className="td px-3 text-right">
                      <button
                        type="button"
                        className="btn-ghost py-1 text-xs whitespace-nowrap"
                        onClick={() => {
                          // Scheduling a version also shows it, so the page and the dialog agree.
                          setPicked(version.objectVersion)
                          setScheduling({ objectVersion: version.objectVersion })
                        }}
                      >
                        Schedule this version
                      </button>
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </section>
      )}

      {rows.length > 0 && (
        <section className="card p-5">
          <h2 className="text-lg font-semibold break-words">
            Parameters of {shortVersion(selected)}
          </h2>
          {parameters.isLoading && <p className="mt-2 text-sm text-text-mute">Loading parameters…</p>}
          {parameters.isError && (
            <div className="mt-2">
              <ErrorBox error={parameters.error} onRetry={() => void parameters.refetch()} />
            </div>
          )}
          {parameters.data && parameters.data.parameters.length === 0 && (
            <p className="mt-2 text-sm text-text-mute">This notebook has no parameters cell.</p>
          )}
          {parameters.data && parameters.data.parameters.length > 0 && (
            <ul className="mt-2 space-y-1 text-sm">
              {parameters.data.parameters.map((parameter) => (
                <li key={parameter.name} className="break-words font-mono text-xs">
                  {parameter.name} = {parameter.default}
                </li>
              ))}
            </ul>
          )}
        </section>
      )}

      <section className="card p-5">
        <h2 className="text-lg font-semibold">Where it is used</h2>
        {usage.isLoading && <p className="mt-2 text-sm text-text-mute">Loading schedules…</p>}
        {usage.isError && (
          <div className="mt-2"><ErrorBox error={usage.error} onRetry={() => void usage.refetch()} /></div>
        )}
        {usage.data && usage.data.schedules.length === 0 && (
          <p className="mt-2 text-sm text-text-mute">
            No schedule runs this notebook yet. Pick a version above and choose “Schedule this version”.
          </p>
        )}
        {usage.data && usage.data.schedules.length > 0 && (
          <table className="mt-3 w-full text-sm">
            <caption className="sr-only">Pipelines that run {name}, sorted by name.</caption>
            <thead className="bg-bg-soft/50">
              <tr>
                <th scope="col" className="th px-2 py-2">Pipeline</th>
                <th scope="col" className="th px-2 py-2">When</th>
                <th scope="col" className="th px-2 py-2">Pinned version</th>
                <th scope="col" className="th px-2 py-2">Runs as</th>
              </tr>
            </thead>
            <tbody>
              {usage.data.schedules.map((row) => {
                const behind = rows.findIndex((version) => version.objectVersion === row.objectVersion)
                return (
                  <tr key={`${row.namespace}/${row.name}`} className="border-t border-border/70">
                    <td className="td px-2">
                      <Link
                        to={`/pipelines/${encodeURIComponent(row.namespace)}/${encodeURIComponent(row.name)}`}
                        className="font-mono text-xs underline break-all"
                      >
                        {row.name}
                      </Link>
                    </td>
                    <td className="td px-2 text-xs">
                      {row.cron ? <span className="font-mono">{row.cron} · {row.timezone ?? 'UTC'}</span> : 'On demand'}
                    </td>
                    <td className="td px-2 text-xs">
                      <span className="font-mono" title={row.objectVersion}>{shortVersion(row.objectVersion)}</span>
                      {behind > 0 && <span className="text-text-mute"> · {behind} version{behind === 1 ? '' : 's'} behind</span>}
                      {behind > 0 && (
                        <Link to={`/pipelines/${encodeURIComponent(row.namespace)}/${encodeURIComponent(row.name)}?edit=newest`}
                          className="ml-2 underline text-accent">Move to newest</Link>
                      )}
                    </td>
                    <td className="td px-2 text-xs font-mono">{row.runAs ?? '—'}</td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        )}
      </section>

      {scheduling && (
        <ScheduleDialog
          mode="create"
          initialKind="notebook"
          initialNotebook={{ group, name, objectVersion: scheduling.objectVersion }}
          onClose={() => setScheduling(null)}
          onScheduled={(view) => {
            setScheduling(null)
            setScheduled(view)
          }}
        />
      )}
    </div>
  )
}
