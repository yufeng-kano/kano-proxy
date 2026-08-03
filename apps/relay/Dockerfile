# Codex egress relay — deployed with `gcloud run deploy --source .` (see
# docs/deployment.md "Codex egress relay (Cloud Run)"). Pinned to the Deno
# version this app was developed/tested against (`deno --version`); bump
# deliberately, not opportunistically.
FROM denoland/deno:alpine-2.9.3

WORKDIR /app

COPY main.ts relay.ts deno.json ./

# Pre-compile so the first request after a cold start isn't also paying for
# a JIT/type-check pass — this app has zero external deps, so there is
# nothing to fetch here, only local compilation.
RUN deno cache main.ts

USER deno

ENV PORT=8080

CMD ["run", "--allow-net", "--allow-env", "main.ts"]
