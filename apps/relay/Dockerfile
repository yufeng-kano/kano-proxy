# Codex egress relay — deployed with `gcloud run deploy --source .` (see
# docs/deployment.md "Codex egress relay (Cloud Run)"). Pinned to the Deno
# version this app was developed/tested against (`deno --version`); bump
# deliberately, not opportunistically.
FROM denoland/deno:alpine-2.9.3

WORKDIR /app

COPY main.ts relay.ts tokenizer.ts deno.json ./

# Pre-compile and pre-fetch so the first request after a cold start isn't
# also paying for a JIT/type-check pass or an npm download. tokenizer.ts is
# cached explicitly: relay.ts only loads it lazily (dynamic import on the
# first /count-tokens), and the ranks it pulls must be baked into the image,
# not fetched at runtime.
RUN deno cache main.ts tokenizer.ts

USER deno

ENV PORT=8080

CMD ["run", "--allow-net", "--allow-env", "main.ts"]
