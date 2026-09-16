---
title: Cline
description: Configure Cline's OpenAI Compatible provider for Kano Proxy, and why the Anthropic provider is the wrong choice here.
---

# Cline

Works with the **OpenAI Compatible** provider. Use that one even for Claude models: Cline's Anthropic provider has a fixed model list and cannot take a `provider/model` id.

## Settings

In Cline's settings, pick **API Provider: OpenAI Compatible** and fill in:

| Field | Value |
|-------|-------|
| Base URL | `https://<your-domain>/openai/v1` |
| API Key | `<your-api-key>` |
| Model ID | an id from your Models page, for example `claude-code/claude-opus-5` |

Include the `/v1`. Cline passes the base URL through as is and appends `/chat/completions`.

Cline also reads `GET /openai/v1/models` from the proxy, so your ids show up in its model dropdown. If one is missing, choose **Use custom model ID** and type it.

## The Anthropic provider

Cline's Anthropic provider has a **Use custom base URL** option. It sends requests to `<base>/messages`, so the value would have to be `https://<your-domain>/anthropic/v1`. But its model picker only offers Cline's built-in Claude names, with no way to enter `claude-code/claude-opus-5`, and the proxy rejects bare names. Stay on OpenAI Compatible.

## Checked against

Cline documentation and source, 2026-09-04: [OpenAI Compatible](https://docs.cline.bot/provider-config/openai-compatible), [Anthropic](https://docs.cline.bot/provider-config/anthropic), and the provider code in the [cline repository](https://github.com/cline/cline). The `/v1` requirement and the missing custom model field are verified from source, not from the docs.
