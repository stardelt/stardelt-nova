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
         ├─ GET  /api/catalog/*        → Lakekeeper REST catalog
         ├─ POST /api/catalog/namespaces  create schema
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
         ├─ GET  /api/connections      the caller's connections (needs the secret store)
         ├─ POST /api/connections      create one: the values go to the store, never back
         ├─ PUT  /api/connections/…/secret  rotate fields of one connection
         ├─ DELETE /api/connections/…  delete one connection
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
│   │   ├── connection_routes.rs  `/api/connections`: list, create, rotate, delete
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
| `NOVA_WAREHOUSE_NAME` | `warehouse` | Lakekeeper warehouse name to resolve on startup |
| `NOVA_SUPERSET_URL` | `http://superset:8088` | Superset base URL (probe + embed check) |
| `NOVA_JUPYTERHUB_URL` | `http://proxy-public:80` | JupyterHub public proxy base URL (probe + embed check) |
| `NOVA_AIRFLOW_URL` | — (empty) | Airflow API server base URL, plain HTTP in-cluster (`http://airflow-api-server:8080`). Needs `NOVA_AIRFLOW_USER` and `NOVA_AIRFLOW_PASSWORD` too; unless all three are set the pipelines API is disabled and says so. Nova signs in once as that service user (built-in role `User`, enough to read, trigger, pause and mark a run failed), keeps the 24 h token until shortly before it expires, and decides itself who may see what — Airflow cannot scope the credential to owner groups |
| `NOVA_AIRFLOW_USER` | — (required with the URL) | The Airflow user Nova signs in as (`nova-api`) |
| `NOVA_AIRFLOW_PASSWORD` | — (required with the user) | That user's password, from a Kubernetes Secret. Never logged, never part of an error |
| `POD_NAMESPACE` | `stardelt` | Namespace whose workloads Nova reports (downward API in the manifests); `NOVA_NAMESPACE` overrides it for a local run |
| `NOVA_BASE_DOMAIN` | — (empty) | Base domain for the app deep-links (`trino.<domain>`, …). Empty = the UI links nothing; the install sets it from a ConfigMap |
| `NOVA_FORWARD_IDENTITY` | off | Send the caller's own token to Trino and the catalog (`Authorization: Bearer`). Only useful once the platform configures those to validate it — the identity contract's phase 2; until then the catalog sees Nova's service connection and `/api/me` says so |
| `NOVA_QUERY_TIMEOUT_SECS` | `300` | How long `POST /api/query` waits for Trino before cancelling the query server-side and answering `400 {"error":"query exceeded the Ns limit and was cancelled"}`. A client disconnect cancels the Trino query too. Results larger than 50,000 rows are cut to exactly that many and answered with `"truncated": true` |
| `NOVA_DATABASE_URL` | — (empty) | PostgreSQL URL of the state store (`postgres://user:password@host:5432/nova`). Unset = disabled: Nova behaves as before and `/api/health` answers `{"status":"ok","db":"disabled"}`. Set = connect and apply the migrations at start-up, or refuse to start; the platform fills it from the `nova-pg-app` Secret. After start-up an unreachable database only turns `db` into `down` (the probe still answers 200) |
| `NOVA_QUERY_LOG_RETENTION_DAYS` | `90` | How long query-log rows are kept; the backend purges hourly. `0` disables the purge; an unparsable value warns and uses the default |
| `NOVA_SECRET_STORE_URL` | — (empty) | Base URL of the platform's secret store (OpenBao), for example `http://openbao.stardelt.svc.cluster.local:8200`. Unset = disabled: features that need the store answer 503. Nova signs in with the caller's own token and sees only what the store's policies give that person; values are never logged |
| `NOVA_SECRET_STORE_ROLE` | `platform-users` | The store's JWT sign-in role used for the caller's token |
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
| `group` | a **member** of the owner group; being an administrator is not enough | `connections/group/<group>/<name>` | yes, the operator syncs it |
| `managed` | administrators only; nobody can read it | `connections/managed/<name>` | yes |

Every route signs in to the store with the caller's own token (never a service identity), so the
store's policies decide what is possible and its audit log names the person. Without
`NOVA_SECRET_STORE_URL` the routes answer `503 {"error":"the secret store is not configured"}`; without
a cluster client the group and managed routes answer `503 {"error":"no cluster connection"}`. An
unreachable store or cluster is `502`, never `500`.

A rotation sends only the fields to change; a managed connection cannot be read, so it sends every
field. Deleting removes the value first and the resource second — if the second step fails the value is
gone and the entry stays visible as not-ready, which is retryable. Each create, rotation and delete
also writes a `connection` line on the `audit` target with the scope, the group, the name and the
person — never a value.

## Sign-in (OIDC)

Nova is an OIDC *client*; the identity provider is either your own (Microsoft Entra
ID, Okta, any standards-compliant OIDC provider) or the platform's Keycloak. The
contract is [Identity](https://stardelt.io/architecture/identity).

`NOVA_AUTH_MODE=dev` (the default) keeps the no-login behaviour that CI, the kind gate
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
| `NOVA_OIDC_ADMIN_GROUP` | `stardelt-admins` | Group that makes a Nova admin; Keycloak's full path `/stardelt-admins` also matches |
| `NOVA_OIDC_ADMIN_ROLE` | = admin group | Value in the `roles` claim (Entra app roles) that makes a Nova admin |
| `NOVA_OIDC_AUDIENCES` | client id | Comma-separated `aud` values accepted on bearer access tokens |

**Claims.** `sub` is the identifier; display uses `preferred_username` → `email` →
`upn` → `name`. Admin precedence: the `roles` claim is checked against
`NOVA_OIDC_ADMIN_ROLE` first, then the group claim against `NOVA_OIDC_ADMIN_GROUP`;
either grants admin. When Entra reports a *group overage* (`_claim_names.groups` or
`hasgroups`), Nova logs a warning and treats the user as having no groups (it does
not call Graph) — use app roles or a group filter on the app registration. `/api/me`
returns claims, never tokens, and `identity_forwarding: false`: Nova still queries
Trino and Lakekeeper with its service connection.

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
