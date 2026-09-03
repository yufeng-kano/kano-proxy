---
title: Gemini CLI
description: Gemini CLI 無法使用 Kano Proxy。它只講 Google 的 Gemini API，proxy 沒有提供這種格式。
---

# Gemini CLI

**無法使用。** Gemini CLI 沒有 OpenAI 或 Anthropic 相容的供應商。它僅有的端點覆寫 `GOOGLE_GEMINI_BASE_URL` 與 `GOOGLE_VERTEX_BASE_URL`，送出的仍是 Gemini 格式的請求（`generateContent` 與 `streamGenerateContent`）和 Gemini 模型名稱。Kano Proxy 不提供這種 API。

想透過 proxy 用 Gemini 模型，請在 Providers 頁面連接 **Antigravity**，然後從清單上其他工具呼叫 `antigravity/gemini-3-flash` 等模型，例如 [Claude Code](/zh-TW/agents/claude-code) 或 [OpenCode](/zh-TW/agents/opencode)。

## 什麼情況會改變

proxy 必須在 `/v1beta/models/<model>:streamGenerateContent` 提供 Gemini 格式的 API。目前沒有這個計畫。Gemini CLI 的 gateway 路徑本身也有未解的 bug，就算做了，搭配起來也不穩。

## 查證來源

Gemini CLI 文件與原始碼，2026-09-04：[Configuration reference](https://geminicli.com/docs/reference/configuration/)，以及 [gemini-cli 儲存庫](https://github.com/google-gemini/gemini-cli)的驗證指南與 `contentGenerator.ts`。
