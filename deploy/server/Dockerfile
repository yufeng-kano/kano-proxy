# The API server: the core library plus the standalone binary.
# Build context is the repository root:
#   docker build -f deploy/server/Dockerfile -t kano-proxy-server:<version> .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY apps/server ./apps/server
WORKDIR /src/apps/server
RUN cargo build --release -p kano-proxy-server

# The runtime carries no static files: the web app is its own container.
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --create-home kano
COPY --from=build /src/apps/server/target/release/kano-proxy-server /usr/local/bin/kano-proxy-server
USER kano
ENV LISTEN_ADDR=0.0.0.0:8787
EXPOSE 8787
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s CMD curl -fsS http://127.0.0.1:8787/health || exit 1
ENTRYPOINT ["/usr/local/bin/kano-proxy-server"]
