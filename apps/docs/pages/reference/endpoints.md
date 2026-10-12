---
title: Endpoints and model ids
description: Kano Proxy base URLs, the paths clients append, authentication headers, provider/model ids, listing models, streaming, reasoning effort, and what is not supported.
---

# Endpoints and model ids

Every request goes to one of two base URLs with a Kano Proxy key, and names its model as `provider/model`.

## Base URLs

| Protocol | Base URL | The client appends |
|----------|----------|--------------------|
| OpenAI-compatible | `https://<your-domain>/openai/v1` | `/chat/completions`, `/responses`, `/models`, `/audio/transcriptions` |
| Anthropic Messages | `https://<your-domain>/anthropic` | `/v1/messages`, `/v1/messages/count_tokens`, `/v1/models` |

Some Anthropic SDKs and tools append `/messages` instead of `/v1/messages`. For those, set the base to `https://<your-domain>/anthropic/v1`. Each agent page says which form its tool expects.

The **Connect** tab on **Access** shows these URLs for your instance, ready to copy.

## Authentication

Send a key from **Access** in either header. Both headers work on both bases.

```http
Authorization: Bearer <your-api-key>
```

```http
x-api-key: <your-api-key>
```

Keys start with `sk-kano-proxy-`. Your provider credentials stay on the proxy and never reach a client. A key can be revoked on **Access** at any time, and stops working at once.

## Model ids

A model id has two parts, split at the first slash:

```text
<provider>/<upstream model id>
```

| Provider | Example id |
|----------|------------|
| `claude-code` | `claude-code/claude-opus-5` |
| `codex` | `codex/gpt-5.6-sol` |
| `grok` | `grok/grok-4.5` |
| `antigravity` | `antigravity/gemini-3-flash` |
| a custom endpoint with slug `mygw` | `mygw/<model the endpoint knows>` |
| a local model with slug `my-mac` | `my-mac/<model the local server reports>` |

These ids show the shape only. Which models exist for you depends on the accounts you connected. The live list is on **Models**, or from the API:

```bash
curl https://<your-domain>/openai/v1/models -H "Authorization: Bearer <your-api-key>"
```

One model id works on both bases. You can ask the Anthropic base for a Codex or Gemini model, or the OpenAI base for a Claude model: the proxy converts the request and the response.

An id without a provider prefix is rejected on these bases with `400 invalid_model`.

## Group endpoints

A model group is a private endpoint with model names you choose. Each name points to one or more `provider/model` targets, tried in order. Use one when a tool sends fixed model names, or to fall back from one provider to another. [Groups](/app/groups) explains how to set one up.

| Protocol | Group base URL |
|----------|----------------|
| OpenAI-compatible | `https://<your-domain>/g/<group-slug>/openai/v1` |
| Anthropic Messages | `https://<your-domain>/g/<group-slug>/anthropic` |

A group endpoint accepts only that group's model names. `GET .../models` on it lists exactly those names.

## Responses API

`/responses` is the OpenAI Responses API, the protocol Codex CLI uses. It works with every model id: Codex models pass through unchanged, other providers are converted. Features that need the server to keep state are not available: `previous_response_id`, `conversation`, `background`, and fetching a stored response. Hosted tools such as web search work only on Codex models. See [Codex CLI](/agents/codex-cli).

## Streaming and tools

Streaming (`stream: true`, or the Anthropic event stream) is passed through as it arrives. Tool calls, images, JSON output modes, `stop`, `temperature`, and `top_p` are forwarded where the target provider supports them.

A streamed response has already sent status `200` when it starts. A failure after that arrives as a final error event in the stream. See [Errors](/reference/errors#errors-during-a-stream).

## Reasoning effort

Set it with `reasoning_effort` on `/chat/completions`, `reasoning.effort` on `/responses`, and `output_config.effort` or `reasoning_effort` on the Anthropic base. Accepted values, lowest to highest:

```text
none  low  medium  high  xhigh  max
```

`none` turns reasoning off on `claude-code`, `grok` and `antigravity`. `codex` has no off switch, so the field is left out and the model uses its default. Any other value is rejected with `400 invalid_reasoning`. A value above what the provider accepts is lowered to the provider's highest level instead of failing:

| Provider | Highest effort |
|----------|----------------|
| `claude-code` | `max` |
| `codex` | `xhigh` |
| `grok` | `xhigh` |
| `antigravity` | `high` |
| custom endpoint, OpenAI format | sent as is, never lowered |
| custom endpoint, Anthropic format | not sent from the OpenAI base; on the Anthropic base your `thinking` settings pass through unchanged |

## Audio

- **Transcription.** `POST /openai/v1/audio/transcriptions` works only with custom endpoints in OpenAI format. Other providers answer `400 unsupported_modality`.
- **Audio input in chat.** An `input_audio` part in a `/chat/completions` message reaches `antigravity`, `grok`, and OpenAI-format custom endpoints. `claude-code`, `codex`, and Anthropic-format custom endpoints reject it with `400 unsupported_modality`. The Anthropic base has no audio input.

## Which account answered

A non-streamed response carries two headers naming the provider and model that produced it, even when you called a group:

| Header | Example |
|--------|---------|
| `x-kano-upstream-provider` | `claude-code` |
| `x-kano-upstream-model` | `claude-opus-5` |

Streamed responses do not carry them. **Logs** shows the same information for every request.

## Not available

- Embeddings.
- Image generation.
- Audio output, including text to speech.
