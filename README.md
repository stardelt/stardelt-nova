<img src="frontend/public/logo-wordmark.svg" alt="stardelt" height="48" />

# Nova

Nova is the unified control-plane UI + API gateway for stardelt. A Rust (axum)
backend serves the React SPA and proxies the Lakekeeper Iceberg REST catalog and
the Trino query engine.

---

## Architecture

```
Browser
  │
  └─── :5173 (dev) / :8080 (prod)
         │
         ├─ GET  /api/health           liveness + state-store status (never behind sign-in)
         ├─ GET  /api/me               current user + demo flag
         ├─ GET  /api/auth/login       start OIDC sign-in (oidc mode)
         ├─ GET  /api/auth/callback    OIDC redirect target (oidc mode)
         ├─ POST /api/auth/logout      sign out here and at the IdP
         ├─ GET  /api/trino/cluster    → Trino /v1/info
         ├─ GET  /api/catalog/catalogs the catalogs the caller may browse (?catalog= names one)
         ├─ GET  /api/catalog/metrics  what is in each of them: schemas, tables, rows, bytes, last change
         ├─ GET  /api/catalog/*        → Lakekeeper REST catalog, in the catalog ?catalog= names
         ├─ POST /api/catalog/namespaces  create schema
         ├─ GET/PUT /api/catalog/tables/:ns/:tbl/documentation  description, tags, owner, domain
         ├─ PUT  /api/catalog/tables/:ns/:tbl/columns/:column/description  one column's description
         ├─ POST /api/query            → Trino query engine
         ├─ GET  /api/jobs             recent activity
         ├─ GET  /api/saved-queries    the caller's saved queries (needs the state store)
         ├─ POST /api/saved-queries    save one: {title, sql, warehouse?}
         ├─ DELETE /api/saved-queries/:id  delete one of the caller's own
         ├─ GET  /api/settings         the caller's personal settings, one JSON object (needs the state store)
         ├─ PUT  /api/settings/:key    set one (body = any JSON value, at most 16 KiB)
         ├─ DELETE /api/settings/:key  remove one
         ├─ GET  /api/query-log        the caller's queries (admins: everyone's), keyset-paged, one query per row
         ├─ GET  /api/query-log/:id    one query's detail: timing, tables read, error code
         ├─ GET  /api/apps             installed apps: live status + images
         ├─ GET  /api/apps/:id         one app: workloads, facts, links, embed check
         ├─ GET  /api/workloads        workloads in Nova's namespace + read state (ok/stale/unreachable)
         ├─ GET  /api/admin/catalogs   every Catalog and its grants (administrators only)
         ├─ PUT  /api/admin/catalogs/:name/access  change owner, managed access and grants (administrators only)
         ├─ GET  /api/connections      the caller's connections (needs the secret store)
         ├─ POST /api/connections      create one: the values go to the store, never back
         ├─ PUT  /api/connections/…/secret  rotate fields of one connection
         ├─ DELETE /api/connections/…  delete one connection
         ├─ POST /api/connections/group/:group/:name/break-glass  emergency read (administrators only)
         ├─ GET  /api/connections/break-glass-log  who used break-glass and why (administrators only)
         ├─ GET  /api/pipelines        the pipelines of the caller's groups (administrators: all)
         ├─ GET  /api/pipelines/principals  each of the caller's groups and its pipeline principal
         ├─ POST /api/pipelines        schedule a statement or a published notebook: a Nova-owned Pipeline
         ├─ POST /api/pipelines/check  what the engine says about a statement (EXPLAIN, never runs it)
         ├─ POST /api/pipelines/preview-schedule  the next fire times of a cron in its zone
         ├─ GET  /api/pipelines/:namespace/:name  one pipeline: latest runs, params, options
         ├─ PUT  /api/pipelines/:namespace/:name  change the statement or the notebook version, schedule, params and options
         ├─ DELETE /api/pipelines/:namespace/:name  delete a Nova pipeline (kubectl/GitOps ones are 409)
         ├─ POST /api/pipelines/:namespace/:name/runs  run now (409 paused/active/unloaded, 429 over 10 a minute)
         ├─ POST /api/pipelines/:namespace/:name/pause  pause (idempotent, answers the fresh view)
         ├─ POST /api/pipelines/:namespace/:name/resume  resume (idempotent, answers the fresh view)
         ├─ GET  /api/pipelines/…/runs/:run_id    one run with its task instances
         ├─ POST /api/pipelines/…/runs/:run_id/stop  mark a run failed — not a cancel
         ├─ GET  /api/pipelines/…/tasks/:task_id/logs  one task's log, paged and capped
         ├─ POST /api/ingest/upload    load CSV/JSON into a table (a failed load keeps the old table)
         ├─ POST /api/transfer         copy table → table (a failed copy keeps the target)
         │
         └─ /*                        React SPA (static files)
```

### HTTP conventions

Every non-2xx answer under `/api/` is `{"error": "..."}` with
`content-type: application/json` — including a malformed or missing request body
(400/415/422) and an unknown path, which is `404 {"error":"not found"}` rather than
the SPA's `index.html`. The 401 that starts sign-in keeps its `detail`/`login` fields.
Every response carries `X-Content-Type-Options: nosniff`, `X-Frame-Options: SAMEORIGIN`,
`Content-Security-Policy: frame-ancestors 'self'` and `Referrer-Policy: same-origin`;
`/api/*` responses additionally carry `Cache-Control: no-store` (static assets do not,
their names are hashed). Upstream failures answer one sentence — Trino's raw error
document, with its Java stack, stays in Nova's log at debug level.

### Table documentation

`GET /api/catalog/tables/:ns/:tbl/documentation` reads a table's description, tags,
owner, domain and every column of the **current** schema, plus `canEdit`/`editRule`.
`PUT` on the same path applies any subset of `{description, tags, owner, domain}` in one
Lakekeeper commit; `PUT …/columns/:column/description` writes one column's description
through Trino.

The fields live in the catalog, so every engine sees them:

| Field | Where |
|---|---|
| description | Iceberg property `comment` (the REST `set-properties` path, one commit) |
| tags | custom property `stardelt.tags` (sorted, comma separated; at most 20, `^[a-z0-9][a-z0-9_-]{0,31}$`) |
| owner | custom property `stardelt.owner` (a group name) |
| domain | custom property `stardelt.domain` (`^[a-z0-9][a-z0-9_-]{0,39}$`) |
| column description | the field's `doc` in a new Iceberg schema — only `COMMENT ON COLUMN` writes it, run as the caller with source `nova-catalog` |

A write body is a patch: an absent key changes nothing, `null` (or `[]`, or a blank
description) removes the property, and a write with nothing to change sends no commit.
Descriptions are capped at 4,000 characters. Writes are serialised **per table** in the
process and a commit answered 500/503 is retried (three attempts), because Lakekeeper
0.12.2 deadlocks on concurrent commits to one table; a catalog that stays busy answers
`502 {"error":"the catalog is busy, try again"}`.

**Interim rule, enforced by Nova and not yet by the catalog:** an administrator may edit;
a member of the owner group may edit; a table with no owner may be edited by anybody, but
the first editor must set the owner to one of their own groups. Everyone else gets 403.
Anyone who reaches Lakekeeper or Trino directly can still change these properties until
the catalog enforces permissions. The lock covers one Nova replica; more than one replica
needs a database lock (a later change). Demo mode answers 503.

### Several catalogs

The platform runs several catalogs (one Lakekeeper warehouse each, rendered into Trino as
one catalog per warehouse). **A person sees only the catalogs they may browse**, and Nova
does not decide that itself: it asks the engine.

`GET /api/catalog/catalogs` answers the caller's catalogs, each with its warehouse id and
— when it is one — its medallion layer, plus the default:

```json
{"catalogs": [{"name": "bronze", "id": "3d0a…", "layer": "bronze"},
              {"name": "finance", "id": "5e6c…"},
              {"name": "warehouse", "id": "a1b2…"}],
 "default": "warehouse"}
```

The order is **bronze, silver, gold first** (in that order), then every other catalog
alphabetically. The layer is the `stardelt.io/layer` label on the catalog's `Catalog`
resource, and when the label is absent the catalog's own name if it is one of the three;
labels are not sensitive, and a value that is not `bronze`, `silver` or `gold` is ignored.
The label list is read once and cached for 60 s, so a changed label shows up within a
minute; without a cluster connection only the name convention applies. `layer` is absent,
not `null`, for every other catalog.

Every `/api/catalog/*` route takes an optional `?catalog=<name>`; omitted, the default is
used — the configured `NOVA_WAREHOUSE_NAME` when the caller may browse it, otherwise the
first catalog they may. What the list contains is what Trino's `SHOW CATALOGS` answers
**for that person, through their own token** (the platform's permission bridge filters it);
Nova only drops the engine's own catalogs — `system`, `information_schema`, `memory`,
`tpch`, `tpcds`, `jmx`. The answer is cached for 30 s per person, so a grant change is
visible within half a minute without a statement per click. Nova never proxies a catalog
outside that list:

| Request | Answer |
|---|---|
| `?catalog=finance`, allowed | the catalog's namespaces/tables |
| `?catalog=product`, a catalog the caller may not browse | `403 {"error":"you cannot browse catalog 'product'"}` |
| `?catalog=nope`, no catalog by that name | `404 {"error":"catalog 'nope' not found"}` |
| `?catalog=System`, not a catalog name (1–40 chars, `[a-z][a-z0-9]*`) | `400 {"error":"catalog name must be lower-case letters and digits"}` |
| no catalog is open to the caller and none was named | `403 {"error":"no catalog is open to you yet; ask an administrator for access"}` |

Unreachable Trino or Lakekeeper stays `502`; a configured default warehouse Lakekeeper does
not know is `404` on the catalog routes (`503` on `/api/catalog/warehouse`, which names the
configured warehouse itself). In `dev` mode and in demo mode there is one catalog — the
configured warehouse — and no permission bridge, so every request works exactly as before.
Table documentation, history and rollback are scoped to the same catalog: the SQL they run
names `"<catalog>".<ns>.<tbl>`, and the per-table edit lock keys on all three.

In the UI the **Catalog** page and the **SQL editor** carry the choice: a switcher above the
table filter (and above the editor) lists the catalogs the API returned, writes
`?catalog=` into the URL, and every request the pages make names it. With one catalog — demo
mode, a single-warehouse install — the switcher is not shown at all. Generated SQL names the
chosen catalog (`finance.seed.nation`), so a statement keeps working after it is pasted
elsewhere; a catalog the person may not browse answers with the API's own refusal instead of
an empty list.

### Overview numbers

`GET /api/catalog/metrics` answers what is in the lakehouse for the catalogs the caller may
browse — the tile on the Overview page instead of a static "Format: Iceberg":

```json
{"catalogs": [{"name": "finance", "schemas": 1, "tables": 4, "records": 1200,
               "sizeBytes": 524288, "lastCommitMs": 1761000000000, "truncated": false}],
 "totals": {"catalogs": 1, "schemas": 1, "tables": 4, "records": 1200, "sizeBytes": 524288},
 "generatedAt": 1761000001234}
```

The list is the caller's own (`GET /api/catalog/catalogs`): a catalog they may not browse is
never read and never appears. Rows carry the same `layer` and the same order — bronze,
silver, gold first, then by name. The numbers come from each table's Iceberg metadata — the
current snapshot's `total-records`, `total-files-size` and `timestamp-ms` — so the endpoint
runs no query over the data. They are cached per catalog for 60 s (they describe the
catalog, not the person), and at most 200 tables per catalog are loaded: `tables` stays
exact, `truncated: true` says `records` and `sizeBytes` are a sample. A catalog that cannot
be read is listed with zeros and an `error` sentence rather than failing the page; only an
unreachable engine or catalog is `502`, and no catalog open to the caller is `403` with the
sentence above. Demo mode derives the numbers from its seeded tables.

### Catalog permissions

An administrator changes a catalog's owner, managed access and grants in **Admin →
Catalogs**: **Edit** on a row opens a dialog with the owner, the managed-access switch and
one line per group with the five verbs (`browse`, `select`, `write`, `create`, `manage`).
**Save** writes the change; removing every grant, or removing `stardelt-users` from
`warehouse`, asks for a second **Confirm** because those actions hide the catalog from
people who are not administrators.

The dialog writes a JSON merge patch on the `Catalog` resource (`spec.owner`,
`spec.managedAccess` and the whole `spec.grants` array), carrying the `resourceVersion`
the page read. A concurrent change is a `409` and the dialog says so instead of
overwriting; more than 30 changes per administrator in an hour is a `429`. Nova never
writes Lakekeeper: the resource is the only state, and the access-sync job applies the new
spec to Lakekeeper every two minutes. The row says `Applying…` until the job reports
`status.observedGeneration` equal to the generation the save answered, then `Applied`, or
the sync's own verdict (a group that does not exist in the realm is `UnknownGroup`; it
grants nothing for that group).

The same change is one ordinary resource edit, so kubectl or Git works exactly as the
dialog does:

```yaml
apiVersion: stardelt.io/v1alpha1
kind: Catalog
metadata:
  name: sandbox
  namespace: stardelt
spec:
  lakehouseRef:
    name: default
  owner: stardelt-finops
  managedAccess: true
  grants:
    - group: stardelt-users
      verb: browse
    - group: stardelt-finops
      verb: select
```

```bash
kubectl -n stardelt apply -f sandbox-catalog.yaml
# or a one-field change, the same merge patch the dialog sends:
kubectl -n stardelt patch catalog sandbox --type merge \
  -p '{"spec":{"grants":[{"group":"stardelt-users","verb":"browse"},{"group":"stardelt-finops","verb":"select"}]}}'
```

The resource stays the source of truth. A later GitOps source or `kubectl apply` of the
same resource overwrites a change made in the dialog; keep the manifest as the record, or
do not reconcile this resource from both places. Nova's ServiceAccount needs `patch` on
`catalogs.stardelt.io` for the dialog (part of the platform's `nova-pipelines` Role);
without a cluster connection the route answers `503` and demo mode changes memory.

## Demo mode (no cluster)

`NOVA_DEMO=1` serves an in-memory lakehouse — a seeded Iceberg-shaped catalog
(`nyc_taxi`, `analytics`), a small SQL responder, and working load/transfer — so the
whole UI can be explored without Kubernetes:

```bash
make demo        # backend :8080 + frontend :5173
```

In this mode every operation mutates memory and is recorded as a job. Set
`NOVA_DEMO` unset to talk to real Trino and Lakekeeper.

```
stardelt-nova/
├── backend/          Rust crate (axum, tower-http, reqwest, sqlx)
│   ├── src/
│   │   ├── main.rs   server setup, /api/health, /api/me, /api/trino
│   │   ├── api_json.rs  `ApiJson` body extractor: JSON, never text, on rejection
│   │   ├── catalog.rs Lakekeeper proxy routes
│   │   ├── catalog_docs.rs  table documentation: description, tags, owner, domain, column docs
│   │   ├── connection_routes.rs  `/api/connections`: list, create, rotate, delete, break-glass
│   │   ├── connections.rs  connection rules: validation, authorization, payloads
│   │   ├── secret_store.rs  secret store client, signed in with the caller's own token
│   │   ├── db.rs     optional PostgreSQL state store: pool, migrations, health
│   │   ├── saved_queries.rs  /api/saved-queries (owner-scoped rows in the state store)
│   │   ├── user_settings.rs  /api/settings (one JSON value per person and key)
│   │   └── trino.rs  SQL query proxy
│   └── migrations/   numbered `NNNN_name.sql` files, embedded in the binary (sqlx)
├── frontend/         Vite + React + TypeScript + Tailwind SPA
│   └── public/       brand assets (logo.svg, logo-wordmark.svg)
├── image/
│   └── Dockerfile    multi-stage build: node → rust → debian-slim
├── Cargo.toml        workspace root (members: ["backend"])
└── Makefile          dev / build / image targets
```

---

## Platform apps

`/apps` lists every service Nova knows about with its live status and the images
its workloads actually run. `/apps/:id` opens one service: the workloads and
their replicas, deep links into the app's own UI, the URL Nova probes for
liveness, and facts read from the service's own API —

| App | Facts Nova reads |
|---|---|
| Trino | engine version, state, coordinator, uptime (`/v1/info`) |
| Lakekeeper | warehouse name + id, catalog version, bootstrap state, authorization backend, license, dependency health (`/management/v1/*`, `/health`) |
| Superset | health, version (`/health`) |
| Airflow | per-component health: metadata database, scheduler, triggerer, DAG processor (`/health`) |
| JupyterHub | hub/proxy/scheduler workloads and their replicas — the hub API needs a token, so Nova reads no fact from it |

Every fact names its source and is omitted when the service does not answer: the
page shows what Nova observed, never a guess.

### Embedded apps

Nova frames Superset (`/dashboards`) and JupyterHub (`/notebooks`) in an iframe,
and both ship with headers that refuse framing by default — JupyterHub sends
`content-security-policy: frame-ancestors 'none'`, Superset `x-frame-options`.
The browser enforces that client-side, so the naive result is a blank frame with
the reason hidden in the console.

`/api/apps/:id` therefore reports what the app itself says (`embed.state`:
`allowed` / `blocked` / `unknown`, with the header that blocks it), read from the
app's own response. Those pages show an “embeds ok” badge when framing is
permitted, and the offending header plus what to change when it is not — while
still offering the deep link out. `unknown` (demo mode, or a service Nova cannot
reach) makes no claim either way.

The header check follows redirects only while they stay on the app's own hosts:
a signed-out JupyterHub visitor is sent through the hub's login page (the same
headers as its pages) and then off to Keycloak, and judging Keycloak's login page
would report “blocked” for an app that is frameable. That off-host hop is
reported as `embed.sign_in_host` instead, and the Notebooks page turns it into an
explicit “open in a new tab and sign in” action — a deliberately unframeable
sign-in page must never look like a broken notebook.

---

## Dev quickstart

**Prerequisites:** Rust stable, Node 20+, npm, Docker (for image builds).

```bash
# Clone and install frontend deps
git clone https://github.com/stardelt/stardelt-nova.git
cd stardelt-nova
npm --prefix frontend install

# Start both servers in one terminal
make dev
```

| Service | URL |
|---------|-----|
| React SPA (Vite HMR) | <http://localhost:5173> |
| Rust backend | <http://localhost:8080> |

The Vite dev server is already configured to proxy `/api/*` → `http://localhost:8080`,
so the frontend can reach the backend transparently.

**Useful targets:**

```bash
make check   # cargo check + TypeScript type-check (fast, no emit)
make fmt     # cargo fmt + frontend lint
make build   # release binary + production frontend bundle
make image   # build container image locally (tag: ghcr.io/stardelt/nova:dev)
```

---

## UI conformance suite

`npm --prefix frontend run e2e` opens every page in a real browser (dark and light, desktop and phone width)
against the backend with no cluster behind it and checks the rules in the design note (accessible names,
contrast, page headings, readable errors, admin pages hidden from ordinary users, hostile names). Needs
`cargo build`, `npm --prefix frontend run build` and `npx playwright install chromium` once. A finding that already has
a ticket is listed in `frontend/e2e/known.ts`; a new finding fails the run. CI runs it as the job `ui-conformance`.

## Building the image

```bash
make image
# or
docker build -t ghcr.io/stardelt/nova:dev -f image/Dockerfile .
```

The multi-stage `image/Dockerfile` produces a `debian:bookworm-slim` image
with the static frontend served directly by the backend — no separate web
server required.

---

## Configuration

The backend is configured entirely through environment variables:

| Variable | Default | Description |
|---|---|---|
| `NOVA_BIND_ADDR` | `0.0.0.0:8080` | TCP address the HTTP server listens on |
| `NOVA_INTERNAL_ADDR` | — (off) | Second, internal-only listener that receives the engine's query events (e.g. `0.0.0.0:8081`). Opened only when `NOVA_EVENTS_TOKEN` is set too; with the address alone a warning is logged and no second port opens. The route exists on this listener only — never on the public port, never through the Ingress |
| `NOVA_EVENTS_TOKEN` | — (required with the address) | Shared bearer token the engine presents on `POST /internal/trino-events` (env from the `nova-events` Secret). Wrong or missing token = `401`; full queue = `503`; accepted = `204`. Compared in constant time and never logged |
| `NOVA_STATIC_DIR` | `/app/static` | Directory containing the compiled frontend assets |
| `NOVA_TRINO_URL` | `http://trino.stardelt.svc.cluster.local:8080` | Trino coordinator base URL. The platform's installer points this at the coordinator's in-cluster HTTPS endpoint (`https://trino.<ns>.svc.cluster.local:8443`) once an IdP is installed — see `NOVA_TRINO_CA_PEM` |
| `NOVA_TRINO_CA_PEM` | — (empty) | PEM of a CA to **add** to the TLS roots the upstream client trusts (the platform's internal CA for the coordinator certificate, from the `trino-ca` Secret). Empty = only the public WebPKI roots; never a replacement, and verification is never disabled |
| `NOVA_LAKEKEEPER_URL` | `http://lakekeeper.stardelt.svc.cluster.local:8181` | Lakekeeper REST catalog base URL |
| `NOVA_CATALOG_TOKEN_URL` | — (empty) | The Keycloak token endpoint for Nova's own catalog client, e.g. `https://id.<domain>/realms/stardelt/protocol/openid-connect/token`. With the two below, every Lakekeeper call carries `Authorization: Bearer <token>`; unset, no header is sent (the catalog still answers `allowall`). All three `NOVA_CATALOG_*` variables or none — a partial set refuses to start rather than calling the catalog unauthenticated |
| `NOVA_CATALOG_CLIENT_ID` | — (required with the URL) | Confidential client id `svc-catalog-nova` (audience `lakekeeper`, stardelt-platform#282) |
| `NOVA_CATALOG_CLIENT_SECRET` | — (required with the id) | That client's secret, from the `svc-catalog-nova` Secret (key `client-secret`). Never logged, and no error or response ever carries it |
| `NOVA_WAREHOUSE_NAME` | `warehouse` | The **default catalog**: the Lakekeeper warehouse the catalog routes work in when a request names none and the caller may browse it. `/api/catalog/warehouse` and the dashboard facts always describe this warehouse |
| `NOVA_SUPERSET_URL` | `http://superset:8088` | Superset base URL (probe + embed check) |
| `NOVA_JUPYTERHUB_URL` | `http://proxy-public:80` | JupyterHub public proxy base URL (probe + embed check) |
| `NOVA_AIRFLOW_URL` | — (empty) | Airflow API server base URL, plain HTTP in-cluster (`http://airflow-api-server:8080`). Needs `NOVA_AIRFLOW_USER` and `NOVA_AIRFLOW_PASSWORD` too; unless all three are set the pipelines API is disabled and says so. Nova signs in once as that service user (built-in role `User`, enough to read, trigger, pause and mark a run failed), keeps the 24 h token until shortly before it expires, and decides itself who may see what — Airflow cannot scope the credential to owner groups |
| `NOVA_AIRFLOW_USER` | — (required with the URL) | The Airflow user Nova signs in as (`nova-api`) |
| `NOVA_AIRFLOW_PASSWORD` | — (required with the user) | That user's password, from a Kubernetes Secret. Never logged, never part of an error |
| `NOVA_S3_URL` | — (empty) | S3 endpoint of the versioned `notebooks` bucket, plain HTTP in-cluster (`http://seaweedfs-s3.stardelt.svc.cluster.local:8333`). Needs the three below; unless all four are set the notebook routes answer `503 {"error":"notebook storage is not configured"}` and publishing is off. A partial set refuses to start |
| `NOVA_S3_ACCESS_KEY` | — (required with the URL) | Access key of the platform identity every object-store consumer already uses (Secret `stardelt-s3-creds`, key `access-key`) |
| `NOVA_S3_SECRET_KEY` | — (required with the URL) | That identity's secret key. Held as an `auth::Secret`; never logged, never part of an error or a response |
| `NOVA_S3_NOTEBOOK_BUCKET` | — (required with the URL) | Bucket for published notebooks (`notebooks`). Versioning must be enabled (`make notebooks-bucket` in stardelt-platform); a publish without a returned version is `502` rather than a Notebook that is not immutable |
| `POD_NAMESPACE` | `stardelt` | Namespace whose workloads Nova reports (downward API in the manifests); `NOVA_NAMESPACE` overrides it for a local run |
| `NOVA_BASE_DOMAIN` | — (empty) | Base domain for the app deep-links (`trino.<domain>`, …). Empty = the UI links nothing; the install sets it from a ConfigMap |
| `NOVA_FORWARD_IDENTITY` | off | Send the caller's own token to Trino (`Authorization: Bearer`). Only useful once the platform configures the engine to validate it — the identity contract's phase 2. With it on, the engine can also filter `SHOW CATALOGS` per person, which is what Nova's catalog list reads (`GET /api/catalog/catalogs`); with it off the engine sees an unauthenticated request. Lakekeeper still sees Nova's own service token (above), never the caller's. `/api/me` reports whether forwarding is on (`identity_forwarding`) |
| `NOVA_QUERY_TIMEOUT_SECS` | `300` | How long `POST /api/query` waits for Trino before cancelling the query server-side and answering `400 {"error":"query exceeded the Ns limit and was cancelled"}`. A client disconnect cancels the Trino query too. Results larger than 50,000 rows are cut to exactly that many and answered with `"truncated": true` |
| `NOVA_DATABASE_URL` | — (empty) | PostgreSQL URL of the state store (`postgres://user:password@host:5432/nova`). Unset = disabled: Nova behaves as before and `/api/health` answers `{"status":"ok","db":"disabled"}`. Set = connect and apply the migrations at start-up, or refuse to start; the platform fills it from the `nova-pg-app` Secret. After start-up an unreachable database only turns `db` into `down` (the probe still answers 200) |
| `NOVA_QUERY_LOG_RETENTION_DAYS` | `90` | How long query-log rows are kept; the backend purges hourly. `0` disables the purge; an unparsable value warns and uses the default |
| `NOVA_SECRET_STORE_URL` | — (empty) | Base URL of the platform's secret store (OpenBao), for example `http://openbao.stardelt.svc.cluster.local:8200`. Unset = disabled: features that need the store answer 503. Nova signs in with the caller's own token and sees only what the store's policies give that person; values are never logged |
| `NOVA_SECRET_STORE_ROLE` | `platform-users` | The store's JWT sign-in role used for the caller's token |
| `NOVA_SECRET_STORE_BREAKGLASS_ROLE` | `breakglass` | The store's JWT sign-in role for an administrator's break-glass read of a group connection: read on `connections/group/*` only, bound to `azp = nova` and the administrator group, 15-minute token. Declared and audited on the store side (stardelt-platform#225) |
| `NOVA_SECRET_STORE_MOUNT` | `secret` | The KV v2 mount that holds `connections/…` |
| `NOVA_DEV_USER` | `stardelt-dev` | Static user identity returned by `/api/me` (dev mode only) |
| `NOVA_AUTH_MODE` | `dev` | `dev` (no login) or `oidc` — see [Sign-in](#sign-in-oidc) |
| `RUST_LOG` | `info` | Tracing filter for the backend (`debug`, `info`, `warn`, …) |

---

## Audit trail

Every **mutating** request to the API gets one structured line on the `audit` tracing target —
actor, subject, how they authenticated, method, path, status:

```
INFO audit: mutating request actor="admin" sub="52f3331c…" via=session method=POST path=/query status=200
```

Reads are not recorded (they dwarf everything else and would bury the signal), and neither query
strings nor request bodies are: the record is the *action*, not its content. Statement-level auditing
belongs in Trino's query events, which see the SQL and are already configured for it.

Refusals land on the same `audit` target from the middleware that refused — a 403 names the caller and
the path — because the audit layer itself sits inside authentication (it needs the principal, and a
refused request never reaches it).

Route it separately from the application log:

```
RUST_LOG=audit=info,info
```

## Connections

A connection is typed settings plus secret fields — a database password, an S3 key. Nova keeps it in
the platform's secret store, never in its own database, and **a value is written and never read back**:
no answer, error sentence or log line carries one, only the field *names* (`secretKeys`).

| Scope | Who may create, rotate, delete | Where the value lives | A `Connection` resource too |
|---|---|---|---|
| `personal` | the person, only their own | `connections/user/<sub>/<name>` | no |
| `group` | a **member** of the owner group; an administrator who is not one may **delete** (incident) but never create or rotate | `connections/group/<group>/<name>` | yes, the operator syncs it |
| `managed` | administrators only; nobody can read it | `connections/managed/<name>` | yes |

`GET /api/connections` lists the caller's own connections, their groups', and — for an
administrator — every other group's (the store names the groups; an administrator's policy lists
every scope's metadata) and the managed ones. Every row carries `member`: `true` for a personal or
managed connection and for a group connection of one of the caller's groups, `false` for a group
row an administrator sees only because they are an administrator. `false` means read-only for that
person — no create, no rotation — except delete and break-glass.

Every route signs in to the store with the caller's own token (never a service identity), so the
store's policies decide what is possible and its audit log names the person. Without
`NOVA_SECRET_STORE_URL` the routes answer `503 {"error":"the secret store is not configured"}`; without
a cluster client the group and managed routes answer `503 {"error":"no cluster connection"}`. An
unreachable store or cluster is `502`, never `500`.

A rotation sends only the fields to change; a managed connection cannot be read, so it sends every
field. Deleting removes the value first and the resource second — if the second step fails the value is
gone and the entry stays visible as not-ready, which is retryable. Each create, rotation and delete
also writes a `connection` line on the `audit` target with the scope, the group, the name and the
person — never a value; a delete by an administrator who is not a member of the group also says
`via=administrator`.

**Break-glass.** In an emergency an administrator may read a **group** connection through
`POST /api/connections/group/<group>/<name>/break-glass` with `{"reason": "…"}` (20 to 500 characters).
The route signs in to the store's `breakglass` role with the administrator's **own** token, reads only
that group's path, revokes the 15-minute store token, and returns the fields **once** with
`Cache-Control: no-store` and `Pragma: no-cache`. Before answering it writes the notice
`stardelt.io/break-glass` (`by`, `at`, `reason`) on the `Connection`, which the group's Connections page
shows as `breakGlass`; a rotation clears it. Every attempt — also a refusal — writes an `audit` line and
a row in `break_glass_read`, which never has a column for a value. Personal and managed connections are
always refused (`403 break-glass covers group connections only`); only administrators may call it, and
at most five attempts per administrator in ten minutes (`429`). If the notice cannot be written, nothing
is returned (`502`) and the attempt is recorded as `notice-failed`. `GET /api/connections/break-glass-log`
returns the last 200 events (newest first) to administrators. Without a state store the log answers
`503 the state store is not configured`; the read itself needs only the secret store.

## Pipelines

A pipeline is a `Pipeline` resource (`stardelt.io/v1alpha1`) plus what Airflow says about it. Nova
shows the pipelines whose `spec.ownerGroup` is one of the caller's groups; an administrator sees all,
and a pipeline without an `ownerGroup` is administrators-only. Whoever may see one may act on it — v1
has no read-only role — and a pipeline of another group answers `404`, not `403`.

*Schedule* in the SQL editor writes the resource; the loader turns it into a DAG. Nova validates what
the loader would refuse, so a pipeline it writes always becomes a DAG: the name (1 to 40 lower-case
letters, digits and dashes, starting with a letter), the statement (at most 20,000 characters, one
trailing `;` dropped), its parameter table (at most 16 names of `[a-z][a-z0-9_]{0,31}`, values up to
256 characters), every `${…}` placeholder the statement uses (`run.date`, `run.id`, or a
`params.<name>` the table defines — an unknown one would fail the run, so it is a 400), a five-field
cron no more often than every five minutes, and an IANA timezone. The optional `options` carry the
retries (0 to 5, default 1), the wait between them (1 to 60 minutes, default 5), the time limit (1 to
720 minutes, default 120) and the run mode (`Forbid`, the default, or `Allow`); an omitted field keeps
its default, so an older body writes what it always wrote.
The owner group must be one of the caller's own — an administrator schedules for the groups they
belong to, never on somebody else's behalf — and the group's principal must exist and be ready:

```bash
# one principal per group; the convention is `pipeline-<group>`
make sync-principal NAME=pipeline-analytics GROUP=analytics
```

`GET /api/pipelines/principals` answers every group of the caller with `principal`, `ready` and, when
not ready, a `message` — the `Ready` condition's, or the `make sync-principal` command for a group
without one. A create for a group without a ready principal answers `409` with that sentence; the
resource carries `runAs: pipeline-<group>` next to `ownerGroup`.

### Scheduled notebooks

`POST /api/pipelines` also schedules a **published notebook version** instead of a statement
(stardelt-nova#237): send `notebook: {"name", "objectVersion", "params"}` and no `sql`. A body gives
exactly one of the two — both or neither is `400 give a statement or a notebook`. The resource then
carries one task `{"name": "run", "kind": "notebook", "config": {"notebook", "objectVersion",
"params"}}`, where `params` is a JSON object string with sorted keys: the shape the operator's
`Pipeline` CRD and the loader accept (operator#130). Everything else — `runAs`, the schedule, the
retries, the time limit, the run mode — is the statement shape's.

Nova validates what the CRD and the loader would refuse: the notebook file name
(`^[A-Za-z0-9][A-Za-z0-9._ -]{0,127}\.ipynb$`), at most 32 parameters named
`[A-Za-z_][A-Za-z0-9_]{0,63}` with values up to 256 characters, a `params` JSON text of at most 4096
characters — and that the pinned version **exists** in the store. That last check is an S3 `HEAD`:
`400 that notebook version does not exist in group <g>` for a version nobody published,
`503 notebook storage is not configured` without `NOVA_S3_*`, `502` when S3 does not answer. A
version is never a branch or a tag, and the loader reads `<ownerGroup>/<notebook>`, so a notebook of
another group can never be named (and a hidden notebook pipeline is a `404`, like any other).

A `PUT` on a notebook pipeline changes the notebook it runs (file name, version and parameters),
its schedule and its options — never what it runs: a body with `sql` is `400 this pipeline runs a
notebook`, and a notebook body on a statement pipeline is `400 this pipeline runs a statement`. The detail route carries the task's `notebook`,
`objectVersion` and `notebookParams`, so the edit dialog can prefill; the list route leaves them out
like `sql`.

Only a pipeline labelled `app.kubernetes.io/managed-by: stardelt-nova` may be changed or deleted: a
`kubectl` or GitOps manifest answers `409 this pipeline is managed outside Nova (kubectl or Git);
change it there`, so Nova never rewrites somebody's file. `PUT` changes `spec.tasks[0].sql` (a
notebook task's `config` instead), `spec.schedule`, `spec.params` and the options and nothing else —
the body describes the whole editable state, so an omitted `schedule` or `params` removes it and an
omitted option keeps its default — carrying the `resourceVersion` it read (a concurrent change is
`409 the pipeline changed while you edited it: reload`); a task shape other than one `run` task of
kind `sql` or `notebook` is `409`. At most ten pipelines may be created per person per hour (`429`,
in memory). Airflow is not needed to create, change or delete one — the resource is the source of
truth and the loader removes a deleted DAG from its default list after about 90 seconds, while the
run history stays readable; the read routes keep answering `503` without Airflow. Each create, change
and delete writes one `audit` line with the statement's **length** and the cron — and, for a
notebook, the group, the file name and the pinned version — never the statement and never a
parameter value.

Two helpers belong to the Schedule dialog. `POST /api/pipelines/check` fills the placeholders and sends
`EXPLAIN <statement>` to the engine as the caller (never `EXPLAIN ANALYZE`, never the statement itself),
so the dialog can say whether the engine accepts it before anything is saved: `{"checked": true, "ok":
true}`, `{"checked": true, "ok": false, "message": "<engine sentence>"}` for a refused statement,
`{"checked": false}` for a kind of statement that cannot be explained, and `502` when the engine does not
answer. `POST /api/pipelines/preview-schedule` answers the next fire times of a cron in its zone
(`{"runs": ["2026-10-21T03:00:00+02:00", …]}`, `count` 1 to 10, default 5), following the same cron and
timezone rules as a create.

## Published notebooks

A notebook is published as **an immutable object version** in the versioned `notebooks` bucket, so a
schedule can pin exactly the bytes a person approved. The bucket and its versioning are created by
stardelt-platform (`make notebooks-bucket`); Nova only writes to it with the platform identity in
`NOVA_S3_*` (see [Configuration](#configuration)), never with the caller's token.

```text
PUT /api/notebooks/{group}/{name}                body: the .ipynb (nbformat 4, at most 5 MiB)
GET /api/notebooks                               the caller's groups, latest version, newest first
GET /api/notebooks/{group}/{name}/versions       at most 50 versions, newest first
GET /api/notebooks/{group}/{name}/parameters     one version's parameters (?version=<id>, latest when omitted)
```

Publishing needs a signed-in person (browser session or `Authorization: Bearer`). `{group}` must be
one of the caller's groups (`403 you are not a member of the group <g>`); `{name}` must match
`^[A-Za-z0-9][A-Za-z0-9._ -]{0,127}\.ipynb$`; the body must be an nbformat 4 notebook with a `cells`
array. **Cell outputs, `execution_count` and the notebook's widget metadata are removed before the
bytes are stored** — a cell that printed `os.environ` leaves no token or secret in the object store.
The answer carries the new `objectVersion`, the `sha256` of the stored bytes, the size and the
notebook's parameters (the `name = literal` lines of the first cell tagged `parameters`, at most 32,
defaults as text and never evaluated). A 61st publish by one person in an hour is `429`. Every publish
writes one `audit` line with the group, the name, the version and the size — never content.

The key is built by Nova as `{group}/{name}`; the browser never supplies a path. Listing and versions
are owner-group scoped: an administrator sees their own groups only, because being a platform
administrator does not make someone a member of a tenant's group. Every list row carries the
immutable `objectVersion`, the size, the publisher's `uploadedBy` and `uploadedAt` **as RFC 3339 in
UTC** (`2026-10-21T01:24:28.000Z`) — the gateway's RFC 1123 `Last-Modified` is normalised, so the UI
parses one shape. Without `NOVA_S3_URL` (or any of the
other three `NOVA_S3_*` variables) the three routes answer
`503 {"error":"notebook storage is not configured"}`; an S3 gateway that is down or refuses answers
`502`, never `500`. Answers are `application/json` with `Cache-Control: no-store`.

`GET …/parameters?version=<id>` answers the parameters of that exact version
(`{"parameters": [{"name": "region", "default": "eu"}, …], "objectVersion": "<id>"}`); without
`version` it reads the latest and names it. The Schedule dialog uses it to offer the form the bytes
it will run declare. An unknown version is `404 no such notebook version` — never an empty list a
schedule could pin.

The hub token already carries audience `nova` (the `aud-nova` mapper, stardelt-platform#290), which is
Nova's client id, so a notebook can call `PUT /api/notebooks/...` with the token it holds.

## Sign-in (OIDC)

Nova is an OIDC *client*; the identity provider is either your own (Microsoft Entra
ID, Okta, any standards-compliant OIDC provider) or the platform's Keycloak. The
contract is [Identity](https://stardelt.io/architecture/identity).

`NOVA_AUTH_MODE=dev` (the default) keeps the no-login behaviour that CI
and `make demo` use. `NOVA_AUTH_MODE=oidc` puts every `/api/*` route except
`/api/health` and `/api/auth/*` behind sign-in: a browser gets a 401 and the UI sends
it to `/api/auth/login?return_to=<path>` (authorization code + PKCE + `state` +
`nonce`); scripts send `Authorization: Bearer <access token>`. Any configuration
error in `oidc` mode makes Nova refuse to start; an unreachable IdP is a failed
sign-in, never a fallback to `dev`.

| Variable | Default | Description |
|---|---|---|
| `NOVA_OIDC_ISSUER` | — (required) | Issuer URL; discovery at `/.well-known/openid-configuration` must report the same issuer. Entra: `https://login.microsoftonline.com/<tenant-id>/v2.0`; Keycloak: `https://id.<baseDomain>/realms/stardelt`. `https` only (plain `http` only for localhost). |
| `NOVA_OIDC_CLIENT_ID` | — (required) | Registered (confidential) client id |
| `NOVA_OIDC_CLIENT_SECRET` | — (required) | Client secret |
| `NOVA_PUBLIC_URL` | — (required) | Nova's external origin, e.g. `https://nova.example.com`. Redirect URI is `<NOVA_PUBLIC_URL>/api/auth/callback`; post-logout redirect is `<NOVA_PUBLIC_URL>/` — register both at the IdP. Cookies are `Secure` when this is `https`. |
| `NOVA_SESSION_SECRET` | — (required) | ≥ 32 bytes; signs the session cookie (`openssl rand -base64 48`) |
| `NOVA_OIDC_SCOPES` | `openid profile email` | Must include `openid` |
| `NOVA_OIDC_GROUP_CLAIM` | `groups` | Claim carrying group names (or Entra group object ids) |
| `NOVA_OIDC_ADMIN_GROUP` | `stardelt-admins` | Group that makes a Nova admin; Keycloak's full path `/stardelt-admins` also matches. Set it to an empty value to turn the group mechanism off (nobody becomes an admin through a group). |
| `NOVA_OIDC_ADMIN_ROLE` | = admin group | Value in the `roles` claim (Entra app roles) that makes a Nova admin. Empty turns the role mechanism off. |
| `NOVA_OIDC_AUDIENCES` | client id | Comma-separated `aud` values accepted on bearer access tokens |

**Claims.** `sub` is the identifier; display uses `preferred_username` → `email` →
`upn` → `name`. Admin precedence: the `roles` claim is checked against
`NOVA_OIDC_ADMIN_ROLE` first, then the group claim against `NOVA_OIDC_ADMIN_GROUP`;
either grants admin. When Entra reports a *group overage* (`_claim_names.groups` or
`hasgroups`), Nova logs a warning and treats the user as having no groups (it does
not call Graph) — use app roles or a group filter on the app registration. `/api/me`
returns claims, never tokens, and `identity_forwarding: false`: Nova still queries
Trino and Lakekeeper with its own service connection — and Lakekeeper with the
`svc-catalog-nova` service token, never the caller's. Catalog *visibility* is the one
question Nova asks with the caller's token (`SHOW CATALOGS`, see "Several catalogs"),
which needs `NOVA_FORWARD_IDENTITY=1`.

**The person behind the request.** In `oidc` mode `/api/me` adds three facts to the
claim fields, so the Profile page can explain the caller's role and warn before a
sign-in is needed. `dev` mode keeps exactly its old answer.

| Field | Meaning |
|---|---|
| `admin_group` | The configured value behind the caller's administrator decision: the group that matched (`NOVA_OIDC_ADMIN_GROUP`) or the role that was held (`NOVA_OIDC_ADMIN_ROLE`); for a member, the configured precedence (role, then group). `null` when neither is configured (both variables empty). |
| `admin_by` | `"group"` or `"role"`: which of the two `admin_group` holds. `null` with it. |
| `session_expires_at` | Unix seconds at which the browser session can no longer be renewed — the session's absolute 8 h lifetime (a refresh never moves it) — so the person signs in again then. `null` for an `Authorization: Bearer` call (no session), and never the access token's own `exp`. |

**Entra ID notes.** Single-tenant issuer only (the `common`/`organizations` endpoints'
`{tenantid}` issuer template is not supported). For bearer access, set
`accessTokenAcceptedVersion: 2` in the app manifest so access tokens carry the v2.0
issuer, and list the token's `aud` in `NOVA_OIDC_AUDIENCES` (e.g. `api://<client-id>`).
Tokens Entra issues for Microsoft Graph cannot be validated by anyone but Graph.

**Token refresh.** One sign-in outlives many access tokens. There is no timer and no
background task: when a request finds the session's access token inside its refresh
window, that request exchanges the refresh token at the IdP's token endpoint (the same
client id, secret and auth method as the code exchange), stores the new tokens and
re-derives the identity from the refreshed claims before serving anything. So

- a token that expires mid-session does not turn Trino queries into 401s — the request
  carrying it is served with a fresh one;
- group changes take effect within one token lifetime, not at the end of the session: a
  user removed from `NOVA_OIDC_ADMIN_GROUP` loses `/api/me`'s `is_admin`, the admin UI
  and the admin API as soon as their next token is fetched, with no restart and no
  re-login;
- the refresh window is `min(60 s, half the token's life)`, so a realm with 60-second
  tokens refreshes half-way through them instead of on every request. `expires_in`
  schedules the next refresh; when the IdP reports neither `expires_in` nor a decodable
  `exp`, Nova assumes 300 s and says so in the log.

Refreshes are serialized per session (Keycloak rotates refresh tokens, so spending one
twice would invalidate the session). Any refresh failure — revoked session, spent refresh
token, an unreachable IdP, or tokens whose claims cannot be verified — drops the session
and answers a plain `401 {"detail":"not signed in","error":"unauthenticated",
"login":"/api/auth/login"}`, so the browser signs in again instead of looping; a refresh
never extends the 8 h `SESSION_TTL`. Nova keeps the refresh token server-side only, never
in a cookie, a URL, a log line or a response body.

**Keycloak notes.** Add a *Group Membership* mapper (`groups`) to the `nova` client;
for bearer access add an *Audience* mapper so access tokens carry `aud: nova`.
With a Keycloak-style issuer (`…/realms/<realm>`) `/api/me` also carries
`admin_console_url`, the realm's own admin console (`…/admin/<realm>/console/`); the
sidebar links to it as **Users** for administrators. It is `null` in `dev` mode, for
non-Keycloak issuers (Entra ID) and for the master realm — Nova never links to a console
it cannot address, and the console still enforces who may manage users.
Keycloak shows a logout confirmation page because Nova does not put the ID token in
the end-session URL (`client_id` + `post_logout_redirect_uri` are sent instead).

**Limitations.** Sessions are held in memory: run one Nova replica in `oidc` mode; a
restart signs everyone out. Nova does not request `offline_access`, so the refresh token
lives as long as the IdP's SSO session while it is in use (Keycloak's defaults: 30 min
idle, 10 h max) — a session left idle past that signs in again rather than refreshing.
Revoking a *single* session at the IdP is noticed at the next refresh (within one token
lifetime); the session is otherwise not revoked.

---

## Links

- [stardelt.io](https://stardelt.io) — full documentation
- [Architecture overview](https://stardelt.io/architecture) — how Nova fits into the stardelt platform
- [stardelt/stardelt](https://github.com/stardelt/stardelt) — mono-repo with Helm charts and platform infrastructure
