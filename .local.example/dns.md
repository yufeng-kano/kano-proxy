# DNS and hostname (private)

Copy to `.local/dns.md` and replace placeholders. **Do not commit** the filled file.

## Production host

| Item | Value |
|------|--------|
| Hostname | `<your-domain>` e.g. `proxy.example.com` |
| DNS zone / provider | `<zone-name>` |
| Server address | `<server-ip-or-name>` |

The same values go into the server's `.env` (`DOMAIN`, `APP_URL`, `GOOGLE_REDIRECT_URI` — docs/deployment.md § Domains).

## DNS records

| Type | Name | Target | Notes |
|------|------|--------|-------|
| A / AAAA | `<subdomain or @>` | `<server-ip>` | |

## Ingress

One hostname serves the UI and the API (docs/deployment.md § Domains). Record which front end terminates TLS:

- the bundled `caddy` service (ports 80 and 443 open to the server), or
- your own reverse proxy: which one, and where its configuration lives. It must route the API paths listed in docs/deployment.md § Quick start to the API container and everything else to the web container, without buffering.

## Google OAuth

Authorized redirect URI (Google Cloud Console):

```text
https://<your-domain>/api/auth/callback
```
