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
  /**
   * The configured value behind the administrator decision (#213): the matched admin
   * group, or the held role. `admin_by` says which one; both are absent when the
   * installation configures neither.
   */
  admin_group?: string | null
  admin_by?: 'group' | 'role' | string | null
  /**
   * Unix seconds at which the browser session can no longer be renewed (#213). Absent
   * for a bearer call, which has no browser session; never the access token's `exp`.
   */
  session_expires_at?: number | null
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

/**
 * One SQL compute class (`GET /api/warehouses`, stardelt-nova#236): the Trino resource
 * group a query lands in. Not the catalog warehouse of `Warehouse` above — that is the
 * Iceberg REST catalog the SQL addresses (`me.warehouse`), this is the class the engine
 * schedules the query into.
 *
 * `size` is `xs`, `small`, `medium` or `large`; `null` for the synthetic administrator row,
 * which is not a class of its own. `running`, `queued` and `memoryShare` are the numbers
 * the class is rendered with (the class defaults, overridden by `spec.concurrency`).
 * `canUse` is non-authoritative: one of the caller's groups is granted the class. `yours`
 * is the class a query will land in — the largest usable one, or the default class.
 */
export type SqlWarehouse = {
  name: string
  /** `spec.displayName`; `null` when the object carries none — show `name` then. */
  displayName: string | null
  size: string | null
  running: number
  queued: number
  memoryShare: string
  /** The class a query lands in when no selector matches. */
  default: boolean
  /** One of the caller's groups is granted this class (the engine decides, not Nova). */
  canUse: boolean
  /** The class this caller lands in: the largest usable one, or the default class. */
  yours: boolean
}

/**
 * One catalog the caller may browse (`GET /api/catalog/catalogs`): the name the SQL
 * addresses and the Lakekeeper warehouse id behind it. A catalog a person may not
 * browse is not in the list at all — the engine's `SHOW CATALOGS` filters it.
 *
 * `layer` is the medallion layer Nova knows for the catalog (stardelt-nova#109):
 * `bronze`, `silver` or `gold`, from the `stardelt.io/layer` label on the `Catalog`
 * resource or the catalog's own name. The API omits it for every other catalog, and
 * sends the list already ordered bronze, silver, gold, then alphabetically.
 */
export type Catalog = { name: string; id: string; layer?: string | null }

export type CatalogList = { catalogs: Catalog[]; default: string }

/**
 * One catalog's numbers (`GET /api/catalog/metrics`, stardelt-nova#249, #263), read from
 * the catalog's Iceberg metadata rather than counted by a query. `lastCommitMs` is the
 * newest commit among the loaded tables (null when none has one). `truncated` means
 * `records` and `sizeBytes` cover the first 200 tables while `tables` stays exact. `error`
 * is the sentence for a catalog whose last scan failed; its numbers are then the last good
 * ones, or zeros for a catalog that never answered, never omitted. `updatedAt` is when
 * those numbers were read (null while `pending`: no numbers yet, one scan is reading it).
 * `layer` and the row order are the catalog layers of stardelt-nova#109.
 */
export type CatalogStat = {
  name: string
  layer?: string | null
  schemas: number
  tables: number
  records: number
  sizeBytes: number
  lastCommitMs: number | null
  truncated: boolean
  error?: string
  updatedAt: number | null
  pending: boolean
}

/** The sums over the catalogs the caller may see; pending catalogs are left out. */
export type CatalogMetricsTotals = {
  catalogs: number
  schemas: number
  tables: number
  records: number
  sizeBytes: number
}

/** `GET /api/catalog/metrics`: one row per catalog the caller may browse, plus the sums. */
export type CatalogMetrics = {
  catalogs: CatalogStat[]
  totals: CatalogMetricsTotals
  generatedAt: number
}

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

/** One column as the table documentation reports it: the current schema's name, type and `doc`. */
export type ColumnDoc = {
  name: string
  type: string
  description: string | null
}

/**
 * `GET /api/catalog/tables/{ns}/{tbl}/documentation` — the Iceberg `comment` as
 * `description`, the `stardelt.tags`/`stardelt.owner`/`stardelt.domain` properties,
 * and every field of the current schema.
 *
 * `canEdit`/`editRule` are Nova's interim rule: until the catalog enforces permissions,
 * Nova decides who may edit these properties. Anyone who reaches Lakekeeper or Trino
 * directly can still change them.
 */
export type TableDocumentation = {
  description: string | null
  tags: string[]
  owner: string | null
  domain: string | null
  columns: ColumnDoc[]
  canEdit: boolean
  editRule: string
}

/**
 * A `PUT …/documentation` body. An absent key changes nothing; `null` removes the
 * property, and an empty tag list removes the tags.
 */
export type TableDocsUpdate = {
  description?: string | null
  tags?: string[] | null
  owner?: string | null
  domain?: string | null
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

/**
 * One catalog as an administrator sees it (`GET /api/admin/catalogs`, stardelt-nova#251):
 * the `Catalog` resource's identity, owner, managed access, storage key prefix, phase and
 * what the access-sync job last wrote. Administrator-only — everyone else gets 403.
 * The credential reference and the bucket never leave the cluster, so they are not here.
 */
export type AdminCatalog = {
  /** `metadata.name`, also the name the SQL addresses and the warehouse name. */
  name: string
  /** The medallion layer (stardelt-nova#109), from the label or the name; absent otherwise. */
  layer?: string | null
  displayName: string | null
  description: string | null
  /** The one owning group; ownership means `manage`, not `select`. */
  owner: string | null
  /** The owner cannot grant on this catalog. */
  managedAccess: boolean
  keyPrefix: string | null
  /** `Ready` | `Invalid`, or `Unknown` when the access-sync job has not observed it. */
  phase: string
  warehouseId: string | null
  generation: number | null
  observedGeneration: number | null
  resourceVersion: string | null
  /** `spec.grants`, in the order the resource declares them: one verb per entry. */
  grants: AdminCatalogGrant[]
  conditions: AdminCatalogCondition[]
}

/** One declared grant: a group and one verb (`browse`, `select`, `write`, `create`, `manage`). */
export type AdminCatalogGrant = { group: string; verb: string }

/** One condition of a catalog's status, reduced to the four fields the page shows. */
export type AdminCatalogCondition = {
  type: 'CatalogReady' | 'GrantsReady' | 'Drift' | string
  status: 'True' | 'False' | string
  reason: string | null
  message: string | null
}

/**
 * The body of `PUT /api/admin/catalogs/{name}/access` (stardelt-nova#253): the whole
 * `spec.owner`, `spec.managedAccess` and `spec.grants` of one `Catalog`, plus the
 * `resourceVersion` the page read. A concurrent change answers 409 and never overwrites.
 */
export type CatalogAccessUpdate = {
  /** `metadata.resourceVersion` from the row the page holds. */
  resourceVersion: string
  /** Exactly one group name. */
  owner: string
  /** The owner cannot grant on this catalog; only administrators can. */
  managedAccess: boolean
  /** One `{group, verb}` per group and verb, in the order the dialog shows them. */
  grants: AdminCatalogGrant[]
}

/**
 * One row of `GET /api/connections` (stardelt-nova#171). A connection is a credential
 * for a system outside the platform; the page lists metadata only. `secretKeys` holds the
 * *names* of the secret fields — a value is never sent, by the store or by Nova, so the
 * UI cannot show one even if it tried.
 */
export type Connection = {
  name: string
  scope: 'personal' | 'group' | 'managed'
  type: string
  /** Group scope only: the group whose members may use and rotate it. */
  ownerGroup?: string
  /** Managed scope only: the groups allowed to use it. */
  usableBy?: string[]
  /** Non-secret settings (host, port, bucket, …) as the store keeps them. */
  config: Record<string, string>
  /** Names of the secret fields; the values are never sent. */
  secretKeys: string[]
  /**
   * Whether the caller belongs to the connection's scope. Always true for personal and managed
   * rows, and for a group row of one of the caller's own groups. `false` only on a group row an
   * administrator sees because they are an administrator (stardelt-nova#203): they may delete it
   * and read it through break-glass, but they cannot rotate it.
   */
  member: boolean
  /** ISO time of the last rotation. */
  lastRotated?: string
  version: number
  /** Group and managed scope only: whether the operator synced it. */
  sync?: { ready: boolean; reason: string; message?: string }
  /**
   * Group scope only: the last break-glass read, until a rotation clears it. The
   * owning group is told that an administrator read the credential and why.
   */
  breakGlass?: { by: string; at: string; reason: string }
}

/**
 * The one answer of a break-glass read (stardelt-nova#183): the credential's fields,
 * and when they were read. Never cached or logged; only the open dialog holds it.
 */
export type BreakGlassAnswer = {
  fields: Record<string, string>
  readAt: string
}

/**
 * The single state the Pipelines page shows (`GET /api/pipelines`, stardelt-nova#188).
 * The backend computes it so the UI cannot disagree with the API about it.
 */
export type PipelineState =
  | 'loading'
  | 'not-loaded'
  | 'paused'
  | 'running'
  | 'failed'
  | 'succeeded'
  | 'idle'

/**
 * One task as the `Pipeline` resource declares it. `sql` is the statement of a `kind: "sql"`
 * task; the read API does not send it yet (stardelt-nova#211 says it should), so the editor
 * treats it as absent and asks the person to type the statement until the API carries it.
 * A `kind: "notebook"` task carries the file name, the version it pins and its parameter
 * values — only on the detail route, as with `sql` (stardelt-nova#237).
 */
export type PipelineTaskSpec = {
  name: string
  kind: string
  sql?: string | null
  notebook?: string | null
  objectVersion?: string | null
  notebookParams?: Record<string, string> | null
}

/**
 * One published notebook, the newest version of it (`GET /api/notebooks`, stardelt-nova#225).
 * A schedule pins `objectVersion` exactly; `uploadedAt` is S3's ISO 8601 time.
 */
export type PublishedNotebook = {
  group: string
  name: string
  objectVersion: string
  size: number
  uploadedAt: string
  uploadedBy: string
}

/** One immutable published version of a notebook (`GET …/versions`), newest first. */
export type NotebookVersion = {
  objectVersion: string
  size: number
  uploadedAt: string
  uploadedBy: string
  sha256: string
}

/** One parameter of a notebook's tagged `parameters` cell: the name and its default as text. */
export type NotebookParameter = { name: string; default: string }

/** `GET …/parameters?version=` — the parameters of one pinned version (stardelt-nova#237). */
export type NotebookParameters = {
  parameters: NotebookParameter[]
  /** The version the answer describes; a schedule pins this id. */
  objectVersion: string
}

/** A cron and its IANA zone, as `POST`/`PUT /api/pipelines` carries them. */
export type ScheduleSpec = { cron: string; timezone: string }

/**
 * The advanced options of a schedule (stardelt-nova#223). Every field is optional in a request:
 * an omitted one keeps its default (one retry, five minutes between retries, a two-hour time
 * limit, one run at a time). `Replace` is deliberately not offered — the loader runs it as
 * `Forbid`.
 */
export type PipelineOptions = {
  retries?: number
  backoffMinutes?: number
  timeoutMinutes?: number
  concurrency?: 'Allow' | 'Forbid'
}

/** The options as the pipeline detail carries them, defaults filled in — what edit mode prefills. */
export type PipelineOptionsView = {
  retries: number
  backoffMinutes: number
  timeoutMinutes: number
  concurrency: string
}

/** One of the caller's groups and the pipeline principal that would run its schedules. */
export type PipelinePrincipal = {
  group: string
  /** `pipeline-<group>` when such a principal exists, else null. */
  principal: string | null
  ready: boolean
  /** Why not, when `ready` is false: the sentence the API refuses a schedule with. */
  message: string | null
}

/** `POST /api/pipelines` (`Schedule`, stardelt-nova#210, options stardelt-nova#223). */
export type CreatePipelineRequest = {
  name: string
  ownerGroup: string
  /** A statement; exactly one of this and `notebook` (stardelt-nova#237). */
  sql?: string
  /** A published notebook version to run instead of a statement. */
  notebook?: NotebookRequest
  schedule: ScheduleSpec | null
  /** Values for `${params.<name>}` in the statement; at most 16. */
  params?: Record<string, string>
  /** Retries, the wait between them, the time limit and the run mode. */
  options?: PipelineOptions
}

/**
 * `PUT /api/pipelines/{namespace}/{name}`: the statement or the notebook, the schedule, the
 * parameters and the options. The body is the whole editable state — an omitted `schedule` or
 * `params` removes it and an omitted option keeps its default — so the dialog sends all four.
 * A pipeline never changes kind: exactly one of `sql` and `notebook` is given (#237).
 */
export type UpdatePipelineRequest = {
  sql?: string
  notebook?: NotebookRequest
  schedule: ScheduleSpec | null
  params?: Record<string, string>
  options?: PipelineOptions
  /**
   * The `resourceVersion` the editor read (`pipelines_spec`): when it is not the stored one the
   * update answers 409 and writes nothing (stardelt-nova#387).
   */
  resourceVersion?: string
}

/** The published notebook version a schedule pins (stardelt-nova#237). */
export type NotebookRequest = {
  /** The `.ipynb` file name; its group is the pipeline's `ownerGroup`. */
  name: string
  /** The S3 object version, never a branch or a tag. */
  objectVersion: string
  /** The values papermill binds; names are the tagged cell's, values at most 256 characters. */
  params: Record<string, string>
}

/** `POST /api/pipelines/check`: what the engine says about a statement, without running it. */
export type CheckStatementRequest = {
  sql: string
  /** Values for the statement's `${params.<name>}` placeholders. Only names, never secrets. */
  params?: Record<string, string>
}

/**
 * The engine's verdict. `checked: false` means this kind of statement cannot be checked here
 * (then `ok` is absent). `ok: false` carries the engine's sentence in `message`.
 */
export type CheckStatementView = {
  checked: boolean
  ok?: boolean
  message?: string
}

/** `POST /api/pipelines/preview-schedule`: the next fire times of a cron in its zone. */
export type PreviewScheduleRequest = { cron: string; timezone: string; count?: number }

/** The next fire times, RFC 3339 with the schedule's offset. */
export type PreviewScheduleView = { runs: string[] }

/**
 * One run, as the list and the detail share it. Times are Airflow's RFC 3339 strings;
 * `durationSeconds` is null until the run has ended and both times parsed.
 */
export type RunView = {
  runId: string
  /** `queued`, `running`, `success`, `failed`, `upstream_failed`, `skipped`, … */
  state: string
  /** Airflow's `run_type`: `scheduled`, `manual`, … */
  type: string
  logicalDate: string | null
  startedAt: string | null
  endedAt: string | null
  durationSeconds: number | null
  triggeredBy: string | null
  note: string | null
}

/** One pipeline in the list: the resource plus what the scheduler says about it. */
/**
 * Where `stardelt bundle deploy` published a pipeline from (stardelt-nova#397): the bundle, the
 * target, the Git commit (`-dirty` appended for uncommitted changes), who deployed and when.
 */
export type DeploymentView = {
  bundle: string
  target: string
  revision?: string
  deployedBy?: string
  deployedAt?: string
}

export type PipelineView = {
  name: string
  namespace: string
  /** The owning group; null means administrators only. */
  ownerGroup: string | null
  /** The cron, with the time zone in parentheses when the resource has one. */
  schedule: string | null
  paused: boolean
  /** False while the scheduler has no DAG for the resource yet. */
  loaded: boolean
  nextRun: string | null
  state: PipelineState
  lastRun: RunView | null
  tasks: PipelineTaskSpec[]
  /** The service principal a run acts as. */
  runAs: string | null
  /** `Allow`, `Forbid` or `Replace`. */
  concurrency: string
  createdAt: string | null
  /** Absent for a pipeline made in the interface or with `stardelt apply`. */
  deployment?: DeploymentView
}

/** `GET /api/pipelines/{namespace}/{name}`: the view plus the latest 20 runs. */
export type PipelineDetail = PipelineView & {
  runs: RunView[]
  params: Record<string, string>
  retries: { attempts: number; backoff: string | null } | null
  /**
   * The advanced options, defaults filled in (stardelt-nova#223). A backend that predates them
   * sends nothing; the dialog then starts from the defaults.
   */
  options?: PipelineOptionsView | null
}

/** `POST /api/pipelines/validate`: what a create or an update of this document would do, without doing it. */
export type ValidatePipelineResponse = {
  ok: boolean
  /** `create` for a free name, `update` for a pipeline the caller may change; absent when `ok` is false. */
  action?: 'create' | 'update'
  errors: { message: string }[]
  /** What an update would change: `statement` or `notebook`, `schedule`, `params`, `options`. */
  changes: string[]
  /** The next five fire times (RFC 3339) of an accepted document with a schedule. */
  nextRuns: string[]
}

/**
 * `GET /api/pipelines/{namespace}/{name}/spec`: the editable document of one pipeline, in the shape
 * of the body of `POST /api/pipelines` (stardelt-nova#383). `ownerGroup` and the statement are
 * absent for a pipeline whose shape the form cannot show.
 */
export type PipelineDocument = {
  name: string
  ownerGroup?: string
  sql?: string
  notebook?: NotebookRequest
  schedule?: ScheduleSpec
  params?: Record<string, string>
  options: PipelineOptionsView
}

export type PipelineSpec = {
  document: PipelineDocument
  resourceVersion: string | null
  /** `nova` when Nova wrote the pipeline, `other` for one made with kubectl or by GitOps. */
  managedBy: 'nova' | 'other'
  /** True only for a pipeline Nova wrote that has the shape the form edits. */
  editable: boolean
}

/** One task instance of a run. */
export type TaskView = {
  taskId: string
  state: string
  tryNumber: number
  startedAt: string | null
  endedAt: string | null
  durationSeconds: number | null
  operator: string | null
}

/** `GET …/runs/{runId}`: one run and its task instances. */
export type RunResponse = { run: RunView; tasks: TaskView[] }

/** One log message. Plain text: it is rendered as text, never as markup. */
export type LogLine = {
  timestamp: string | null
  level: string | null
  text: string
}

/** One page of a task's log; `nextToken` asks for the next page, `truncated` says it was cut. */
export type LogPage = {
  lines: LogLine[]
  nextToken: string | null
  truncated: boolean
}

/**
 * `POST /api/connections` body (stardelt-nova#173). `secret` holds the values; they are
 * written to the store and never returned. Casing matters: the backend reads
 * `ownerGroup`, `usableBy` and `refreshInterval`.
 */
export type NewConnectionRequest = {
  name: string
  type: string
  scope: Connection['scope']
  ownerGroup?: string
  usableBy?: string[]
  config: Record<string, string>
  secret: Record<string, string>
  refreshInterval?: string
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

/** The sentence to show for any failed request: the API's own message, never a path or a status code. */
export function errorMessage(e: unknown): string {
  return e instanceof ApiError ? e.message : 'Something went wrong. Try again.'
}

/** The sentence for a status when the body carried none (an HTML error page from a proxy, an empty body). */
function statusSentence(status: number): string {
  if (status === 401) return 'Signing in…'
  if (status === 403) return 'You are not allowed to see this.'
  if (status === 404) return 'Not found.'
  return status >= 500 ? 'Something went wrong on the server. Try again.' : 'The request was refused.'
}

/** The backend answers errors as `{"error": "<message>"}`; any other body (an HTML page, an empty one) carries no sentence. */
function errorText(body: string): string {
  try {
    const parsed: unknown = JSON.parse(body)
    if (parsed && typeof parsed === 'object' && 'error' in parsed && typeof parsed.error === 'string') {
      return parsed.error
    }
  } catch {
    /* not JSON: it carries no sentence */
  }
  return ''
}

/** One request; a refused connection is Nova being unreachable, not the browser's TypeError text. */
async function send(path: string, init?: RequestInit): Promise<Response> {
  try {
    return await fetch(path, init)
  } catch {
    throw new ApiError('Nova is not reachable. Check your connection and try again.', 0)
  }
}

/** Parse a 2xx body as JSON; a non-JSON body (e.g. an HTML fallback page) gets a readable error. */
async function readJson<T>(path: string, r: Response): Promise<T> {
  try {
    return (await r.json()) as T
  } catch {
    console.debug('api', path, r.status)
    throw new ApiError('Nova answered with something unexpected. Try again.', r.status)
  }
}

/** A 401 means the session is missing or expired: go sign in, then come back. */
function checkAuth(path: string, r: Response) {
  if (r.status === 401) {
    console.debug('api', path, r.status)
    redirectToLogin()
    throw new ApiError('Signing in…', 401)
  }
}

async function jget<T>(path: string): Promise<T> {
  const r = await send(path)
  checkAuth(path, r)
  if (!r.ok) {
    console.debug('api', path, r.status)
    throw new ApiError(errorText(await r.text().catch(() => '')) || statusSentence(r.status), r.status)
  }
  return readJson<T>(path, r)
}

async function jsend<T>(method: 'POST' | 'PUT', path: string, body: unknown): Promise<T> {
  const r = await send(path, {
    method,
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  })
  checkAuth(path, r)
  if (!r.ok) {
    console.debug('api', path, r.status)
    throw new ApiError(errorText(await r.text().catch(() => '')) || statusSentence(r.status), r.status)
  }
  return readJson<T>(path, r)
}

async function jpost<T>(path: string, body: unknown): Promise<T> {
  return jsend<T>('POST', path, body)
}

async function jput<T>(path: string, body: unknown): Promise<T> {
  return jsend<T>('PUT', path, body)
}

/** A delete answers 204 with no body; a failure carries the API's sentence like any other. */
async function jdel(path: string): Promise<void> {
  const r = await send(path, { method: 'DELETE' })
  checkAuth(path, r)
  if (!r.ok) {
    console.debug('api', path, r.status)
    throw new ApiError(errorText(await r.text().catch(() => '')) || statusSentence(r.status), r.status)
  }
}

const seg = encodeURIComponent

/**
 * A catalog-scoped path. `?catalog=<name>` is appended only when a catalog is named, so a
 * call without one works in the default catalog exactly as it always has. The name is
 * encoded with `seg`: it comes from the API or the URL, never pasted into a request raw.
 */
function catalogPath(path: string, catalog?: string): string {
  return catalog ? `${path}?catalog=${seg(catalog)}` : path
}

/** One row of the query log list (`GET /api/query-log`). Times are epoch milliseconds. */
export type QueryLogItem = {
  id: string
  /** QUEUED (in progress), FINISHED, FAILED or CANCELED. */
  state: string
  user: string
  /** The person's display name; absent when Nova does not know the engine user. */
  userName?: string | null
  source: string | null
  warehouse: string
  queryType: string | null
  /** The first 300 characters of the statement. */
  sql: string
  createdAt: number
  wallMs: number | null
  outputRows: number | null
  errorCode: string | null
}

export type QueryLogPage = { items: QueryLogItem[]; nextCursor: string | null }

/** Everything stored about one query (`GET /api/query-log/{id}`). */
export type QueryLogDetail = {
  id: string
  state: string
  user: string
  /** The person's display name; absent when Nova does not know the engine user. */
  userName?: string | null
  source: string | null
  clientTags: string[]
  warehouse: string
  catalog: string | null
  schema: string | null
  queryType: string | null
  sql: string
  sqlHash: string
  createdAt: number
  startedAt: number | null
  endedAt: number | null
  timing: {
    queuedMs: number | null
    planningMs: number | null
    executionMs: number | null
    wallMs: number | null
    cpuMs: number | null
  }
  data: {
    inputRows: number | null
    inputBytes: number | null
    outputRows: number | null
    outputBytes: number | null
    writtenRows: number | null
    writtenBytes: number | null
    peakMemoryBytes: number | null
  }
  tablesRead: string[]
  tablesWritten: string[]
  error: { code: string; message: string | null } | null
}

export type QueryLogParams = {
  scope: 'mine' | 'all'
  state?: string
  q?: string
  /** Epoch milliseconds: only queries created at or after this. */
  from?: number
  cursor?: string
  limit?: number
}

/** One page of a task's log; `tryNumber` and `token` are optional, as the backend's own query. */
export type PipelineLogParams = { tryNumber?: number; token?: string }

function pipelineLogUrl(
  namespace: string,
  name: string,
  runId: string,
  taskId: string,
  p: PipelineLogParams,
): string {
  const qs = new URLSearchParams()
  // `try=0` is refused by the API; omitting it lets the backend pick the task's last attempt.
  if (p.tryNumber != null && p.tryNumber > 0) qs.set('try', String(p.tryNumber))
  if (p.token) qs.set('token', p.token)
  const query = qs.toString()
  const base = `/api/pipelines/${seg(namespace)}/${seg(name)}/runs/${seg(runId)}/tasks/${seg(taskId)}/logs`
  return query ? `${base}?${query}` : base
}

function queryLogUrl(p: QueryLogParams): string {
  const qs = new URLSearchParams({ scope: p.scope })
  if (p.state) qs.set('state', p.state)
  if (p.q) qs.set('q', p.q)
  if (p.from != null) qs.set('from', String(p.from))
  if (p.cursor) qs.set('cursor', p.cursor)
  qs.set('limit', String(p.limit ?? 50))
  return `/api/query-log?${qs.toString()}`
}

/** One answer of `PUT /api/settings/{key}`: the value as the store now holds it. */
export type SettingAnswer = { key: string; value: unknown }

export const api = {
  me:           ()                            => jget<Me>('/api/me'),
  /** The caller's personal settings as one JSON object (`{}` when none). */
  settings:     ()                            => jget<Record<string, unknown>>('/api/settings'),
  putSetting:   (key: string, value: unknown) => jput<SettingAnswer>(`/api/settings/${seg(key)}`, value),
  deleteSetting: (key: string)                => jdel(`/api/settings/${seg(key)}`),
  connections:  ()                            => jget<{ connections: Connection[] }>('/api/connections'),
  createConnection: (req: NewConnectionRequest) => jpost<Connection>('/api/connections', req),
  rotateConnection: (path: string, secret: Record<string, string>) => jput<Connection>(path, { secret }),
  deleteConnection: (path: string)           => jdel(path),
  /** A group connection read once by an administrator, with a reason the group will see. */
  breakGlassRead: (group: string, name: string, reason: string) =>
    jpost<BreakGlassAnswer>(`/api/connections/group/${seg(group)}/${seg(name)}/break-glass`, { reason }),
  trinoCluster: ()                            => jget<unknown>('/api/trino/cluster'),
  warehouse:    ()                            => jget<Warehouse>('/api/catalog/warehouse'),
  /** The SQL compute classes and the one the caller lands in (#236). */
  warehouses:   ()                            => jget<SqlWarehouse[]>('/api/warehouses'),
  /** The catalogs the caller may browse and the one a request without `?catalog=` uses. */
  catalogs:     ()                            => jget<CatalogList>('/api/catalog/catalogs'),
  /** Schemas, tables, rows, bytes and the newest commit per catalog, plus the totals (#249). */
  catalogMetrics: ()                          => jget<CatalogMetrics>('/api/catalog/metrics'),
  /** Read every catalog again now (administrators; 429 when a minute has not passed). */
  refreshCatalogMetrics: ()                   => jpost<{ accepted: boolean }>('/api/catalog/metrics/refresh', {}),
  namespaces:   (catalog?: string)            => jget<NamespaceList>(catalogPath('/api/catalog/namespaces', catalog)),
  tables:       (ns: string, catalog?: string) =>
    jget<TableList>(catalogPath(`/api/catalog/namespaces/${seg(ns)}/tables`, catalog)),
  tableMeta:    (ns: string, tbl: string, catalog?: string) =>
    jget<TableMetadata>(catalogPath(`/api/catalog/tables/${seg(ns)}/${seg(tbl)}`, catalog)),
  tableDocs:    (ns: string, tbl: string, catalog?: string) =>
    jget<TableDocumentation>(catalogPath(`/api/catalog/tables/${seg(ns)}/${seg(tbl)}/documentation`, catalog)),
  putTableDocs: (ns: string, tbl: string, body: TableDocsUpdate, catalog?: string) =>
    jput<TableDocumentation>(catalogPath(`/api/catalog/tables/${seg(ns)}/${seg(tbl)}/documentation`, catalog), body),
  putColumnDoc: (ns: string, tbl: string, column: string, description: string | null, catalog?: string) =>
    jput<TableDocumentation>(
      catalogPath(`/api/catalog/tables/${seg(ns)}/${seg(tbl)}/columns/${seg(column)}/description`, catalog),
      { description },
    ),
  tableHistory: (ns: string, tbl: string, catalog?: string) =>
    jget<QueryResult>(catalogPath(`/api/catalog/tables/${seg(ns)}/${seg(tbl)}/history`, catalog)),
  rollback:     (ns: string, tbl: string, snapshotId: string, catalog?: string) =>
    jpost<QueryResult>(catalogPath(`/api/catalog/tables/${seg(ns)}/${seg(tbl)}/rollback`, catalog), { snapshot_id: snapshotId }),
  query:        (sql: string)                 => jpost<QueryResult>('/api/query', { sql }),
  jobs:         ()                            => jget<Job[]>('/api/jobs'),
  queryLog:     (p: QueryLogParams)           => jget<QueryLogPage>(queryLogUrl(p)),
  queryLogDetail: (id: string)                => jget<QueryLogDetail>(`/api/query-log/${seg(id)}`),
  pipelines:    ()                            => jget<{ pipelines: PipelineView[] }>('/api/pipelines'),
  /** The caller's own groups and their principals (`Schedule`, #210). */
  pipelinePrincipals: ()                      => jget<{ groups: PipelinePrincipal[] }>('/api/pipelines/principals'),
  /** The latest published versions of the notebooks in the caller's groups (#225). */
  notebooks:    ()                            => jget<PublishedNotebook[]>('/api/notebooks'),
  /** One notebook's published versions, newest first, at most 50. */
  notebookVersions: (group: string, name: string) =>
    jget<NotebookVersion[]>(`/api/notebooks/${seg(group)}/${seg(name)}/versions`),
  /** The parameters of one published version; no `version` means the latest (#237). */
  notebookParameters: (group: string, name: string, version?: string) => {
    const qs = version ? `?${new URLSearchParams({ version }).toString()}` : ''
    return jget<NotebookParameters>(`/api/notebooks/${seg(group)}/${seg(name)}/parameters${qs}`)
  },
  /** Fill the placeholders and ask the engine to plan the statement: nothing runs (#223). */
  checkStatement: (req: CheckStatementRequest) => jpost<CheckStatementView>('/api/pipelines/check', req),
  /** The next fire times of a cron in its zone, at most ten (#223). */
  previewSchedule: (req: PreviewScheduleRequest) => jpost<PreviewScheduleView>('/api/pipelines/preview-schedule', req),
  createPipeline: (req: CreatePipelineRequest) => jpost<PipelineView>('/api/pipelines', req),
  updatePipeline: (ns: string, name: string, req: UpdatePipelineRequest) =>
    jput<PipelineView>(`/api/pipelines/${seg(ns)}/${seg(name)}`, req),
  deletePipeline: (ns: string, name: string)  => jdel(`/api/pipelines/${seg(ns)}/${seg(name)}`),
  pipeline:     (ns: string, name: string)    => jget<PipelineDetail>(`/api/pipelines/${seg(ns)}/${seg(name)}`),
  validatePipeline: (req: CreatePipelineRequest) => jpost<ValidatePipelineResponse>('/api/pipelines/validate', req),
  pipelineSpec: (ns: string, name: string)    => jget<PipelineSpec>(`/api/pipelines/${seg(ns)}/${seg(name)}/spec`),
  startPipelineRun: (ns: string, name: string) => jpost<RunView>(`/api/pipelines/${seg(ns)}/${seg(name)}/runs`, {}),
  pausePipeline: (ns: string, name: string)   => jpost<PipelineView>(`/api/pipelines/${seg(ns)}/${seg(name)}/pause`, {}),
  resumePipeline: (ns: string, name: string)  => jpost<PipelineView>(`/api/pipelines/${seg(ns)}/${seg(name)}/resume`, {}),
  pipelineRun:  (ns: string, name: string, runId: string) =>
    jget<RunResponse>(`/api/pipelines/${seg(ns)}/${seg(name)}/runs/${seg(runId)}`),
  stopPipelineRun: (ns: string, name: string, runId: string) =>
    jpost<RunView>(`/api/pipelines/${seg(ns)}/${seg(name)}/runs/${seg(runId)}/stop`, {}),
  pipelineLog:  (ns: string, name: string, runId: string, taskId: string, p: PipelineLogParams = {}) =>
    jget<LogPage>(pipelineLogUrl(ns, name, runId, taskId, p)),
  apps:         ()                            => jget<PlatformApp[]>('/api/apps'),
  embeds:       ()                            => jget<EmbedApp[]>('/api/embeds'),
  links:        ()                            => jget<AppShortcut[]>('/api/apps/links'),
  app:          (id: string)                  => jget<AppDetail>(`/api/apps/${seg(id)}`),
  /** Every catalog of the platform namespace with its grants (administrators only, #251). */
  adminCatalogs: ()                           => jget<AdminCatalog[]>('/api/admin/catalogs'),
  /** Change one catalog's owner, managed access and grants (#253); the resource stays the source. */
  putCatalogAccess: (name: string, body: CatalogAccessUpdate) =>
    jput<AdminCatalog>(`/api/admin/catalogs/${seg(name)}/access`, body),
  workloads:    ()                            => jget<WorkloadSnapshot>('/api/workloads'),
  createNamespace: (namespace: string, catalog?: string) =>
    jpost<{ namespace: string }>(catalogPath('/api/catalog/namespaces', catalog), { namespace }),
  upload:       (req: UploadRequest)          => jpost<UploadResult>('/api/ingest/upload', req),
  transfer:     (req: TransferRequest)        => jpost<TransferResult>('/api/transfer', req),
}

/**
 * Every table across every namespace of one catalog (small catalogs; fine for the MVP).
 * Without a catalog the API's default is used, exactly as before several catalogs existed.
 */
export async function allTables(catalog?: string): Promise<TableRef[]> {
  const { namespaces } = await api.namespaces(catalog)
  const refs: TableRef[] = []
  await Promise.all(
    namespaces.map(async parts => {
      const ns = parts.join('.')
      const { identifiers } = await api.tables(ns, catalog)
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
