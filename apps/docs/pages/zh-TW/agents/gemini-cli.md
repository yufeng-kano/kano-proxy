---
title: Gemini CLI
description: Gemini CLI 無法使用 Kano Proxy。它只講 Google 的 Gemini API，proxy 沒有提供這種格式。
---

# Gemini CLI

**無法使用。** Gemini CLI 只會用 Google 的 Gemini API，而 Kano Proxy 沒有提供這種 API。

不論用 CLI 或手動，都沒有可以設定的地方。

## 改用其他方式使用 Gemini 模型

1. 在 [Providers](/zh-TW/app/providers) 頁面連接 **Antigravity**。
2. 從清單上的其他工具呼叫 `antigravity/gemini-3-flash` 或 **Models** 頁面上其他 Antigravity 的 id，例如用 [Claude Code](/zh-TW/agents/claude-code) 或 [OpenCode](/zh-TW/agents/opencode)。這個 id 只是範例。

## 限制

- Gemini CLI 沒有 OpenAI 或 Anthropic 相容的供應商。
- 它僅有的端點覆寫 `GOOGLE_GEMINI_BASE_URL` 和 `GOOGLE_VERTEX_BASE_URL`，送出的仍是 Gemini 格式的請求（`generateContent` 與 `streamGenerateContent`）和 Gemini 的模型名稱。

::: details 什麼情況會改變
proxy 必須在 `/v1beta/models/<model>:streamGenerateContent` 提供 Gemini 格式的 API，目前沒有這個計畫。Gemini CLI 的 gateway 路徑本身也有尚未修正的 bug，就算做了，搭配起來也不穩。
:::

## 查證來源

Gemini CLI 文件與原始碼，2026-09-04：[Configuration reference](https://geminicli.com/docs/reference/configuration/)，以及 [gemini-cli 儲存庫](https://github.com/google-gemini/gemini-cli)的驗證指南與 `contentGenerator.ts`。
