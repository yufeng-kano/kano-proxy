---
title: Codex CLI
description: Codex CLI cannot use Kano Proxy today because it requires the OpenAI Responses API. Codex as a provider works; Codex as a client does not.
---

# Codex CLI

**Not usable today.** Codex CLI only speaks the OpenAI Responses API and posts to `<base_url>/responses`. Kano Proxy serves Chat Completions and Anthropic Messages, not Responses, so Codex CLI has nowhere to send its requests.

Two different things share the name:

- **Codex as a provider** works. Connect your ChatGPT subscription on the Providers page and call `codex/gpt-5.4` and other Codex models from any tool on this list.
- **Codex CLI as a client** does not work. Pointing it at the proxy fails on the first request.

## Why

Until early 2026 Codex CLI accepted `wire_api = "chat"` in its provider config, which sent Chat Completions requests. That option was removed. The current config reference states that `responses` is the only supported value, and the CLI refuses to start with `wire_api = "chat"`:

```text
`wire_api = "chat"` is no longer supported.
How to fix: set `wire_api = "responses"` in your provider config.
```

## What would change this

The proxy would need a `POST /openai/v1/responses` endpoint that converts Responses requests and streams. That is not built. If it lands, this page will carry the config:

```toml
# ~/.codex/config.toml, for reference only. Does not work yet.
model = "codex/gpt-5.4"
model_provider = "kano"

[model_providers.kano]
name = "Kano Proxy"
base_url = "https://<your-domain>/openai/v1"
env_key = "KANO_PROXY_API_KEY"
wire_api = "responses"
```

## Checked against

Codex documentation and source, 2026-09-04: [Config reference](https://learn.chatgpt.com/docs/config-file/config-reference), [Deprecation of the chat wire API](https://github.com/openai/codex/discussions/7782).
