---
title: Groups
description: Kano Proxy 的 Groups 頁面：模型群組是你自己的私有端點，模型名稱由你決定，並可設定跨供應商、跨帳號的備援順序。
---

# Groups

Groups 是建立模型群組的地方。模型群組是一個屬於你自己的端點：客戶端送出的模型名稱由你決定，每個名稱再對應一串有順序的實際模型。

## 群組用來做什麼

每個群組有自己的 base URL：

| 協定 | 群組 base URL |
|------|---------------|
| OpenAI 相容 | `https://<your-domain>/g/<group-slug>/openai/v1` |
| Anthropic Messages | `https://<your-domain>/g/<group-slug>/anthropic` |

客戶端指向這些網址時，用的是跟其他地方相同的 API key，但只能呼叫群組裡定義的模型名稱。每個名稱對應一個或多個**目標**：一個 `provider/model` id，綁定一個帳號。proxy 依順序嘗試目標，用第一個接得了請求的。

常見的兩種用法：

- **工具寫死了模型名稱。** 有些工具只會送 `claude-opus-5`、`gpt-4o` 這類沒有 `provider/` 前綴的名稱。在群組裡定義一模一樣的名稱，再把工具指向群組的網址，這些名稱就會跑你對應的模型。
- **跨帳號、跨供應商的備援。** 在同一個名稱下列多個目標。第一個被限制、暫停或失敗時，請求會換到下一個，同一個請求內就會換。[Providers](/zh-TW/app/providers) 上的帳號池只能在同一個供應商的帳號之間切換；群組可以從 Claude Code 帳號換到 Codex 帳號，再換到自訂端點。

因為每個群組都是獨立的端點，兩個群組可以用同一個模型名稱指向不同的東西。在共用的 base URL `/openai/v1` 和 `/anthropic` 上，群組的名稱不會生效。

## 頁面上有什麼

### 群組清單

左欄列出你的群組。每一列顯示群組名稱、slug 和模型數。出現警告圓點，代表至少有一個目標現在接不了請求。可以用方向鍵在群組之間移動。

在窄螢幕上只會顯示清單。點一個群組打開它，按 **All groups** 返回。

### 群組內容

右側顯示選取的群組：

- **名稱**、最後更新時間，以及 **Edit** 按鈕。
- **兩個端點欄位**：**OpenAI** 和 **Anthropic**，各自是這個站的完整 base URL，旁邊有複製按鈕。這個網址就是要填進客戶端 base URL 設定的值。
- **模型表格**：每個模型名稱一列。
  - **Model** 是客戶端在 `model` 送出的名稱，點一下就能複製。
  - **Route · failover order** 由左到右列出目標，依這個順序嘗試。每個目標顯示它的 `provider/model` id，底下是它使用的帳號。
  - 綁定帳號的目標只會用那個帳號，那個帳號接不了時就跳過。標示 **Any account** 的目標會依帳號池自己的順序使用該供應商的所有帳號。你在這個頁面加的目標一定會綁定帳號；[CLI](/zh-TW/cli/agents) 建立的目標則是 **Any account**。

表格也會顯示下一個請求會去哪裡：

- 以強調色畫出、標示 **Current** 的目標，就是這個名稱下一個請求會用的目標。
- 被劃掉的目標現在接不了請求，旁邊會寫原因，例如 **Limit reached · resumes in 2 hours** 或 **Paused · resumes in 4 minutes**。
- **Account removed** 表示目標綁定的帳號已被刪除。在你改選別的帳號之前，它都會被跳過。
- **This model can't be reached anymore** 表示它的供應商已經不存在，例如自訂端點被刪掉了。

這個畫面讀的是 proxy 分配請求時用的同一份狀態，所以會跟下一個請求的實際走向一致。

## 常見操作

### 建立群組

1. 按 **Create group**。
2. 在第一欄填 **Name**。這是給你自己看的標籤，客戶端不會送出它。
3. 填 **Endpoint slug**：小寫英文字母、數字和連字號，2 到 32 個字元。它產生的兩個 base URL 會顯示在下方。
4. 在第三欄 **Group models** 下，於 **New model name** 輸入名稱，按 **Add model**。請用客戶端實際送出的名稱。名稱可以包含 `/`，但不能有空白。
5. 在中間欄 **Pick targets**：
   1. 從上方選一個供應商分頁。
   2. 在左側清單選一個帳號，它的模型會列出來。
   3. 點一個模型。它會被加為目前選取名稱的目標，並綁定那個帳號。
6. 重複上一步加入備援目標。要把同一個模型加在第二個帳號上，就選另一個帳號，再點同一個模型。
7. 在目標上按 **Move up** 或 **Move down** 調整位置。第一個目標最先嘗試。
8. 按 **Create group**。

群組至少要有一個模型，每個模型至少要有一個目標。上限：50 個群組，每個群組 20 個模型，每個模型 20 個目標。

第一欄的 **Strategy** 只有 **Ordered** 一個選項：依清單順序嘗試目標。

### 讓客戶端使用群組

1. 選取群組。
2. 依客戶端使用的協定，複製 **OpenAI** 或 **Anthropic** 的 base URL。
3. 把它設為客戶端的 base URL，並搭配 [Access](/zh-TW/app/access) 的 API key。
4. 把客戶端的模型設為群組裡的某個模型名稱。

[CLI](/zh-TW/cli/agents) 幫 Claude Code 和 Codex CLI 設定時，就是用這種方式建立群組，名稱分別是 `claude-code` 和 `codex-cli`。

### 修改目標或名稱

1. 選取群組，按 **Edit**。
2. 在第三欄點一個模型名稱選取它。之後挑選的目標都會加到這個名稱底下。
3. 直接編輯名稱欄位即可改名，按 **Remove model** 刪除模型；每個目標列上可以調整順序或移除。
4. 按 **Save changes**。

修改 slug 會立刻搬動群組的網址。舊網址會失效，所有用到它的客戶端都要更新。

### 刪除群組

1. 選取群組，按 **Edit**。
2. 按 **Delete group**，再按 **Delete** 確認。

群組的端點會立刻失效。你的帳號、金鑰和紀錄都不受影響。

## 客戶端看到什麼

回應裡的模型名稱，會是客戶端送出的那個名稱，而不是實際回應的目標。非串流回應另外會在 `x-kano-upstream-provider` 和 `x-kano-upstream-model` 標頭中寫出實際的目標。[Logs](/zh-TW/app/logs) 兩者都會記錄：實際回應的目標，以及請求送往的群組和模型名稱。

呼叫群組裡沒有定義的名稱，會回 `400 invalid_model`。slug 不是你的群組，會回 `404`。

## 相關頁面

- [Providers](/zh-TW/app/providers)：接上目標要用的帳號。
- [Models](/zh-TW/app/models)：所有可以當作目標的 `provider/model` id。
- [請求如何分配](/zh-TW/reference/routing)：目標或帳號什麼時候會被跳過，請求什麼時候放棄。
- [端點與模型 id](/zh-TW/reference/endpoints)：共用的 base URL 和 id 格式。
