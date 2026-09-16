---
title: Getting started
description: Sign in, connect a provider, create an API key, and send a first request through Kano Proxy.
---

# Getting started

Four steps: sign in, connect a provider, create a key, send a request. The whole thing takes a few minutes.

## 1. Sign in

Open `https://<your-domain>/login` and continue with Google. There is no password. Every account is separate: your providers, keys, and usage are never visible to anyone else.

## 2. Connect a provider

Go to **Providers** and connect at least one:

- **Claude Code, Codex, Grok, Antigravity.** Sign in to the subscription you already pay for. The page walks you through the provider's own OAuth flow. You can bind more than one account per provider. When one account hits its limit, requests move to the next.
- **Custom endpoint.** Any OpenAI- or Anthropic-compatible API you have a key for. Give it a short slug, the base URL, the key, and the format. It then behaves like a built-in provider.
- **Local model.** A model server on your own machine, exposed through the CLI. See [Local models](/guide/local-models).

## 3. Create an API key

Go to **Keys** and create one. The key starts with `sk-kano-proxy-` and is shown once. Copy it when it appears. You can create several keys, one per tool, and revoke any of them later. The Keys page also shows the exact base URLs for your instance.

## 4. Send a request

Pick a model id from **Models**. Ids are `provider/model`. The example below uses one; replace it with an id from your own list.

```bash
curl https://<your-domain>/openai/v1/chat/completions \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-code/claude-opus-5",
    "messages": [{"role": "user", "content": "Say hello in five words."}],
    "max_tokens": 32
  }'
```

The same model works on the Anthropic base:

```bash
curl https://<your-domain>/anthropic/v1/messages \
  -H "x-api-key: <your-api-key>" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-code/claude-opus-5",
    "max_tokens": 32,
    "messages": [{"role": "user", "content": "Say hello in five words."}]
  }'
```

## Next

- [Endpoints and model ids](/guide/endpoints) for the full URL and naming rules.
- One page per tool under **Coding agents**, starting with [Claude Code](/agents/claude-code).
- **Overview** and **Logs** in the app show what each key sent, which provider answered, and the estimated cost.
