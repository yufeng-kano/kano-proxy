---
title: Models
description: Kano Proxy 的 Models 頁面：依供應商分組，列出你現在可以呼叫的每個 provider/model id，以及怎麼搜尋和複製。
---

# Models

Models 列出你現在可以呼叫的每一個模型 id，依來源供應商分組。在這裡複製 id，貼到客戶端的 `model` 欄位。

這份清單是即時的，來自你在 [Providers](/zh-TW/app/providers) 接上的帳號和端點。接上或移除一個，清單就會跟著變。

## 頁面上有什麼

### 頁首

- **Search models**：輸入時即時篩選所有分組，比對模型 id 和名稱。
- 重新整理按鈕：重新向每個供應商取得清單。
- 標題下方的分頁：只看某一個供應商。**All models** 顯示全部。

### 每個供應商一張卡

卡片的順序是：

1. 內建供應商：Claude Code、Codex、Grok、Antigravity。
2. 你新增的每個自訂端點。
3. 透過 [CLI](/zh-TW/cli/local-models) 連上的每個本機模型伺服器。

卡片標題是供應商名稱。自訂端點和本機模型另外會標出格式（OpenAI 或 Anthropic），以及 id 的前綴，例如 `mygw/*`。右側數字是這張卡列出幾個模型。

每一列左邊是模型 id，右邊是名稱。點整列或點複製按鈕都能複製 id。

id 的格式一律是 `provider/model`，例如 `claude-code/claude-opus-5`（僅為範例，請以你自己的清單為準）。同一個 id 在 OpenAI 和 Anthropic 兩種 base URL 上都能用。格式說明請見 [端點與模型 id](/zh-TW/reference/endpoints)。

Model group 不會出現在這裡。Group 有自己的端點和自己的模型名稱，列在 [Groups](/zh-TW/app/groups)。

### 空白與錯誤

卡片沒有內容可列時，會改顯示一段說明：

- **Connect an account to load these models.**：這個供應商還沒有可用的帳號。
- **This provider doesn't publish a model list.**：Codex 沒有公開的模型清單。請使用你的 ChatGPT 方案提供的模型 id，前面加上 `codex/`。
- **Add model ids to this endpoint, or let it load them automatically.**：這個自訂端點沒有任何模型，請到 Providers 編輯它。

某個供應商的清單載入失敗時，錯誤訊息會顯示在那張卡片裡，不影響其他卡片。

整頁顯示 **No models yet**，代表什麼都還沒接上。按 **Connect a provider** 前往 Providers。

## 常見操作

### 複製模型 id 給客戶端

1. 在 **Search models** 輸入部分名稱，例如 `opus`。
2. 點那一列，複製按鈕會變成勾勾。
3. 把 id 貼到客戶端的 `model` 設定。

### 讓新模型出現

1. 按重新整理。不按的話，清單最多會沿用一小時。
2. 還是沒有的話，到 Providers 確認帳號已連上且沒有錯誤。自訂端點則把 id 加進端點的模型清單。

### 用程式讀取清單

客戶端可以用 API key 從 API 拿到同一份清單：

```bash
curl https://<your-domain>/openai/v1/models -H "Authorization: Bearer <your-api-key>"
```

## 相關頁面

- [Providers](/zh-TW/app/providers)：接上這些模型的來源帳號。
- [Groups](/zh-TW/app/groups)：替模型取自己的名稱並設定備援順序。
- [端點與模型 id](/zh-TW/reference/endpoints)：base URL 與 id 格式。
