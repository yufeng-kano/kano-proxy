---
title: Codex CLI
description: Codex CLI 目前無法使用 Kano Proxy，因為它只接受 OpenAI Responses API。Codex 作為供應商可以用，作為客戶端不行。
---

# Codex CLI

**目前無法使用。** Codex CLI 只講 OpenAI Responses API，會送到 `<base_url>/responses`。Kano Proxy 提供的是 Chat Completions 與 Anthropic Messages，沒有 Responses，所以 Codex CLI 的請求無處可去。

同名的兩件事要分開看：

- **Codex 作為供應商**可以用。在 Providers 頁面連接你的 ChatGPT 訂閱，就能從這份清單上的任何工具呼叫 `codex/gpt-5.4` 等 Codex 模型。
- **Codex CLI 作為客戶端**不行。把它指向 proxy，第一個請求就會失敗。

## 原因

2026 年初以前，Codex CLI 的供應商設定接受 `wire_api = "chat"`，會送 Chat Completions 請求。這個選項已被移除。目前的設定文件寫明 `responses` 是唯一支援的值，設成 `chat` 時 CLI 會拒絕啟動：

```text
`wire_api = "chat"` is no longer supported.
How to fix: set `wire_api = "responses"` in your provider config.
```

## 什麼情況會改變

proxy 需要一個能轉換 Responses 請求與串流的 `POST /openai/v1/responses` 端點。這還沒做。若日後實作，這一頁會放上設定：

```toml
# ~/.codex/config.toml，僅供參考，目前不能用。
model = "codex/gpt-5.4"
model_provider = "kano"

[model_providers.kano]
name = "Kano Proxy"
base_url = "https://<your-domain>/openai/v1"
env_key = "KANO_PROXY_API_KEY"
wire_api = "responses"
```

## 查證來源

Codex 文件與原始碼，2026-09-04：[Config reference](https://learn.chatgpt.com/docs/config-file/config-reference)、[chat wire API 棄用公告](https://github.com/openai/codex/discussions/7782)。
