---
title: Codex CLI
description: Point the Codex CLI at Kano Proxy, generate a model catalog so /model lists your proxy models, and optionally run any other connected model in it.
---

# Codex CLI

Works. Codex CLI speaks the OpenAI Responses API, and Kano Proxy serves it at `<base_url>/responses`. With a Codex model the request passes through to your ChatGPT subscription untouched, so the CLI behaves as it does against OpenAI, with the proxy's account pooling, failover, and logs on top.

Two different things share the name:

- **Codex as a provider**: connect your ChatGPT subscription on the Providers page. Its models show up as `codex/<model>` for every tool on this list.
- **Codex CLI as a client**: this page. The CLI sends its requests to the proxy, which routes them to whichever provider the model id names.

Three steps: write the config, generate the model catalog, start `codex`.

## 1. Config

Add a provider to `~/.codex/config.toml` and select it:

```toml
model = "codex/gpt-5.6-sol"
model_provider = "kano"
model_reasoning_effort = "high"          # a default; /model changes it later
model_catalog_json = "kano-models.json"  # generated in step 2, relative to ~/.codex

[model_providers.kano]
name = "Kano Proxy"
base_url = "https://<your-domain>/openai/v1"
experimental_bearer_token = "<your-api-key>"
wire_api = "responses"
```

- `model_catalog_json` and `model_reasoning_effort` are top-level keys: keep them above `[model_providers.kano]`, or they land inside that table and are ignored.
- To keep the key out of the file, replace `experimental_bearer_token` with `env_key = "KANO_PROXY_API_KEY"` and `export KANO_PROXY_API_KEY=<your-api-key>` in your shell. Codex reads `~/.codex/auth.json` only for its own OpenAI login, not for a custom provider.
- `wire_api = "responses"` is the only value current Codex releases accept.
- A group endpoint works too: set `base_url` to `https://<your-domain>/g/<group-slug>/openai/v1` and `model` to one of the group's names.

## 2. Model catalog

Codex takes its `/model` picker and each model's metadata (context window, effort levels, instructions) from a catalog. That catalog knows `gpt-5.6-sol` but not `codex/gpt-5.6-sol`, so without your own catalog a proxy id is an unknown model: it runs on fallback metadata, never shows in the picker, and Codex may not send the reasoning effort at all ([openai/codex#30697](https://github.com/openai/codex/issues/30697)).

The script below copies the catalog shipped with your Codex binary, keeps the models Codex itself lists, prefixes each slug with `codex/`, and trims the effort levels to the ones the proxy accepts. Instructions, tools, and context windows stay exactly as Codex ships them. Re-run it after upgrading Codex.

macOS / Linux (`jq` is preinstalled on current macOS; on Linux, install it from your package manager):

```bash
codex debug models --bundled | jq '{models: [ .models[]
  | select(.visibility == "list")
  | .slug = "codex/" + (.slug | sub("^codex/"; ""))
  | .supported_reasoning_levels |= map(select(.effort | IN("low","medium","high","xhigh")))
  | .default_reasoning_level |= (if IN("low","medium","high","xhigh") then . else "medium" end)
]}' > ~/.codex/kano-models.json
```

Windows (PowerShell):

```powershell
$catalog = (codex debug models --bundled | Out-String) | ConvertFrom-Json
$keep = "low", "medium", "high", "xhigh"
$models = foreach ($m in $catalog.models) {
  if ($m.visibility -ne "list") { continue }
  $m.slug = "codex/" + ($m.slug -replace "^codex/", "")
  $m.supported_reasoning_levels = @($m.supported_reasoning_levels | Where-Object { $keep -contains $_.effort })
  if ($keep -notcontains $m.default_reasoning_level) { $m.default_reasoning_level = "medium" }
  $m
}
$json = @{ models = $models } | ConvertTo-Json -Depth 64
[IO.File]::WriteAllText("$HOME\.codex\kano-models.json", $json)
```

Check the result without sending a request:

```bash
codex debug models
```

It prints exactly the catalog Codex will use: `codex/gpt-6-astra`, `codex/gpt-5.6-sol`, and the rest, each with its context window and effort levels.

## 3. Use

```bash
codex
```

`/model` inside the TUI lists your proxy models; pick one, then an effort. The choice is written back to `config.toml`, so the next start uses it without any flags. `codex --model <id>` still works for a one-off.

Which models exist for you depends on the accounts you connected. **Models** in the app, or `GET https://<your-domain>/openai/v1/models`, is the live list; the catalog only decides what the picker shows.

## Reasoning effort

The proxy accepts `low`, `medium`, `high`, `xhigh`, and `max`, and rejects `minimal` and `ultra` with `400 invalid reasoning.effort`. A Codex model tops out at `xhigh`: `max` is lowered to `xhigh` before the request goes upstream. On any other provider the value is clamped the same way, to the highest effort that provider accepts. The full ladder is in [Endpoints and model ids](/guide/endpoints).

## Other models

Any id from your Models page works as the Codex model: `claude-code/claude-opus-5`, `grok/grok-4.5`, `antigravity/gemini-3-flash`, or `<slug>/<model>` for a custom endpoint. The proxy converts the Responses request and stream on the fly.

To put one in the picker, add an entry to `kano-models.json`: copy one of the generated entries, set `slug` to the proxy id, and set `display_name`, `context_window`, and `max_context_window` for that model. Keep the copied instructions; an entry without `base_instructions` or `model_messages.instructions_template` is rejected at startup. What changes on a non-Codex model:

- **Unknown model warning.** Without a catalog entry, Codex prints `Model metadata for "<id>" not found. Defaulting to fallback metadata` on startup. It is harmless: Codex falls back to generic settings (272k context window, plain function tools) and runs normally.
- **Prompt caching on Claude targets.** On a `claude-code/...` model the proxy places Anthropic `cache_control` breakpoints for you (Codex cannot express them on its wire), so each tool round reads the previous turn from cache. The Logs page shows it as cache read tokens from the second request of a session on.
- **Web search.** Codex attaches its hosted web-search tool to every request. On a Codex model that tool passes through and works. On any other model the proxy replaces it with a stub tool that tells the model web search is unavailable here. If the model calls it anyway, Codex itself reports `unsupported call: web_search` back to the model and the turn continues. Set `web_search = "disabled"` in the config to drop the tool entirely.
- **Sub-agents, plans, goals.** Codex's own tools (`exec_command`, `spawn_agent`, `update_plan`, and the rest) are ordinary function tools and work on every model.
- **Not available:** `previous_response_id`, stored responses, remote compaction, and hosted tools other than web search on Codex models. Codex does not use any of these against a custom provider.

## Checked against

Codex CLI 0.150.1, request captured against a local endpoint, 2026-09-04. Catalog script and `codex debug models` checked with Codex CLI 0.154.0 on macOS, 2026-09-13; the PowerShell version is written to the same logic but was not run on Windows. Config reference: [learn.chatgpt.com](https://learn.chatgpt.com/docs/config-file/config-reference).
