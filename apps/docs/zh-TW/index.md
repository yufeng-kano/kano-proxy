---
layout: home
title: Kano Proxy
description: 接上你已付費的 AI 訂閱，讓任何 OpenAI 或 Anthropic 相容的客戶端都指向同一個網址。
hero:
  name: Kano Proxy
  text: 所有 coding agent 共用一個端點。
  tagline: 接上你已付費的 AI 訂閱，讓任何 OpenAI 或 Anthropic 相容的客戶端都指向同一個網址。
  actions:
    - theme: brand
      text: 開始使用
      link: /zh-TW/guide/getting-started
    - theme: alt
      text: 設定 coding agent
      link: /zh-TW/agents/claude-code
features:
  - title: 兩種 API 格式，一把金鑰
    details: 用 OpenAI 相容端點或 Anthropic Messages 端點呼叫所有已連接的供應商。同一把金鑰，同一組模型 id。
  - title: 你的訂閱，你的帳號池
    details: 綁定 Claude Code、Codex、Grok、Antigravity 帳號，或接上自己的 OpenAI 或 Anthropic 相容端點。帳號額度用完會自動切換。
  - title: 本機模型也能用
    details: 在 Ollama、LM Studio、vLLM 旁邊跑 kano-proxy CLI，那台機器就成為可以從任何地方呼叫的供應商。
---

## 兩個 base URL

| 協定 | Base URL |
|------|----------|
| OpenAI 相容 | `https://<your-domain>/openai/v1` |
| Anthropic Messages | `https://<your-domain>/anthropic` |

驗證用 Keys 頁面產生的 API key，放在 `Authorization: Bearer <your-api-key>` 或 `x-api-key: <your-api-key>` 都可以。模型 id 是 `provider/model` 格式，例如 `claude-code/claude-opus-5`。完整清單在 Models 頁面，或呼叫 `GET https://<your-domain>/openai/v1/models`。

下一步：[開始使用](/zh-TW/guide/getting-started)。
