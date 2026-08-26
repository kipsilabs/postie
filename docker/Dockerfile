# Multi-stage build for Postie Web - Multi-platform optimized
# This is the self-contained development/local build Dockerfile
# For CI builds, use Dockerfile.ci which expects pre-built frontend assets
# Use buildkit syntax for advanced features
# syntax=docker/dockerfile:1.11

# Frontend build stage
FROM oven/bun:1 AS frontend-builder

# Set working directory for frontend
WORKDIR /app/frontend

# Copy only dependency manifests first to leverage caching for bun install
COPY frontend/bun.lock frontend/package.json ./

# Install dependencies with enhanced cache mounting
RUN --mount=type=cache,target=/root/.bun/install/cache,sharing=locked \
    --mount=type=cache,target=/tmp/bun-cache,sharing=locked \
    bun install --frozen-lockfile

# Copy the rest of the frontend sources and build with cache
COPY frontend/ ./
RUN --mount=type=cache,target=/tmp/bun-build-cache,sharing=locked \
    bun run build

# Backend build stage with native arch support (no cross-compilation)
FROM golang:1.24-alpine AS backend-builder

# Install build dependencies including sqlite with cache mount
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --no-cache git gcc g++ musl-dev sqlite-dev

# Set working directory
WORKDIR /app

# Copy go module files first for better caching
COPY go.mod go.sum ./

# Download dependencies with enhanced cache mount
RUN --mount=type=cache,target=/go/pkg/mod,sharing=locked \
    --mount=type=cache,target=/root/.cache/go-mod-download,sharing=locked \
    go mod download

# Copy source code in optimal order for caching
COPY ./pkg ./pkg
COPY ./internal ./internal
COPY ./cmd ./cmd
COPY ./frontend/embed.go ./frontend/embed.go

# Copy built frontend from previous stage
COPY --from=frontend-builder /app/frontend/build ./frontend/build

# Build web binary with enhanced cache mount for native architecture
# netgo/osusergo are required: this is a fully static cgo binary, and without
# them DNS goes through glibc getaddrinfo -> NSS, which dlopen()s the host's
# libnss_*.so into a process with a different statically-linked glibc and
# crashes with SIGFPE (issue #243). netgo compiles that path out entirely.
RUN --mount=type=cache,target=/root/.cache/go-build,sharing=locked \
    --mount=type=cache,target=/tmp/go-build-cache,sharing=locked \
    CGO_ENABLED=1 GOOS=linux \
    go build -a -tags netgo,osusergo -ldflags '-linkmode external -extldflags "-static"' -o postie-web cmd/web/main.go

# Final stage - use LinuxServer.io base image (has s6-overlay + PUID/PGID support)
FROM ghcr.io/linuxserver/baseimage-ubuntu:jammy

ARG BUILD_DATE
ARG VERSION
ARG PUID=1000
ARG PGID=1000

# Set up environment variables for PUID and PGID
ENV PUID=${PUID}
ENV PGID=${PGID}

# Set the working directory inside the container
WORKDIR /app

# Copy web binary from builder
COPY --from=backend-builder /app/postie-web /app/postie-web
COPY --from=frontend-builder /app/frontend/build ./frontend/build

# Make binary executable
RUN chmod +x /app/postie-web

# Copy s6-overlay service configuration
COPY docker/root/ /

# Create necessary directories (ownership handled at runtime by init-adduser)
RUN mkdir -p /config /watch /output

# Set environment variables
ENV PORT=8080
ENV HOST=0.0.0.0

# Expose port for web interface
EXPOSE 8080

# Volume
VOLUME ["/config", "/watch", "/output"]

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8080/live || exit 1

LABEL org.opencontainers.image.source="https://github.com/javi11/postie"
LABEL build_version="version: Build-date:- ${VERSION} ${BUILD_DATE}"
LABEL maintainer="javi11"

# Base image provides ENTRYPOINT ["/init"]
