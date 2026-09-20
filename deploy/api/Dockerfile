# The API server: the core library plus the standalone binary.
# Build context is the repository root:
#   docker build -f deploy/api/Dockerfile -t kano-proxy-api:<version> .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY apps/api ./apps/api
WORKDIR /src/apps/api
# The registry and target live in BuildKit cache mounts, so a source change does not recompile
# every dependency. A cache mount is not part of an image layer: copy the binary out in the
# same RUN. The target id belongs to this workspace; if the cache is ever suspected:
#   docker builder prune --filter type=exec.cachemount
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=kano-proxy-api-target,target=/src/apps/api/target \
    cargo build --release -p kano-proxy-api \
 && cp target/release/kano-proxy-api /usr/local/bin/kano-proxy-api

# The runtime carries no static files: the web app is its own container.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --create-home kano
COPY --from=build /usr/local/bin/kano-proxy-api /usr/local/bin/kano-proxy-api
USER kano
ENV LISTEN_ADDR=0.0.0.0:8787
EXPOSE 8787
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s CMD curl -fsS http://127.0.0.1:8787/health || exit 1
ENTRYPOINT ["/usr/local/bin/kano-proxy-api"]
