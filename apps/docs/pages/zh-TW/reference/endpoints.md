---
title: 端點與模型 id
description: Kano Proxy 的 base URL、客戶端接上的路徑、驗證標頭、provider/model id、列出模型、串流、推理強度，以及不支援的功能。
---

# 端點與模型 id

每個請求都送到兩個 base URL 之一，帶上 Kano Proxy 金鑰，模型寫成 `provider/model`。

## Base URL

| 協定 | Base URL | 客戶端自己會接上 |
|------|----------|------------------|
| OpenAI 相容 | `https://<your-domain>/openai/v1` | `/chat/completions`、`/responses`、`/models`、`/audio/transcriptions` |
| Anthropic Messages | `https://<your-domain>/anthropic` | `/v1/messages`、`/v1/messages/count_tokens`、`/v1/models` |

有些 Anthropic SDK 和工具接的是 `/messages`，不是 `/v1/messages`。這類工具要把 base 設成 `https://<your-domain>/anthropic/v1`。各 agent 頁面都會寫明該工具要哪一種。

**Access** 的 **Connect** 分頁會列出你這個站的網址，可以直接複製。

## 驗證

把 **Access** 建立的金鑰放進下面任一個標頭。兩個標頭在兩個 base 上都能用。

```http
Authorization: Bearer <your-api-key>
```

```http
x-api-key: <your-api-key>
```

金鑰以 `sk-kano-proxy-` 開頭。你的供應商憑證只留在 proxy 上，不會交給任何客戶端。金鑰隨時可以在 **Access** 撤銷，撤銷後立刻失效。

## 模型 id

模型 id 以第一個斜線分成兩段：

```text
<provider>/<上游模型 id>
```

| 供應商 | 範例 id |
|--------|---------|
| `claude-code` | `claude-code/claude-opus-5` |
| `codex` | `codex/gpt-5.6-sol` |
| `grok` | `grok/grok-4.5` |
| `antigravity` | `antigravity/gemini-3-flash` |
| slug 為 `mygw` 的自訂端點 | `mygw/<該端點認得的模型>` |
| slug 為 `my-mac` 的本機模型 | `my-mac/<本機伺服器回報的模型>` |

上表只是示範格式。你實際有哪些模型，要看你連接了哪些帳號。最新清單在 **Models**，也可以用 API 查：

```bash
curl https://<your-domain>/openai/v1/models -H "Authorization: Bearer <your-api-key>"
```

同一個模型 id 在兩個 base 都能用。在 Anthropic base 呼叫 Codex 或 Gemini 模型、在 OpenAI base 呼叫 Claude 模型都沒問題，proxy 會轉換請求和回應。

沒有供應商前綴的 id 在這兩個 base 上會被拒絕，回 `400 invalid_model`。

## Group 端點

Model group 是一個專屬端點，模型名稱由你自己取。每個名稱對應一個或多個 `provider/model` 目標，依序嘗試。工具會送固定的模型名稱時，或你想從一個供應商備援到另一個時，就用 group。設定方法見 [Groups](/zh-TW/app/groups)。

| 協定 | Group base URL |
|------|----------------|
| OpenAI 相容 | `https://<your-domain>/g/<group-slug>/openai/v1` |
| Anthropic Messages | `https://<your-domain>/g/<group-slug>/anthropic` |

Group 端點只接受該 group 的模型名稱，在上面呼叫 `GET .../models` 也只會列出這些名稱。

## Responses API

`/responses` 是 OpenAI Responses API，也就是 Codex CLI 使用的協定。所有模型 id 都能用：Codex 模型原樣轉送，其他供應商由 proxy 轉換。需要伺服器保存狀態的功能不支援，包括 `previous_response_id`、`conversation`、`background`，以及讀取已儲存的回應。網頁搜尋這類託管工具只有 Codex 模型能用。詳見 [Codex CLI](/zh-TW/agents/codex-cli)。

## 串流與工具

串流（`stream: true`，或 Anthropic 的事件串流）收到多少就轉多少。Tool call、圖片、JSON 輸出模式、`stop`、`temperature`、`top_p`，只要目標供應商支援就會轉送過去。

串流回應一開始就已經送出狀態碼 `200`。之後才發生的錯誤，會以串流最後一個錯誤事件出現。見[錯誤訊息](/zh-TW/reference/errors#串流中的錯誤)。

## 推理強度

`/chat/completions` 用 `reasoning_effort`，`/responses` 用 `reasoning.effort`，Anthropic base 用 `output_config.effort` 或 `reasoning_effort`。可用的值由低到高：

```text
none  low  medium  high  xhigh  max
```

`none` 會關閉 `claude-code`、`grok` 和 `antigravity` 的推理。`codex` 沒有關閉選項，所以不送這個欄位，由模型用預設值。其他值一律回 `400 invalid_reasoning`。超過供應商上限的值會降到該供應商的最高等級，不會失敗：

| 供應商 | 最高等級 |
|--------|----------|
| `claude-code` | `max` |
| `codex` | `xhigh` |
| `grok` | `xhigh` |
| `antigravity` | `high` |
| 自訂端點，OpenAI 格式 | 原樣送出，不會調降 |
| 自訂端點，Anthropic 格式 | 從 OpenAI base 呼叫時不送；從 Anthropic base 呼叫時，你的 `thinking` 設定原樣轉送 |

## 音訊

- **語音轉文字。** `POST /openai/v1/audio/transcriptions` 只支援 OpenAI 格式的自訂端點，其他供應商回 `400 unsupported_modality`。
- **對話中的音訊輸入。** `/chat/completions` 訊息裡的 `input_audio` 會送到 `antigravity`、`grok` 和 OpenAI 格式的自訂端點。`claude-code`、`codex` 和 Anthropic 格式的自訂端點會回 `400 unsupported_modality`。Anthropic base 不接受音訊輸入。

## 是哪個帳號回應的

非串流回應會帶兩個標頭，寫明實際回應的供應商和模型，透過 group 呼叫時也一樣：

| 標頭 | 範例 |
|------|------|
| `x-kano-upstream-provider` | `claude-code` |
| `x-kano-upstream-model` | `claude-opus-5` |

串流回應不會帶這兩個標頭。**Logs** 會為每個請求列出同樣的資訊。

## 不支援

- Embeddings。
- 圖片生成。
- 音訊輸出，包括文字轉語音。
