# AGENTS.md — stardelt-nova

Nova is the unified control-plane UI + API gateway of stardelt — the Experience layer
of the product (master spec §6). A Rust (axum) backend serves the React SPA and proxies
the Lakekeeper Iceberg REST catalog and the Trino query engine.

## Read first

- `../stardelt-docs/docs/design/master-spec.md` — canonical spec; when documents
  disagree, it wins. Also `execution-plan.md` and `gap-decisions.md` in the same folder.
- `../stardelt-docs/docs/architecture/licenses.md` — license policy.
- Published copies: https://stardelt.io

The running development environment is the Hetzner k3s cluster
(`alpha.dev.stardelt.io`), provisioned from `stardelt-platform/dev/hetzner-k3s`; Nova
is deployed there by the platform install (`manifests/nova-deployment.yaml`). UI work
needs no cluster: `make demo` serves in-memory data.

## Layout

- `backend/` — Rust crate: axum server, Lakekeeper/Trino proxies, kube client for the
  platform app launcher (`apps.rs`), demo mode (`demo.rs`), OIDC sign-in (`auth.rs`).
- `frontend/` — Vite + React + TypeScript + Tailwind SPA.
- `image/Dockerfile` — multi-stage node → rust → debian-slim image.
- `.github/workflows/ci.yml` — CI: `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo check --all-targets`; `npx tsc -b --noEmit`; `npm run build`.

## Build / check / run

Requires Rust (rustup) and Node + npm. In this machine's shell, `~/.cargo/bin` may not
be on `PATH`.

```bash
make demo        # backend :8080 (NOVA_DEMO=1) + Vite :5173, in-memory, no cluster
make dev         # same pair, but talks to real Trino/Lakekeeper (see NOVA_* env vars)
cargo check --all-targets        # backend check
npm --prefix frontend ci         # install frontend dependencies
npm --prefix frontend run build  # tsc --noEmit && vite build
make check       # cargo check --all-targets + npx tsc --noEmit
make image       # docker build -t ghcr.io/stardelt/nova:dev
```

## Conventions

- **PR-only.** Never push to `main`; branch off `origin/main` and keep PRs small and
  scoped. Update docs when user-visible behavior changes.
- **License policy: OSI-permissive only** (Apache-2.0, MIT, BSD-2/3-Clause). The single
  documented exception is OpenBao (MPL-2.0). See
  `../stardelt-docs/docs/architecture/licenses.md`; do not add dependencies outside it.
- Read the master spec before implementing. Nova is a viewport — platform state lives in
  CRDs; nothing in Nova is state that is not a resource.

## Gotchas

- Auth: `NOVA_AUTH_MODE=dev` (default) must keep working unchanged — `/api/me` returns
  the exact pre-OIDC shape. `oidc` mode lives in `backend/src/auth.rs` (env vars in
  README "Sign-in"); protected routes go in the `protected` router in `main.rs`, and
  only `/api/health` and `/api/auth/*` stay public. Sessions are in-memory (single
  replica). Never log, return or put in a URL any token; `Secret` redacts `Debug`.
  `backend/src/testdata/` holds a test-only RSA key — never use it elsewhere.

- The frontend is type-checked with `tsc --noEmit`; `frontend/tsconfig.json` sets
  `noEmit: true` and has no `outDir`. Do not turn emit back on — `tsc` would write `.js`
  files next to the `.ts` sources. Keep `frontend/src` free of generated `.js`.
- `make fmt` formats Rust only; its frontend "lint" step is a no-op (`npm run lint` does
  not exist and the Makefile swallows the failure with `|| true`).
