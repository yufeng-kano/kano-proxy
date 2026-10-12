---
title: Cline
description: Configure Cline's OpenAI Compatible provider for Kano Proxy, and why the Anthropic provider is the wrong choice here.
---

# Cline

Works with the **OpenAI Compatible** provider, for every model including Claude.

The kano-proxy CLI does not set up Cline. Set it up by hand.

## Set it up by hand

1. Create a key on the **Access** page.
2. In Cline's settings, pick **API Provider: OpenAI Compatible**.
3. Fill in the fields:

| Field | Value |
|-------|-------|
| Base URL | `https://<your-domain>/openai/v1` |
| API Key | `<your-api-key>` |
| Model ID | an id from your **Models** page, for example `claude-code/claude-opus-5` |

Include the `/v1`. Cline passes the base URL through as is and appends `/chat/completions`.

Cline reads `GET /openai/v1/models` from the proxy, so your ids show up in its model dropdown. If one is missing, choose **Use custom model ID** and type it. Model id rules are in [Endpoints and model ids](/reference/endpoints).

::: details Why not the Anthropic provider
Cline's Anthropic provider has a **Use custom base URL** option. It sends requests to `<base>/messages`, so the value would have to be `https://<your-domain>/anthropic/v1`. But its model picker only offers Cline's built-in Claude names, with no way to enter `claude-code/claude-opus-5`, and the proxy rejects bare names. Stay on OpenAI Compatible.
:::

## Check it

Send a message in Cline. The request appears on the [Logs](/app/logs) page within seconds.

## Limits

- The Anthropic provider cannot be used, for the reason above.

## Checked against

Cline documentation and source, 2026-09-04: [OpenAI Compatible](https://docs.cline.bot/provider-config/openai-compatible), [Anthropic](https://docs.cline.bot/provider-config/anthropic), and the provider code in the [cline repository](https://github.com/cline/cline). The `/v1` requirement and the missing custom model field are verified from source, not from the docs.
