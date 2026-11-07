import { useQuery } from '@tanstack/react-query'
import { api } from '../api'
import ErrorBox from '../components/ErrorBox'
import { formatWhen } from './Pipelines'

const NEUTRAL = 'border-border bg-bg-soft text-text-dim'
const GOOD = 'border-emerald-400/40 bg-emerald-400/10 text-ok'
const BAD = 'border-red-400/40 bg-red-400/10 text-bad'
const BUSY = 'border-sky-400/40 bg-sky-400/10 text-info'

type PhaseLook = { word: string; cls: string }

/** The phase words of `StreamView.phase`; the word is shown as-is for unknown values. */
const PHASES: Record<string, PhaseLook> = {
  Running: { word: 'Running', cls: GOOD },
  Stopped: { word: 'Stopped', cls: NEUTRAL },
  Failed: { word: 'Failed', cls: BAD },
  Pending: { word: 'Pending', cls: BUSY },
}

function PhaseBadge({ phase }: { phase: string }) {
  const look = PHASES[phase] ?? { word: phase || 'Unknown', cls: NEUTRAL }
  return <span className={`badge ${look.cls}`}>{look.word}</span>
}

export default function Streams() {
  const streams = useQuery({ queryKey: ['streams'], queryFn: api.streams, refetchInterval: 15_000 })

  return (
    <div className="flex flex-col gap-4">
      <div>
        <h1 className="text-2xl font-bold tracking-tight">Streams</h1>
        <p className="text-sm text-text-dim">
          Jobs that read a topic from the streaming bus and write an Iceberg table. The operator runs them; this page
          reports what it sees.
        </p>
      </div>

      {streams.isError && <ErrorBox error={streams.error} onRetry={() => streams.refetch()} />}

      {streams.data && streams.data.streams.length === 0 && (
        <p className="text-sm text-text-dim">
          No streams yet. A stream is created as a <span className="font-mono">StreamApp</span> resource; ask an
          administrator to add one.
        </p>
      )}

      {streams.data && streams.data.streams.length > 0 && (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-border text-left text-text-dim">
                <th className="py-2 pr-3 font-medium">Name</th>
                <th className="py-2 pr-3 font-medium">Phase</th>
                <th className="py-2 pr-3 font-medium">Source</th>
                <th className="py-2 pr-3 font-medium">Sink</th>
                <th className="py-2 pr-3 font-medium">Checkpoint</th>
                <th className="py-2 font-medium">Last error</th>
              </tr>
            </thead>
            <tbody>
              {streams.data.streams.map((s) => (
                <tr key={s.namespace + '/' + s.name} className="border-b border-border/60 align-top">
                  <td className="py-2 pr-3">
                    <span className="font-mono">{s.name}</span>
                    <div className="text-text-dim text-xs">{s.owner_group}</div>
                  </td>
                  <td className="py-2 pr-3">
                    <PhaseBadge phase={s.phase} />
                  </td>
                  <td className="py-2 pr-3">
                    {s.source_connection} · {s.source_topic}
                  </td>
                  <td className="py-2 pr-3">
                    <div>
                      {s.sink_catalog}.{s.sink_table}
                    </div>
                    <div className="text-xs text-text-dim">{s.sink_mode}</div>
                  </td>
                  <td className="py-2 pr-3">{formatWhen(s.last_checkpoint_at).text}</td>
                  <td className="py-2 text-bad text-xs break-words">{s.last_error ?? '—'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}
