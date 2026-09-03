---
title: Cursor
description: Use Kano Proxy models in Cursor through the OpenAI API key and the Override OpenAI Base URL setting, and what Cursor features stay excluded.
---

# Cursor

Works for chat and Agent through Cursor's OpenAI-compatible path. Tab completion, subagents, and Cursor's own built-in models do not go through your key. Read the limits before switching it on.

## Settings

In **Cursor Settings › Models**:

1. Turn on **OpenAI API Key** and paste `<your-api-key>`.
2. Turn on **Override OpenAI Base URL** and enter `https://<your-domain>/openai/v1`.
3. Use **+ Add model** and type a model id from your Models page, for example `claude-code/claude-opus-5` or `codex/gpt-5.4`.
4. Select that model in the chat model picker.

Claude models go through the OpenAI-compatible base as well. Cursor has no base URL override for its Anthropic key, so leave that key off.

## Limits

- **The override is global.** While it is on, every model that uses one of your own keys is routed to the proxy. Cursor's built-in models and any Anthropic-key models stop working. Turn the override off when you want those back. Cursor staff list this as a known issue.
- **Tab** always uses Cursor's own models.
- **Subagents** ignore custom keys and bill against your Cursor plan.
- **Background and cloud agents** have no base URL override.
- Requests travel through Cursor's servers to reach the proxy, so the proxy must be reachable from the internet and your `sk-kano-proxy-` key passes through Cursor on every request. Use a dedicated key so you can revoke it on its own.

## Checked against

Cursor documentation and staff replies on the Cursor forum, 2026-09-04: [API keys](https://cursor.com/docs/settings/api-keys), [Override OpenAI Base URL](https://forum.cursor.com/t/override-openai-base-url-not-working/168323), [Anthropic models break when the override is set](https://forum.cursor.com/t/anthropic-models-break-when-override-openai-baseurl-is-set/144899), [Subagents ignore the override](https://forum.cursor.com/t/custom-models-openai-base-url-override-not-usable-in-subagents-silently-ignored/159369). The official docs page does not describe the base URL override; that part rests on staff replies.
