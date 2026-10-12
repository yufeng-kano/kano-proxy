---
title: Errors
description: The error responses Kano Proxy returns on the OpenAI and Anthropic bases, what each one means, and what to do about it.
---

# Errors

An error from the proxy is a JSON body in the shape of the base you called. The `code` on the OpenAI base, or the `type` and message on the Anthropic base, tells you which error it is.

## Error shape

OpenAI base, and OpenAI group endpoints:

```json
{ "error": { "message": "Invalid API key", "type": "invalid_request_error", "code": "invalid_api_key" } }
```

Anthropic base, and Anthropic group endpoints:

```json
{ "type": "error", "error": { "type": "authentication_error", "message": "Invalid API key" } }
```

The Anthropic shape has no `code`. Use the status and the message there.

## Errors and what to do

### 401 `invalid_api_key`

Message `Missing API key` or `Invalid API key`. On the Anthropic base the type is `authentication_error`.

The request had no key, or the key is wrong or revoked. Send it as `Authorization: Bearer <your-api-key>` or `x-api-key: <your-api-key>`. Check on **Access** that the key still exists; create a new one if not.

### 400 `invalid_model`

Message `model must be provider/model (e.g. claude-code/claude-opus-5)`, or on a group endpoint a message naming the group's model list.

The model id has no provider prefix, names a provider you have not connected, or is not one of the group's model names. Copy an id from **Models**, or a model name from the group on **Groups**.

### 404 `not_found`

Message `unknown group endpoint "<slug>"`. On the Anthropic base the type is `not_found_error`.

The group slug in the URL does not exist in your account. Check the slug on **Groups**.

### 400 `no_upstream_account`

Message `No usable <provider> account for this user` on the OpenAI base, `No usable <provider> account` on the Anthropic base.

You have no account for that provider. Connect one on **Providers**, or pick a model from a provider you have.

### 503 `upstream_unavailable`

Message `All upstream accounts unavailable` on the OpenAI base, `upstream_unavailable` on the Anthropic base.

You have accounts for the provider, but none can answer right now: each is paused after an error, or has used up its current usage window. The response carries `Retry-After` with a wait in seconds, at most 60. Most coding agents retry this on their own. If it keeps happening, check the accounts' usage on **Providers**, add another account, or use a group that falls back to a different provider. [How requests are routed](/reference/routing) explains when an account is paused.

### 429 `spend_limit_exceeded`

Message `Spend limit reached for this API key ($<limit> per <interval>)`. On the Anthropic base the type is `rate_limit_error`.

The key has reached the spend limit set on it. Raise or remove the limit in the key's settings on **Access**, or wait until the limit resets.

### 400 `invalid_reasoning`

Message `invalid reasoning_effort`, or `invalid reasoning.effort` on `/responses`.

The reasoning effort is not one of `none`, `low`, `medium`, `high`, `xhigh`, `max`. Values such as `minimal` or `ultra` are rejected. See [Reasoning effort](/reference/endpoints#reasoning-effort).

### 400 `loop_detected`

Message starting `degenerate tool-call loop detected`.

The conversation ends with the same tool call, with the same arguments, at least 8 times in a row. The model is stuck. The proxy stops the request before it reaches the provider so the agent does not keep spending. Interrupt the agent and change the instruction. This check does not apply to Claude Code models or Anthropic-format custom endpoints.

### 400 `unsupported_field`

The `/responses` request uses a feature that needs stored server state or a part the proxy cannot convert: `previous_response_id`, `conversation`, `background`, `item_reference`, `input_file`, or `input_audio`. Send the full conversation in each request instead.

### 400 `unsupported_modality` or `unsupported_audio_format`

The request carries audio the target cannot take: audio sent to a provider without audio input, or a transcription request to a provider other than an OpenAI-format custom endpoint. `unsupported_audio_format` means the audio `format` value is not recognized. See [Audio](/reference/endpoints#audio).

### 413 `request_too_large`

The request body is too large for the Codex upstream. Shorten the conversation, or start a new session. Separately, any body over 512 MiB is refused with a plain-text `413` before the key is checked.

### An upstream error, `upstream_error`

Any other error status from the provider, for example a `400` for a request the model does not accept, is passed back with the provider's status. On the OpenAI base the code is `upstream_error`. The headers `x-kano-upstream-provider` and `x-kano-upstream-model` name the provider and model that refused it. Read the message: it comes from the provider.

### 500 `internal_error`

The proxy itself failed. Retry. If it persists, contact whoever runs this instance.

## Errors during a stream

A streamed response sends status `200` before the proxy knows whether the request will succeed. A failure after that cannot change the status. It arrives as the last event of the stream instead, and the stream ends.

OpenAI base:

```text
data: {"error":{"message":"All upstream accounts unavailable","type":"api_error","code":"upstream_unavailable"}}
```

Anthropic base:

```text
event: error
data: {"type":"error","error":{"type":"api_error","message":"upstream_unavailable"}}
```

`no_upstream_account`, `upstream_unavailable`, `upstream_error`, and `request_too_large` can all arrive this way. One error exists only in a stream:

- **`upstream_stall`.** The provider sent nothing for 120 seconds, so the proxy ended the stream with `upstream stalled: no data received for 120s`. On the Anthropic base the type is `overloaded_error`. Retry the request.

Errors that are known before the stream starts, such as `invalid_api_key`, `invalid_model`, and `spend_limit_exceeded`, still come back with their real status.

## Retry hints

- `Retry-After` is set on `503 upstream_unavailable` when the proxy knows when an account frees up.
- `x-should-retry: false` marks errors where retrying cannot help: `invalid_model`, `not_found`, `no_upstream_account`, `loop_detected`, `unsupported_field`, `unsupported_modality`, `request_too_large`, and `spend_limit_exceeded`.
- `x-should-retry: true` marks `503 upstream_unavailable`.

Claude Code reads `x-should-retry`. Other clients may ignore it.

## Finding a failed request

A request with a valid key that fails appears on **Logs** with its status, model, and key. Requests rejected for a missing or invalid key never appear there.
