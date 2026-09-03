---
title: Claude Code
description: Point Claude Code at Kano Proxy with ANTHROPIC_BASE_URL and ANTHROPIC_AUTH_TOKEN, and choose which provider/model answers.
---

# Claude Code

Works fully. Claude Code speaks the Anthropic Messages API, which the proxy serves natively. Any connected provider can answer, not only Claude: set the model to a `codex/`, `grok/`, or `antigravity/` id and the proxy converts the request and the stream.

## Settings

Add an `env` block to `~/.claude/settings.json`. Values there apply to every session and take precedence over the shell environment.

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "https://<your-domain>/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "<your-api-key>",
    "ANTHROPIC_MODEL": "claude-code/claude-opus-5",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-code/claude-opus-5",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-code/claude-sonnet-5",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-code/claude-haiku-4-5"
  }
}
```

- `ANTHROPIC_BASE_URL` is the gateway root **without** `/v1`. Claude Code appends `/v1/messages` itself.
- `ANTHROPIC_AUTH_TOKEN` is sent as `Authorization: Bearer`. `ANTHROPIC_API_KEY` also works and is sent as `x-api-key`; the proxy accepts both. Use one of them.
- `ANTHROPIC_MODEL` is the model used at start. The three `ANTHROPIC_DEFAULT_*_MODEL` values are what the Opus, Sonnet, and Haiku entries in `/model` resolve to. Fill them with ids from your own Models page.

Or export the same variables in your shell for a one-off session:

```bash
export ANTHROPIC_BASE_URL=https://<your-domain>/anthropic
export ANTHROPIC_AUTH_TOKEN=<your-api-key>
export ANTHROPIC_MODEL=claude-code/claude-opus-5
claude
```

Do not put the key in a project's committed `.claude/settings.json`.

## Switching models

`/model <id>` inside a session switches to any id your proxy serves, for example `/model codex/gpt-5.4`. Press Enter to save it as your default or `s` to keep it for this session only.

Setting `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` makes Claude Code read `GET /v1/models` from the proxy and add the results to the `/model` picker. Claude Code keeps only ids containing `claude` or `anthropic`, so `claude-code/...` ids appear and `codex/...` ids do not; type those by hand.

## Check it

Run `/status` in Claude Code. It shows the base URL and whether an auth token is in use. On the proxy side, the **Logs** page shows the request within seconds.

## Limits

- Fast mode and the WebFetch safety check call Anthropic directly and ignore the base URL.
- The Claude desktop app ignores these variables. It has its own third-party inference form, where the same base URL and key go.
- The VS Code extension reads them from the `claudeCode.environmentVariables` setting instead of the shell.

## Checked against

Claude Code documentation, 2026-09-04: [Connect to an LLM gateway](https://code.claude.com/docs/en/llm-gateway-connect), [Gateway protocol](https://code.claude.com/docs/en/llm-gateway-protocol), [Model configuration](https://code.claude.com/docs/en/model-config).
