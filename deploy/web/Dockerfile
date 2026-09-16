# The web app: the Vue SPA with the documentation site mounted inside it.
# Build context is the repository root:
#   docker build -f deploy/web/Dockerfile -t kano-proxy-web:<version> .
FROM node:22-bookworm-slim AS build
RUN corepack enable
WORKDIR /src
COPY apps/web ./apps/web
COPY apps/docs ./apps/docs
RUN cd apps/web  && pnpm install --frozen-lockfile && pnpm build
RUN cd apps/docs && pnpm install --frozen-lockfile && pnpm build
# The documentation site is served at /docs/ from the same origin (docs/docs-site.md).
RUN cp -r apps/docs/.vitepress/dist apps/web/dist/docs

FROM nginx:alpine
COPY --from=build /src/apps/web/dist /usr/share/nginx/html
COPY deploy/web/nginx.conf /etc/nginx/conf.d/default.conf
EXPOSE 80
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s CMD wget -qO- http://127.0.0.1/index.html >/dev/null || exit 1
