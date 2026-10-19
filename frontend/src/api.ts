// API client for the Nova backend.
// Same-origin in production (backend serves static UI), proxied via vite dev server locally.
// Auth is a same-origin HttpOnly session cookie (oidc mode) — no tokens in JS.

import { redirectToLogin } from './auth'

export type Me = {
  /** Display name (`preferred_username` / `email` / `upn` / `name` in oidc mode). */
  user: string
  /** `static-dev` (no login) or `oidc`. */
  auth_mode: 'static-dev' | 'oidc' | string
  demo: boolean
  warehouse: string
  // The fields below are present in oidc mode only — claims, never tokens.
  sub?: string
  email?: string | null
  name?: string | null
  groups?: string[]
  roles?: string[]
  groups_overage?: boolean
  is_admin?: boolean
  via?: 'session' | 'bearer'
  /** False until Nova forwards the user's token to Trino/Lakekeeper (phase 2). */
  identity_forwarding?: boolean
  /**
   * The Keycloak admin console for the realm, built by the backend from the
   * configured issuer. `null`/absent in dev mode and for non-Keycloak issuers, so the
   * UI shows no Users link where there is no console to open.
   */
  admin_console_url?: string | null
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
  /** True when the backend cut the result at its 50,000-row cap. */
  truncated?: boolean
}

export type Job = {
  id: string
  kind: string
  title: string
  status: string
  started_at: number
  detail?: string | null
  rows?: number | null
  user?: string
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

export type PlatformApp = {
  id: string
  name: string
  description: string
  category: string
  external_url: string
  /**
   * False for API-only services such as Lakekeeper: `external_url` is then only the
   * service's address, not a page a browser can open. Optional because the backend
   * field (issue #69, `apps.rs`) is landing separately; see `isBrowsable()`.
   */
  browsable?: boolean
  /**
   * `up` | `unreachable` | `unknown` | `not-installed`. The last one means the cluster
   * was read and holds no workload for this app: an optional component that was never
   * installed is a fact, not an outage, so it is shown as "not installed" and left out
   * of the "x / y up" counts.
   */
  status: 'up' | 'unreachable' | 'unknown' | 'not-installed'
  /** Images the app's workloads actually run, read from the cluster (may be empty). */
  images: string[]
}

/** A workload in Nova's namespace, as the cluster reports it. */
export type Workload = {
  name: string
  kind: 'Deployment' | 'StatefulSet' | string
  image: string
  ready: number
  desired: number
}

/**
 * `GET /api/workloads` — the namespace as Nova last managed to read it.
 *
 * An empty list alone is ambiguous ("nothing is running" or "nobody answered"), so the
 * backend says which: `ok` (read just now), `stale` (the read failed and `workloads` is
 * the last known good state, `age_seconds` old) or `unreachable` (never read it).
 * `state` is computed by the backend so the UI cannot disagree with the API about it.
 */
export type WorkloadSnapshot = {
  workloads: Workload[]
  /** Why the last read failed; null when it did not. */
  error: string | null
  /** Seconds since the last successful read; null when there never was one. */
  age_seconds: number | null
  state: 'ok' | 'stale' | 'unreachable'
}

/** One observed fact about an app, with where Nova read it. */
export type AppFact = {
  label: string
  value: string
  /** e.g. `Trino /v1/info`; absent for facts Nova knows from its own config. */
  source?: string
  /** `ok` | `bad` when the value is a health judgement. */
  tone?: 'ok' | 'bad'
}

export type AppLink = { label: string; url: string }

/**
 * Whether a browser will let Nova frame the app's UI, as the app's own headers
 * report it. `unknown` means Nova could not read them — not the same as allowed.
 */
export type AppEmbed = {
  url: string
  state: 'allowed' | 'blocked' | 'unknown'
  blocked_by?: string
  detail?: string
  /** In-cluster page Nova read the headers from. */
  checked?: string
  /**
   * Sign-in host the app sends visitors without a session to (Keycloak), when
   * that is a different host from the app itself. That page refuses framing, so
   * the frame stays empty until the visitor signs in in a top-level tab.
   */
  sign_in_host?: string
}

/** `GET /api/apps/links` — the header's app switcher, safe for every signed-in user. */
export type AppShortcut = {
  id: string
  name: string
  category: string
  external_url: string
}

/** `GET /api/embeds` — just what a signed-in user needs to use the embedded apps. */
export type EmbedApp = {
  id: string
  name: string
  external_url: string
  status: PlatformApp['status']
  embed: AppEmbed
}

/** `GET /api/apps/:id` — the detail view of one platform app. */
export type AppDetail = {
  app: PlatformApp
  /** In-cluster URL the backend probes for liveness. */
  probe: string
  replicas: { ready: number; desired: number }
  workloads: Workload[]
  facts: AppFact[]
  links: AppLink[]
  /** Present only for apps Nova frames in its own UI. */
  embed?: AppEmbed
}

/** A non-2xx answer from the backend; `status` lets callers tell 404 from 5xx. */
export class ApiError extends Error {
  status: number
  constructor(message: string, status: number) {
    super(message)
    this.name = 'ApiError'
    this.status = status
  }
}

/** Parse a 2xx body as JSON; a non-JSON body (e.g. an HTML fallback page) gets a readable error. */
async function readJson<T>(path: string, r: Response): Promise<T> {
  try {
    return (await r.json()) as T
  } catch {
    throw new Error(`${path} → ${r.status}: response was not JSON`)
  }
}

/** A 401 means the session is missing or expired: go sign in, then come back. */
function checkAuth(path: string, r: Response) {
  if (r.status === 401) {
    redirectToLogin()
    throw new ApiError(`${path} → 401: signing in…`, 401)
  }
}

async function jget<T>(path: string): Promise<T> {
  const r = await fetch(path)
  checkAuth(path, r)
  if (!r.ok) throw new ApiError(`${path} → ${r.status}`, r.status)
  return readJson<T>(path, r)
}

/** The backend answers errors as `{"error": "<message>"}`; show the message, not the JSON. */
function errorText(body: string): string {
  try {
    const parsed: unknown = JSON.parse(body)
    if (parsed && typeof parsed === 'object' && 'error' in parsed && typeof parsed.error === 'string') {
      return parsed.error
    }
  } catch {
    /* not JSON: show it as it came */
  }
  return body
}

async function jpost<T>(path: string, body: unknown): Promise<T> {
  const r = await fetch(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  })
  checkAuth(path, r)
  if (!r.ok) {
    const text = await r.text().catch(() => '')
    throw new ApiError(`${path} → ${r.status}: ${errorText(text)}`, r.status)
  }
  return readJson<T>(path, r)
}

const seg = encodeURIComponent

export const api = {
  me:           ()                            => jget<Me>('/api/me'),
  trinoCluster: ()                            => jget<unknown>('/api/trino/cluster'),
  warehouse:    ()                            => jget<Warehouse>('/api/catalog/warehouse'),
  namespaces:   ()                            => jget<NamespaceList>('/api/catalog/namespaces'),
  tables:       (ns: string)                  => jget<TableList>(`/api/catalog/namespaces/${seg(ns)}/tables`),
  tableMeta:    (ns: string, tbl: string)     => jget<TableMetadata>(`/api/catalog/tables/${seg(ns)}/${seg(tbl)}`),
  tableHistory: (ns: string, tbl: string)     => jget<QueryResult>(`/api/catalog/tables/${seg(ns)}/${seg(tbl)}/history`),
  rollback:     (ns: string, tbl: string, snapshotId: string) => jpost<QueryResult>(`/api/catalog/tables/${seg(ns)}/${seg(tbl)}/rollback`, { snapshot_id: snapshotId }),
  query:        (sql: string)                 => jpost<QueryResult>('/api/query', { sql }),
  jobs:         ()                            => jget<Job[]>('/api/jobs'),
  apps:         ()                            => jget<PlatformApp[]>('/api/apps'),
  embeds:       ()                            => jget<EmbedApp[]>('/api/embeds'),
  links:        ()                            => jget<AppShortcut[]>('/api/apps/links'),
  app:          (id: string)                  => jget<AppDetail>(`/api/apps/${seg(id)}`),
  workloads:    ()                            => jget<WorkloadSnapshot>('/api/workloads'),
  createNamespace: (namespace: string)        => jpost<{ namespace: string }>('/api/catalog/namespaces', { namespace }),
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
    // Arrays / ROWs / maps from the engine would otherwise export as "[object Object]".
    const s = v === null || v === undefined ? '' : typeof v === 'object' ? JSON.stringify(v) : String(v)
    return /[",\r\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s
  }
  const body = [columns.map(escape).join(','), ...rows.map(r => r.map(escape).join(','))].join('\n')
  const blob = new Blob([body], { type: 'text/csv;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  // Attached while clicking (older Firefox ignores detached anchors); the URL is
  // revoked on a later tick so the browser has started the download first.
  document.body.appendChild(a)
  a.click()
  a.remove()
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}
