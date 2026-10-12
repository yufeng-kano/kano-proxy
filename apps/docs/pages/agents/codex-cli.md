---
title: Codex CLI
description: Point the Codex CLI at Kano Proxy, generate a model catalog so /model lists your proxy models, and optionally run any other connected model in it.
---

# Codex CLI

Works: Codex CLI speaks the OpenAI Responses API, which the proxy serves at `<base_url>/responses`, and with a Codex model it behaves as it does against OpenAI, with the proxy's account pooling, failover and logs on top.

This page is about Codex CLI as a **client**. Codex as a **provider**, your ChatGPT subscription whose models show up as `codex/<model>`, is connected on the [Providers](/app/providers) page.

## Set it up with the CLI

With the [kano-proxy CLI](/cli/install) installed:

```console
$ kano-proxy login                      # once per machine
$ kano-proxy agent setup codex
```

1. Pick the default model, its reasoning effort, and which models on your proxy answer behind it, in order. Any provider works. The first one answers; the rest take over when it is benched or out of quota.
2. The CLI creates a model group named `codex-cli`, mints an API key named after this machine, edits `~/.codex/config.toml` and generates `~/.codex/kano-models.json` from your installed Codex binary. Your other settings and comments in `config.toml` stay.
3. Start `codex`.

::: details What the CLI does differently from the steps by hand
The group's model names are the plain ids (`gpt-5.6-sol`, not `codex/gpt-5.6-sol`), so Codex's own catalog metadata lines up with them without any renaming. The catalog is generated with the same transformation as the scripts below, with no `jq` needed. See [Groups](/app/groups).
:::

To undo it, or to see what the CLI manages, see [Set up agents](/cli/agents).

## Set it up by hand

Three steps: write the config, generate the model catalog, start `codex`.

### 1. Config

Add a provider to `~/.codex/config.toml` and select it. The model id is an example; use one from your own **Models** page.

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

Keep `model_catalog_json` and `model_reasoning_effort` above `[model_providers.kano]`. They are top-level keys; below the table header they land inside that table and are ignored.

::: details Other ways to write it
- To keep the key out of the file, replace `experimental_bearer_token` with `env_key = "KANO_PROXY_API_KEY"` and export `KANO_PROXY_API_KEY=<your-api-key>` in your shell. Codex reads `~/.codex/auth.json` only for its own OpenAI login, not for a custom provider.
- `wire_api = "responses"` is the only value current Codex releases accept.
- A group endpoint works too: set `base_url` to `https://<your-domain>/g/<group-slug>/openai/v1` and `model` to one of the group's names.
:::

### 2. Model catalog

Run the script for your system. It writes `~/.codex/kano-models.json`. Re-run it after upgrading Codex.

::: code-group

```bash [macOS / Linux]
codex debug models --bundled | jq '{models: [ .models[]
  | select(.visibility == "list")
  | .slug = "codex/" + (.slug | sub("^codex/"; ""))
  | .supported_reasoning_levels |= map(select(.effort | IN("low","medium","high","xhigh")))
  | .default_reasoning_level |= (if IN("low","medium","high","xhigh") then . else "medium" end)
]}' > ~/.codex/kano-models.json
```

```powershell [Windows]
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

:::

`jq` is preinstalled on current macOS. On Linux, install it from your package manager.

::: details Why Codex needs its own catalog
Codex takes its `/model` picker and each model's metadata (context window, effort levels, instructions) from a catalog. The built-in catalog knows `gpt-5.6-sol` but not `codex/gpt-5.6-sol`. Without your own catalog, a proxy id is an unknown model: it runs on fallback metadata, never shows in the picker, and Codex may not send the reasoning effort at all ([openai/codex#30697](https://github.com/openai/codex/issues/30697)).

The script copies the catalog shipped with your Codex binary, keeps the models Codex itself lists, prefixes each slug with `codex/`, and trims the effort levels to the ones the proxy accepts. Instructions, tools and context windows stay exactly as Codex ships them.
:::

### 3. Start

```bash
codex
```

`/model` inside the TUI lists your proxy models; pick one, then an effort. The choice is written back to `config.toml`, so the next start uses it without any flags. `codex --model <id>` still works for a one-off.

Which models exist for you depends on the accounts you connected. The **Models** page, or `GET https://<your-domain>/openai/v1/models`, is the live list; the catalog only decides what the picker shows.

## Reasoning effort

A Codex model tops out at `xhigh`: `max` is lowered to `xhigh` before the request goes upstream. `minimal` and `ultra` are rejected. The full ladder for every provider is in [Endpoints and model ids](/reference/endpoints).

## Use other models

Any id from your **Models** page works as the Codex model, for example `claude-code/claude-opus-5` or `<slug>/<model>` for a custom endpoint. The proxy converts the Responses request and stream on the fly.

To put one in the picker:

1. Copy one of the generated entries in `kano-models.json`.
2. Set `slug` to the proxy id.
3. Set `display_name`, `context_window` and `max_context_window` for that model.

Keep the copied instructions. An entry without `base_instructions` or `model_messages.instructions_template` is rejected at startup.

What changes on a non-Codex model:

- **Unknown model warning.** Without a catalog entry, Codex prints `Model metadata for "<id>" not found. Defaulting to fallback metadata` on startup. It is harmless: Codex falls back to generic settings (272k context window, plain function tools) and runs normally.
- **Prompt caching on Claude targets.** On a `claude-code/...` model the proxy places Anthropic `cache_control` breakpoints for you, because Codex cannot express them on its wire. Each tool round reads the previous turn from cache. The [Logs](/app/logs) page shows cache read tokens from the second request of a session on.
- **Web search.** Codex attaches its hosted web-search tool to every request. On a Codex model it passes through and works. On any other model the proxy replaces it with a stub tool that tells the model web search is unavailable. If the model calls it anyway, Codex reports `unsupported call: web_search` back to the model and the turn continues. Set `web_search = "disabled"` in the config to drop the tool entirely.
- **Sub-agents, plans, goals.** Codex's own tools (`exec_command`, `spawn_agent`, `update_plan`, and the rest) are ordinary function tools and work on every model.

## Check it

1. Run `codex debug models`. It prints the catalog Codex will use, without sending a request: `codex/gpt-6-astra`, `codex/gpt-5.6-sol` and the rest, each with its context window and effort levels.
2. Start `codex` and send a message. The request appears on the [Logs](/app/logs) page within seconds.

## Limits

- `previous_response_id`, stored responses and remote compaction are not available. Codex does not use them against a custom provider.
- Hosted tools other than web search are not available, and web search works only on Codex models.

## Checked against

Codex CLI 0.150.1, request captured against a local endpoint, 2026-09-04. Catalog script and `codex debug models` checked with Codex CLI 0.154.0 on macOS, 2026-09-13; the PowerShell version is written to the same logic but was not run on Windows. Config reference: [learn.chatgpt.com](https://learn.chatgpt.com/docs/config-file/config-reference).
