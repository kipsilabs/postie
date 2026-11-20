# syntax=docker/dockerfile:1.11
# Postie Web (Rust): frontend built with bun, postie-web built with cargo, served from a
# LinuxServer.io image (s6-overlay + PUID/PGID support). Self-contained: `docker build -f docker/Dockerfile .`

# ---- frontend ----
FROM oven/bun:1 AS frontend-builder
WORKDIR /app/frontend
COPY frontend/bun.lock frontend/package.json ./
RUN --mount=type=cache,target=/root/.bun/install/cache,sharing=locked \
    bun install --frozen-lockfile
COPY frontend/ ./
RUN bun run build

# ---- backend ----
# bookworm (glibc 2.36) builds a binary that runs on the noble runtime below (glibc 2.39).
FROM rust:1-bookworm AS backend-builder
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    apt-get update -qq && apt-get install -y --no-install-recommends cmake
WORKDIR /app/rust
COPY rust/ ./
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/app/rust/target,sharing=locked \
    cargo build --release --locked -p postie-web \
    && cp target/release/postie-web /app/postie-web

# ---- runtime ----
FROM ghcr.io/linuxserver/baseimage-ubuntu:noble

ARG BUILD_DATE
ARG VERSION
ARG PUID=1000
ARG PGID=1000
ENV PUID=${PUID}
ENV PGID=${PGID}

WORKDIR /app

# postie-web serves the UI from the `frontend/` directory next to its binary
COPY --from=backend-builder /app/postie-web /app/postie-web
COPY --from=frontend-builder /app/frontend/build /app/frontend
RUN chmod +x /app/postie-web

# s6-overlay service configuration
COPY docker/root/ /

# ownership is handled at runtime by init-adduser
RUN mkdir -p /config /watch /output

ENV PORT=8080
ENV HOST=0.0.0.0
EXPOSE 8080
VOLUME ["/config", "/watch", "/output"]

HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8080/live || exit 1

LABEL org.opencontainers.image.source="https://github.com/kipsilabs/postie"
LABEL build_version="version: Build-date:- ${VERSION} ${BUILD_DATE}"
LABEL maintainer="javi11"

# Base image provides ENTRYPOINT ["/init"]
