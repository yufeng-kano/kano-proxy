# DNS and hostname (private)

Copy to `.local/dns.md` and replace placeholders. **Do not commit** the filled file.

## Production host

| Item | Value |
|------|--------|
| Hostname | `<your-domain>` e.g. `proxy.example.com` |
| Zone (Cloudflare) | `<zone-name>` |
| Same-host UI+API | yes (recommended) |

Production vars (also set in Cloudflare, not only here):

```text
APP_URL=https://<your-domain>
GOOGLE_REDIRECT_URI=https://<your-domain>/api/auth/callback
```

## DNS records

| Type | Name | Target | Proxy | Notes |
|------|------|--------|-------|-------|
| CNAME or A/AAAA | `<subdomain or @>` | `<pages-or-worker-target>` | Proxied | |

## Worker routes (same host)

| Route | Service |
|-------|---------|
| `<your-domain>/openai/*` | Worker `kano-proxy` |
| `<your-domain>/anthropic/*` | Worker `kano-proxy` |
| `<your-domain>/api/*` | Worker `kano-proxy` |
| (optional) `<your-domain>/health` | Worker `kano-proxy` |

Pages serves remaining paths (SPA).

## Bind order / dashboard notes

Record the order you attached custom domain + routes so recreate is possible:

1. …
2. …

## Google OAuth

Authorized redirect URI (Google Cloud Console):

```text
https://<your-domain>/api/auth/callback
```
