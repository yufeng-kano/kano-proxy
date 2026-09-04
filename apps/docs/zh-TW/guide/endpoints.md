---
title: 端點與模型 id
description: Kano Proxy 的 base URL、驗證標頭、provider/model id、模型群組，以及如何列出模型。
---

# 端點與模型 id

## Base URL

| 協定 | Base URL | 客戶端自己會接上 |
|------|----------|------------------|
| OpenAI 相容 | `https://<your-domain>/openai/v1` | `/chat/completions`、`/responses`、`/models`、`/audio/transcriptions` |
| Anthropic Messages | `https://<your-domain>/anthropic` | `/v1/messages`、`/v1/models` |

有些 Anthropic SDK 與工具會直接接 `/messages` 而不是 `/v1/messages`。這類工具要把 base 設成 `https://<your-domain>/anthropic/v1`。各 agent 頁面都會註明該工具要哪一種。

`/responses` 是 OpenAI Responses API，也就是 Codex CLI 使用的協定。所有模型 id 都能用：Codex 模型原樣透傳，其他供應商由 proxy 轉換。有狀態的功能（`previous_response_id`、`conversation`、讀取已儲存的回應）不支援；網頁搜尋等託管工具只有 Codex 模型能用。詳見 [Codex CLI](/zh-TW/agents/codex-cli) 頁面。

不支援：embeddings、圖片生成、音訊輸出。

## 驗證

把 Keys 頁面的金鑰放進以下任一標頭，兩個端點都接受。

```http
Authorization: Bearer <your-api-key>
```

```http
x-api-key: <your-api-key>
```

你的供應商憑證永遠不會離開 proxy。客戶端手上只有一把 `sk-kano-proxy-` 金鑰，隨時可以撤銷。

## 模型 id

每個模型 id 由第一個斜線切成兩段：

```text
<provider>/<上游模型 id>
```

| 供應商 | 範例 id |
|--------|---------|
| `claude-code` | `claude-code/claude-opus-5` |
| `codex` | `codex/gpt-5.4` |
| `grok` | `grok/grok-4.5` |
| `antigravity` | `antigravity/gemini-3-flash` |
| slug 為 `mygw` 的自訂端點 | `mygw/<該端點認得的模型>` |
| slug 為 `my-mac` 的本機模型 | `my-mac/<Ollama 回報的模型>` |

上面的 id 只是格式範例。你實際有哪些模型，取決於你連接了哪些帳號。請以應用程式的 **Models** 頁面或 API 的即時清單為準：

```bash
curl https://<your-domain>/openai/v1/models -H "Authorization: Bearer <your-api-key>"
```

同一個模型 id 在兩個端點都能用。在 Anthropic 端點呼叫 Codex 或 Gemini 模型也沒問題，proxy 會轉換請求與串流。

在共用端點上，沒有供應商前綴的 id 會被拒絕。工具無法送出帶前綴的名稱時，請用模型群組。

## 模型群組

模型群組是一個私有端點，有自己的一組模型名稱。每個名稱對應一串有順序的 `provider/model` 目標。兩種用途：

- **工具寫死了模型名稱。** 建一個群組，把模型名稱取成工具送出的字串，再把工具指向群組的 base URL。
- **跨供應商或跨帳號的備援。** 列出多個目標，第一個可用的回應。它的帳號被暫停或額度用完時，換下一個。

群組的 base URL 取代共用的：

| 協定 | 群組 base URL |
|------|---------------|
| OpenAI 相容 | `https://<your-domain>/g/<group-slug>/openai/v1` |
| Anthropic Messages | `https://<your-domain>/g/<group-slug>/anthropic` |

群組端點上只有該群組定義的模型名稱有效。對群組呼叫 `GET .../models` 會列出正好那些名稱。群組在 **Groups** 頁面建立與編輯。

## 串流與工具呼叫

串流（`stream: true` 或 Anthropic 的 SSE）逐塊直接轉送。工具呼叫、圖片、JSON 輸出模式、`reasoning_effort`、`stop`、`temperature`、`top_p` 會在目標供應商支援的範圍內轉送。
