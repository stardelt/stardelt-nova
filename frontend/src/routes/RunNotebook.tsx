import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, runNotebookUrl } from '../api'
import { EmptyState } from '../components/EmptyState'
import ErrorBox from '../components/ErrorBox'
import NotebookFrame from '../components/NotebookFrame'
import { renderNotebook } from '../notebookHtml'
import { PipelineStateBadge, formatWhen, runDuration } from './Pipelines'

/**
 * One run's executed notebook (#683): the copy the run wrote, every cell's output and the
 * traceback, opened at the cell that failed.
 *
 * The notebook is rendered into the sandboxed frame `NotebookFrame`, whose sandbox omits the
 * same-origin permission; its HTML, images and scripts can run but reach neither Nova's DOM nor
 * its API.
 */
export default function RunNotebook() {
  const { namespace = '', name = '', runId = '', taskId = '' } = useParams()
  const navigate = useNavigate()
  const [rerunning, setRerunning] = useState(false)

  const notebook = useQuery({
    queryKey: ['run-notebook', namespace, name, runId, taskId],
    queryFn: () => api.runNotebook(namespace, name, runId, taskId),
  })

  // The run's state is a fact about the run, not about the notebook: a failure here only
  // leaves the badge out.
  const run = useQuery({
    queryKey: ['pipeline-run', namespace, name, runId],
    queryFn: () => api.pipelineRun(namespace, name, runId),
  })

  // What ran: the pinned notebook version and its stored parameters. Like the run, a failure
  // here only leaves those parts out — the executed notebook is the page.
  const pipeline = useQuery({
    queryKey: ['pipeline', namespace, name],
    queryFn: () => api.pipeline(namespace, name),
  })
  const spec = pipeline.data?.tasks.find((t) => t.name === taskId)
  const started = formatWhen(run.data?.run.startedAt)
  const params = spec?.notebookParams && Object.keys(spec.notebookParams).length > 0 ? spec.notebookParams : null

  const rendered = notebook.data ? renderNotebook(notebook.data) : null
  const missing = notebook.isError && notebook.error instanceof ApiError && notebook.error.status === 404

  async function again() {
    setRerunning(true)
    try {
      await api.startPipelineRun(namespace, name)
      navigate(`/pipelines/${encodeURIComponent(namespace)}/${encodeURIComponent(name)}`)
    } finally {
      setRerunning(false)
    }
  }

  return (
    <div className="max-w-6xl space-y-5">
      <header className="space-y-2">
        <Link to={`/pipelines/${encodeURIComponent(namespace)}/${encodeURIComponent(name)}`} className="btn-ghost w-fit px-2 py-1 text-xs">
          ◂ {name}
        </Link>
        <div className="flex flex-wrap items-center gap-3">
          <h1 className="text-2xl font-bold tracking-tight break-all">Run {runId}</h1>
          {run.data && <PipelineStateBadge state={run.data.run.state} />}
        </div>
        <p className="text-sm text-text-dim" data-testid="run-provenance">
          {spec && (
            <>
              <span>{spec.notebook || 'notebook'}</span>
              {spec.objectVersion && (
                <span title={spec.objectVersion}> · version {spec.objectVersion.slice(0, 8)}</span>
              )}
            </>
          )}
          {run.data?.run.startedAt && (
            <span title={started.title}> · started {started.text}</span>
          )}
          {run.data && run.data.run.durationSeconds != null && (
            <span> · took {runDuration(run.data.run.durationSeconds)}</span>
          )}
          {run.data && (
            <span> · started by {run.data.run.triggeredBy ?? run.data.run.type}</span>
          )}
          {pipeline.data?.runAs && <span> · as {pipeline.data.runAs}</span>}
        </p>
      </header>

      {params && (
        <details className="text-sm">
          <summary className="cursor-pointer text-text-dim">Parameters ({Object.keys(params).length})</summary>
          <div className="mt-2 space-y-1 font-mono text-xs">
            {Object.entries(params).map(([key, value]) => (
              <div key={key}>
                {key} = {value}
              </div>
            ))}
          </div>
        </details>
      )}

      {missing ? (
        <div className="card p-2">
          <EmptyState icon="notebook" title="No executed notebook for this run">
            This run's executed notebook is not there: it was never written, or it expired. The task
            log still has what the scheduler saw.
          </EmptyState>
        </div>
      ) : notebook.isError ? (
        <ErrorBox error={notebook.error} onRetry={() => void notebook.refetch()} />
      ) : !rendered ? (
        <p className="text-sm text-text-mute">Loading the executed notebook…</p>
      ) : (
        <>
          {rendered.error && (
            <div role="alert" className="rounded-[10px] border border-bad/30 bg-bad/5 px-4 py-3 text-sm">
              <p className="text-text font-medium break-words">
                Failed in cell {(rendered.failedCell ?? 0) + 1}: {rendered.error.ename}: {rendered.error.evalue}
              </p>
              <details className="mt-2">
                <summary className="cursor-pointer text-xs text-text-dim">Traceback</summary>
                <pre className="mt-2 overflow-x-auto rounded-lg bg-bg-soft px-3 py-2 font-mono text-xs whitespace-pre-wrap break-words">
                  {rendered.error.traceback}
                </pre>
              </details>
            </div>
          )}

          <div className="flex flex-wrap items-center gap-2">
            <button type="button" className="btn-primary py-1.5" onClick={() => void again()} disabled={rerunning}>
              Run again
            </button>
            <a
              className="btn-ghost py-1.5"
              href={runNotebookUrl(namespace, name, runId, taskId, true)}
              download
            >
              Download
            </a>
          </div>

          <NotebookFrame title="Executed notebook" html={rendered.html} />
        </>
      )}
    </div>
  )
}
