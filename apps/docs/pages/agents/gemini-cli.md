---
title: Gemini CLI
description: Gemini CLI cannot use Kano Proxy. It speaks only Google's Gemini API, which the proxy does not expose.
---

# Gemini CLI

**Not usable:** Gemini CLI speaks only Google's Gemini API, which Kano Proxy does not serve.

There is nothing to set up, with the CLI or by hand.

## Use Gemini models another way

1. Connect **Antigravity** on the [Providers](/app/providers) page.
2. Call `antigravity/gemini-3-flash` or another Antigravity id from your **Models** page, from any other tool on this list, for example [Claude Code](/agents/claude-code) or [OpenCode](/agents/opencode). The id is an example.

## Limits

- Gemini CLI has no OpenAI- or Anthropic-compatible provider.
- Its only endpoint overrides, `GOOGLE_GEMINI_BASE_URL` and `GOOGLE_VERTEX_BASE_URL`, still send Gemini-format requests (`generateContent` and `streamGenerateContent`) with Gemini model names.

::: details What would change this
The proxy would have to expose a Gemini-format API under `/v1beta/models/<model>:streamGenerateContent`. That is not planned. Gemini CLI's gateway path also has open bugs of its own, so even then the pairing would be fragile.
:::

## Checked against

Gemini CLI documentation and source, 2026-09-04: [Configuration reference](https://geminicli.com/docs/reference/configuration/), the authentication guide and `contentGenerator.ts` in the [gemini-cli repository](https://github.com/google-gemini/gemini-cli).
