# Postie (Rust). Run `make help` for the targets.
CARGO ?= cargo
RUST_DIR := rust

.PHONY: help
help:
	@echo "build           frontend + postie CLI + postie-web (release)"
	@echo "build-frontend  bun build of the web UI into frontend/build"
	@echo "build-desktop   Tauri desktop bundle (needs tauri-cli)"
	@echo "dev             run postie-web against the built frontend"
	@echo "desktop-dev     run the Tauri desktop app in dev mode"
	@echo "test            cargo + bun tests"
	@echo "lint            cargo fmt --check, clippy -D warnings, svelte-check"
	@echo "fmt             cargo fmt"
	@echo "docker-build    build the postie:dev image from docker/Dockerfile"
	@echo "clean           remove build output"

.PHONY: build-frontend
build-frontend:
	cd frontend && bun install --frozen-lockfile && bun run build

.PHONY: build
build: build-frontend
	cd $(RUST_DIR) && $(CARGO) build --release -p postie-cli -p postie-web

.PHONY: build-desktop
build-desktop: build-frontend
	cd $(RUST_DIR)/desktop && $(CARGO) tauri build

.PHONY: dev
dev: build-frontend
	cd $(RUST_DIR) && $(CARGO) run -p postie-web -- $(ARGS)

.PHONY: desktop-dev
desktop-dev: build-frontend
	cd $(RUST_DIR)/desktop && $(CARGO) tauri dev

.PHONY: test
test: build-frontend
	cd $(RUST_DIR) && $(CARGO) test --workspace
	cd frontend && bun test

.PHONY: lint
lint:
	cd $(RUST_DIR) && $(CARGO) fmt --all -- --check
	cd $(RUST_DIR) && $(CARGO) clippy --workspace --all-targets -- -D warnings
	cd frontend && bun run check

.PHONY: fmt
fmt:
	cd $(RUST_DIR) && $(CARGO) fmt --all

.PHONY: docker-build
docker-build:
	docker build -f docker/Dockerfile -t postie:dev .

.PHONY: clean
clean:
	cd $(RUST_DIR) && $(CARGO) clean
	rm -rf frontend/build frontend/.svelte-kit
