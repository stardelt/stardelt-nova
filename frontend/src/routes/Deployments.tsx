import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api, type PipelineView, type StageRow } from '../api'
import ErrorBox from '../components/ErrorBox'
import { SkeletonTable } from '../components/Skeleton'
import { shortRevision } from '../format'
import { PipelineStateBadge, formatWhen } from './Pipelines'

/**
 * Deployments: the pipelines that `stardelt bundle deploy` published, one block per bundle and
 * stage (target) — which commit each runs, who deployed it and when, and whether anyone changed it
 * since (`deployment.modified`). It only reads the pipeline list; a pipeline made in the interface
 * or with `stardelt apply` carries no stamp, so the card below says a rebuild of the stage from Git
 * would not bring it back.
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

/** Pipelines a rebuild from Git would not bring back: no bundle stamp, or changed since deploy. */
export function notInGit(pipelines: PipelineView[]) {
  return {
    unstamped: pipelines.filter((p) => !p.deployment).sort((a, b) => a.name.localeCompare(b.name)),
    changed: pipelines.filter((p) => p.deployment?.modified).length,
  }
}

/** "commit 0123abc" when every row runs one commit, else how many differ. */
function commitsOf(rows: PipelineView[]): string {
  const distinct = new Set(rows.map((row) => row.deployment?.revision ?? ''))
  if (distinct.size > 1) return `${distinct.size} different commits`
  const [only] = [...distinct]
  return only ? `commit ${shortRevision(only)}` : 'no commit recorded'
}

/** Where a pipeline stands between this stage and the peer. No Git ancestry, no "ahead/behind". */
export function compareStatus(here: StageRow | undefined, there: StageRow | undefined, peer: string): string {
  if (here && !there) return 'only here'
  if (!here && there) return `only in ${peer}`
  if (!here || !there) return '—'
  if (there.modified) return `changed in ${peer}`
  if ((here.revision ?? '') === (there.revision ?? '')) return 'same'
  return 'different commit'
}

/**
 * The forge's compare page from the peer's commit to this stage's (GitLab uses `/-/compare`).
 * `null` when the repository is not a URL with a host: a peer may store a value this Nova
 * would refuse, and one bad row must not blank the page (nova#676).
 */
export function compareUrl(repository: string, from: string, to: string): string | null {
  const base = repository.replace(/\/+$/, '')
  let host: string
  try {
    host = new URL(base).hostname
  } catch {
    return null
  }
  const path = host.includes('gitlab') ? '/-/compare/' : '/compare/'
  return `${base}${path}${from.replace(/-dirty$/, '')}...${to.replace(/-dirty$/, '')}`
}

/** The rows of the comparison, keyed by bundle/target/name, sorted like the page's sections. */
export function comparisonRows(here: StageRow[], there: StageRow[]) {
  const key = (row: StageRow) => `${row.bundle}\^@${row.target}\^@${row.name}`
  const found = new Map<string, { here?: StageRow; there?: StageRow }>()
  for (const row of here) found.set(key(row), { ...(found.get(key(row)) ?? {}), here: row })
  for (const row of there) found.set(key(row), { ...(found.get(key(row)) ?? {}), there: row })
  const sample = (pair: { here?: StageRow; there?: StageRow }) => (pair.here ?? pair.there) as StageRow
  return [...found.entries()]
    .map(([k, pair]) => ({ key: k, ...pair }))
    .sort((a, b) => {
      const x = sample(a), y = sample(b)
      return x.bundle.localeCompare(y.bundle) || x.target.localeCompare(y.target) || x.name.localeCompare(y.name)
    })
}

/** The compare table for one peer: what each stage runs and what differs. `keep401` lets the
 * backend's refusal reach the page in dev auth mode (nova#671): there is no token to forward,
 * so the answer is a sentence about the install, not a session that expired. */
function StageCompare({ peer, keep401 }: { peer: string; keep401: boolean }) {
  const compare = useQuery({
    queryKey: ['stages-compare', peer, keep401],
    queryFn: () => api.stagesCompare(peer, keep401),
    retry: false,
    staleTime: 30_000,
  })
  if (compare.isLoading) return <p className="text-sm text-text-mute">Comparing with {peer}…</p>
  if (compare.isError) {
    return (
      <div className="card px-5 py-5">
        <ErrorBox error={compare.error} onRetry={() => void compare.refetch()} />
      </div>
    )
  }
  const answer = compare.data
  if (!answer) return null
  if (answer.status !== 'ok') {
    return (
      <div className="card px-5 py-5 text-sm text-text-dim">
        {answer.detail ?? `Could not read ${peer}.`}
      </div>
    )
  }
  const rows = comparisonRows(answer.here, answer.there)
  return (
    <div className="card relative overflow-x-auto">
      <table className="w-full text-sm">
        <caption className="sr-only">The pipelines of this stage against {answer.peerUrl}.</caption>
        <thead className="bg-bg-soft/50">
          <tr>
            <th scope="col" className="th px-5 py-2.5">Bundle · stage</th>
            <th scope="col" className="th px-3 py-2.5">Pipeline</th>
            <th scope="col" className="th px-3 py-2.5">Here</th>
            <th scope="col" className="th px-3 py-2.5">{peer}</th>
            <th scope="col" className="th px-3 py-2.5">Status</th>
          </tr>
        </thead>
        <tbody>
          {rows.map(({ key, here, there }) => {
            const row = (here ?? there) as StageRow
            const promote =
              here?.repository && here.revision && there?.revision && here.revision !== there.revision
                ? compareUrl(here.repository, there.revision, here.revision)
                : null
            return (
              <tr key={key} className="border-t border-border/70 hover:bg-bg-soft/30">
                <td className="td px-5 font-mono text-xs break-words">{row.bundle} · {row.target}</td>
                <td className="td px-3 break-words">{row.name}</td>
                <td className="td px-3 font-mono text-xs" title={here?.revision}>
                  {here?.revision ? shortRevision(here.revision) : '—'}
                  {here?.modified ? ' · changed' : ''}
                </td>
                <td className="td px-3 font-mono text-xs" title={there?.revision}>
                  {there?.revision ? shortRevision(there.revision) : '—'}
                  {there?.modified ? ' · changed' : ''}
                </td>
                <td className="td px-3">
                  {compareStatus(here, there, peer) === 'only here' ||
                  compareStatus(here, there, peer).startsWith('only in') ? (
                    <span className="badge border-amber-400/40 bg-amber-400/10 text-warn">{compareStatus(here, there, peer)}</span>
                  ) : (
                    <span className="text-text-dim">{compareStatus(here, there, peer)}</span>
                  )}
                  {promote && (
                    <a
                      href={promote}
                      target="_blank"
                      rel="noreferrer"
                      className="ml-2 underline text-accent"
                    >
                      Promote via pull request
                    </a>
                  )}
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}

export default function Deployments() {
  const list = useQuery({ queryKey: ['pipelines'], queryFn: api.pipelines, refetchInterval: 30_000 })
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const peers = me.data?.peers ?? []
  const [peer, setPeer] = useState<string | null>(null)
  const groups = stages(list.data?.pipelines ?? [])
  const outside = notInGit(list.data?.pipelines ?? [])
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

      {peers.length > 0 && (
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-sm text-text-mute">Compare with</span>
          {peers.map((p) => (
            <button
              key={p.name}
              type="button"
              onClick={() => setPeer(peer === p.name ? null : p.name)}
              aria-pressed={peer === p.name}
              className={`badge ${peer === p.name ? 'border-accent/50 bg-accent/10 text-accent' : 'border-border text-text-dim hover:bg-bg-soft/40'}`}
              title={p.url}
            >
              {p.name}
            </button>
          ))}
          {peer && <span className="text-xs text-text-mute">reading {peers.find((p) => p.name === peer)?.url}</span>}
        </div>
      )}
      {peer && <StageCompare peer={peer} keep401={me.data?.auth_mode !== 'oidc'} />}

      {list.isLoading && (
        <div className="card p-5"><SkeletonTable rows={5} label="Reading deployments" /></div>
      )}
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

      {list.data && (outside.unstamped.length > 0 || outside.changed > 0) && (
        <section aria-labelledby="not-in-git" className="card px-5 py-4 border-amber-400/40 space-y-2">
          <h2 id="not-in-git" className="text-base font-semibold">Not in Git</h2>
          <p className="text-sm text-text-dim">
            A rebuild of this stage from Git would not bring these back. Add them to a bundle, or export them with{' '}
            <span className="font-mono text-xs">stardelt export</span>.
          </p>
          {outside.unstamped.length > 0 && (
            <ul className="text-sm flex flex-wrap gap-x-3 gap-y-1">
              {outside.unstamped.map((p) => (
                <li key={`${p.namespace}/${p.name}`}>
                  <Link to={`/pipelines/${encodeURIComponent(p.namespace)}/${encodeURIComponent(p.name)}`} className="underline break-words">{p.name}</Link>
                </li>
              ))}
            </ul>
          )}
          {outside.changed > 0 && (
            <p className="text-sm text-text-mute">
              {outside.changed} deployed {outside.changed === 1 ? 'pipeline was' : 'pipelines were'} changed since deploy; Git has the older version.
            </p>
          )}
        </section>
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
