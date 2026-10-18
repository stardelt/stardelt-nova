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
         ├─ GET  /api/health           liveness (never behind sign-in)
         ├─ GET  /api/me               current user + demo flag
         ├─ GET  /api/auth/login       start OIDC sign-in (oidc mode)
         ├─ GET  /api/auth/callback    OIDC redirect target (oidc mode)
         ├─ POST /api/auth/logout      sign out here and at the IdP
         ├─ GET  /api/trino/cluster    → Trino /v1/info
         ├─ GET  /api/catalog/*        → Lakekeeper REST catalog
         ├─ POST /api/catalog/namespaces  create schema
         ├─ POST /api/query            → Trino query engine
         ├─ GET  /api/jobs             recent activity
         ├─ GET  /api/apps             installed apps: live status + images
         ├─ GET  /api/apps/:id         one app: workloads, facts, links, embed check
         ├─ GET  /api/workloads        workloads in Nova's namespace + read state (ok/stale/unreachable)
         ├─ POST /api/ingest/upload    load CSV/JSON into a table
         ├─ POST /api/transfer         copy table → table
         │
         └─ /*                        React SPA (static files)
```

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
├── backend/          Rust crate (axum, tower-http, reqwest)
│   └── src/
│       ├── main.rs   server setup, /api/health, /api/me, /api/trino
│       ├── catalog.rs Lakekeeper proxy routes
│       └── trino.rs  SQL query proxy
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
and both ship with headers that refuse framing — JupyterHub sends
`content-security-policy: frame-ancestors 'none'`, Superset `x-frame-options`.
The browser enforces that client-side, so the naive result is a blank frame with
the reason hidden in the console.

`/api/apps/:id` therefore reports what the app itself says (`embed.state`:
`allowed` / `blocked` / `unknown`, with the header that blocks it), read from the
app's own response. Those pages show an “embeds ok” badge when framing is
permitted, and the offending header plus what to change when it is not — while
still offering the deep link out. `unknown` (demo mode, or a service Nova cannot
reach) makes no claim either way.

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
| `NOVA_STATIC_DIR` | `/app/static` | Directory containing the compiled frontend assets |
| `NOVA_TRINO_URL` | `http://trino.stardelt.svc.cluster.local:8080` | Trino coordinator base URL |
| `NOVA_LAKEKEEPER_URL` | `http://lakekeeper.stardelt.svc.cluster.local:8181` | Lakekeeper REST catalog base URL |
| `NOVA_WAREHOUSE_NAME` | `warehouse` | Lakekeeper warehouse name to resolve on startup |
| `NOVA_SUPERSET_URL` | `http://superset:8088` | Superset base URL (probe + embed check) |
| `NOVA_JUPYTERHUB_URL` | `http://proxy-public:80` | JupyterHub public proxy base URL (probe + embed check) |
| `POD_NAMESPACE` | `stardelt` | Namespace whose workloads Nova reports (downward API in the manifests); `NOVA_NAMESPACE` overrides it for a local run |
| `NOVA_BASE_DOMAIN` | — (empty) | Base domain for the app deep-links (`trino.<domain>`, …). Empty = the UI links nothing; the install sets it from a ConfigMap |
| `NOVA_FORWARD_IDENTITY` | off | Send the caller's own token to Trino and the catalog (`Authorization: Bearer`). Only useful once the platform configures those to validate it — the identity contract's phase 2; until then the catalog sees Nova's service connection and `/api/me` says so |
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

**Keycloak notes.** Add a *Group Membership* mapper (`groups`) to the `nova` client;
for bearer access add an *Audience* mapper so access tokens carry `aud: nova`.
Keycloak shows a logout confirmation page because Nova does not put the ID token in
the end-session URL (`client_id` + `post_logout_redirect_uri` are sent instead).

**Limitations.** Sessions are held in memory: run one Nova replica in `oidc` mode; a
restart signs everyone out. Sessions last 8 hours regardless of token lifetime and
are not refreshed or revoked when the IdP revokes the user.

---

## Links

- [stardelt.io](https://stardelt.io) — full documentation
- [Architecture overview](https://stardelt.io/architecture) — how Nova fits into the stardelt platform
- [stardelt/stardelt](https://github.com/stardelt/stardelt) — mono-repo with Helm charts and platform infrastructure
