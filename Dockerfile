# syntax=docker/dockerfile:1
#
# Multi-stage build for the Orchestrator single binary.
#
#   1. ui       — build the UI into one self-contained ui/build/index.html
#   2. build    — compile the server + plugin bundles, embedding that UI file
#   3. runtime  — a slim image with the binary and its plugins laid out for
#                 auto-discovery (plugins/ beside the binary)
#
# Build:  docker build -t orchestrator .
# Run:    docker run --rm -p 4400:4400 -v orchestrator-data:/data orchestrator
#         then open http://127.0.0.1:4400
#
# Data (SQLite db + master.key) lives under /data (HOME) — mount a volume there
# to persist flows, runs, and secrets across container restarts.

# ---------------------------------------------------------------------------
# Stage 1 — UI (Vite/SvelteKit → one inlined index.html)
# ---------------------------------------------------------------------------
FROM node:22-slim AS ui
WORKDIR /ui
# Install deps first so the layer caches across UI source edits.
COPY ui/package.json ui/package-lock.json ./
RUN npm ci
COPY ui/ ./
RUN npm run build

# ---------------------------------------------------------------------------
# Stage 2 — build the Rust workspace (server + shipped plugin bundles)
# ---------------------------------------------------------------------------
FROM rust:1-bookworm AS build
WORKDIR /src

# Whole workspace, minus what .dockerignore excludes (target, ui/build, .git…).
COPY . .
# The release build embeds ui/build/index.html via include_str!, so it must
# exist at compile time — drop in the artifact from the UI stage.
COPY --from=ui /ui/build ./ui/build

# Build the server and every plugin binary in the workspace. --workspace is
# required: the root is itself a package, so without it cargo would build only
# the `orchestrator` binary and skip the plugin crates under plugins/.
RUN cargo build --release --workspace --bins

# Lay out the runtime tree: the binary plus a plugins/ dir beside it, one
# bundle per plugins/<name>/ that carries a plugin.json. Mirrors the mise
# `stage-plugins` task. plugins/sdk has no plugin.json and is skipped.
RUN set -eux; \
    mkdir -p /out/plugins; \
    cp target/release/orchestrator /out/orchestrator; \
    for pj in plugins/*/plugin.json; do \
      [ -e "$pj" ] || continue; \
      name=$(basename "$(dirname "$pj")"); \
      entrypoint=$(sed -n 's/.*"entrypoint"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$pj"); \
      mkdir -p "/out/plugins/$name"; \
      cp "$pj" "/out/plugins/$name/plugin.json"; \
      if [ -f "target/release/$entrypoint" ]; then \
        cp "target/release/$entrypoint" "/out/plugins/$name/$entrypoint"; \
      elif [ -f "plugins/$name/$entrypoint" ]; then \
        cp "plugins/$name/$entrypoint" "/out/plugins/$name/$entrypoint"; \
      else \
        echo "stage-plugins: entrypoint '$entrypoint' for plugin '$name' not found" >&2; \
        exit 1; \
      fi; \
      echo "staged $name"; \
    done

# ---------------------------------------------------------------------------
# Stage 3 — runtime
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
LABEL org.opencontainers.image.source="https://github.com/webstonehq/orchestrator"
LABEL org.opencontainers.image.description="Single-binary workflow orchestration tool: web UI, JSON API, cron scheduler, task executor"
LABEL org.opencontainers.image.licenses="MIT"

# ca-certificates for outbound TLS (the http.request plugin, secrets to APIs…).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Non-root user; its home is /data, so ~/.orchestrator resolves to
# /data/.orchestrator (the db + master.key live there, created 0700 at runtime).
RUN useradd --system --create-home --home-dir /data --uid 10001 app

WORKDIR /app
COPY --from=build /out/orchestrator /app/orchestrator
COPY --from=build /out/plugins /app/plugins

ENV HOME=/data
# The server binds 0.0.0.0:$PORT (see config::default_listen), so it is
# reachable from outside the container and honors the PORT a platform router
# injects (Railway/Render/Fly). Defaulted here so a plain `docker run` still
# lands on 4400; override with -e PORT=… or `serve --listen …`.
ENV PORT=4400
USER app
VOLUME ["/data"]
EXPOSE 4400

# NOTE: Orchestrator has no built-in auth — only expose it behind an
# authenticating reverse proxy or on a trusted network (see README security
# notes). The binary is the entrypoint, so `docker run … <image> worker …`
# overrides the default `serve`.
ENTRYPOINT ["/app/orchestrator"]
CMD ["serve"]
