---
title: Codex CLI
description: Point the Codex CLI at Kano Proxy as a custom model provider and run it on any connected model, Codex or not.
---

# Codex CLI

Works. Codex CLI speaks the OpenAI Responses API, and Kano Proxy serves it at `<base_url>/responses`. Any model id from your Models page works as the Codex model: Codex models pass through to your ChatGPT subscription untouched, and every other provider is converted on the fly.

Two different things share the name:

- **Codex as a provider**: connect your ChatGPT subscription on the Providers page and call `codex/gpt-5.4` and other Codex models from any tool on this list.
- **Codex CLI as a client**: this page. The CLI sends its requests to the proxy, which routes them to whichever provider the model id names.

## Settings

Add a provider to `~/.codex/config.toml` and select it:

```toml
model = "claude-code/claude-opus-5"
model_provider = "kano"

[model_providers.kano]
name = "Kano Proxy"
base_url = "https://<your-domain>/openai/v1"
env_key = "KANO_PROXY_API_KEY"
wire_api = "responses"
```

Then export the key:

```bash
export KANO_PROXY_API_KEY=<your-api-key>
```

- `model` takes any id from your Models page: `codex/gpt-5.4`, `claude-code/claude-opus-5`, `grok/grok-4.5`, `antigravity/gemini-3-flash`, or `<slug>/<model>` for a custom endpoint. Switch with `codex --model <id>` or `/model` inside the CLI.
- A group endpoint works too: set `base_url` to `https://<your-domain>/g/<group-slug>/openai/v1` and `model` to one of the group's names.
- `wire_api = "responses"` is the only value current Codex releases accept.

## What to expect

- **Unknown model warning.** For any non-Codex id, Codex prints `Model metadata for "<id>" not found. Defaulting to fallback metadata` on startup. It is harmless: Codex falls back to generic settings (272k context window, plain function tools) and runs normally.
- **Reasoning effort.** With the fallback metadata Codex sends no effort by default. Set `model_reasoning_effort = "high"` (or `low`, `medium`, `xhigh`) in the config to forward one; the proxy clamps it to what the target provider accepts.
- **Prompt caching on Claude targets.** On a `claude-code/...` model the proxy places Anthropic `cache_control` breakpoints for you (Codex cannot express them on its wire), so each tool round reads the previous turn from cache. The Logs page shows it as cache read tokens from the second request of a session on.
- **Web search.** Codex attaches its hosted web-search tool to every request. On a Codex model that tool passes through and works. On any other model the proxy replaces it with a stub tool that tells the model web search is unavailable here. If the model calls it anyway, Codex itself reports `unsupported call: web_search` back to the model and the turn continues. Set `web_search = "disabled"` in the config to drop the tool entirely.
- **Sub-agents, plans, goals.** Codex's own tools (`exec_command`, `spawn_agent`, `update_plan`, and the rest) are ordinary function tools and work on every model.
- **Not available:** `previous_response_id`, stored responses, remote compaction, and hosted tools other than web search on Codex models. Codex does not use any of these against a custom provider.

## Checked against

Codex CLI 0.150.1, request captured against a local endpoint, 2026-09-04. Config reference: [learn.chatgpt.com](https://learn.chatgpt.com/docs/config-file/config-reference).
