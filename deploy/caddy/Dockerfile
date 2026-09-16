# TLS and routing. The configuration is baked in, like every other service here.
# Build context is the repository root:
#   docker build -f deploy/caddy/Dockerfile -t kano-proxy-caddy:<version> .
FROM caddy:2-alpine
COPY deploy/caddy/Caddyfile /etc/caddy/Caddyfile
EXPOSE 80 443
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s CMD wget -qO- http://127.0.0.1:2019/config/ >/dev/null || exit 1
