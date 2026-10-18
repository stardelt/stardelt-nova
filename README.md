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
         ├─ GET  /api/health           liveness
         ├─ GET  /api/me               current user + demo flag
         ├─ GET  /api/trino/cluster    → Trino /v1/info
         ├─ GET  /api/catalog/*        → Lakekeeper REST catalog
         ├─ POST /api/catalog/namespaces  create schema
         ├─ POST /api/query            → Trino query engine
         ├─ GET  /api/jobs             recent activity
         ├─ GET  /api/apps             installed apps: live status + images
         ├─ GET  /api/apps/:id         one app: workloads, facts, deep links
         ├─ GET  /api/workloads        workloads in Nova's namespace + images
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
| `NOVA_DEV_USER` | `stardelt-dev` | Static user identity returned by `/api/me` (dev mode only) |
| `RUST_LOG` | `info` | Tracing filter for the backend (`debug`, `info`, `warn`, …) |

---

## Links

- [stardelt.io](https://stardelt.io) — full documentation
- [Architecture overview](https://stardelt.io/architecture) — how Nova fits into the stardelt platform
- [stardelt/stardelt](https://github.com/stardelt/stardelt) — mono-repo with Helm charts and platform infrastructure
