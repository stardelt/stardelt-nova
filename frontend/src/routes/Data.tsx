import { useState, type ReactNode } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api, allTables, errorMessage, type UploadResult, type TransferResult } from '../api'
import ErrorBox from '../components/ErrorBox'
import { Icon } from '../components/Icon'
import { rowsLabel } from '../format'

const SAMPLE_CSV = `vendor_id,pickup_at,dropoff_at,passenger_count,trip_distance,fare_amount,tip_amount,total_amount,payment_type,pickup_borough
1,2026-10-02 08:00,2026-10-02 08:20,2,3.1,13.0,3.0,18.2,card,Manhattan
2,2026-10-02 08:35,2026-10-02 08:52,1,1.9,8.5,1.7,12.1,card,Brooklyn
1,2026-10-02 09:10,2026-10-02 09:28,3,4.2,16.5,3.9,23.4,cash,Queens`

const SAMPLE_JSON = JSON.stringify(
  [
    { zone_id: 1, zone: 'Manhattan', borough: 'Manhattan' },
    { zone_id: 2, zone: 'Brooklyn', borough: 'Brooklyn' },
  ],
  null,
  2,
)

export default function Data() {
  const [tab, setTab] = useState<'load' | 'transfer'>('load')
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })

  return (
    <div className="max-w-5xl space-y-6">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Load & transfer</h1>
        <p className="text-text-dim text-sm mt-1">
          Bring files into Iceberg, or move data between tables. Every operation is recorded as a job.
          {me.data?.demo && <span className="text-warn"> Demo mode writes to memory.</span>}
        </p>
      </header>

      <div className="flex gap-2">
        <Tab active={tab === 'load'} onClick={() => setTab('load')} icon="upload" label="Load data" />
        <Tab active={tab === 'transfer'} onClick={() => setTab('transfer')} icon="arrow" label="Transfer table" />
      </div>

      {tab === 'load' ? <LoadForm /> : <TransferForm />}
    </div>
  )
}

function Tab({ active, onClick, icon, label }: {
  active: boolean
  onClick: () => void
  icon: 'upload' | 'arrow'
  label: string
}) {
  return (
    <button
      onClick={onClick}
      aria-pressed={active}
      className={[
        'btn border',
        active
          ? 'bg-accent/15 border-accent/30 text-text'
          : 'border-border bg-bg-elev/40 text-text-dim hover:text-text',
      ].join(' ')}
    >
      <Icon name={icon} className="w-4 h-4" />
      {label}
    </button>
  )
}

function LoadForm() {
  const qc = useQueryClient()
  const [namespace, setNamespace] = useState('nyc_taxi')
  const [table, setTable] = useState('yellow_trips_copy')
  const [format, setFormat] = useState<'csv' | 'json'>('csv')
  const [content, setContent] = useState(SAMPLE_CSV)
  const [result, setResult] = useState<UploadResult | null>(null)
  const [error, setError] = useState<string | null>(null)

  const upload = useMutation({
    mutationFn: api.upload,
    onMutate: () => { setError(null); setResult(null) },
    onSuccess: r => {
      setResult(r)
      qc.invalidateQueries({ queryKey: ['catalog'] })
      qc.invalidateQueries({ queryKey: ['jobs'] })
    },
    onError: (e: unknown) => setError(errorMessage(e)),
  })

  return (
    <div className="grid lg:grid-cols-[minmax(0,1fr)_320px] gap-4">
      <div className="card p-5 space-y-4 min-w-0">
        <div className="grid sm:grid-cols-2 gap-4">
          <Field label="Schema">
            <input value={namespace} onChange={e => setNamespace(e.target.value)} className="input" placeholder="nyc_taxi" />
          </Field>
          <Field label="Table">
            <input value={table} onChange={e => setTable(e.target.value)} className="input" placeholder="yellow_trips" />
          </Field>
        </div>

        <div className="flex items-center gap-2">
          <span className="label mr-2">Format</span>
          {(['csv', 'json'] as const).map(f => (
            <button
              key={f}
              onClick={() => { setFormat(f); setContent(f === 'csv' ? SAMPLE_CSV : SAMPLE_JSON) }}
              aria-pressed={format === f}
              className={[
                'px-2.5 py-1 rounded-[8px] text-xs border',
                format === f
                  ? 'bg-accent/15 border-accent/30 text-text'
                  : 'border-border text-text-dim hover:text-text',
              ].join(' ')}
            >
              {f.toUpperCase()}
            </button>
          ))}
          <label className="ml-auto btn-ghost cursor-pointer">
            <Icon name="upload" className="w-4 h-4" />
            Choose file
            <input
              type="file"
              accept=".csv,.json,text/csv,application/json"
              className="hidden"
              onChange={e => {
                const input = e.currentTarget
                const file = input.files?.[0]
                // Reset so choosing the same file again (after editing it) fires onChange again.
                input.value = ''
                if (file) {
                  file.text().then(
                    text => {
                      setError(null)
                      setContent(text)
                      setFormat(file.name.toLowerCase().endsWith('.json') ? 'json' : 'csv')
                    },
                    (err: unknown) => setError(`Could not read ${file.name}: ${errorMessage(err)}`),
                  )
                }
              }}
            />
          </label>
        </div>

        <Field label="Content">
          <textarea
            value={content}
            onChange={e => setContent(e.target.value)}
            spellCheck={false}
            className="w-full h-56 bg-bg-soft/40 border border-border rounded-[10px] p-3 font-mono text-xs outline-hidden focus:border-accent/60 resize-y"
          />
        </Field>

        <div className="flex items-center gap-3">
          <button
            className="btn-primary"
            disabled={upload.isPending || !namespace.trim() || !table.trim() || !content.trim()}
            onClick={() => upload.mutate({ namespace: namespace.trim(), table: table.trim(), format, content })}
          >
            <Icon name="upload" className="w-4 h-4" />
            {upload.isPending ? 'Loading…' : 'Load into table'}
          </button>
          <span className="text-xs text-text-mute">
            Creates or replaces <span className="font-mono">{namespace}.{table}</span>
          </span>
        </div>

        {error && <FormError text={error} />}
        {result && (
          <div className="rounded-[10px] border border-emerald-400/30 bg-emerald-400/10 p-4 text-sm">
            <div className="font-medium text-ok">
              Loaded {rowsLabel(result.rows_loaded)} into {result.namespace}.{result.table}
            </div>
            <div className="text-text-dim mt-1 flex gap-3">
              <Link
                to={`/catalog?ns=${encodeURIComponent(result.namespace)}&tbl=${encodeURIComponent(result.table)}`}
                className="hover:text-text"
              >
                Open in catalog →
              </Link>
              <Link to="/jobs" className="hover:text-text">View job →</Link>
            </div>
          </div>
        )}
      </div>

      <aside className="card p-5 h-fit">
        <h3 className="text-sm font-semibold mb-2">How it works</h3>
        <ol className="text-sm text-text-dim space-y-2 list-decimal list-inside">
          <li>The file is parsed and column types are inferred.</li>
          <li>A table is created (or replaced) in the selected schema; the schema is created if it does not exist.</li>
          <li>Rows are written through the engine into Iceberg.</li>
          <li>The job appears under Jobs with a row count.</li>
        </ol>
        <div className="mt-4 pt-4 border-t border-border text-xs text-text-mute">
          CSV and JSON arrays of objects are supported. The whole file is sent in one request, so
          keep it small.
        </div>
      </aside>
    </div>
  )
}

function TransferForm() {
  const qc = useQueryClient()
  const tables = useQuery({ queryKey: ['catalog', 'all'], queryFn: allTables })
  const [source, setSource] = useState('')
  const [targetNs, setTargetNs] = useState('analytics')
  const [targetTable, setTargetTable] = useState('trips_copy')
  const [mode, setMode] = useState<'overwrite' | 'append'>('overwrite')
  const [result, setResult] = useState<TransferResult | null>(null)
  const [error, setError] = useState<string | null>(null)

  const transfer = useMutation({
    mutationFn: api.transfer,
    onMutate: () => { setError(null); setResult(null) },
    onSuccess: r => {
      setResult(r)
      qc.invalidateQueries({ queryKey: ['catalog'] })
      qc.invalidateQueries({ queryKey: ['jobs'] })
    },
    onError: (e: unknown) => setError(errorMessage(e)),
  })

  const refs = tables.data ?? []

  return (
    <div className="grid lg:grid-cols-[minmax(0,1fr)_320px] gap-4">
      <div className="card p-5 space-y-4 min-w-0">
        <Field label="Source table">
          <select value={source} onChange={e => setSource(e.target.value)} className="input">
            <option value="">Select a table…</option>
            {refs.map(r => (
              <option key={`${r.ns}.${r.name}`} value={`${r.ns}.${r.name}`}>{r.ns}.{r.name}</option>
            ))}
          </select>
          {tables.isError && <div className="mt-1.5"><ErrorBox error={tables.error} onRetry={() => tables.refetch()} /></div>}
        </Field>

        <div className="grid sm:grid-cols-2 gap-4">
          <Field label="Target schema">
            <input value={targetNs} onChange={e => setTargetNs(e.target.value)} className="input" />
          </Field>
          <Field label="Target table">
            <input value={targetTable} onChange={e => setTargetTable(e.target.value)} className="input" />
          </Field>
        </div>

        <Field label="Mode">
          <div className="flex gap-2">
            {(['overwrite', 'append'] as const).map(m => (
              <button
                key={m}
                onClick={() => setMode(m)}
                aria-pressed={mode === m}
                className={[
                  'px-3 py-1.5 rounded-[8px] text-xs border',
                  mode === m
                    ? 'bg-accent/15 border-accent/30 text-text'
                    : 'border-border text-text-dim hover:text-text',
                ].join(' ')}
              >
                {m}
              </button>
            ))}
          </div>
        </Field>

        <button
          className="btn-primary"
          disabled={transfer.isPending || !source || !targetNs.trim() || !targetTable.trim()}
          onClick={() => transfer.mutate({ source, target: `${targetNs.trim()}.${targetTable.trim()}`, mode })}
        >
          <Icon name="arrow" className="w-4 h-4" />
          {transfer.isPending ? 'Transferring…' : 'Transfer table'}
        </button>

        {error && <FormError text={error} />}
        {result && (
          <div className="rounded-[10px] border border-emerald-400/30 bg-emerald-400/10 p-4 text-sm">
            <div className="font-medium text-ok">
              Transferred {rowsLabel(result.rows)} → {result.target}
            </div>
            <Link to="/jobs" className="text-text-dim mt-1 inline-block hover:text-text">View job →</Link>
          </div>
        )}
      </div>

      <aside className="card p-5 h-fit">
        <h3 className="text-sm font-semibold mb-2">What this does</h3>
        <p className="text-sm text-text-dim">
          <span className="font-mono text-xs">overwrite</span> recreates the target from the source
          (<span className="font-mono text-xs">CREATE TABLE AS SELECT</span>).{' '}
          <span className="font-mono text-xs">append</span> inserts the source rows into the target.
        </p>
        <div className="mt-4 pt-4 border-t border-border text-xs text-text-mute">
          Cross-schema copies stay inside the same catalog, so lineage and policy apply to both ends.
        </div>
      </aside>
    </div>
  )
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="block">
      <span className="label block mb-1.5">{label}</span>
      {children}
    </label>
  )
}

/** A failed upload or transfer: the sentence from the API, without a retry (the form holds the input). */
function FormError({ text }: { text: string }) {
  return (
    <div className="rounded-[10px] border border-red-500/40 bg-red-500/10 p-3 text-sm text-bad font-mono whitespace-pre-wrap break-words">
      {text}
    </div>
  )
}
