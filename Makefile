GO ?= go

.DEFAULT_GOAL := check

.PHONY: generate
generate:
	go generate ./...

.PHONY: govulncheck
govulncheck:
	go tool govulncheck ./...

.PHONY: tidy go-mod-tidy
tidy: go-mod-tidy
go-mod-tidy:
	$(GO) mod tidy

.PHONY: golangci-lint golangci-lint-fix
golangci-lint-fix: ARGS=--fix
golangci-lint-fix: golangci-lint
golangci-lint:
	go tool golangci-lint run $(ARGS)

.PHONY: junit
junit: | $(JUNIT)
	mkdir -p ./test-results && $(GO) test -v 2>&1 ./... | go tool go-junit-report -set-exit-code > ./test-results/report.xml

.PHONY: coverage
coverage: build-frontend
	$(GO) test -v -coverprofile=coverage.out ./...

.PHONY: coverage-html
coverage-html: coverage
	$(GO) tool cover -html=coverage.out -o coverage.html

.PHONY: coverage-func
coverage-func: coverage
	$(GO) tool cover -func=coverage.out

.PHONY: coverage-ci
coverage-ci: build-frontend
	$(GO) test -v -race -coverprofile=coverage.out -covermode=atomic ./...

.PHONY: coverage-total
coverage-total: coverage
	@$(GO) tool cover -func=coverage.out | grep total | awk '{print $$3}' | sed 's/%//'

.PHONY: lint
lint: go-mod-tidy golangci-lint

.PHONY: test test-race
test-race: ARGS=-race
test-race: test
test: build-frontend
	$(GO) test $(ARGS) ./...

.PHONY: check
check: generate go-mod-tidy golangci-lint test-race

.PHONY: git-hooks
git-hooks:
	@echo '#!/bin/sh\nmake' > .git/hooks/pre-commit
	@chmod +x .git/hooks/pre-commit

.PHONY: release
release:
	goreleaser --skip-validate --skip-publish --rm-dist

.PHONY: snapshot
snapshot:
	goreleaser --skip-docker --snapshot --skip-publish --rm-dist 

.PHONY: publish
publish:
	goreleaser --rm-dist

.PHONY: docker docker-build
docker: snapshot
	mkdir -p ./example/watch
	mkdir -p ./example/config
	mkdir -p ./example/output
	docker-compose up

# Build Docker images using GoReleaser with optimizations
docker-build: build-frontend
	REGISTRY=ghcr.io IMAGE_NAME=javi11/postie goreleaser release --config .goreleaser-docker.yml --snapshot --skip-publish --clean

# Build targets
.PHONY: dev
dev:
	@echo "Starting development mode (GUI with hot reload)..."
	go tool wails dev

.PHONY: build
build: build-cli build-web
	@echo "Build completed!"
	@echo "CLI binary: ./postie-cli"
	@echo "Web server binary: ./postie-web"
	@echo "GUI binary: ./build/bin/postie(.app on macOS)"

.PHONY: build-debug
build-debug: build-cli-debug build-web-debug build-gui-debug
	@echo "Debug build completed!"
	@echo "CLI binary: ./postie-cli-debug"
	@echo "Web server binary: ./postie-web-debug"
	@echo "GUI binary: ./build/bin/postie(.app on macOS)"

.PHONY: build-cli
build-cli:
	@echo "Building CLI..."
	$(GO) build -o postie-cli ./cmd/postie

.PHONY: build-cli-debug
build-cli-debug:
	@echo "Building CLI (debug)..."
	$(GO) build -tags debug -o postie-cli-debug ./cmd/postie

.PHONY: build-web
build-web:
	@echo "Building Web server..."
	$(GO) build -o postie-web ./cmd/web

.PHONY: build-web-debug
build-web-debug:
	@echo "Building Web server (debug)..."
	$(GO) build -tags debug -o postie-web-debug ./cmd/web

.PHONY: build-gui
build-gui:
	@echo "Building GUI..."
	go tool wails build

.PHONY: build-frontend
build-frontend:
	@echo "Building Frontend..."
	cd frontend && bun i && bun run build

.PHONY: build-gui-debug
build-gui-debug:
	@echo "Building GUI (debug)..."
	go tool wails build -debug

.PHONY: run
run: build-gui
	@echo "Running GUI..."
ifeq ($(shell uname),Darwin)
	./build/bin/postie.app/Contents/MacOS/postie
else
	./build/bin/postie
endif

.PHONY: run-cli
run-cli: build-cli
	@echo "Running CLI..."
	./postie-cli $(ARGS)

.PHONY: run-web
run-web: build-web
	@echo "Running web server..."
	./postie-web $(ARGS)

.PHONY: clean-build
clean-build:
	@echo "Cleaning build artifacts..."
	rm -rf build/
	rm -f postie-cli postie-cli-debug postie-web postie-web-debug
	rm -rf frontend/dist/
	rm -rf frontend/node_modules/
	@echo "Clean completed!"

.PHONY: help
help:
	@echo "Postie Build Commands"
	@echo ""
	@echo "Development:"
	@echo "  dev              - Start development mode with hot reload (GUI only)"
	@echo ""
	@echo "Building:"
	@echo "  build            - Build production version (CLI + Client + GUI)"
	@echo "  build-debug      - Build debug version (CLI + Client + GUI)"
	@echo "  build-cli        - Build CLI only (cmd/postie)"
	@echo "  build-web        - Build web server only (cmd/web)"
	@echo "  build-gui        - Build GUI only"
	@echo ""
	@echo "Running:"
	@echo "  run              - Build and run the GUI"
	@echo "  run-cli ARGS=... - Build and run CLI"
	@echo "  run-web ARGS=...    - Build and run the web server"
	@echo ""
	@echo "Maintenance:"
	@echo "  clean-build      - Clean build artifacts"
	@echo "  clean            - Clean all artifacts"
	@echo ""
	@echo "Examples:"
	@echo "  make dev                    # Start GUI development mode"
	@echo "  make build                  # Build all components"
	@echo "  make run-cli ARGS='--help'  # Show CLI help"
	@echo "  make run-web ARGS='...'     # Run web server with args"

.PHONY: clean
clean: clean-build
	@echo "Cleaning all artifacts..."
	rm -rf test-results/
	rm -f coverage.out coverage.html