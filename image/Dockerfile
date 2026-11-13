# syntax=docker/dockerfile:1.7

# ------- Stage 1: UI build -------
FROM node:26-alpine AS ui-build
WORKDIR /src
COPY frontend/package.json frontend/package-lock.json* frontend/.npmrc* ./
RUN npm install
COPY frontend/ .
RUN npm run build

# ------- Stage 2: backend build -------
FROM rust:1-slim AS backend-build
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock* ./
COPY backend/Cargo.toml backend/
RUN mkdir -p backend/src && \
    echo "fn main() {}" > backend/src/main.rs && \
    cargo build --release -p stardelt-nova-backend && \
    # The build state of the placeholder compile must go too: COPY gives the real sources the
    # checkout mtime, which is older than the placeholder build, so Cargo would call the crate
    # fresh and stage 3 would copy a 437 KB `fn main() {}` binary (stardelt-nova#909).
    # cargo 1.100 keeps that state in `target/release/build/<pkg>/<hash>/`; older cargo in
    # `target/release/.fingerprint/<pkg>-*`. Both are removed so either layout is safe.
    rm -rf backend/src target/release/deps/stardelt_nova_backend* \
           target/release/stardelt-nova-backend target/release/.fingerprint/stardelt-nova-backend-* \
           target/release/build/stardelt-nova-backend
COPY backend/src backend/src
# `sqlx::migrate!` embeds backend/migrations/*.sql at compile time: without this line the
# build fails with "error canonicalizing migration directory".
COPY backend/migrations backend/migrations
RUN cargo build --release -p stardelt-nova-backend

# ------- Stage 3: minimal runtime -------
FROM debian:trixie-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=backend-build /src/target/release/stardelt-nova-backend /usr/local/bin/nova
COPY --from=ui-build      /src/dist                                /app/static
ENV NOVA_BIND_ADDR=0.0.0.0:8080 \
    NOVA_STATIC_DIR=/app/static \
    RUST_LOG=info
EXPOSE 8080
# Run as non-root (uid 65532, matching the platform's runAsUser/runAsGroup).
# The binary only reads /app/static and listens on unprivileged 8080; sessions
# are in-memory, so nothing needs to be writable. World-readable bits are
# enforced here rather than assumed from the build stages' umask.
RUN chmod 755 /usr/local/bin/nova && chmod -R a=rX /app/static
# The real backend is ~34 MB; the placeholder is ~437 KB. Fail here rather than ship it.
RUN test "$(stat -c%s /usr/local/bin/nova)" -gt 1000000 \
    || { echo "the runtime image holds a $(stat -c%s /usr/local/bin/nova)-byte binary: the placeholder was copied (stardelt-nova#909)" >&2; exit 1; }
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/nova"]
