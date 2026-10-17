// API client for the Nova backend.
// Same-origin in production (backend serves static UI), proxied via vite dev server locally.

export type Me = {
  user: string
  auth_mode: string
  demo: boolean
  warehouse: string
}

export type Warehouse = { name: string; id: string }

export type NamespaceList = { namespaces: string[][] }

export type TableIdentifier = { namespace: string[]; name: string }
export type TableList = { identifiers: TableIdentifier[] }

export type IcebergField = {
  id: number
  name: string
  required: boolean
  type: string | object
}
export type IcebergSchema = { 'schema-id': number; fields: IcebergField[] }

export type TableMetadata = {
  'metadata-location': string
  metadata: {
    'table-uuid': string
    location: string
    'last-updated-ms': number
    'current-schema-id'?: number
    schemas: IcebergSchema[]
    'partition-specs': Array<{
      'spec-id': number
      fields: Array<{ name: string; transform: string; 'source-id': number }>
    }>
    properties?: Record<string, string>
  }
}

export type QueryColumn = { name: string; type: string }
export type QueryResult = {
  columns: QueryColumn[]
  rows: unknown[][]
  row_count: number
  query_id?: string
  stats?: unknown
}

export type Job = {
  id: string
  kind: string
  title: string
  status: string
  started_at: number
  detail?: string | null
  rows?: number | null
}

export type UploadRequest = {
  namespace: string
  table: string
  format: 'csv' | 'json' | string
  content: string
}
export type UploadResult = {
  namespace: string
  table: string
  rows_loaded: number
  job_id: string
}

export type TransferRequest = {
  source: string
  target: string
  mode: 'overwrite' | 'append' | string
}
export type TransferResult = {
  source: string
  target: string
  rows: number
  job_id: string
}

export type TableRef = { ns: string; name: string }

async function jget<T>(path: string): Promise<T> {
  const r = await fetch(path)
  if (!r.ok) throw new Error(`${path} → ${r.status}`)
  return r.json() as Promise<T>
}

async function jpost<T>(path: string, body: unknown): Promise<T> {
  const r = await fetch(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (!r.ok) {
    const text = await r.text()
    throw new Error(`${path} → ${r.status}: ${text}`)
  }
  return r.json() as Promise<T>
}

export const api = {
  me:           ()                            => jget<Me>('/api/me'),
  trinoCluster: ()                            => jget<unknown>('/api/trino/cluster'),
  warehouse:    ()                            => jget<Warehouse>('/api/catalog/warehouse'),
  namespaces:   ()                            => jget<NamespaceList>('/api/catalog/namespaces'),
  tables:       (ns: string)                  => jget<TableList>(`/api/catalog/namespaces/${ns}/tables`),
  tableMeta:    (ns: string, tbl: string)     => jget<TableMetadata>(`/api/catalog/tables/${ns}/${tbl}`),
  query:        (sql: string)                 => jpost<QueryResult>('/api/query', { sql }),
  jobs:         ()                            => jget<Job[]>('/api/jobs'),
  upload:       (req: UploadRequest)          => jpost<UploadResult>('/api/ingest/upload', req),
  transfer:     (req: TransferRequest)        => jpost<TransferResult>('/api/transfer', req),
}

/** Every table across every namespace (small catalogs; fine for the MVP). */
export async function allTables(): Promise<TableRef[]> {
  const { namespaces } = await api.namespaces()
  const refs: TableRef[] = []
  await Promise.all(
    namespaces.map(async parts => {
      const ns = parts.join('.')
      const { identifiers } = await api.tables(ns)
      for (const id of identifiers) refs.push({ ns, name: id.name })
    }),
  )
  return refs.sort((a, b) => `${a.ns}.${a.name}`.localeCompare(`${b.ns}.${b.name}`))
}

export function formatCell(c: unknown): string {
  if (c === null || c === undefined) return '∅'
  if (typeof c === 'string') return c
  return JSON.stringify(c)
}

export function formatTime(ms: number): string {
  if (!ms) return '—'
  const d = new Date(ms)
  return d.toLocaleString(undefined, {
    month: 'short',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

export function downloadCsv(filename: string, columns: string[], rows: unknown[][]) {
  const escape = (v: unknown) => {
    const s = v === null || v === undefined ? '' : String(v)
    return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s
  }
  const body = [columns.join(','), ...rows.map(r => r.map(escape).join(','))].join('\n')
  const blob = new Blob([body], { type: 'text/csv;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  a.click()
  URL.revokeObjectURL(url)
}
