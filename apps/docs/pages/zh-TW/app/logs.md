---
title: Logs
description: Kano Proxy 的 Logs 頁面：每個請求一列，含模型、帳號、API key、狀態、token、費用與延遲，以及怎麼用它查錯誤。
---

# Logs

Logs 列出你的 API key 透過 proxy 送出的每一個請求，一個請求一列，最新的在最上面。要查某個請求是哪個帳號處理、哪把 key 送的、為什麼失敗，就看這頁。

Proxy 不會儲存 prompt 或回應內容。每一列只有中繼資料：誰送的、哪個模型、結果、token 數和時間。

## 頁面上有什麼

### 篩選

頁首有三個控制項：

- **Provider**：**All providers**，或單一內建供應商、自訂端點。
- **Show**：**All** 或 **Errors**。選 **Errors** 只留下失敗的請求。
- 重新整理按鈕：載入最新的紀錄。

往下捲動會自動載入更多。最下面的 **Load more** 是手動載入。

### 欄位

| 欄位 | 內容 |
|------|------|
| **Time** | 記錄時間，以你的當地時間顯示 |
| **Model** | 實際回應的 `provider/model` id。出現 **via** 加上 group 名稱，表示客戶端呼叫的是 [model group](/zh-TW/app/groups) |
| **Account** | 處理這個請求的帳號 |
| **API key** | 送出請求的 key 名稱 |
| **Type** | 訂閱帳號（Claude Code、Codex、Grok、Antigravity）顯示 **OAuth**，自訂端點或本機模型顯示 **API** |
| **Status** | 成功時是 HTTP 狀態碼；失敗時以紅字顯示錯誤代碼，例如 `upstream_unavailable` |
| **Input**、**Cache read**、**Cache write**、**Output** | 供應商回報的 token 數 |
| **Cost** | 依模型 API 牌價估算的美元費用。查不到價格時顯示 `—` |
| **Latency** | 供應商回應花了多久。串流回應只算到開始串流，不是到最後一個 token |

螢幕窄時會隱藏部分欄位。太長的內容只顯示兩行，滑鼠移到格子上可以看到全文。

名稱旁邊可能出現的標籤：

- **Shared**：這個帳號是別人分享給你的。
- **Removed**：請求之後，這把 key 或這個帳號被刪除了。列上保留它當時的名稱。
- **Account removed** 或 **Key removed**：同上，但這是較舊的紀錄，當時沒有存名稱。

### 請求明細

點任一列會打開 **Request detail**，顯示這一列的所有欄位，另外還有：

- **Request id**：這筆紀錄自己的 id。
- **Provider** 和 **Alias**：供應商，以及客戶端送出的 group 模型名稱（如果有）。
- 帳號名稱下方的帳號 id。帳號刪除後，靠它還能認出是哪一個。
- **Upstream status**：供應商最後回傳的 HTTP 狀態碼。串流一旦開始，**Status** 一律記為 `200`，供應商真正的錯誤要看這裡。
- **Error**：錯誤代碼（如果有）。

Key 的 id 和金鑰本身都不會顯示。

## 常見操作

### 查客戶端為什麼收到錯誤

1. 把 **Show** 設成 **Errors**。
2. 依 **Time** 和 **API key** 找到那個請求。
3. 點開，看 **Error** 和 **Upstream status**。各錯誤代碼的意思請見 [錯誤訊息](/zh-TW/reference/errors)。

### 確認是哪個帳號回應的

1. 找到請求，看 **Account**。
2. 如果不是你預期的帳號，表示當時排在它前面的帳號已達上限或被暫停。順序請見 [Providers](/zh-TW/app/providers)。

### 確認新工具有接上

1. 從工具送出一個請求。
2. 按重新整理。幾秒內就會出現，**API key** 欄是你給這個工具的 key 名稱。

## 紀錄保留多久

舊紀錄會自動刪除。除非站台另有設定，保留 90 天。

## 相關頁面

- [Overview](/zh-TW/app/overview)：同樣的流量，依模型加總。
- [Providers](/zh-TW/app/providers)：帳號順序與狀態。
- [錯誤訊息](/zh-TW/reference/errors)：各錯誤代碼的意思。
