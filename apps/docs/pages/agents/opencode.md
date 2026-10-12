---
title: OpenCode
description: Add Kano Proxy as a custom provider in opencode.json using @ai-sdk/openai-compatible, with the API key read from an environment variable.
---

# OpenCode

Works fully: OpenCode takes any OpenAI-compatible endpoint as a custom provider in its config file.

The kano-proxy CLI does not set up OpenCode. Set it up by hand.

## Set it up by hand

1. Create a key on the **Access** page.
2. Put this in `~/.config/opencode/opencode.json` for all projects, or in `opencode.json` at a project root for one project. The model ids are examples; list ids from your own **Models** page.

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "kano": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Kano Proxy",
      "options": {
        "baseURL": "https://<your-domain>/openai/v1",
        "apiKey": "{env:KANO_PROXY_API_KEY}"
      },
      "models": {
        "claude-code/claude-opus-5": { "name": "Claude Opus 5" },
        "codex/gpt-5.6-sol": { "name": "GPT-5.6-Sol" }
      }
    }
  },
  "model": "kano/claude-code/claude-opus-5"
}
```

3. Export the key in the shell you start OpenCode from:

::: code-group

```bash [macOS / Linux]
export KANO_PROXY_API_KEY=<your-api-key>
```

```powershell [Windows]
$env:KANO_PROXY_API_KEY = "<your-api-key>"
```

:::

4. Start `opencode`.

::: details What each setting does
- `kano` is your name for the provider. Model references are `<provider name>/<model key>`, so the slash inside `claude-code/claude-opus-5` is fine.
- `models` lists the ids you want in the picker. The display `name` is free text. Model id rules are in [Endpoints and model ids](/reference/endpoints).
- `{env:VAR}` reads an environment variable at startup. A missing variable becomes an empty string with no error, so an auth failure usually means the export did not happen.
:::

### Anthropic format instead

OpenCode can also talk to the Anthropic base by loading `@ai-sdk/anthropic`. That SDK appends `/messages`, so the base must end in `/v1`:

```json
{
  "provider": {
    "kano-anthropic": {
      "npm": "@ai-sdk/anthropic",
      "name": "Kano Proxy (Anthropic)",
      "options": {
        "baseURL": "https://<your-domain>/anthropic/v1",
        "apiKey": "{env:KANO_PROXY_API_KEY}"
      },
      "models": {
        "claude-code/claude-opus-5": { "name": "Claude Opus 5" }
      }
    }
  }
}
```

Use a provider name other than `anthropic`.

::: details Why not the name `anthropic`
Overriding the built-in `anthropic` provider makes OpenCode add Anthropic-specific beta headers. The OpenCode docs describe only the OpenAI-compatible custom provider; the Anthropic form is verified from the source and a third-party gateway guide, not from the docs.
:::

## Check it

Send a message in OpenCode with one of the listed models. The request appears on the [Logs](/app/logs) page within seconds. If it fails with an auth error, check that `KANO_PROXY_API_KEY` is exported in that shell.

## Limits

No known limits through the proxy.

## Checked against

OpenCode documentation and source, 2026-09-04: [Providers](https://opencode.ai/docs/providers/), [Config](https://opencode.ai/docs/config/), [Models](https://opencode.ai/docs/models/), and the provider loader in the [opencode repository](https://github.com/anomalyco/opencode).
