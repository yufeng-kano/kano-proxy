---
title: OpenCode
description: Add Kano Proxy as a custom provider in opencode.json using @ai-sdk/openai-compatible, with the API key read from an environment variable.
---

# OpenCode

Works fully. OpenCode takes any OpenAI-compatible endpoint as a custom provider in its config file.

## Settings

Put this in `~/.config/opencode/opencode.json` for all projects, or in `opencode.json` at a project root for one project:

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
        "codex/gpt-5.4": { "name": "GPT-5.4" }
      }
    }
  },
  "model": "kano/claude-code/claude-opus-5"
}
```

Then export the key:

```bash
export KANO_PROXY_API_KEY=<your-api-key>
```

- `kano` is your name for the provider. Model references are `<provider name>/<model key>`, so the slash inside `claude-code/claude-opus-5` is fine.
- List under `models` the ids you want in the picker, taken from your Models page. The display `name` is free text.
- `{env:VAR}` reads an environment variable at startup. A missing variable becomes an empty string with no error, so an auth failure usually means the export did not happen.

## Anthropic format instead

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

Use a name other than `anthropic`, because overriding the built-in `anthropic` provider makes OpenCode add Anthropic-specific beta headers. The OpenCode docs describe only the OpenAI-compatible custom provider; the Anthropic form is verified from the source and a third-party gateway guide, not from the docs.

## Checked against

OpenCode documentation and source, 2026-09-04: [Providers](https://opencode.ai/docs/providers/), [Config](https://opencode.ai/docs/config/), [Models](https://opencode.ai/docs/models/), and the provider loader in the [opencode repository](https://github.com/anomalyco/opencode).
