---
title: Claude Code
description: Point Claude Code at Kano Proxy with one CLI command, or by hand with ANTHROPIC_BASE_URL and ANTHROPIC_AUTH_TOKEN, and choose which provider/model answers.
---

# Claude Code

Works fully: Claude Code speaks the Anthropic Messages API, which the proxy serves natively, and any connected provider can answer, not only Claude.

## Set it up with the CLI

With the [kano-proxy CLI](/cli/install) installed:

```console
$ kano-proxy login                      # once per machine
$ kano-proxy agent setup claude-code
```

1. For each of Claude Code's slots, Fable, Opus, Sonnet and Haiku, pick which models on your proxy answer, in order. Any provider works. The first one answers; the rest take over when it is benched or out of quota.
2. The CLI creates a model group named `claude-code`, mints an API key named after this machine, and writes an `env` block into `~/.claude/settings.json` that points at the group.
3. Start `claude`.

Keep the Haiku slot on a cheap model: Claude Code also runs its background work there, such as session titles.

::: details Why the CLI goes through a model group
Claude Code decides which features to enable from the model id it sees. The group's endpoint, `/g/claude-code/anthropic`, serves plain Claude ids such as `claude-opus-5`, even when the proxy routes them to another provider. So a slot can be answered by `antigravity` or `codex` first while Claude Code still sees a Claude id. See [Groups](/app/groups).

The CLI also turns on gateway model discovery, so every name in the group shows up in `/model`. It never writes `ANTHROPIC_MODEL`, so the default you save with `/model` sticks.
:::

To undo it, or to see what the CLI manages, see [Set up agents](/cli/agents). The group stays on the **Groups** page, where you can reorder or add fallback targets at any time.

## Set it up by hand

1. Create a key on the **Access** page.
2. Add this to `~/.claude/settings.json`. Fill the `ANTHROPIC_DEFAULT_*_MODEL` values with ids from your own **Models** page; the ones below are examples.

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

3. Start `claude`.

Do not put the key in a project's committed `.claude/settings.json`.

::: details What each setting does
- Values in the `env` block apply to every session and take precedence over the shell environment.
- `ANTHROPIC_BASE_URL` is the gateway root **without** `/v1`. Claude Code appends `/v1/messages` itself.
- `ANTHROPIC_AUTH_TOKEN` is sent as `Authorization: Bearer`. `ANTHROPIC_API_KEY` also works and is sent as `x-api-key`; the proxy accepts both. Use one of them.
- The `ANTHROPIC_DEFAULT_*_MODEL` values are what the Fable, Opus, Sonnet and Haiku entries in `/model` resolve to. Haiku is also what Claude Code uses for background work.
- `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` makes Claude Code read `GET /v1/models` from the proxy at startup and add the results to the `/model` picker.
- `disableClaudeAiConnectors: true` sits at the top level, beside `env`. It stops the start-up notice that claude.ai connectors are disabled because another auth source takes precedence. With the proxy's token set they are disabled either way.
- Leave `ANTHROPIC_MODEL` unset unless you want to pin the start model. It overrides whatever you save with `/model`, every launch.
:::

For a one-off session, export the same variables in your shell instead:

::: code-group

```bash [macOS / Linux]
export ANTHROPIC_BASE_URL=https://<your-domain>/anthropic
export ANTHROPIC_AUTH_TOKEN=<your-api-key>
export ANTHROPIC_MODEL=claude-code/claude-opus-5
claude
```

```powershell [Windows]
$env:ANTHROPIC_BASE_URL = "https://<your-domain>/anthropic"
$env:ANTHROPIC_AUTH_TOKEN = "<your-api-key>"
$env:ANTHROPIC_MODEL = "claude-code/claude-opus-5"
claude
```

:::

## Switch models

`/model <id>` inside a session switches to any id your proxy serves, for example `/model codex/gpt-5.6-sol`. Press Enter to save it as your default, or `s` to keep it for this session only.

Gateway model discovery keeps only ids containing `claude` or `anthropic`. Claude ids appear in the picker; `codex/...` ids do not, so type those by hand. Model id rules are in [Endpoints and model ids](/reference/endpoints).

## Check it

1. Run `/status` in Claude Code. It shows the base URL and whether an auth token is in use.
2. Send a message. The request appears on the [Logs](/app/logs) page within seconds.

## Limits

- Fast mode and the WebFetch safety check call Anthropic directly and ignore the base URL.
- The Claude desktop app ignores these variables. It has its own third-party inference form, where the same base URL and key go.
- The VS Code extension reads them from the `claudeCode.environmentVariables` setting instead of the shell.

## Checked against

Claude Code documentation, 2026-09-04: [Connect to an LLM gateway](https://code.claude.com/docs/en/llm-gateway-connect), [Gateway protocol](https://code.claude.com/docs/en/llm-gateway-protocol), [Model configuration](https://code.claude.com/docs/en/model-config).
