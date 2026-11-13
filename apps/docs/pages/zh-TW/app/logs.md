---
title: Logs
description: Kano Proxy 的 Logs 頁面：每個請求一列，含模型、帳號、API key、狀態、token、費用與延遲，怎麼依模型或別名篩選，以及怎麼匯出紀錄。
---

# Logs

Logs 列出你的 API key 透過 proxy 送出的每一個請求，一個請求一列，最新的在最上面。要查某個請求是哪個帳號處理、哪把 key 送的、為什麼失敗，就看這頁。

Proxy 不會儲存 prompt 或回應內容。每一列只有中繼資料：誰送的、哪個模型、結果、token 數和時間。

## 頁面上有什麼

### 篩選

頁首有這些控制項：

- **Model**：**All models**，或單一模型、單一別名。清單列出你今天用過的項目，每項附上今天的請求數和費用。**By model** 列出模型；**By alias** 列出 [model group](/zh-TW/app/groups) 名稱，外加 **No alias**。切換時，每一列開頭顯示的名稱也會跟著換。
- **Provider**：**All providers**，或單一內建供應商、自訂端點，附上類型和今天的請求數、費用。選了之後，Model 清單只留這個供應商的項目。
- **Show**：**All** 或 **Errors**。選 **Errors** 只留下失敗的請求。
- **Columns**：選擇要顯示哪些欄位。**Reset** 會恢復所有欄位和預設寬度。
- 重新整理按鈕：載入最新的紀錄。

紀錄依日期分組，每天一個標題，最新的在最上面。往下捲動會自動載入更多。最下面的 **Load more** 是手動載入。

### 欄位

| 欄位 | 內容 |
|------|------|
| **Time** | 記錄時間，以你的當地時間顯示。日期在上方的分組標題 |
| **Model** | 一個色點，接著是模型名稱，灰字是供應商；客戶端呼叫的是 [model group](/zh-TW/app/groups) 時，灰字還有 **via** 加別名。在 **By alias** 下，別名放前面，接著 **→** 和模型。色點和 **Model** 清單裡的選項顏色一致 |
| **Account** | 處理這個請求的帳號 |
| **API key** | 送出請求的 key 名稱 |
| **Status** | HTTP 狀態碼。請求失敗時顯示紅字，包括以 `200` 開始串流、之後才失敗的請求。錯誤代碼要點開該列看 |
| **Input**、**Cache read**、**Cache write**、**Output** | 供應商回報的 token 數 |
| **Cost** | 依模型 API 牌價估算的美元費用。查不到價格時顯示 `—` |
| **Latency** | 供應商回應花了多久。串流回應只算到開始串流，不是到最後一個 token |

**Time** 和 **Model** 一定會顯示。拖動欄位標題的邊緣可以調整寬度。選了哪些欄位和欄寬，存在這個瀏覽器，不跟著帳號走。螢幕窄時會隱藏部分欄位。每列只佔一行，內容被截斷時，滑鼠移到格子上可以看到全文。

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
- **Type**：訂閱帳號（Claude Code、Codex、Grok、Antigravity）顯示 **OAuth**，自訂端點或本機模型顯示 **API**。
- **Error**：錯誤代碼（如果有）。

Key 的 id 和金鑰本身都不會顯示。

### 匯出

點 **Logs** 標題旁的箭頭，選 **Export…**。在對話框裡選：

- **Time range**：**Today**、**7 days**、**30 days**，或用 **Custom** 自訂日期。
- **Use the page's filters**：只匯出頁面上 Model、Provider 和 **Show** 篩選後的紀錄。
- **Columns**：表格目前顯示的欄位，或 **All fields**，多了 id、類型、upstream status 和錯誤代碼。
- **Format**：**CSV** 可以用 Excel、Numbers 或 Google Sheets 開，時間是你的當地時間。**JSON** 是一個物件陣列，數字不經格式化，時間是 UTC。

檔案內容和頁面上一樣是中繼資料。Proxy 不儲存 prompt 和回應，所以檔案裡也不會有。

## 常見操作

### 查客戶端為什麼收到錯誤

1. 把 **Show** 設成 **Errors**。
2. 依 **Time** 和 **API key** 找到那個請求，它的 **Status** 是紅字。
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
