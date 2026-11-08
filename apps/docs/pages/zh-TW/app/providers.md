---
title: Providers
description: Kano Proxy 的 Providers 頁面：連接訂閱帳號、自訂端點與本機模型，看懂帳號狀態與用量區間，調整帳號池的順序，以及從請求移除 fast mode 或 Claude Code 那一行。
---

# Providers

Providers 是連接帳號和端點的地方，負責回應你的請求。你能呼叫的每一個模型，都來自這裡接上的東西。

可以接三種：

- **訂閱帳號**：Claude Code、Codex、Grok、Antigravity。登入你已經在付費的訂閱。
- **自訂端點**：任何你有金鑰的 OpenAI 或 Anthropic 相容 API。
- **CLI 供應商**：跑在你自己電腦上的模型伺服器，透過 `kano-proxy` CLI 接進來。

## 頁面上有什麼

### 頁首

- 分頁可以只看其中一區：**All**、每個訂閱供應商各一個分頁、**Custom**、**CLI**。**All** 會把所有區塊依序列出。分頁上的數字是該區有幾列；**All** 要等每一區都載入後才顯示總數。
- 重新整理按鈕會重新載入每一區，並向每個帳號取得最新用量。不按的話，頁面開著時每 2 分鐘自動更新一次用量。

### 訂閱帳號

每個供應商一張卡，每個帳號一列。同一個供應商的帳號組成一個帳號池：該供應商的請求會送到第一個可用的帳號，它接不了就往下一個。完整規則見[請求如何分配](/zh-TW/reference/routing)。

每一列顯示：

- **帳號名稱**：供應商回報的 email 或使用者名稱，或是你用 Rename 取的名字。改過名的帳號，底下仍會顯示供應商那邊的身分。
- **Primary**：帳號池順序中的第一個。它被限制或暫停時仍保留這個標記，讓你看得出流量已經離開它。
- **方案**：例如 `Pro plan`，供應商有回報時才顯示。Claude 用的是 Anthropic 從 2026 年 10 月 30 日起的名稱：原本的 Max 5x 顯示為 `Max plan`，原本的 Max 20x 顯示為 `Ultra plan`。
- **狀態**：圓點旁一定附文字。

| 狀態 | 意思 |
|------|------|
| **Active** | 這個供應商的下一個請求會送到這裡。 |
| **Standby** | 可以用。上面的帳號接不了時才輪到它。 |
| **Limit reached** | 有一個用量區間到了 100%。在該區間重置前都會跳過它。 |
| **Paused** | 供應商最近拒絕了一個請求，例如觸發速率限制。暫停結束前都會跳過它。 |
| **Sign-in expired** | 儲存的登入憑證已經失效。如果是你自己的帳號，狀態旁會出現 **Sign in again**。別人分享給你的帳號就算登入失效，也不會顯示這個狀態，只有擁有者看得到，也只有擁有者能重新登入。 |
| **Active · no Fable** | 只有 Claude Code 會出現。下一個請求會送到這裡，但這個席位不能用 Fable 模型。 |
| **Active · Fable** | 只有 Claude Code 會出現。Fable 的請求會送到這裡，因為上面那個帳號不能用 Fable。 |
| **Needs attention** | 伺服器回報了這個版本的頁面還不認得的狀態。 |

- **On credits**：狀態旁的標記。Codex 帳號只靠付費點數才接得了請求時，就會顯示它。
- **Couldn't read usage**：上一次讀取用量失敗時，狀態旁會出現這段簡短提示。滑鼠移到上面可以看到供應商回傳的錯誤。用量條保留最後一次讀到的數值。分享給你的帳號、**Sign-in expired** 的帳號，以及不影響請求的讀取錯誤（例如讀取用量本身被限流），都不會顯示這段提示。
- **用量條**：供應商回報幾個用量區間就有幾條，例如 **5h** 和 **Week**，各自顯示已用百分比和重置時間。通過驗證的 Claude Ultra 帳號沒有 5 小時限制，所以只有週額度的用量條。Antigravity 依序是 **Gemini 5h**、**Gemini Week**，Google AI Pro 方案接著是 Gemini 4 Argon 專屬的週額度 **Argon Week**，最後是 **Other 5h**、**Other Week**。這幾條各對應一組模型。某一條滿了，只會擋下那一組的模型，帳號狀態不會改變。沒有組名的用量條（例如 **Week**）涵蓋所有模型，滿了會讓這一列變成 **Limit reached**。剛加入的帳號會顯示 **Usage will appear once this account is used**。**Sign-in expired** 的帳號，用量條會變淡，顯示的是最後一次讀到的用量。
- **Credits**：Codex 或 Antigravity 回報點數餘額時，用量條下方會多一列，例如 `1,500 left` 或 `Unlimited`。Codex 帳號有點數但沒有回報餘額時，顯示 `Available`。餘額沒有總量，所以不畫成用量條。

### Custom endpoints

每個自訂端點一列：名稱、格式（**OpenAI** 或 **Anthropic**）、模型 id 使用的 **Model prefix**（例如 `mygw/*`）、base URL（**Endpoint**）、遮罩過的金鑰（**Key**），以及狀態 **Active** 或 **Paused**。OpenAI 格式的端點如果設了 token-count URL，會多一行 **Token count** 顯示這個網址。自訂端點沒有用量區間可以顯示。

### CLI providers

每個從 CLI 註冊的本機模型伺服器一列：名稱和前綴、格式、**Connected** 或 **Offline**、上次回報的模型數，以及從哪一台裝置註冊。這裡沒有新增按鈕，伺服器要從終端機註冊，見[本機模型](/zh-TW/cli/local-models)。

### 編輯模式

平常每一列只顯示身分和用量，唯一的例外是你自己的帳號登入失效時出現的 **Sign in again**。要修改其他東西，先按該區標題列的鉛筆（**Edit Claude Code**、**Edit Custom endpoints**、**Edit CLI providers** 等）。該區至少有一列時才會出現鉛筆。按下後，該區每一列的操作按鈕會一起出現。再按一次（這時變成打勾按鈕）就收起來。編輯模式開著時，加號按鈕會隱藏。

在編輯模式下，訂閱帳號的卡片會在標題列和帳號列之間多出一塊設定區。最上面是 **Strategy**，目前只有 **Ordered** 一個選項：依優先順序，用第一個可用的帳號。**Strategy** 下面是這張卡的勾選項，說明見[轉送前的修改](#轉送前的修改)。自訂端點和 CLI 供應商也有自己的勾選項，各自的位置列在[轉送前的修改](#轉送前的修改)。

### 轉送前的修改

這些勾選項會改變 Kano Proxy 轉送給供應商的內容，每個供應商最多兩個，預設都不勾選。勾選或取消勾選都會立刻儲存。

**Remove fast mode** 讓請求以標準速度送出。Claude 的 fast mode 一律以額外用量點數（usage credits）計費，方案額度還有剩也一樣。Codex 的 fast mode 會更快耗掉方案額度。

**Remove the Claude Code line** 會從 system prompt 拿掉 Claude Code 的身分宣告 `You are Claude Code, Anthropic's official CLI for Claude.`。Claude Agent SDK 用的兩種寫法也會一起拿掉：`You are Claude Code, Anthropic's official CLI for Claude, running within the Claude Agent SDK.` 和 `You are a Claude agent, built on Anthropic's Claude Agent SDK.`。Claude Code 每個請求都會帶這一行。它透過 Kano Proxy 呼叫其他供應商的模型時，這一行也會送到那個供應商，所以每個供應商都有這個勾選項。如果是 Claude Code 帳號，Kano Proxy 也不會再自己補上這一行。少了它，Claude 可能拒絕請求，或改用額外用量點數計費。

勾選項在編輯模式下的位置：

- **Claude Code** 和 **Codex** 卡片：兩個都有，在設定區的 **Strategy** 下面。
- **Grok** 和 **Antigravity** 卡片：只有 **Remove the Claude Code line**，一樣在 **Strategy** 下面。Kano Proxy 本來就不會把 fast mode 送給這兩個供應商。
- 每個自訂端點：兩個都有，在該列的端點資訊下方。
- 每個 CLI 供應商：兩個都有，在 **CLI providers** 表格下方，每個供應商一組。

勾選後會出現標記：**Fast mode removed** 或 **Claude Code line removed**。訂閱帳號的卡片顯示在標題旁，自訂端點顯示在名稱下方的狀態和格式旁，CLI 供應商顯示在前綴和格式旁。不管編輯模式開著還是關著，標記都會一直顯示。

每個供應商只套用自己的勾選項。[群組](/zh-TW/app/groups)從一個供應商備援到另一個時，每個目標各自套用自己的勾選項。勾選之後，供應商的回應可能不一樣，也可能直接拒絕請求。

分享的帳號會同時看擁有者和被分享者的勾選項。只要其中一方勾選 **Remove fast mode**，就會移除 fast mode。Claude Code 那一行則要雙方都勾選 **Remove the Claude Code line** 才會移除。每張卡片只顯示看的人自己的勾選項和標記，擁有者的勾選不會顯示在分享出去的帳號上。

## 常見操作

### 連接訂閱帳號

1. 在該供應商的卡片上按加號（**Add account**）。對話框最上面有一段簡短提醒：在供應商自己的 app 以外使用訂閱帳號，可能違反它的條款。提醒裡的 **Risks and terms** 連到[風險與條款](/zh-TW/guide/risks)。
2. 按 **Continue**。供應商自己的登入頁會在新分頁打開。如果被瀏覽器擋下，按 **Open sign-in page**。
3. 依對話框對該供應商的說明完成登入：
   - **Claude Code**：同意授權，複製拿到的授權碼，貼回對話框，再按 **Finish**。
   - **Codex** 和 **Grok**：在打開的頁面輸入對話框顯示的代碼，然後同意。對話框過幾秒會自己偵測到；按 **I've approved it** 可以立刻檢查。
   - **Antigravity**：同意授權。Google 接著會把你導到一個打不開的 `localhost` 網址，這是正常的。從瀏覽器網址列複製這個網址，貼回對話框，再按 **Finish**。

新帳號會排到帳號池最上面，成為 **Primary**。同一個供應商可以接好幾個帳號。

如果你手上已經有某個帳號的 token，可以改用 **Enter tokens manually**，填入 access token，refresh token 和名稱可以不填。

### 處理登入失效的帳號

1. 在顯示 **Sign-in expired** 的那一列按 **Sign in again**。不需要打開編輯模式。
2. 用同一個帳號登入，步驟和[連接訂閱帳號](#連接訂閱帳號)相同。這裡沒有 **Enter tokens manually**。

帳號在順序中的位置、名稱和群組設定都會保留。新舊登入都帶有 email 或帳號 id 時，Kano Proxy 才能比對。比對出是別的帳號就會失敗，對話框會列出兩個帳號。比對不了時，新的登入會直接取代舊的，所以請確認用的是同一個帳號。

### 調整帳號池順序

1. 在該供應商的卡片上按鉛筆。
2. 在某一列按 **Move up** 或 **Move down**。順序會立刻儲存。

最上面那列是 **Primary**，最先嘗試。順序只影響可用的帳號之間：被限制或暫停的帳號，不管排在哪裡都會被跳過。

### 恢復被暫停的帳號

1. 在該供應商的卡片上按鉛筆。
2. 在暫停的那一列按播放鍵（**Resume**）。

帳號會立刻重新接受請求。如果供應商又拒絕它，它會再次被暫停。**Limit reached** 的帳號不會出現 Resume：用量區間滿了是供應商設下的限制，只有等它重置才會解除。**Sign-in expired** 的帳號也不會出現 Resume，要用 **Sign in again** 處理。

只有你自己的帳號才有 Resume，別人分享給你的帳號被暫停時，要由擁有者恢復或重新登入。

### 移除 fast mode 或 Claude Code 那一行

訂閱供應商：

1. 在該供應商的卡片上按鉛筆。
2. 在標題列下方的設定區，勾選 **Remove fast mode** 或 **Remove the Claude Code line**。會立刻儲存，卡片標題旁也會馬上出現對應的標記。
3. 按打勾按鈕收起編輯模式。

自訂端點：在 **Custom endpoints** 卡片上按鉛筆，再到該端點那一列勾選。CLI 供應商：在 **CLI providers** 卡片上按鉛筆，再到表格下方該供應商那一組勾選。要還原，用同樣的方式取消勾選。

### 讓 Codex 帳號在額度用完後改用點數

1. 在 **Codex** 卡片上按鉛筆。
2. 勾選帳號用量條下方的 **Use credits after limits**。

這個勾選項只會出現在你自己的帳號上，而且供應商要回報這個帳號有點數。勾選之後，就算點數用完，它也會留在該列，所以隨時都能取消勾選。

方案額度用完之後才會開始花點數。帳號改用點數時，狀態會是 **Active** 或 **Standby**，而不是 **Limit reached**，旁邊會多一個 **On credits** 標記。取消勾選就會停止。

### 改名或移除帳號

在編輯模式下，列上的鉛筆（**Rename**）可以設定顯示名稱。留空就恢復成供應商那邊的名稱。改名不會影響分配。

垃圾桶（**Remove**）會先請你確認，再刪除這個帳號。請求會立刻停止使用它。要加回來就得重新登入一次。

只有你自己的帳號才有 Rename 和 Remove。

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

- 播放鍵（**Resume**）：只出現在 **Paused** 的端點上。按下後端點會立刻重新接受請求。
- **Test**：送一個探測請求到端點，結果顯示在該列。
- 鉛筆（**Edit**）：打開同一個表單。**API key** 留空代表沿用原本的金鑰。
- 垃圾桶（**Remove**）：確認後刪除端點和它的金鑰。`<prefix>/…` 的請求會立刻失效。
- **Move up** 和 **Move down**：只改變這個頁面上的顯示順序，不影響分配。
- **Remove fast mode** 和 **Remove the Claude Code line**：說明見[轉送前的修改](#轉送前的修改)。

修改既有端點的 **Model prefix** 時，會先跳出確認畫面，列出哪些群組目標會被改成新前綴。還在呼叫舊前綴的客戶端，在你更新之前都會收到模型無效的錯誤。

### 改名或移除 CLI 供應商

在編輯模式下，CLI 供應商那一列的鉛筆（**Rename**）可以設定顯示名稱。垃圾桶（**Remove**）會先請你確認，再移除這個供應商。`<prefix>/…` 的請求會立刻失效。

## 相關頁面

- [Models](/zh-TW/app/models)：已接上的供應商提供的所有模型 id。
- [Groups](/zh-TW/app/groups)：自訂模型名稱，並設定跨供應商、跨帳號的備援順序。
- [請求如何分配](/zh-TW/reference/routing)：請求怎麼挑帳號，什麼時候換下一個。
- [本機模型](/zh-TW/cli/local-models)：接上你自己電腦上的模型伺服器。
- [Claude Code](/zh-TW/agents/claude-code)：讓 Claude Code 連到這個 proxy。
