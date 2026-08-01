# Deploy notes (private)

Optional scratchpad for bootstrap progress. Copy to `.local/deploy-notes.md`.

## Cloudflare

| Item | Value / status |
|------|----------------|
| Account label | (name only — not tokens) |
| Account ID | optional local note |
| D1 `kano-proxy` database_id | |
| KV `kano-proxy-bench` id | |
| KV `kano-proxy-cache` id | |
| Worker deployed | no / yes (date) |
| Pages project `kano-proxy` | no / yes (date) |
| Remote migrations applied | no / yes |

## Checklist

- [ ] D1 + KV created; ids in `apps/api/wrangler.toml` (or noted here if not committing prod ids)
- [ ] Production vars set (`APP_URL`, `GOOGLE_REDIRECT_URI`)
- [ ] Secrets via `wrangler secret put`
- [ ] `db:migrate:remote` + Worker deploy + Pages deploy
- [ ] DNS + Worker routes (see `dns.md`)
- [ ] Google OAuth production redirect
- [ ] Smoke: login, issue key, sample LLM call

## GitHub Actions (release deploy)

Set on the GitHub repo (not in git):

| Kind | Name |
|------|------|
| secret | `CLOUDFLARE_API_TOKEN` |
| secret | `CLOUDFLARE_ACCOUNT_ID` |
| secret | `CF_D1_DATABASE_ID` |
| secret | `CF_KV_BENCH_ID` |
| secret | `CF_KV_CACHE_ID` |
| variable | `APP_URL` |

Default version bump: minor → `x.(y+1).0`. Publish a Release with tag `v…` to deploy.

## Notes

…
