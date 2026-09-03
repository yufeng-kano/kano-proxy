---
title: Gemini CLI
description: Gemini CLI cannot use Kano Proxy. It speaks only Google's Gemini API, which the proxy does not expose.
---

# Gemini CLI

**Not usable.** Gemini CLI has no OpenAI- or Anthropic-compatible provider. Its only endpoint overrides, `GOOGLE_GEMINI_BASE_URL` and `GOOGLE_VERTEX_BASE_URL`, still send Gemini-format requests (`generateContent` and `streamGenerateContent`) with Gemini model names. Kano Proxy does not serve that API.

If you want Gemini models through the proxy, connect **Antigravity** on the Providers page and call `antigravity/gemini-3-flash` and its siblings from any of the other tools on this list, for example [Claude Code](/agents/claude-code) or [OpenCode](/agents/opencode).

## What would change this

The proxy would have to expose a Gemini-format API under `/v1beta/models/<model>:streamGenerateContent`. That is not planned. Gemini CLI's gateway path also has open bugs of its own, so even then the pairing would be fragile.

## Checked against

Gemini CLI documentation and source, 2026-09-04: [Configuration reference](https://geminicli.com/docs/reference/configuration/), the authentication guide and `contentGenerator.ts` in the [gemini-cli repository](https://github.com/google-gemini/gemini-cli).
