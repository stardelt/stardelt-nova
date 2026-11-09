import { Fragment, useId, useMemo, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { ApiError, api, errorMessage, type PipelinePrincipal, type PipelineView, type PublishedNotebook } from '../api'
import { formatBytes, formatTime, shortVersion } from '../format'
import { EmptyState } from './EmptyState'
import ErrorBox from './ErrorBox'
import ScheduleDialog from './ScheduleDialog'

const EMPTY =
  'No notebook is published in your groups yet. Publish one from JupyterLab with stardelt.publish("file.ipynb").'

/**
 * The notebooks the caller's groups have published (`GET /api/notebooks`, stardelt-nova#225): the
 * newest version of each, with a row that opens the versions and the parameters. The API scopes the
 * list to the caller's groups; the filter only chooses among the groups the answer contains.
 *
 * Every member of the owning group may schedule a notebook of the group (stardelt-nova#439). It runs
 * as the group's principal (`GET /api/pipelines/principals`), not as the person who published it.
 */
export default function PublishedNotebooks() {
  const list = useQuery({ queryKey: ['notebooks'], queryFn: api.notebooks })
  const principals = useQuery({ queryKey: ['pipeline-principals'], queryFn: api.pipelinePrincipals })
  const queryClient = useQueryClient()
  const [group, setGroup] = useState('')
  const [scheduling, setScheduling] = useState<PublishedNotebook | null>(null)
  const [scheduled, setScheduled] = useState<PipelineView | null>(null)
  const rows = useMemo(() => (Array.isArray(list.data) ? list.data : []), [list.data])
  const groups = useMemo(() => [...new Set(rows.map((row) => row.group))].sort(), [rows])
  const shown = useMemo(
    () => rows
      .filter((row) => group === '' || row.group === group)
      .sort((a, b) => a.group.localeCompare(b.group) || a.name.localeCompare(b.name)),
    [rows, group],
  )
  const failed = list.isError && !list.data
  // Without a notebook store the route answers 503: a fact about the installation, not a failure
  // a Retry could fix.
  const storeMissing = failed && list.error instanceof ApiError && list.error.status === 503
  const filterId = useId()

  return (
    <>
    <section aria-labelledby={`${filterId}-title`} className="card">
      <div className="flex flex-wrap items-center justify-between gap-3 px-5 py-3">
        <div className="min-w-0">
          <h2 id={`${filterId}-title`} className="text-lg font-semibold tracking-tight">Published notebooks</h2>
          <p className="text-xs text-text-mute">
            The newest version of every notebook your groups have published. Every member of the owning
            group may schedule it. A schedule runs as the group&apos;s service principal, not as the person
            who published the notebook, and reads only what the catalogs grant to that principal.
          </p>
        </div>
        {groups.length > 1 && (
          <div className="flex items-center gap-2 text-sm">
            <label htmlFor={filterId} className="text-text-dim">Group</label>
            <select
              id={filterId}
              className="input py-1"
              value={group}
              onChange={(event) => setGroup(event.target.value)}
            >
              <option value="">All groups</option>
              {groups.map((name) => <option key={name} value={name}>{name}</option>)}
            </select>
          </div>
        )}
      </div>
      {scheduled && (
        <p role="status" className="px-5 pb-3 text-sm">
          Scheduled {scheduled.name}.{' '}
          <Link
            to={`/pipelines/${encodeURIComponent(scheduled.namespace)}/${encodeURIComponent(scheduled.name)}`}
            className="underline"
          >
            Open it
          </Link>
        </p>
      )}
      {list.isLoading && <p className="px-5 pb-4 text-sm text-text-mute">Loading published notebooks…</p>}
      {failed && (
        <div className="px-5 pb-4">
          <ErrorBox error={list.error} onRetry={storeMissing ? undefined : () => void list.refetch()} />
        </div>
      )}
      {list.data && rows.length === 0 && (
        <EmptyState icon="notebook" title="No published notebooks yet">
          {EMPTY}
        </EmptyState>
      )}
      {shown.length > 0 && (
        <div className="relative max-h-[22rem] overflow-auto border-t border-border/70">
          <table className="w-full text-sm">
            <caption className="sr-only">Notebooks published in your groups, newest version of each.</caption>
            <thead className="bg-bg-soft/50">
              <tr>
                <th scope="col" className="th px-5 py-2.5">Notebook</th>
                <th scope="col" className="th px-3 py-2.5">Group</th>
                <th scope="col" className="th px-3 py-2.5">Version</th>
                <th scope="col" className="th px-3 py-2.5">Size</th>
                <th scope="col" className="th px-3 py-2.5">Published</th>
                <th scope="col" className="th px-3 py-2.5">Published by</th>
                <th scope="col" className="th px-3 py-2.5">Runs as</th>
                <th scope="col" className="th px-3 py-2.5"><span className="sr-only">Actions</span></th>
              </tr>
            </thead>
            <tbody>
              {shown.map((row) => (
                <NotebookRow
                  key={`${row.group}/${row.name}`}
                  row={row}
                  principal={principals.data?.groups.find((candidate) => candidate.group === row.group) ?? null}
                  principalsPending={principals.isLoading}
                  principalsError={principals.isError ? principals.error : null}
                  onSchedule={setScheduling}
                />
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
    {scheduling && (
      <ScheduleDialog
        mode="create"
        initialKind="notebook"
        initialNotebook={{ group: scheduling.group, name: scheduling.name, objectVersion: scheduling.objectVersion }}
        onClose={() => setScheduling(null)}
        onScheduled={(view) => {
          setScheduling(null)
          setScheduled(view)
          void queryClient.invalidateQueries({ queryKey: ['pipelines'] })
        }}
      />
    )}
    </>
  )
}

function NotebookRow({
  row,
  principal,
  principalsPending,
  principalsError,
  onSchedule,
}: {
  row: PublishedNotebook
  principal: PipelinePrincipal | null
  principalsPending: boolean
  principalsError: unknown
  onSchedule: (row: PublishedNotebook) => void
}) {
  const [open, setOpen] = useState(false)
  const detailId = useId()
  const runsAsId = useId()
  const ready = principal?.ready === true
  return (
    <Fragment>
      <tr className="border-t border-border/70 hover:bg-bg-soft/30">
        <td className="td px-5">
          <Link
            className="underline font-mono text-xs break-all"
            to={`/notebooks/${encodeURIComponent(row.group)}/${encodeURIComponent(row.name)}`}
          >
            {row.name}
          </Link>
        </td>
        <td className="td px-3">{row.group}</td>
        <td className="td px-3">
          <span className="font-mono text-xs" title={row.objectVersion}>{shortVersion(row.objectVersion)}</span>
        </td>
        <td className="td px-3 tabular-nums">{formatBytes(row.size)}</td>
        <td className="td px-3">{formatTime(row.uploadedAt)}</td>
        <td className="td px-3">{row.uploadedBy}</td>
        <td className="td px-3">
          <div id={runsAsId} className="text-xs">
            {principalsPending ? (
              <span className="text-text-mute">Checking…</span>
            ) : principalsError ? (
              <span className="text-text-mute break-words">{errorMessage(principalsError)}</span>
            ) : ready ? (
              <>
                <span className="font-mono">{principal?.principal}</span>{' '}
                <span className="badge border-emerald-400/40 bg-emerald-400/10 text-ok">ready</span>
              </>
            ) : (
              <>
                <span className="badge border-border bg-bg-soft text-text-mute">not ready</span>
                <span className="block text-text-mute break-words">
                  {principal?.message ?? 'This group has no pipeline principal yet.'}
                </span>
              </>
            )}
          </div>
        </td>
        <td className="td px-3 text-right">
          <div className="flex justify-end gap-1">
            <button
              type="button"
              className="btn-ghost py-1 text-xs"
              aria-expanded={open}
              aria-controls={detailId}
              onClick={() => setOpen((value) => !value)}
            >
              Versions<span className="sr-only"> of {row.name} in {row.group}</span>
            </button>
            <button
              type="button"
              className="btn-primary py-1 text-xs"
              disabled={!ready}
              aria-describedby={runsAsId}
              onClick={() => onSchedule(row)}
            >
              Schedule<span className="sr-only"> {row.name} in {row.group}</span>
            </button>
          </div>
        </td>
      </tr>
      {open && (
        <tr id={detailId} className="border-t border-border/70 bg-bg-soft/20">
          <td colSpan={8} className="px-5 py-3"><NotebookDetail row={row} /></td>
        </tr>
      )}
    </Fragment>
  )
}

/** Mounted only while a row is open, so a closed page asks the store for nothing but the list. */
function NotebookDetail({ row }: { row: PublishedNotebook }) {
  const versions = useQuery({
    queryKey: ['notebook-versions', row.group, row.name],
    queryFn: () => api.notebookVersions(row.group, row.name),
  })
  const parameters = useQuery({
    queryKey: ['notebook-parameters', row.group, row.name, 'newest'],
    queryFn: () => api.notebookParameters(row.group, row.name),
  })
  return (
    <div className="grid gap-4 md:grid-cols-2">
      <div className="min-w-0">
        <h3 className="text-sm font-semibold">Versions</h3>
        {versions.isLoading && <p className="text-xs text-text-mute">Loading versions…</p>}
        {versions.isError && <ErrorBox error={versions.error} onRetry={() => void versions.refetch()} />}
        {versions.data && (
          <ul className="mt-1 space-y-1 text-xs">
            {versions.data.map((version) => (
              <li key={version.objectVersion} className="break-words">
                <span className="font-mono" title={version.objectVersion}>{shortVersion(version.objectVersion)}</span>
                {' · '}{formatBytes(version.size)}
                {' · '}{formatTime(version.uploadedAt)}
                {' · '}{version.uploadedBy}
                {' · sha256 '}<span className="font-mono">{version.sha256.slice(0, 12)}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
      <div className="min-w-0">
        <h3 className="text-sm font-semibold">Parameters of the newest version</h3>
        {parameters.isLoading && <p className="text-xs text-text-mute">Loading parameters…</p>}
        {parameters.isError && <ErrorBox error={parameters.error} onRetry={() => void parameters.refetch()} />}
        {parameters.data && parameters.data.parameters.length === 0 && (
          <p className="text-xs text-text-mute">This notebook has no parameters cell.</p>
        )}
        {parameters.data && parameters.data.parameters.length > 0 && (
          <ul className="mt-1 space-y-1 text-xs">
            {parameters.data.parameters.map((parameter) => (
              <li key={parameter.name} className="break-words">
                <span className="font-mono">{parameter.name}</span>
                {' = '}<span className="font-mono">{parameter.default}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}
