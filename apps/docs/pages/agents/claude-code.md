---
title: Claude Code
description: Point Claude Code at Kano Proxy with one CLI command, or by hand with ANTHROPIC_BASE_URL and ANTHROPIC_AUTH_TOKEN, and choose which provider/model answers.
---

# Claude Code

Works fully. Claude Code speaks the Anthropic Messages API, which the proxy serves natively. Any connected provider can answer, not only Claude: set the model to a `codex/`, `grok/`, or `antigravity/` id and the proxy converts the request and the stream.

## Set it up with the CLI

The [kano-proxy CLI](/guide/local-models) can write this configuration for you:

```console
$ kano-proxy login                      # once per machine
$ kano-proxy agent setup claude-code
```

It reads the models your account can serve and lets you pick, for each of Claude Code's slots — Fable, Opus, Sonnet and Haiku — which models on your proxy answer, in order: any provider, the first answers, the rest take over when it is benched or out of quota. It then creates a model group named `claude-code` whose model names are plain Claude ids, mints an API key named after this machine, and writes an `env` block like the one below into `~/.claude/settings.json` with `ANTHROPIC_BASE_URL` set to the group's endpoint. Going through a group matters: Claude Code decides what to enable from the model id it sees, and `/g/claude-code/anthropic` serves real ids such as `claude-opus-5` even when the proxy routes them elsewhere — so a slot can be answered by `antigravity` or `codex` first and Claude Code still sees a Claude id. It also turns on Claude Code's gateway model discovery, so every name in the group shows up in `/model`, and it never writes `ANTHROPIC_MODEL`, so the default you save with `/model` sticks. The Haiku slot is also where Claude Code runs its background work (session titles and the like), so keep it on a cheap model.

`kano-proxy agent restore claude-code` puts the fields it wrote back the way they were and revokes the key; `--full` restores the whole file from the backup it took first. `kano-proxy agent list` shows what is managed. The group stays on the Groups page, where you can reorder or add fallback targets at any time.

## Settings by hand

Add an `env` block to `~/.claude/settings.json`. Values there apply to every session and take precedence over the shell environment.

```json
{
  "disableClaudeAiConnectors": true,
  "env": {
    "ANTHROPIC_BASE_URL": "https://<your-domain>/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "<your-api-key>",
    "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY": "1",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-code/claude-opus-5",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-code/claude-sonnet-5",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-code/claude-haiku-4-5",
    "ANTHROPIC_DEFAULT_FABLE_MODEL": "claude-code/claude-fable-5-1"
  }
}
```

- `ANTHROPIC_BASE_URL` is the gateway root **without** `/v1`. Claude Code appends `/v1/messages` itself.
- `ANTHROPIC_AUTH_TOKEN` is sent as `Authorization: Bearer`. `ANTHROPIC_API_KEY` also works and is sent as `x-api-key`; the proxy accepts both. Use one of them.
- The `ANTHROPIC_DEFAULT_*_MODEL` values are what the Fable, Opus, Sonnet and Haiku entries in `/model` resolve to. Fill them with ids from your own Models page. Haiku is also what Claude Code uses for background work.
- Leave `ANTHROPIC_MODEL` unset unless you want to pin the start model: it overrides whatever you save with `/model`, every launch.
- `disableClaudeAiConnectors: true` (top level, beside `env`) stops the start-up notice that claude.ai connectors are disabled because another auth source takes precedence — with the proxy's token set they are disabled either way.
- `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` makes Claude Code read `GET /v1/models` from the proxy at startup and add the results to the `/model` picker (see below).

Or export the same variables in your shell for a one-off session:

```bash
export ANTHROPIC_BASE_URL=https://<your-domain>/anthropic
export ANTHROPIC_AUTH_TOKEN=<your-api-key>
export ANTHROPIC_MODEL=claude-code/claude-opus-5
claude
```

Do not put the key in a project's committed `.claude/settings.json`.

## Switching models

`/model <id>` inside a session switches to any id your proxy serves, for example `/model codex/gpt-5.6-sol`. Press Enter to save it as your default or `s` to keep it for this session only.

With `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` (the CLI sets it) Claude Code reads `GET /v1/models` from the proxy at startup and adds the results to the `/model` picker. Claude Code keeps only ids containing `claude` or `anthropic`, so Claude ids appear and `codex/...` ids do not; type those by hand.

## Check it

Run `/status` in Claude Code. It shows the base URL and whether an auth token is in use. On the proxy side, the **Logs** page shows the request within seconds.

## Limits

- Fast mode and the WebFetch safety check call Anthropic directly and ignore the base URL.
- The Claude desktop app ignores these variables. It has its own third-party inference form, where the same base URL and key go.
- The VS Code extension reads them from the `claudeCode.environmentVariables` setting instead of the shell.

## Checked against

Claude Code documentation, 2026-09-04: [Connect to an LLM gateway](https://code.claude.com/docs/en/llm-gateway-connect), [Gateway protocol](https://code.claude.com/docs/en/llm-gateway-protocol), [Model configuration](https://code.claude.com/docs/en/model-config).
