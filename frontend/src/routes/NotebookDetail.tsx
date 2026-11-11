import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link, useParams } from 'react-router-dom'
import { ApiError, api, type PipelineView } from '../api'
import ErrorBox from '../components/ErrorBox'
import NotebookFrame from '../components/NotebookFrame'
import ScheduleDialog from '../components/ScheduleDialog'
import { formatBytes, formatTime, shortVersion } from '../format'
import { diffNotebooks } from '../notebookDiff'
import { renderNotebook } from '../notebookHtml'
import { RunStateBadge, formatWhen, runDuration } from './Pipelines'

/**
 * One published notebook (stardelt-nova#517): its versions, newest first, the parameters of the
 * version the person picked, a read-only preview of that version (#684), and a schedule for the
 * newest or any older version. The notebook's address is `/notebooks/<group>/<name>`, so a single
 * notebook can be linked and an older version read on its own.
 *
 * The versions route is read first; the parameters and content routes are asked only once a
 * version is known, so an empty list never calls them. A missing notebook store answers 503 — a
 * fact about the installation, not something a Retry could fix.
 */
export default function NotebookDetail() {
  const { group = '', name = '' } = useParams()
  const versions = useQuery({
    queryKey: ['notebook-versions', group, name],
    queryFn: () => api.notebookVersions(group, name),
  })
  const [picked, setPicked] = useState<string | null>(null)
  // The version the picked one is compared with (#755); `null` means the page is as before.
  const [compare, setCompare] = useState<string | null>(null)
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
  // The runs of this notebook's schedules (#517, #756). Asked only once the caller may see the
  // notebook: the versions route has answered, so a 403 here is a real problem.
  const runs = useQuery({
    queryKey: ['notebook-runs', group, name],
    queryFn: () => api.runs({ notebook: `${group}/${name}`, limit: 10 }),
    enabled: versions.data !== undefined,
  })
  const content = useQuery({
    queryKey: ['notebook-content', group, name, selected],
    queryFn: () => api.notebookContent(group, name, selected),
    enabled: selected !== undefined,
  })
  // The other side of the comparison (#755): asked only once a version is chosen.
  const compareContent = useQuery({
    queryKey: ['notebook-content', group, name, compare],
    queryFn: () => api.notebookContent(group, name, compare ?? undefined),
    enabled: compare !== null,
  })
  const [scheduling, setScheduling] = useState<{ objectVersion: string } | null>(null)
  const [scheduled, setScheduled] = useState<PipelineView | null>(null)

  const failed = versions.isError && !versions.data
  // Without a notebook store the route answers 503: a fact about the installation, not a failure
  // a Retry could fix.
  const storeMissing = failed && versions.error instanceof ApiError && versions.error.status === 503
  const rows = versions.data ?? []
  // The versions this one can be compared with: every other one, never itself.
  const comparable = rows.filter((version) => version.objectVersion !== selected)
  const diff =
    compare && content.data && compareContent.data
      ? diffNotebooks(compareContent.data, content.data)
      : null

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
                        onClick={() => {
                          setPicked(version.objectVersion)
                          // A newly picked version starts without a comparison.
                          setCompare(null)
                        }}
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
                          setCompare(null)
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

      {rows.length > 0 && (
        <section className="card p-5">
          <h2 className="text-lg font-semibold break-words">
            Preview of {shortVersion(selected)}
          </h2>
          <p className="mt-1 text-sm text-text-mute">
            What this version holds: its cells, without outputs — publishing strips them.
          </p>
          {comparable.length > 0 && (
            <label className="mt-2 flex flex-wrap items-center gap-2 text-sm">
              <span className="text-text-dim">Compare with</span>
              <select
                className="input py-1"
                value={compare ?? ''}
                onChange={(event) => setCompare(event.target.value || null)}
              >
                <option value="">No comparison</option>
                {comparable.map((version) => (
                  <option key={version.objectVersion} value={version.objectVersion}>
                    {shortVersion(version.objectVersion)} · {formatTime(version.uploadedAt)}
                  </option>
                ))}
              </select>
            </label>
          )}
          {content.isLoading && <p className="mt-2 text-sm text-text-mute">Loading the notebook…</p>}
          {content.isError && (
            <div className="mt-2">
              <ErrorBox error={content.error} onRetry={() => void content.refetch()} />
            </div>
          )}
          {compareContent.isError && (
            <div className="mt-2">
              <ErrorBox error={compareContent.error} onRetry={() => void compareContent.refetch()} />
            </div>
          )}
          {content.data && (
            <div className="mt-3">
              <NotebookFrame title={`Preview of ${shortVersion(selected)}`} html={renderNotebook(content.data).html} />
            </div>
          )}
        </section>
      )}

      {diff && (
        <section className="card p-5" data-testid="notebook-diff">
          <h2 className="text-lg font-semibold break-words">
            What changed: {shortVersion(compare)} → {shortVersion(selected)}
          </h2>
          <p className="mt-1 text-sm text-text-dim">
            {diff.changed} changed · {diff.added} added · {diff.removed} removed
          </p>
          {diff.cells.length === 0 && (
            <p className="mt-2 text-sm text-text-mute">The two versions hold the same cells.</p>
          )}
          {diff.cells.map((cell) => (
            <div key={cell.index} className="mt-3">
              <p className="text-xs text-text-mute">
                Cell {cell.index + 1} · {cell.kind}
              </p>
              <pre className="mt-1 overflow-x-auto rounded-lg bg-bg-soft px-3 py-2 font-mono text-xs whitespace-pre-wrap break-words">
                {cell.lines.map((line, i) => (
                  <div
                    key={i}
                    data-kind={line.kind}
                    className={
                      line.kind === 'add' ? 'text-ok' : line.kind === 'del' ? 'text-bad' : 'text-text-mute'
                    }
                  >
                    {line.kind === 'add' ? '+ ' : line.kind === 'del' ? '- ' : '  '}
                    {line.text}
                  </div>
                ))}
              </pre>
            </div>
          ))}
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

      <section className="card p-5" data-testid="notebook-runs">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="min-w-0">
            <h2 className="text-lg font-semibold">Runs</h2>
            <p className="mt-1 text-sm text-text-mute">
              The last runs of every schedule that runs this notebook, newest first.
            </p>
          </div>
          <Link to="/runs?kind=notebook" className="btn-ghost py-1 text-xs">All runs</Link>
        </div>
        {runs.isLoading && <p className="mt-2 text-sm text-text-mute">Loading runs…</p>}
        {runs.isError && (
          <div className="mt-2"><ErrorBox error={runs.error} onRetry={() => void runs.refetch()} /></div>
        )}
        {runs.data && runs.data.runs.length === 0 && (
          <p className="mt-2 text-sm text-text-mute">
            No run of this notebook yet. A schedule appears above under “Where it is used”; a run
            started from the pipeline page shows up here too.
          </p>
        )}
        {runs.data && runs.data.runs.length > 0 && (
          <table className="mt-3 w-full text-sm">
            <caption className="sr-only">Runs of the schedules that run {name}, newest first.</caption>
            <thead className="bg-bg-soft/50">
              <tr>
                <th scope="col" className="th px-2 py-2">State</th>
                <th scope="col" className="th px-2 py-2">Started</th>
                <th scope="col" className="th px-2 py-2">Took</th>
                <th scope="col" className="th px-2 py-2">Trigger</th>
                <th scope="col" className="th px-2 py-2">Schedule</th>
              </tr>
            </thead>
            <tbody>
              {runs.data.runs.map((row) => {
                const started = formatWhen(row.startedAt)
                return (
                  <tr key={`${row.name}/${row.runId}`} className="border-t border-border/70">
                    <td className="td px-2"><RunStateBadge state={row.state} /></td>
                    <td className="td px-2 whitespace-nowrap" title={started.title}>{started.text}</td>
                    <td className="td px-2 tabular-nums">{runDuration(row.durationSeconds)}</td>
                    <td className="td px-2 text-xs">{row.triggeredBy ?? row.type}</td>
                    <td className="td px-2">
                      <Link
                        to={`/pipelines/${encodeURIComponent(row.namespace)}/${encodeURIComponent(row.name)}`}
                        className="font-mono text-xs underline break-all"
                      >
                        {row.name}
                      </Link>
                    </td>
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
