# Production secrets (private — never commit)

Copy to `.local/secrets.md` only after generating real values. Prefer `chmod 600`.

## Generate

```bash
# SESSION_SECRET — long random string
openssl rand -base64 48

# TOKEN_ENCRYPTION_KEY — exactly 32 bytes, standard base64
openssl rand -base64 32
```

## Put on Cloudflare (after Worker exists)

```bash
cd apps/api
# from shell vars or .local/production.env — do not paste into chat logs
printf %s "$SESSION_SECRET" | npx wrangler secret put SESSION_SECRET
printf %s "$TOKEN_ENCRYPTION_KEY" | npx wrangler secret put TOKEN_ENCRYPTION_KEY
printf %s "$GOOGLE_CLIENT_ID" | npx wrangler secret put GOOGLE_CLIENT_ID
printf %s "$GOOGLE_CLIENT_SECRET" | npx wrangler secret put GOOGLE_CLIENT_SECRET
```

## Values

Fill only in the gitignored `.local/` copy:

### SESSION_SECRET

```
<generated>
```

### TOKEN_ENCRYPTION_KEY

```
<generated-32-byte-base64>
```

## Not generated

| Name | Source |
|------|--------|
| `GOOGLE_CLIENT_ID` | Google Cloud OAuth client |
| `GOOGLE_CLIENT_SECRET` | Google Cloud OAuth client |

## Rotation

- `SESSION_SECRET` — invalidates admin sessions
- `TOKEN_ENCRYPTION_KEY` — stored upstream credentials become unreadable; re-bind accounts
