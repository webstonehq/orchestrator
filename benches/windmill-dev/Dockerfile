# Extends the published Orchestrator image with python3 (for the faithful
# Python fibo plugin) and the benchmark plugin bundle. The base runtime image is
# debian:bookworm-slim with no interpreter, so the Python oneshot plugin needs
# python3 added. Mirrors the Windmill benchmark's "spin up via docker-compose".
FROM ghcr.io/webstonehq/orchestrator:latest

USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*

# Drop the fibo bundle beside the binary's plugins dir; it is auto-discovered at
# startup (no rebuild of Orchestrator itself).
COPY plugins/fibo /app/plugins/fibo
RUN chmod +x /app/plugins/fibo/fibo

USER app
