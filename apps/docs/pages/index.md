---
layout: home
title: Kano Proxy
description: Connect the AI subscriptions you already pay for, then point any OpenAI- or Anthropic-compatible client at a single URL.
hero:
  name: Kano Proxy
  text: One endpoint for every coding agent.
  tagline: Connect the AI subscriptions you already pay for, then point any OpenAI- or Anthropic-compatible client at a single URL.
  actions:
    - theme: brand
      text: Get started
      link: /guide/getting-started
    - theme: alt
      text: Set up a coding agent
      link: /agents/claude-code
features:
  - title: Two API shapes, one key
    details: Call every connected provider through an OpenAI-compatible base or an Anthropic Messages base. Same key, same model ids on both.
  - title: Your subscriptions, your pools
    details: Bind Claude Code, Codex, Grok, or Antigravity accounts, or bring your own OpenAI- or Anthropic-compatible endpoint. Accounts fail over automatically.
  - title: Local models included
    details: Run the kano-proxy CLI next to Ollama, LM Studio, or vLLM and reach that machine from anywhere as a normal provider.
---

## The two base URLs

| Protocol | Base URL |
|----------|----------|
| OpenAI-compatible | `https://<your-domain>/openai/v1` |
| Anthropic Messages | `https://<your-domain>/anthropic` |

Authenticate with an API key from the Keys page, as `Authorization: Bearer <your-api-key>` or `x-api-key: <your-api-key>`. Model ids are `provider/model`, for example `claude-code/claude-opus-5`. The full list is on the Models page and at `GET https://<your-domain>/openai/v1/models`.

Next: [Getting started](/guide/getting-started).
