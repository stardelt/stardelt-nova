.PHONY: help dev fmt check build image clean

# Default target: list available targets
help:
	@echo "stardelt-nova — available targets:"
	@echo ""
	@echo "  make dev     Run backend (port 8080) and frontend dev server (port 5173)"
	@echo "  make fmt     cargo fmt + frontend lint"
	@echo "  make check   cargo check + TypeScript type-check"
	@echo "  make build   cargo build --release + vite build"
	@echo "  make image   Build container image ghcr.io/stardelt/nova:dev"
	@echo "  make clean   Remove build artefacts"
	@echo ""

# Run both processes.  Uses a simple background-job approach so a single
# Ctrl-C tears down both.  Open http://localhost:5173 in your browser.
dev:
	@echo "Starting backend on :8080 and frontend dev server on :5173 …"
	@echo "(Ctrl-C stops both)"
	@trap 'kill 0' INT; \
	  (cd backend && cargo run) & \
	  (cd frontend && npm run dev) & \
	  wait

fmt:
	cargo fmt --all
	cd frontend && npm run lint || true

check:
	cargo check --all-targets
	cd frontend && npx tsc -b --noEmit

build:
	cargo build --release
	cd frontend && npm run build

image:
	docker build -t ghcr.io/stardelt/nova:dev -f image/Dockerfile .

clean:
	cargo clean
	rm -rf frontend/dist frontend/node_modules
