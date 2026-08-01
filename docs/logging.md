# Logging

## Allowed

- Auth events: login success/fail (user id / email), logout
- Request metadata: user_id, api_key_id, provider, model, account_id, status, latency_ms, prompt_tokens, completion_tokens, error_code
- Pool: bench, promote, remove (ids only)

## Forbidden by default

- Full prompts, completions, tool arguments/results bodies
- Upstream access/refresh tokens
- Client API key plaintext
- OAuth codes / cookies

Retention: D1 `request_logs` pragmatic (e.g. 30 days cleanup later); not required for MVP.
