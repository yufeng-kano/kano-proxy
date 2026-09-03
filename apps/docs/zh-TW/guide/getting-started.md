---
title: 開始使用
description: 登入、連接供應商、建立 API key，然後透過 Kano Proxy 送出第一個請求。
---

# 開始使用

四個步驟：登入、連接供應商、建立金鑰、送出請求。幾分鐘就能完成。

## 1. 登入

打開 `https://<your-domain>/login`，用 Google 帳號登入。沒有密碼。每個帳號彼此獨立，你的供應商、金鑰與用量別人都看不到。

## 2. 連接供應商

進入 **Providers**，至少連接一個：

- **Claude Code、Codex、Grok、Antigravity。** 登入你已付費的訂閱。頁面會帶你走完該供應商自己的 OAuth 流程。同一供應商可以綁多個帳號，一個帳號額度用完，請求會自動換到下一個。
- **自訂端點。** 任何你有金鑰的 OpenAI 或 Anthropic 相容 API。填入一個短 slug、base URL、金鑰與格式，之後就跟內建供應商一樣使用。
- **本機模型。** 跑在自己電腦上的模型伺服器，透過 CLI 接進來。見[本機模型](/zh-TW/guide/local-models)。

## 3. 建立 API key

進入 **Keys** 建立一把。金鑰以 `sk-kano-proxy-` 開頭，只會顯示一次，出現時就複製下來。可以建多把，一個工具一把，之後隨時撤銷。Keys 頁面也會顯示你這個站的正確 base URL。

## 4. 送出請求

從 **Models** 挑一個模型 id。格式是 `provider/model`。下面的範例用了其中一個，請換成你清單裡的 id。

```bash
curl https://<your-domain>/openai/v1/chat/completions \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-code/claude-opus-5",
    "messages": [{"role": "user", "content": "用五個字打招呼。"}],
    "max_tokens": 32
  }'
```

同一個模型在 Anthropic 端點也能用：

```bash
curl https://<your-domain>/anthropic/v1/messages \
  -H "x-api-key: <your-api-key>" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-code/claude-opus-5",
    "max_tokens": 32,
    "messages": [{"role": "user", "content": "用五個字打招呼。"}]
  }'
```

## 接下來

- [端點與模型 id](/zh-TW/guide/endpoints)：完整的網址與命名規則。
- **Coding agent 設定**底下每個工具一頁，從 [Claude Code](/zh-TW/agents/claude-code) 開始。
- 應用程式裡的 **Overview** 與 **Logs** 會顯示每把金鑰送了什麼、哪個供應商回應、估算花費多少。
