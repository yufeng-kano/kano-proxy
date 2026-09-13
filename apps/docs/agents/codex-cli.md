---
title: Codex CLI
description: Point the Codex CLI at Kano Proxy as a model provider, keep using your Codex models, and optionally run any other connected model in it.
---

# Codex CLI

Works. Codex CLI speaks the OpenAI Responses API, and Kano Proxy serves it at `<base_url>/responses`. With a Codex model the request passes through to your ChatGPT subscription untouched, so the CLI behaves exactly as it does against OpenAI, with the proxy's account pooling, failover, and logs on top.

Two different things share the name:

- **Codex as a provider**: connect your ChatGPT subscription on the Providers page. Its models show up as `codex/<model>` for every tool on this list.
- **Codex CLI as a client**: this page. The CLI sends its requests to the proxy, which routes them to whichever provider the model id names.

## Settings

Add a provider to `~/.codex/config.toml` and select it. The model id below is an example; take the one you want from your Models page.

```toml
model = "codex/gpt-5.6-sol"
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

- `wire_api = "responses"` is the only value current Codex releases accept.
- A group endpoint works too: set `base_url` to `https://<your-domain>/g/<group-slug>/openai/v1` and `model` to one of the group's names.

## Listing models

Which models exist for you depends on the accounts you connected. Read the live list from **Models** in the app, or from the API:

```bash
curl https://<your-domain>/openai/v1/models -H "Authorization: Bearer <your-api-key>"
```

Codex models appear as `codex/<model>`. Switch with `codex --model <id>` or `/model <id>` inside the CLI; press Enter to save it as the default.

### Putting proxy ids in the model picker

Codex's `/model` picker only knows the models in its catalog, and that catalog has no `codex/...` ids. A ready-made catalog with the GPT-6 and GPT-5.6 models is on this site. Download it and point the config at it:

```bash
curl -o ~/.codex/models.json https://<your-domain>/docs/codex/models.json
```

```toml
model_catalog_json = "models.json"
```

Put that line at the top of `config.toml`, above `[model_providers.kano]`, since it is a top-level key. The picker then lists `codex/gpt-6-astra`, `codex/gpt-5.6-sol`, `codex/gpt-5.6-terra`, and `codex/gpt-5.6-luna` with their real context windows and the effort levels the proxy accepts, and the unknown-model warning below goes away for them. The file's entries are copied from the Codex CLI's own catalog, including its instructions; only the slugs are prefixed with `codex/`.

To add a Claude, Grok, Gemini, or custom model to the picker, copy one entry in the file, set `slug` to its proxy id, and set `display_name`, `context_window`, and `max_context_window` for that model. `codex debug models` prints what Codex will use without sending a request.

## Reasoning effort

Codex sends no effort for a custom provider unless you set one:

```toml
model_reasoning_effort = "high"
```

The proxy accepts `low`, `medium`, `high`, `xhigh`, and `max`, and rejects `minimal` and `ultra` with `400 invalid reasoning.effort`. A Codex model tops out at `xhigh`: `max` is lowered to `xhigh` before the request goes upstream. On any other provider the value is clamped the same way, to the highest effort that provider accepts.

## Other models

Any id from your Models page works as the Codex model: `claude-code/claude-opus-5`, `grok/grok-4.5`, `antigravity/gemini-3-flash`, or `<slug>/<model>` for a custom endpoint. The proxy converts the Responses request and stream on the fly. What changes:

- **Unknown model warning.** For any non-Codex id, Codex prints `Model metadata for "<id>" not found. Defaulting to fallback metadata` on startup. It is harmless: Codex falls back to generic settings (272k context window, plain function tools) and runs normally.
- **Prompt caching on Claude targets.** On a `claude-code/...` model the proxy places Anthropic `cache_control` breakpoints for you (Codex cannot express them on its wire), so each tool round reads the previous turn from cache. The Logs page shows it as cache read tokens from the second request of a session on.
- **Web search.** Codex attaches its hosted web-search tool to every request. On a Codex model that tool passes through and works. On any other model the proxy replaces it with a stub tool that tells the model web search is unavailable here. If the model calls it anyway, Codex itself reports `unsupported call: web_search` back to the model and the turn continues. Set `web_search = "disabled"` in the config to drop the tool entirely.
- **Sub-agents, plans, goals.** Codex's own tools (`exec_command`, `spawn_agent`, `update_plan`, and the rest) are ordinary function tools and work on every model.
- **Not available:** `previous_response_id`, stored responses, remote compaction, and hosted tools other than web search on Codex models. Codex does not use any of these against a custom provider.

## Checked against

Codex CLI 0.150.1, request captured against a local endpoint, 2026-09-04. Catalog steps checked with Codex CLI 0.154.0 and `codex debug models`, 2026-09-13. Config reference: [learn.chatgpt.com](https://learn.chatgpt.com/docs/config-file/config-reference).
