import { useEffect, useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useSearchParams } from 'react-router-dom'
import { ApiError, api, errorMessage, formatCell, type IcebergField, type LineageNodeView, type QueryResult, type TableDocumentation, type TableMetadata } from '../api'
import { useCatalogs } from '../components/CatalogSelect'
import CatalogBreadcrumb from '../components/CatalogBreadcrumb'
import CatalogOverview from '../components/CatalogOverview'
import CatalogTree from '../components/CatalogTree'
import ErrorBox from '../components/ErrorBox'
import { Icon } from '../components/Icon'
import SchemaOverview from '../components/SchemaOverview'
import TableAbout, { type DocsState } from '../components/TableAbout'
import TableHealth from '../components/TableHealth'
import { formatCount, formatTime, sqlTableRef, rowsLabel, sqlIdent } from '../format'
import { isAdmin } from '../roles'

type Sel = { ns: string; tbl: string; kind?: 'table' | 'view' }

/** A 403 says no: the person's grants do not change by asking again. */
function forbidden(error: unknown): boolean {
  if (error instanceof ApiError) return error.status === 403
  return !!error && typeof error === 'object' && 'status' in error && (error as { status?: unknown }).status === 403
}

export default function Catalog() {
  const [params] = useSearchParams()
  // The catalog the person chose. Absent means the API's default, which is what every
  // request did before there were several catalogs.
  const catalog = params.get('catalog') ?? undefined
  // The schema page: `?catalog=C&ns=S` with no table (stardelt-nova#585). A table opened
  // from the tree lives in this page's state, so its URL still has no `tbl`.
  const nsParam = params.get('ns')
  const schema = nsParam && !params.get('tbl') ? nsParam : null
  const [selected, setSelected] = useState<Sel | null>(null)
  const [filter, setFilter] = useState(params.get('q') ?? '')

  // `?catalog=` and the two selection params arrive in one history entry, so the selection
  // is read from the URL rather than kept beside it: switching catalogs clears it.
  useEffect(() => {
    const ns = params.get('ns')
    const tbl = params.get('tbl')
    setSelected(ns && tbl ? { ns, tbl } : null)
  }, [params])

  useEffect(() => {
    const q = params.get('q')
    if (q) setFilter(q)
  }, [params])

  const catalogs = useCatalogs()
  // No catalog is open to this person: the API's own sentence, and no Retry — the list
  // can only change when an administrator grants a catalog (stardelt-nova#234).
  const denied = catalogs.isError && forbidden(catalogs.error)
  const noCatalogs = catalogs.data != null && (catalogs.data.catalogs?.length ?? 0) === 0
  // The catalog the Access line describes: `?catalog=`, else the API's default — the same
  // choice `TableDetail` and `CatalogSelect` make.
  const catalogName = catalog ?? catalogs.data?.default
  const currentCatalog = catalogs.data?.catalogs?.find((row) => row.name === catalogName)
  // The breadcrumb reads where the person is: a table opened from the tree is page state
  // (stardelt-nova#584), not a URL param, so the selection is part of it.
  const crumbNs = selected?.ns ?? schema
  const crumbTbl = selected?.tbl ?? null

  return (
    <div className="grid grid-cols-1 lg:grid-cols-[300px_minmax(0,1fr)] gap-5 h-full min-h-0">
      {/* A page has one h1: the table's, the schema's or the catalog's name is the heading below. */}
      {!selected && !schema && !currentCatalog && <h1 className="sr-only">Catalog</h1>}
      <aside className="card p-3 flex flex-col min-h-0">
        <div className="px-1 pb-2">
          <input
            value={filter}
            onChange={e => setFilter(e.target.value)}
            placeholder="Search tables…"
            aria-label="Filter tables"
            className="input py-1.5"
          />
        </div>
        <div className="overflow-auto flex-1">
          {denied ? (
            <div className="p-2"><ErrorBox error={catalogs.error} /></div>
          ) : noCatalogs ? (
            <div className="p-2">
              <div role="alert" className="text-bad text-sm flex flex-wrap items-center gap-x-3 gap-y-1">
                <span className="break-words">no catalog is open to you yet; ask an administrator for access</span>
              </div>
            </div>
          ) : catalogs.isError ? (
            <div className="p-2"><ErrorBox error={catalogs.error} onRetry={() => void catalogs.refetch()} /></div>
          ) : (
            <CatalogTree
              key={catalogs.data?.default ?? ''}
              catalogs={catalogs.data?.catalogs ?? []}
              defaultCatalog={catalogs.data?.default ?? ''}
              filter={filter}
              selected={selected}
              onSelect={setSelected}
            />
          )}
        </div>
      </aside>

      <section className="min-w-0 min-h-0 overflow-auto space-y-4">
        <CatalogBreadcrumb catalog={catalogName} ns={crumbNs} tbl={crumbTbl} />
        {selected ? (
          <TableDetail ns={selected.ns} tbl={selected.tbl} catalog={catalog} kind={selected.kind} />
        ) : schema ? (
          <SchemaOverview catalog={catalogName ?? ''} ns={schema} />
        ) : currentCatalog ? (
          <CatalogOverview catalog={currentCatalog} />
        ) : (
          <div className="card p-8 h-full grid place-items-center text-center">
            <div>
              <div className="text-text-dim">Select a table to inspect it.</div>
              <div className="text-text-mute text-sm mt-1">Or load a file to create one.</div>
              <Link to="/data" className="btn-ghost mt-4">Load data</Link>
            </div>
          </div>
        )}
      </section>
    </div>
  )
}

function TableDetail({ ns, tbl, catalog, kind }: Sel & { catalog?: string; kind?: 'table' | 'view' }) {
  const [tab, setTab] = useState<'preview' | 'profile' | 'columns' | 'details' | 'history' | 'lineage'>('preview')
  const qc = useQueryClient()
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const isView = kind === 'view'
  const meta = useQuery({
    queryKey: ['catalog', 'table', catalog, ns, tbl],
    queryFn: () => api.tableMeta(ns, tbl, catalog),
    enabled: !isView,
  })
  const docs = useQuery({
    queryKey: ['catalog', 'table-docs', catalog, ns, tbl],
    queryFn: () => api.tableDocs(ns, tbl, catalog),
    enabled: !isView,
  })
  const preview = useQuery({
    queryKey: ['preview', catalog, ns, tbl],
    queryFn: () => api.query(`SELECT * FROM ${sqlTableRef(ns, tbl, catalog)} LIMIT 50`),
    enabled: tab === 'preview',
  })

  /** A write answers the fresh documentation; it replaces the read's data for both halves. */
  const onDocsSaved = (answer: TableDocumentation) => {
    qc.setQueryData(['catalog', 'table-docs', catalog, ns, tbl], answer)
    // The Details tab shows the same catalog properties; let it read them again.
    void qc.invalidateQueries({ queryKey: ['catalog', 'table', catalog, ns, tbl] })
  }

  /** The editor opens the table in the catalog it was browsed in, not the default one. */
  const queryHref = (sql: string) => {
    const query = new URLSearchParams({ sql })
    if (catalog) query.set('catalog', catalog)
    return `/query?${query.toString()}`
  }
  // The lineage page takes the identifier as URL params. The catalog may be implicit
  // (`?catalog=` absent), so resolve the API's default the same way CatalogSelect does.
  const catalogs = useCatalogs()
  const catalogName = catalog ?? catalogs.data?.default ?? ''
  const lineageHref = catalogName
    ? `/lineage?${new URLSearchParams({ catalog: catalogName, schema: ns, table: tbl }).toString()}`
    : ''

  const m = meta.data?.metadata
  const schemaId = m?.['current-schema-id'] ?? m?.schemas[0]?.['schema-id'] ?? 0
  const schema = m?.schemas.find(s => s['schema-id'] === schemaId) ?? m?.schemas[0]
  const partSpec = m?.['partition-specs']?.[0]
  const tabs = isView
    ? (['preview'] as const)
    : (['preview', 'profile', 'columns', 'details', 'history', 'lineage'] as const)

  return (
    <div className="space-y-4">
      <header className="flex items-start justify-between gap-4 flex-wrap">
        <div>
          <div className="text-[11px] uppercase tracking-widest text-text-mute">{ns}</div>
          <h1 className="text-2xl font-bold tracking-tight font-mono flex items-center gap-2">
            {tbl}
            {isView && <span className="badge border-accent/40 bg-accent/10 text-accent">View</span>}
          </h1>
          {m && (
            <div className="text-xs text-text-mute mt-1 font-mono truncate max-w-[620px]">{m.location}</div>
          )}
        </div>
        <div className="flex gap-2">
          <Link to={queryHref(`SELECT * FROM ${sqlTableRef(ns, tbl, catalog)} LIMIT 100`)} className="btn-primary">
            {isView ? 'Query view' : 'Query table'}
          </Link>
          {lineageHref && (
            <Link to={lineageHref} className="btn-ghost" title="Show this table's lineage">Show lineage</Link>
          )}
          <Link to="/data" className="btn-ghost">Load data</Link>
        </div>
      </header>

      {!isView && <TableAbout ns={ns} tbl={tbl} catalog={catalog} me={me.data} docs={docs} onSaved={onDocsSaved} />}
      {!isView && <TableHealth ns={ns} tbl={tbl} catalog={catalog} />}

      <div className="flex gap-2">
        {tabs.map(t => (
          <button
            key={t}
            onClick={() => setTab(t)}
            aria-pressed={tab === t}
            className={[
              'px-3 py-1.5 rounded-[8px] text-sm border capitalize',
              tab === t
                ? 'bg-accent/15 border-accent/30 text-text'
                : 'border-border text-text-dim hover:text-text',
            ].join(' ')}
          >
            {t}
          </button>
        ))}
      </div>

      {meta.isError && <ErrorBox error={meta.error} onRetry={() => meta.refetch()} />}

      {tab === 'preview' && <Preview q={preview} />}

      {tab === 'profile' && <Profile ns={ns} tbl={tbl} catalog={catalog} fields={schema?.fields} />}

      {tab === 'columns' && (
        <ColumnsTab
          ns={ns}
          tbl={tbl}
          catalog={catalog}
          fields={schema?.fields}
          docs={docs}
          onSaved={onDocsSaved}
        />
      )}

      {tab === 'details' && meta.data && <Details meta={meta.data} partition={partSpec} />}

      {tab === 'history' && <HistoryTab ns={ns} tbl={tbl} catalog={catalog} />}

      {tab === 'lineage' && <LineageTab ns={ns} tbl={tbl} catalog={catalogName} />}
    </div>
  )
}

function ColumnsTab({ ns, tbl, catalog, fields, docs, onSaved }: {
  ns: string
  tbl: string
  catalog?: string
  fields?: IcebergField[]
  docs: DocsState
  onSaved: (doc: TableDocumentation) => void
}) {
  const canEdit = docs.data?.canEdit === true
  const descriptions = new Map((docs.data?.columns ?? []).map(c => [c.name, c.description]))
  const [editing, setEditing] = useState<string | null>(null)
  const [text, setText] = useState('')
  const [saving, setSaving] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const area = useRef<HTMLTextAreaElement>(null)
  const editButtons = useRef(new Map<string, HTMLButtonElement>())
  const pendingFocus = useRef<string | null>(null)

  useEffect(() => {
    if (editing !== null) {
      area.current?.focus()
    } else if (pendingFocus.current !== null) {
      editButtons.current.get(pendingFocus.current)?.focus()
      pendingFocus.current = null
    }
  }, [editing])

  function open(name: string) {
    setEditing(name)
    setText(descriptions.get(name) ?? '')
    setError(null)
    setNotice(null)
  }

  function close() {
    if (editing !== null) pendingFocus.current = editing
    setEditing(null)
    setError(null)
  }

  async function save() {
    if (editing === null) return
    const next = text.trim() === '' ? null : text.trim()
    if (next === (descriptions.get(editing) ?? null)) {
      close()
      return
    }
    setSaving(editing)
    setError(null)
    try {
      const answer = await api.putColumnDoc(ns, tbl, editing, next, catalog)
      onSaved(answer)
      setNotice('Saved.')
      pendingFocus.current = editing
      setEditing(null)
    } catch (e) {
      setError(errorMessage(e))
    } finally {
      setSaving(null)
    }
  }

  return (
    <div className="card overflow-hidden min-w-0">
      {notice && <p role="status" className="border-b border-border px-4 py-2 text-sm text-ok break-words">{notice}</p>}
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead className="bg-bg-soft/50">
            <tr>
              <th className="th px-4 py-2.5">#</th>
              <th className="th px-4 py-2.5">Column</th>
              <th className="th px-4 py-2.5">Type</th>
              <th className="th px-4 py-2.5">Required</th>
              <th className="th px-4 py-2.5">Description</th>
            </tr>
          </thead>
          <tbody>
            {fields?.map(f => (
              <tr key={f.id} className="border-t border-border/70">
                <td className="td px-4 text-text-mute font-mono">{f.id}</td>
                <td className="td px-4 font-mono break-words">{f.name}</td>
                <td className="td px-4 text-text-dim font-mono break-words">{typeof f.type === 'string' ? f.type : JSON.stringify(f.type)}</td>
                <td className="td px-4 text-text-mute">{f.required ? 'yes' : '—'}</td>
                <td className="td px-4 text-text-dim max-w-[32rem]">
                  {editing === f.name ? (
                    <div
                      className="min-w-[15rem] space-y-1"
                      onKeyDown={(e) => {
                        if (e.key === 'Escape') {
                          e.preventDefault()
                          close()
                        }
                      }}
                    >
                      <textarea
                        ref={area}
                        aria-label={`Description of ${f.name}`}
                        className="input min-h-[4rem] resize-y"
                        maxLength={4000}
                        value={text}
                        onChange={(e) => setText(e.target.value)}
                      />
                      <div className="text-right text-[11px] text-text-mute">{text.length} / 4000</div>
                      {error && <p role="alert" className="text-xs text-bad whitespace-pre-wrap break-words">{error}</p>}
                      <div className="flex flex-wrap gap-2">
                        <button
                          type="button"
                          className="btn-primary py-1 px-2 text-xs"
                          aria-label={`Save description of ${f.name}`}
                          disabled={saving === f.name}
                          onClick={() => void save()}
                        >
                          {saving === f.name ? 'Saving…' : 'Save'}
                        </button>
                        <button
                          type="button"
                          className="btn-ghost py-1 px-2 text-xs"
                          aria-label={`Cancel description of ${f.name}`}
                          onClick={close}
                        >
                          Cancel
                        </button>
                      </div>
                    </div>
                  ) : (
                    <div className="flex min-w-0 items-start gap-2">
                      <span className="min-w-0 flex-1 break-words">{descriptions.get(f.name) ?? '—'}</span>
                      {canEdit && (
                        <button
                          type="button"
                          ref={(el) => {
                            if (el) editButtons.current.set(f.name, el)
                            else editButtons.current.delete(f.name)
                          }}
                          aria-label={`Edit description of ${f.name}`}
                          className="btn-ghost shrink-0 py-0.5 px-2 text-xs"
                          onClick={() => open(f.name)}
                        >
                          <span aria-hidden="true">✎</span> Edit
                        </button>
                      )}
                    </div>
                  )}
                </td>
              </tr>
            ))}
            {!fields && <tr><td className="px-4 py-5 text-text-mute" colSpan={5}>Loading…</td></tr>}
          </tbody>
        </table>
      </div>
    </div>
  )
}


function Preview({ q }: { q: { data?: QueryResult; isLoading: boolean; error: unknown; refetch: () => void } }) {
  if (q.isLoading) return <div className="card p-6 text-text-mute text-sm">loading preview…</div>
  if (q.error) return <div className="card p-6"><ErrorBox error={q.error} onRetry={() => q.refetch()} /></div>
  if (!q.data) return null
  return (
    <div className="card overflow-hidden">
      <div className="px-4 py-2 text-xs text-text-mute border-b border-border flex justify-between">
        <span>{rowsLabel(q.data.row_count)} · preview</span>
        <span className="font-mono">{q.data.query_id}</span>
      </div>
      <div className="overflow-auto max-h-[60vh]">
        <table className="w-full text-sm font-mono">
          <thead className="bg-bg-soft/60 sticky top-0">
            <tr>{q.data.columns.map((c, i) => <th key={i} className="th px-4 py-2">{c.name}</th>)}</tr>
          </thead>
          <tbody>
            {q.data.rows.map((row, i) => (
              <tr key={i} className="border-t border-border/60">
                {row.map((cell, j) => <td key={j} className="td px-4 text-text-dim">{formatCell(cell)}</td>)}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}

/** Iceberg primitive types whose min/max Trino can order; everything else gets counts only. */
const ORDERABLE = /^(tinyint|smallint|int|long|float|double|decimal|date|timestamp|string|varchar)/

/**
 * A column profile over the first 10,000 rows (stardelt-nova#511): how many are null, how many
 * distinct values, and the range for a type Trino can order. One statement for every column, so
 * a wide table costs one query, and `LIMIT` rather than `TABLESAMPLE` so a small table still
 * has numbers. The card says what it sampled and never claims more.
 */
function Profile({ ns, tbl, catalog, fields }: {
  ns: string
  tbl: string
  catalog?: string
  fields?: IcebergField[]
}) {
  const stats = useQuery({
    queryKey: ['profile', catalog, ns, tbl, fields?.map(f => f.name).join(',')],
    queryFn: () => api.query(profileSql(ns, tbl, catalog, fields ?? [])),
    enabled: (fields?.length ?? 0) > 0,
    retry: false,
    staleTime: 60_000,
  })
  if (!fields) return <div className="card p-6 text-text-mute text-sm">loading columns…</div>
  if (stats.isLoading) return <div className="card p-6 text-text-mute text-sm">profiling the first 10,000 rows…</div>
  if (stats.isError) return <div className="card p-6"><ErrorBox error={stats.error} onRetry={() => stats.refetch()} /></div>
  const row = stats.data?.rows[0]
  const n = row ? Number(row[0]) : 0
  if (!row || n === 0) return <div className="card p-6 text-text-mute text-sm">The first 10,000 rows are empty, so there is nothing to profile.</div>
  const columns = fields.filter(f => typeof f.type === 'string')
  return (
    <div className="card overflow-x-auto">
      <div className="px-4 py-2 text-xs text-text-mute border-b border-border">
        Profiled from the first 10,000 rows · null % and distinct values
      </div>
      <table className="w-full text-sm">
        <thead className="bg-bg-soft/50">
          <tr>
            <th className="th px-4 py-2.5">Column</th>
            <th className="th px-4 py-2.5">Type</th>
            <th className="th px-4 py-2.5">Nulls</th>
            <th className="th px-4 py-2.5">Distinct</th>
            <th className="th px-4 py-2.5">Range</th>
          </tr>
        </thead>
        <tbody>
          {fields.map(f => {
            const i = columns.findIndex(c => c.name === f.name)
            const nulls = i < 0 ? null : Number(row[1 + i * 4])
            const distinct = i < 0 ? null : Number(row[2 + i * 4])
            return (
              <tr key={f.name} className="border-t border-border/60">
                <td className="td px-4 font-mono">{f.name}</td>
                <td className="td px-4 font-mono text-text-mute">{typeof f.type === 'string' ? f.type : 'structured'}</td>
                <td className="td px-4">{nulls == null ? '—' : `${((1 - nulls / n) * 100).toFixed(1)}% null`}</td>
                <td className="td px-4">{distinct == null ? '—' : formatCount(distinct)}</td>
                <td className="td px-4 font-mono text-text-dim">{rangeText(row, i, f.type)}</td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}

function profileSql(ns: string, tbl: string, catalog: string | undefined, fields: IcebergField[]): string {
  const parts = fields
    .filter(f => typeof f.type === 'string')
    .map((f, i) => {
      const col = sqlIdent(f.name)
      // Always four select items per column, so the row offsets stay 1 + i*4: a type
      // Trino cannot order gets two NULLs instead of min/max.
      const range = ORDERABLE.test(f.type as string)
        ? `min(${col}) AS c${i}_min, max(${col}) AS c${i}_max`
        : `NULL AS c${i}_min, NULL AS c${i}_max`
      return `count(${col}) AS c${i}_nn, approx_distinct(${col}) AS c${i}_d, ${range}`
    })
  const select = ['count(*) AS n', ...parts].join(', ')
  return `SELECT ${select}\nFROM (SELECT * FROM ${sqlTableRef(ns, tbl, catalog)} LIMIT 10000)`
}

function rangeText(row: unknown[], i: number, type: string | object): string {
  // Nested fields and types without an order are not profiled; the statement emits NULLs.
  if (i < 0 || typeof type !== 'string' || !ORDERABLE.test(type)) return '—'
  const min = row[3 + i * 4]
  const max = row[4 + i * 4]
  if (min == null && max == null) return '—'
  return `${formatCell(min)} … ${formatCell(max)}`
}

function HistoryTab({ ns, tbl, catalog }: Sel & { catalog?: string }) {
  const qc = useQueryClient()
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const history = useQuery({
    queryKey: ['catalog', 'history', catalog, ns, tbl],
    queryFn: () => api.tableHistory(ns, tbl, catalog),
  })
  const [previewId, setPreviewId] = useState<string | null>(null)
  const [rollbackTarget, setRollbackTarget] = useState<string | null>(null)
  const [confirmText, setConfirmText] = useState('')
  const [rollbackOk, setRollbackOk] = useState<string | null>(null)

  const preview = useQuery({
    queryKey: ['history-preview', catalog, ns, tbl, previewId],
    queryFn: () =>
      api.query(`SELECT * FROM ${sqlTableRef(ns, tbl, catalog)} FOR VERSION AS OF ${previewId} LIMIT 100`),
    enabled: previewId !== null,
  })

  const rollback = useMutation({
    mutationFn: (snapshotId: string) => api.rollback(ns, tbl, snapshotId, catalog),
    onSuccess: (_r, snapshotId) => {
      setRollbackOk(`Rolled back to snapshot ${snapshotId}.`)
      setRollbackTarget(null)
      setConfirmText('')
      qc.invalidateQueries({ queryKey: ['catalog', 'history', catalog, ns, tbl] })
      qc.invalidateQueries({ queryKey: ['preview', catalog, ns, tbl] })
      if (previewId !== null) qc.invalidateQueries({ queryKey: ['history-preview', catalog, ns, tbl, previewId] })
    },
  })

  const docs = useQuery({
    queryKey: ['catalog', 'table-docs', catalog, ns, tbl],
    queryFn: () => api.tableDocs(ns, tbl, catalog),
  })
  const admin = isAdmin(me.data)
  const canRollback = docs.data?.canRollback === true
  const rollbackRule = docs.data?.rollbackRule
  const cols = history.data?.columns ?? []
  const idx = (name: string) => cols.findIndex(c => c.name === name)
  const iTime = idx('committed_at')
  const iSnap = idx('snapshot_id')
  const iOp = idx('operation')
  const iSummary = idx('summary')

  if (history.isLoading || docs.isLoading) return <div className="card p-6 text-text-mute text-sm">loading history…</div>
  if (history.isError) return <div className="card p-6"><ErrorBox error={history.error} onRetry={() => history.refetch()} /></div>
  const rows = history.data?.rows ?? []
  if (rows.length === 0) {
    return (
      <div className="space-y-4">
        <div className="card p-6 text-text-mute text-sm">
          No snapshots recorded. {me.data?.demo ? 'Demo mode has no table history.' : 'The table has no snapshots yet.'}
        </div>
        {!canRollback && rollbackRule && (
          <p className="text-text-mute text-xs break-words">{rollbackRule}</p>
        )}
      </div>
    )
  }

  const snapIdOf = (row: unknown[]): string | null => {
    if (iSnap < 0) return null
    const v = row[iSnap]
    // The backend sends ids as strings to keep all 64 bits through JSON.parse;
    // an old backend may still send a safe number. Anything else is unusable.
    if (typeof v === 'string' && /^-?\d+$/.test(v)) return v
    if (typeof v === 'number' && Number.isSafeInteger(v)) return String(v)
    return null
  }

  const summaryText = (row: unknown[]): string => {
    if (iSummary < 0) return '—'
    const v = row[iSummary] as unknown
    if (v === null || v === undefined) return '—'
    if (typeof v === 'object') {
      const o = v as Record<string, unknown>
      // Iceberg snapshot summaries carry row/file counts under various keys;
      // surface the familiar ones when present, else the raw map.
      const picks = ['total-records', 'added-records', 'deleted-records', 'added-data-files', 'deleted-data-files']
        .filter(k => o[k] !== undefined)
        .map(k => `${k}=${String(o[k])}`)
      if (picks.length > 0) return picks.join(' · ')
      return JSON.stringify(o)
    }
    return String(v)
  }

  return (
    <div className="space-y-4">
      <div className="card overflow-x-auto">
        <table className="w-full text-sm">
          <thead className="bg-bg-soft/50">
            <tr>
              <th className="th px-4 py-2.5">Time</th>
              <th className="th px-4 py-2.5">Operation</th>
              <th className="th px-4 py-2.5">Snapshot</th>
              <th className="th px-4 py-2.5">Summary</th>
              <th className="th px-4 py-2.5"><span className="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row, i) => {
              const id = snapIdOf(row)
              return (
                <tr key={i} className="border-t border-border/70">
                  <td className="td px-4 font-mono whitespace-nowrap">{iTime >= 0 ? formatCell(row[iTime]) : '—'}</td>
                  <td className="td px-4 font-mono">{iOp >= 0 ? formatCell(row[iOp]) : '—'}</td>
                  <td className="td px-4 font-mono text-text-mute">{id !== null ? String(id) : '—'}</td>
                  <td className="td px-4 font-mono text-text-dim break-all">{summaryText(row)}</td>
                  <td className="td px-4 whitespace-nowrap text-right">
                    {id !== null && (
                      <button
                        onClick={() => setPreviewId(id)}
                        aria-pressed={previewId === id}
                        className="btn-ghost py-1 px-2 text-xs"
                      >
                        Preview
                      </button>
                    )}
                    {(canRollback || admin) && id !== null && (
                      <button
                        onClick={() => { setRollbackTarget(id); setConfirmText(''); setRollbackOk(null); rollback.reset() }}
                        className="btn-ghost py-1 px-2 text-xs text-bad ml-2"
                      >
                        Roll back…
                      </button>
                    )}
                  </td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>

      {!canRollback && rollbackRule && (
        <p className="text-text-mute text-xs break-words">{rollbackRule}</p>
      )}

      {rollbackTarget !== null && (
        <div className="card p-5 border-red-500/40" role="dialog" aria-label="Confirm rollback">
          <div className="text-sm font-semibold">Roll back {ns}.{tbl} to snapshot {rollbackTarget}?</div>
          <p className="text-sm text-text-dim mt-1">
            This moves the current pointer to that snapshot. No snapshots or files are deleted,
            and the rollback itself appears in the history.
          </p>
          <label className="block mt-3">
            <span className="label block mb-1.5">Type <span className="font-mono">{tbl}</span> to confirm</span>
            <input
              value={confirmText}
              onChange={e => setConfirmText(e.target.value)}
              className="input"
              aria-label={`Type ${tbl} to confirm rollback`}
            />
          </label>
          {rollback.isError && (
            <div className="rounded-[10px] border border-red-500/40 bg-red-500/10 p-3 mt-3 text-sm text-bad font-mono whitespace-pre-wrap break-words">
              {errorMessage(rollback.error)}
            </div>
          )}
          <div className="flex gap-2 mt-4">
            <button
              disabled={confirmText !== tbl || rollback.isPending}
              onClick={() => rollback.mutate(rollbackTarget)}
              className="btn-primary disabled:opacity-40"
            >
              {rollback.isPending ? 'Rolling back…' : 'Roll back to this version'}
            </button>
            <button onClick={() => { setRollbackTarget(null); setConfirmText(''); rollback.reset() }} className="btn-ghost">
              Cancel
            </button>
          </div>
        </div>
      )}
      {rollbackOk && <div className="rounded-[10px] border border-emerald-400/30 bg-emerald-400/10 p-3 text-sm text-ok">{rollbackOk}</div>}
      {rollback.isError && rollbackTarget === null && (
        <div className="rounded-[10px] border border-red-500/40 bg-red-500/10 p-3 text-sm text-bad font-mono whitespace-pre-wrap break-words">
          {errorMessage(rollback.error)}
        </div>
      )}

      {previewId !== null && (
        <div className="space-y-2">
          <div className="text-xs text-text-mute font-mono">
            Time travel: <span className="text-text-dim">SELECT * FROM {sqlTableRef(ns, tbl, catalog)} FOR VERSION AS OF {previewId} LIMIT 100</span>
            <button onClick={() => setPreviewId(null)} className="btn-ghost py-0.5 px-2 text-xs ml-3">Close</button>
          </div>
          <Preview q={preview} />
        </div>
      )}
    </div>
  )
}

/**
 * One hop up and one hop down from this table (stardelt-nova#511): the datasets that feed it and
 * the ones it feeds, from `/api/lineage` at depth 1. Nodes the person may not read are listed with
 * a note and are not links, so the card never turns into a 403. The full graph stays one click
 * away.
 */
function LineageTab({ ns, tbl, catalog }: { ns: string; tbl: string; catalog: string }) {
  const graph = useQuery({
    queryKey: ['lineage', 'one-hop', catalog, ns, tbl],
    queryFn: () => api.lineage({ catalog: catalog || undefined, schema: ns, table: tbl, depth: 1 }),
    enabled: Boolean(catalog),
    retry: false,
    staleTime: 60_000,
  })
  if (!catalog) return <div className="card p-6 text-text-mute text-sm">No catalog is open, so there is no lineage to read.</div>
  if (graph.isLoading) return <div className="card p-6 text-text-mute text-sm">loading lineage…</div>
  if (graph.isError) return <div className="card p-6"><ErrorBox error={graph.error} onRetry={() => graph.refetch()} /></div>

  const nodes = graph.data?.nodes ?? []
  const edges = graph.data?.edges ?? []
  // The id the backend uses for this table; a demo graph may omit the catalog prefix.
  const byId = new Map(nodes.map(n => [n.id, n]))
  const current = byId.has(`${catalog}.${ns}|${tbl}`) ? `${catalog}.${ns}|${tbl}` : `${ns}|${tbl}`
  const pick = (ids: string[]) => {
    const seen = new Set<string>()
    const out: LineageNodeView[] = []
    for (const id of ids) {
      const n = byId.get(id)
      if (n && !seen.has(n.id)) { seen.add(n.id); out.push(n) }
    }
    return out
  }
  const upstream = pick(edges.filter(e => e.to === current).map(e => e.from))
  const downstream = pick(edges.filter(e => e.from === current).map(e => e.to))

  const row = (n: LineageNodeView) => (
    <li key={n.id} className="flex items-center gap-2 py-1.5">
      <Icon name="table" className="w-4 h-4 text-text-mute shrink-0" />
      {n.visible ? (
        <Link to={lineageHref(n)} className="text-sm text-accent hover:underline font-mono truncate">{n.namespace}.{n.name}</Link>
      ) : (
        <span className="text-sm text-text-mute font-mono truncate" title="You may not read this table">
          {n.namespace}.{n.name} · not visible to you
        </span>
      )}
    </li>
  )

  const fullGraph = new URLSearchParams({ schema: ns, table: tbl, catalog })
  return (
    <div className="space-y-4">
      <div className="grid md:grid-cols-2 gap-4">
        <div className="card p-5">
          <div className="label mb-2">Upstream — what feeds this table</div>
          {upstream.length === 0
            ? <p className="text-sm text-text-mute">No upstream table is recorded.</p>
            : <ul>{upstream.map(row)}</ul>}
        </div>
        <div className="card p-5">
          <div className="label mb-2">Downstream — what reads this table</div>
          {downstream.length === 0
            ? <p className="text-sm text-text-mute">No downstream table is recorded.</p>
            : <ul>{downstream.map(row)}</ul>}
        </div>
      </div>
      <Link to={`/lineage?${fullGraph.toString()}`} className="btn-ghost">Open the full lineage graph</Link>
    </div>
  )
}

/** A lineage node's table page: the namespace is `<catalog>.<schema>` (or just a schema in demo data). */
function lineageHref(n: { namespace: string; name: string }): string {
  const dot = n.namespace.indexOf('.')
  const catalog = dot > 0 ? n.namespace.slice(0, dot) : ''
  const schema = dot > 0 ? n.namespace.slice(dot + 1) : n.namespace
  const q = new URLSearchParams({ schema, table: n.name })
  if (catalog) q.set('catalog', catalog)
  return `/lineage?${q.toString()}`
}

function Details({ meta, partition }: {
  meta: TableMetadata
  partition?: { fields: Array<{ name: string; transform: string; 'source-id': number }> }
}) {
  const m = meta.metadata
  return (
    <div className="grid md:grid-cols-2 gap-4">
      <div className="card p-5">
        <div className="label mb-3">Table</div>
        <Row k="UUID" v={m['table-uuid']} mono />
        <Row k="Location" v={m.location} mono />
        <Row k="Updated" v={formatTime(m['last-updated-ms'])} />
        <Row k="Metadata" v={meta['metadata-location']} mono />
      </div>
      <div className="card p-5">
        <div className="label mb-3">Partitioning</div>
        {partition && partition.fields.length > 0 ? (
          <ul className="text-sm font-mono space-y-1">
            {partition.fields.map(f => (
              <li key={f.name}>
                <span className="text-accent">{f.transform}</span>
                <span className="text-text-mute"> → </span>{f.name}
              </li>
            ))}
          </ul>
        ) : (
          <div className="text-sm text-text-mute">Unpartitioned</div>
        )}
        {m.properties && (
          <>
            <div className="label mt-5 mb-2">Properties</div>
            <ul className="text-sm font-mono space-y-1">
              {Object.entries(m.properties).map(([k, v]) => (
                <li key={k}><span className="text-text-dim">{k}</span> = {v}</li>
              ))}
            </ul>
          </>
        )}
      </div>
    </div>
  )
}

function Row({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <div className="py-1.5 border-b border-border/60 last:border-0">
      <div className="text-[11px] uppercase tracking-widest text-text-mute">{k}</div>
      <div className={`text-sm break-all ${mono ? 'font-mono' : ''}`}>{v}</div>
    </div>
  )
}
