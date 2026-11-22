# Deploy notes (private)

Optional scratchpad for deploy progress. Copy to `.local/deploy-notes.md`.

## Server

| Item | Value |
|------|-------|
| Host label | (name only) |
| Checkout path | `<path to the repository checkout>` |
| Data directory | `./data/` in that checkout (database and certificates; back it up) |

## Checklist

- [ ] `.env` filled from `.env.example` on the server (secrets generated once and kept)
- [ ] DNS points at the server (see `dns.md`)
- [ ] Google OAuth production redirect registered
- [ ] `docker compose up -d --build`
- [ ] Smoke without model traffic: `/health`, sign-in, issue a key

## GitHub Actions

Set on the GitHub repo (not in git):

| Kind | Name | Used by |
|------|------|---------|
| secret | `TAP_PUSH_TOKEN` (optional) | `cli-release.yml`, to bump the Homebrew tap and Scoop bucket |

A `vX.Y.Z` Release deploys nothing; operators update with `git pull && docker compose up -d --build` (docs/deployment.md § Updating).

## Notes

…
