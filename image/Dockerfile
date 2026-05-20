# syntax=docker/dockerfile:1.7

# ------- Stage 1: UI build -------
FROM node:22-alpine AS ui-build
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
    rm -rf backend/src target/release/deps/stardelt_nova_backend* target/release/stardelt-nova-backend
COPY backend/src backend/src
RUN cargo build --release -p stardelt-nova-backend

# ------- Stage 3: minimal runtime -------
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=backend-build /src/target/release/stardelt-nova-backend /usr/local/bin/nova
COPY --from=ui-build      /src/dist                                /app/static
ENV NOVA_BIND_ADDR=0.0.0.0:8080 \
    NOVA_STATIC_DIR=/app/static \
    RUST_LOG=info
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/nova"]
