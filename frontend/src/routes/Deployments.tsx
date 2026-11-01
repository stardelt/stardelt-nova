import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api, type PipelineView } from '../api'
import ErrorBox from '../components/ErrorBox'
import { shortRevision } from '../format'
import { PipelineStateBadge, formatWhen } from './Pipelines'

/**
 * Deployments: the pipelines that `stardelt bundle deploy` published, one block per bundle and
 * stage (target) — which commit each runs, who deployed it and when, and whether anyone changed it
 * since (`deployment.modified`). It only reads the pipeline list; a pipeline made in the interface
 * or with `stardelt apply` carries no stamp and is not listed (the Pipelines page has it).
 */

type Stage = { bundle: string; target: string; rows: PipelineView[] }

/** The deployed pipelines grouped by bundle and target, groups and rows in name order. */
export function stages(pipelines: PipelineView[]): Stage[] {
  const found = new Map<string, Stage>()
  for (const pipeline of pipelines) {
    const stamp = pipeline.deployment
    if (!stamp) continue
    const key = `${stamp.bundle}/${stamp.target}`
    const stage = found.get(key) ?? { bundle: stamp.bundle, target: stamp.target, rows: [] }
    stage.rows.push(pipeline)
    found.set(key, stage)
  }
  return [...found.values()]
    .map((stage) => ({ ...stage, rows: [...stage.rows].sort((a, b) => a.name.localeCompare(b.name)) }))
    .sort((a, b) => a.bundle.localeCompare(b.bundle) || a.target.localeCompare(b.target))
}

/** "commit 0123abc" when every row runs one commit, else how many differ. */
function commitsOf(rows: PipelineView[]): string {
  const distinct = new Set(rows.map((row) => row.deployment?.revision ?? ''))
  if (distinct.size > 1) return `${distinct.size} different commits`
  const [only] = [...distinct]
  return only ? `commit ${shortRevision(only)}` : 'no commit recorded'
}

export default function Deployments() {
  const list = useQuery({ queryKey: ['pipelines'], queryFn: api.pipelines, refetchInterval: 30_000 })
  const groups = stages(list.data?.pipelines ?? [])
  const failed = list.isError && !list.data

  return (
    <div className="max-w-6xl space-y-5">
      <header className="min-w-0">
        <h1 className="text-2xl font-bold tracking-tight">Deployments</h1>
        <p className="text-text-dim text-sm mt-1">
          What <span className="font-mono text-xs">stardelt bundle deploy</span> published, by bundle and stage:
          which commit each pipeline runs, who deployed it, and whether anyone changed it since.
        </p>
      </header>

      {list.isLoading && <p className="text-sm text-text-mute">Reading deployments…</p>}
      {failed && (
        <div className="card px-5 py-5">
          <ErrorBox error={list.error} onRetry={() => void list.refetch()} />
        </div>
      )}
      {list.data && groups.length === 0 && (
        <div className="card px-5 py-5 text-sm text-text-mute">
          Nothing was deployed from a bundle yet. Pipelines made here or with{' '}
          <span className="font-mono text-xs">stardelt apply</span> are on the{' '}
          <Link to="/pipelines" className="underline">Pipelines</Link> page.
        </div>
      )}

      {groups.map((stage, index) => {
        const headingId = `stage-${index}`
        return (
          <section key={`${stage.bundle}/${stage.target}`} aria-labelledby={headingId} className="space-y-2">
            <div className="min-w-0">
              <h2 id={headingId} className="text-lg font-semibold break-words">
                {stage.bundle} · {stage.target}
              </h2>
              <p className="text-sm text-text-mute">
                {stage.rows.length} {stage.rows.length === 1 ? 'pipeline' : 'pipelines'} · {commitsOf(stage.rows)}
              </p>
            </div>
            <div className="card relative overflow-x-auto">
              <table className="w-full text-sm">
                <caption className="sr-only">
                  Pipelines deployed from the bundle {stage.bundle} to the stage {stage.target}.
                </caption>
                <thead className="bg-bg-soft/50">
                  <tr>
                    <th scope="col" className="th px-5 py-2.5">Pipeline</th>
                    <th scope="col" className="th px-3 py-2.5">State</th>
                    <th scope="col" className="th px-3 py-2.5">Commit</th>
                    <th scope="col" className="th px-3 py-2.5">Deployed by</th>
                    <th scope="col" className="th px-3 py-2.5">Deployed</th>
                    <th scope="col" className="th px-3 py-2.5">Since then</th>
                  </tr>
                </thead>
                <tbody>
                  {stage.rows.map((pipeline) => {
                    const stamp = pipeline.deployment
                    const when = formatWhen(stamp?.deployedAt)
                    return (
                      <tr key={`${pipeline.namespace}/${pipeline.name}`} className="border-t border-border/70 hover:bg-bg-soft/30">
                        <td className="td px-5">
                          <Link
                            to={`/pipelines/${encodeURIComponent(pipeline.namespace)}/${encodeURIComponent(pipeline.name)}`}
                            className="font-medium underline break-words"
                          >
                            {pipeline.name}
                          </Link>
                        </td>
                        <td className="td px-3"><PipelineStateBadge state={pipeline.state} /></td>
                        <td className="td px-3 font-mono text-xs" title={stamp?.revision}>
                          {stamp?.revision ? shortRevision(stamp.revision) : '—'}
                        </td>
                        <td className="td px-3 break-words">{stamp?.deployedBy ?? '—'}</td>
                        <td className="td px-3 whitespace-nowrap" title={when.title}>{when.text}</td>
                        <td className="td px-3">
                          {stamp?.modified ? (
                            <span className="badge border-amber-400/40 bg-amber-400/10 text-warn">Changed since deploy</span>
                          ) : (
                            <span className="text-text-mute">—</span>
                          )}
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          </section>
        )
      })}
    </div>
  )
}
