// API client for the Nova backend.
// Same-origin in production (backend serves static UI), proxied via vite dev server locally.

export type Me = { user: string; auth_mode: string }

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
}
