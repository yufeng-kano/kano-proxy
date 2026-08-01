# `.local/` — private operator / agent data

This directory is the **committed template**. Real data lives in gitignored **`.local/`** at the repo root.

```bash
cp -R .local.example .local
# then edit files under .local/ with real hostnames, DNS, account notes, etc.
```

## Purpose

Store **non-open-source** operator facts that agents need for deploy and ops, without putting them in `docs/` or git:

- Production hostname(s) and zone
- DNS records (type, name, target, proxy status)
- Worker route bind order and custom-domain notes
- Cloudflare account labels (not API tokens)
- Personal deploy checklists and “what I already did”

## Do not put here

| Put elsewhere | Why |
|---------------|-----|
| API tokens, OAuth client secrets, `SESSION_SECRET`, `TOKEN_ENCRYPTION_KEY` | Use `apps/api/.dev.vars` or `wrangler secret put` |
| Product behavior / public deploy steps | Stay in `docs/` with placeholders only |

Secrets that can mint access must not live only as loose notes in `.local/` if a better secret store exists. `.local/` is for **private configuration context** (DNS, hostnames, ops notes), not a password dump.

## Suggested layout

```text
.local/
  README.md          # optional personal index
  dns.md             # real DNS + routes (see template)
  deploy-notes.md    # optional: bootstrap progress, account id labels
  secrets.md         # generated SESSION_SECRET / TOKEN_ENCRYPTION_KEY (optional notes)
  production.env     # machine-friendly KEY=value for wrangler secret put (chmod 600)
```

Add more markdown files as needed; keep names descriptive. Agents are told (in `.rule`) to read `.local/` when present and never commit it.

`secrets.md` / `production.env` hold **generated** operator secrets for production bootstrap. Never commit them; prefer `chmod 600`. OAuth client id/secret still come from Google, not a local RNG.
