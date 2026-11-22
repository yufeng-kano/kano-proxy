# `.local/` — private operator / agent data

This directory is the **committed template**. Real data lives in gitignored **`.local/`** at the repo root.

```bash
cp -R .local.example .local
# then edit files under .local/ with real hostnames, DNS and deploy notes
```

## Purpose

Store **non-open-source** operator facts that agents need for deploy and ops, without putting them in `docs/` or git:

- Production hostname(s) and DNS zone
- DNS records (type, name, target)
- Ingress notes: the bundled Caddy, or the reverse proxy in front of the server
- Personal deploy checklists and "what I already did"

## Do not put here

| Put elsewhere | Why |
|---------------|-----|
| API tokens, OAuth client secrets, `SESSION_SECRET`, `TOKEN_ENCRYPTION_KEY`, `CLI_TOKEN_SECRET`, database passwords | They belong in the server's gitignored `.env` (or a CI secret store), never in notes |
| Product behavior / public deploy steps | Stay in `docs/` with placeholders only |

## Suggested layout

```text
.local/
  README.md          # optional personal index
  dns.md             # real hostname, DNS records, ingress (see template)
  deploy-notes.md    # optional: server, checkout path, deploy log
```

Add more Markdown files as needed; keep names descriptive. Agents are told (in `.rule`) to read `.local/` when present and never commit it.
