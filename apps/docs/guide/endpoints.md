---
title: Endpoints and model ids
description: Base URLs, authentication headers, provider/model ids, model groups, and how to list models on Kano Proxy.
---

# Endpoints and model ids

## Base URLs

| Protocol | Base URL | The client appends |
|----------|----------|--------------------|
| OpenAI-compatible | `https://<your-domain>/openai/v1` | `/chat/completions`, `/responses`, `/models`, `/audio/transcriptions` |
| Anthropic Messages | `https://<your-domain>/anthropic` | `/v1/messages`, `/v1/models` |

Some Anthropic SDKs and tools append `/messages` directly instead of `/v1/messages`. For those, set the base to `https://<your-domain>/anthropic/v1`. Each agent page says which form its tool expects.

`/responses` is the OpenAI Responses API, the wire the Codex CLI uses. It works for every model id: Codex models pass through natively, other providers are converted. Stateful features (`previous_response_id`, `conversation`, fetching a stored response) are not available, and hosted tools such as web search only reach Codex models. See the [Codex CLI](/agents/codex-cli) page.

Not available: embeddings, image generation, and audio output.

## Authentication

Send the key from the Keys page in either header. Both work on both bases.

```http
Authorization: Bearer <your-api-key>
```

```http
x-api-key: <your-api-key>
```

Your provider credentials never leave the proxy. A client only ever holds a `sk-kano-proxy-` key, which you can revoke at any time.

## Model ids

Every model id has two parts, split at the first slash:

```text
<provider>/<upstream model id>
```

| Provider | Example id |
|----------|------------|
| `claude-code` | `claude-code/claude-opus-5` |
| `codex` | `codex/gpt-5.4` |
| `grok` | `grok/grok-4.5` |
| `antigravity` | `antigravity/gemini-3-flash` |
| a custom endpoint with slug `mygw` | `mygw/<model the endpoint knows>` |
| a local model with slug `my-mac` | `my-mac/<model Ollama reports>` |

The ids above are examples of the shape only. Which models exist for you depends on the accounts you connected. Read the live list from **Models** in the app, or from the API:

```bash
curl https://<your-domain>/openai/v1/models -H "Authorization: Bearer <your-api-key>"
```

The same model id works on both bases. Asking the Anthropic base for a Codex or Gemini model is fine: the proxy converts the request and the stream.

A bare id without a provider prefix is rejected on the shared bases. Use a model group when a tool cannot send a prefixed name.

## Model groups

A model group is a private endpoint with its own model names. Each name maps to an ordered list of `provider/model` targets. Two uses:

- **A tool with hard-coded model names.** Create a group, name a model exactly what the tool sends, and point the tool at the group's base URL.
- **Failover across providers or accounts.** List several targets. The first usable one answers. If its accounts are paused or out of quota, the next target is tried.

Group base URLs replace the shared ones:

| Protocol | Group base URL |
|----------|----------------|
| OpenAI-compatible | `https://<your-domain>/g/<group-slug>/openai/v1` |
| Anthropic Messages | `https://<your-domain>/g/<group-slug>/anthropic` |

On a group endpoint only that group's model names resolve. `GET .../models` on a group lists exactly those names. Create and edit groups on the **Groups** page.

## Streaming and tools

Streaming (`stream: true` or the Anthropic SSE stream) is passed through chunk by chunk. Tool calls, images, JSON output modes, `reasoning_effort`, `stop`, `temperature`, and `top_p` are forwarded where the target provider supports them.
