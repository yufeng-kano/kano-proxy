---
title: 錯誤訊息
description: Kano Proxy 在 OpenAI 與 Anthropic 端點回傳的錯誤、各自的意思，以及該怎麼處理。
---

# 錯誤訊息

proxy 回傳的錯誤是 JSON，格式跟著你呼叫的 base 走。看 OpenAI base 的 `code`，或 Anthropic base 的 `type` 和訊息，就知道是哪一種錯誤。

## 錯誤格式

OpenAI base 與 OpenAI 格式的 group 端點：

```json
{ "error": { "message": "Invalid API key", "type": "invalid_request_error", "code": "invalid_api_key" } }
```

Anthropic base 與 Anthropic 格式的 group 端點：

```json
{ "type": "error", "error": { "type": "authentication_error", "message": "Invalid API key" } }
```

Anthropic 格式沒有 `code`，請看狀態碼和訊息判斷。

## 各種錯誤與處理方式

### 401 `invalid_api_key`

訊息是 `Missing API key` 或 `Invalid API key`。Anthropic base 的 type 是 `authentication_error`。

請求沒帶金鑰，或金鑰錯誤、已撤銷。請用 `Authorization: Bearer <your-api-key>` 或 `x-api-key: <your-api-key>` 送出。到 **Access** 確認這把金鑰還在，不在就建一把新的。

### 400 `invalid_model`

訊息是 `model must be provider/model (e.g. claude-code/claude-opus-5)`；在 group 端點上，訊息會指向該 group 的模型清單。

模型 id 少了供應商前綴、指到你沒連接的供應商，或不是該 group 的模型名稱。從 **Models** 複製 id，或到 **Groups** 複製該 group 的模型名稱。

### 404 `not_found`

訊息是 `unknown group endpoint "<slug>"`。Anthropic base 的 type 是 `not_found_error`。

網址裡的 group slug 不在你的帳號裡。到 **Groups** 確認 slug。

### 400 `no_upstream_account`

OpenAI base 的訊息是 `No usable <provider> account for this user`，Anthropic base 是 `No usable <provider> account`。

你沒有這個供應商的帳號。到 **Providers** 連接一個，或改用你已有帳號的供應商的模型。

### 503 `upstream_unavailable`

OpenAI base 的訊息是 `All upstream accounts unavailable`，Anthropic base 是 `upstream_unavailable`。

你有這個供應商的帳號，但現在沒有一個能回應：每個帳號不是出錯後暫停中，就是目前的用量額度已經用完。回應會帶 `Retry-After`，告訴你要等幾秒，最多 60 秒。大部分 coding agent 會自己重試。如果一直發生，到 **Providers** 看各帳號的用量、多加一個帳號，或改用會備援到其他供應商的 group。帳號什麼時候會被暫停，見[請求如何分配](/zh-TW/reference/routing)。

### 429 `spend_limit_exceeded`

訊息是 `Spend limit reached for this API key ($<limit> per <interval>)`。Anthropic base 的 type 是 `rate_limit_error`。

這把金鑰已經達到設定的花費上限。到 **Access** 的金鑰設定調高或拿掉上限，或等上限重設。

### 400 `invalid_reasoning`

訊息是 `invalid reasoning_effort`，在 `/responses` 上是 `invalid reasoning.effort`。

推理強度不是 `none`、`low`、`medium`、`high`、`xhigh`、`max` 其中之一。`minimal`、`ultra` 這類值會被拒絕。見[推理強度](/zh-TW/reference/endpoints#推理強度)。

### 400 `loop_detected`

訊息以 `degenerate tool-call loop detected` 開頭。

對話結尾連續出現至少 8 次同一個 tool call，參數也完全相同，代表模型卡住了。proxy 會在請求送到供應商之前擋下，避免 agent 一直花錢。請中斷 agent，換個指示再試。Claude Code 模型和 Anthropic 格式的自訂端點不做這項檢查。

### 400 `unsupported_field`

`/responses` 請求用了需要伺服器保存狀態、或 proxy 無法轉換的內容：`previous_response_id`、`conversation`、`background`、`item_reference`、`input_file`、`input_audio`。請在每個請求裡帶上完整對話。

### 400 `unsupported_modality` 或 `unsupported_audio_format`

請求裡的音訊是目標不能處理的：送音訊給不支援音訊輸入的供應商，或把語音轉文字請求送到 OpenAI 格式自訂端點以外的供應商。`unsupported_audio_format` 表示音訊的 `format` 值無法辨識。見[音訊](/zh-TW/reference/endpoints#音訊)。

### 413 `request_too_large`

請求內容超過 Codex 上游能接受的大小。請縮短對話，或開新的 session。另外，任何超過 512 MiB 的請求都會在檢查金鑰之前就被拒絕，回一個純文字的 `413`。

### 上游錯誤 `upstream_error`

供應商回的其他錯誤狀態，例如模型不接受某個請求而回 `400`，會連同供應商的狀態碼一起傳回。OpenAI base 的 code 是 `upstream_error`。標頭 `x-kano-upstream-provider` 與 `x-kano-upstream-model` 會寫出是哪個供應商和模型拒絕的。訊息內容來自供應商，請直接看訊息。

### 500 `internal_error`

proxy 本身出錯。請重試；如果持續發生，請聯絡這個站的管理者。

## 串流中的錯誤

串流回應在 proxy 知道請求會不會成功之前，就已經送出狀態碼 `200`。之後才發生的錯誤無法再改狀態碼，而是以串流的最後一個事件出現，然後串流結束。

OpenAI base：

```text
data: {"error":{"message":"All upstream accounts unavailable","type":"api_error","code":"upstream_unavailable"}}
```

Anthropic base：

```text
event: error
data: {"type":"error","error":{"type":"api_error","message":"upstream_unavailable"}}
```

`no_upstream_account`、`upstream_unavailable`、`upstream_error`、`request_too_large` 都可能以這種方式出現。有一種錯誤只會在串流中出現：

- **`upstream_stall`。** 供應商 120 秒都沒送任何資料，proxy 會以 `upstream stalled: no data received for 120s` 結束串流。Anthropic base 的 type 是 `overloaded_error`。請重試。

串流開始前就能確定的錯誤，例如 `invalid_api_key`、`invalid_model`、`spend_limit_exceeded`，仍然會回真正的狀態碼。

## 重試提示

- `503 upstream_unavailable` 在 proxy 知道帳號何時恢復時，會帶 `Retry-After`。
- `x-should-retry: false` 標示重試也沒用的錯誤：`invalid_model`、`not_found`、`no_upstream_account`、`loop_detected`、`unsupported_field`、`unsupported_modality`、`request_too_large`、`spend_limit_exceeded`。
- `x-should-retry: true` 標示 `503 upstream_unavailable`。

Claude Code 會讀 `x-should-retry`，其他客戶端不一定會。

## 找出失敗的請求

金鑰有效但失敗的請求，會出現在 **Logs**，附上狀態碼、模型和金鑰。因為沒帶金鑰或金鑰無效而被拒絕的請求，不會出現在那裡。
