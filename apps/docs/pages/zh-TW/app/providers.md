---
title: Providers
description: Kano Proxy 的 Providers 頁面：連接訂閱帳號、自訂端點與本機模型，看懂帳號狀態與用量區間，以及調整帳號池的順序。
---

# Providers

Providers 是連接帳號和端點的地方，負責回應你的請求。你能呼叫的每一個模型，都來自這裡接上的東西。

可以接三種：

- **訂閱帳號**：Claude Code、Codex、Grok、Antigravity。登入你已經在付費的訂閱。
- **自訂端點**：任何你有金鑰的 OpenAI 或 Anthropic 相容 API。
- **CLI 供應商**：跑在你自己電腦上的模型伺服器，透過 `kano-proxy` CLI 接進來。

## 頁面上有什麼

### 頁首

- 分頁可以只看其中一區：**All**、每個訂閱供應商各一個分頁、**Custom**、**CLI**。分頁上的數字是該區的帳號數。**All** 會把所有區塊依序列出。
- 重新整理按鈕會重新載入每一區，並向每個帳號取得最新用量。不按的話，頁面開著時每 2 分鐘自動更新一次用量。

### 訂閱帳號

每個供應商一張卡，每個帳號一列。同一個供應商的帳號組成一個帳號池：該供應商的請求會送到第一個可用的帳號，它接不了就往下一個。完整規則見[請求如何分配](/zh-TW/reference/routing)。

每一列顯示：

- **帳號名稱**：供應商回報的 email 或使用者名稱，或是你用 Rename 取的名字。改過名的帳號，底下仍會顯示供應商那邊的身分。
- **Primary**：帳號池順序中的第一個。它被限制或暫停時仍保留這個標記，讓你看得出流量已經離開它。
- **方案**：例如 `Pro plan`，供應商有回報時才顯示。
- **狀態**：圓點旁一定附文字。

| 狀態 | 意思 |
|------|------|
| **Active** | 這個供應商的下一個請求會送到這裡。 |
| **Standby** | 可以用。上面的帳號接不了時才輪到它。 |
| **Limit reached** | 有一個用量區間到了 100%。在該區間重置前都會跳過它。 |
| **Paused** | 供應商最近拒絕了一個請求，例如觸發速率限制。暫停結束前都會跳過它。 |
| **Needs attention** | 儲存的登入已經失效。移除這個帳號再重新加入。 |
| **Active · no Fable** | 只有 Claude Code 會出現。下一個請求會送到這裡，但這個席位不能用 Fable 模型。 |
| **Active · Fable** | 只有 Claude Code 會出現。Fable 的請求會送到這裡，因為上面那個帳號不能用 Fable。 |

- **用量條**：供應商回報幾個用量區間就有幾條，例如 **5h** 和 **Week**，各自顯示已用百分比和重置時間。Antigravity 依序是 **Gemini 5h**、**Gemini Week**，Google AI Pro 方案接著是 Gemini 4 Argon 專屬的週額度 **Argon Week**，最後是 **Other 5h**、**Other Week**；Google 有回報餘額時，下面還會多一行，例如 `1,500 credits left`。剛加入的帳號會顯示 **Usage will appear once this account is used**。

### Custom endpoints

每個自訂端點一列：名稱、格式（**OpenAI** 或 **Anthropic**）、模型 id 使用的 **Model prefix**（例如 `mygw/*`）、base URL、遮罩過的金鑰，以及狀態 **Active** 或 **Paused**。自訂端點沒有用量區間可以顯示。

### CLI providers

每個從 CLI 註冊的本機模型伺服器一列：前綴、格式、**Connected** 或 **Offline**、上次回報的模型數，以及從哪一台裝置註冊。這裡沒有新增按鈕，伺服器要從終端機註冊，見[本機模型](/zh-TW/cli/local-models)。

### 編輯模式

平常每一列只顯示身分和用量。要修改任何東西，先按該區標題列的鉛筆（**Edit Claude Code**、**Edit Custom endpoints** 等）。這會讓該區每一列的操作按鈕一起出現。再按一次（這時變成勾勾）就收起來。

在編輯模式下，訂閱帳號那一區的標題列會出現 **Strategy**。目前只有 **Ordered** 一個選項：依優先順序，用第一個可用的帳號。

## 常見操作

### 連接訂閱帳號

1. 在該供應商的卡片上按加號（**Add account**）。
2. 按 **Continue**。供應商自己的登入頁會在新分頁打開。如果被瀏覽器擋下，按 **Open sign-in page**。
3. 依對話框對該供應商的說明完成登入：
   - **Claude Code**：同意授權，複製拿到的授權碼，貼回對話框，再按 **Finish**。
   - **Codex** 和 **Grok**：在打開的頁面輸入對話框顯示的代碼，然後同意。對話框過幾秒會自己偵測到；按 **I've approved it** 可以立刻檢查。
   - **Antigravity**：同意授權。Google 接著會把你導到一個打不開的 `localhost` 網址，這是正常的。從瀏覽器網址列複製這個網址，貼回對話框，再按 **Finish**。

新帳號會排到帳號池最上面，成為 **Primary**。同一個供應商可以接好幾個帳號。

如果你手上已經有某個帳號的 token，可以改用 **Enter tokens manually**，填入 access token，refresh token 和名稱可以不填。

### 調整帳號池順序

1. 在該供應商的卡片上按鉛筆。
2. 在某一列按 **Move up** 或 **Move down**。順序會立刻儲存。

最上面那列是 **Primary**，最先嘗試。順序只影響可用的帳號之間：被限制或暫停的帳號，不管排在哪裡都會被跳過。

### 恢復被暫停的帳號

1. 在該供應商的卡片上按鉛筆。
2. 在暫停的那一列按播放鍵（**Resume**）。

帳號會立刻重新接受請求。如果供應商又拒絕它，它會再次被暫停。**Limit reached** 的帳號不會出現 Resume：用量區間滿了是供應商設下的限制，只有等它重置才會解除。

### 改名或移除帳號

在編輯模式下，列上的鉛筆（**Rename**）可以設定顯示名稱。留空就恢復成供應商那邊的名稱。改名不會影響分配。

垃圾桶（**Remove**）會先請你確認，再刪除這個帳號。請求會立刻停止使用它。要加回來就得重新登入一次。

### 新增自訂端點

1. 在 **Custom endpoints** 卡片上按加號（**Add endpoint**）。
2. 選 **API format**：**OpenAI** 或 **Anthropic**。之後不能更改。
3. 填 **Name**。**Model prefix** 會依名稱自動產生，這個端點的模型就用 `<prefix>/<model>` 呼叫。
4. 填 **Base URL**，必須以 `https://` 開頭。欄位下方會顯示請求實際送往的網址：
   - OpenAI 格式：自己加上 `/v1`，例如 `https://api.example.com/v1`。請求會送到 `<base>/chat/completions`。
   - Anthropic 格式：不要加 `/v1`。請求會送到 `<base>/v1/messages`。
5. 填 **API key**。它會加密儲存，之後不會再顯示。
6. 在 **Models** 選 **Load automatically** 從端點讀取清單，或選 **Enter manually** 自己填，一行一個模型 id。
7. 想在儲存前確認網址和金鑰，可以按 **Test connection**。
8. 按 **Add endpoint**。

指向私有網段、`localhost` 或這個 proxy 自己網域的 base URL 會被拒絕。要接自己電腦上的伺服器，請改用 CLI。

**Token-count URL** 只有 OpenAI 格式才會出現。Claude Code 會要求計算 token 數，而 OpenAI 相容的 API 沒有這種端點。如果你的閘道同時提供 Anthropic 相容的 `/v1/messages/count_tokens`，就把它的完整網址填在這裡。網址會原封不動地使用。

自訂端點和 CLI 供應商加起來最多 20 個。

### 編輯、測試或移除自訂端點

在編輯模式下，每個自訂端點列有：

- **Test**：送一個探測請求到端點，結果顯示在該列。
- 鉛筆（**Edit**）：打開同一個表單。**API key** 留空代表沿用原本的金鑰。
- 垃圾桶（**Remove**）：確認後刪除端點和它的金鑰。`<prefix>/…` 的請求會立刻失效。
- **Move up** 和 **Move down**：只改變這個頁面上的顯示順序，不影響分配。

修改既有端點的 **Model prefix** 時，會先跳出確認畫面，列出哪些群組目標會被改成新前綴。還在呼叫舊前綴的客戶端，在你更新之前都會收到模型無效的錯誤。

## 相關頁面

- [Models](/zh-TW/app/models)：已接上的供應商提供的所有模型 id。
- [Groups](/zh-TW/app/groups)：自訂模型名稱，並設定跨供應商、跨帳號的備援順序。
- [請求如何分配](/zh-TW/reference/routing)：請求怎麼挑帳號，什麼時候換下一個。
- [本機模型](/zh-TW/cli/local-models)：接上你自己電腦上的模型伺服器。
