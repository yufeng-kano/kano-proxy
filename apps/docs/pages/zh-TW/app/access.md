---
title: Access
description: Access 頁管理你的憑證：建立 API key 並設定花費上限、管理用 kano-proxy CLI 登入的機器，以及複製客戶端要用的 base URL。
---

# Access

Access 頁放的是你的憑證：工具送出請求時帶的 API key、用 `kano-proxy` CLI 登入的機器，以及客戶端要連的 base URL。頁面分成三個分頁：**Keys**、**Devices**、**Connect**。

## Keys

列出你所有的 API key。每個送到 proxy 的請求都要帶其中一把。

| 欄位 | 內容 |
|------|------|
| **Name** | 你取的名稱。由 CLI 建立的 key，名稱下方會多一行 `from <裝置名稱>`。 |
| **Key** | key 的開頭幾個字元，用來分辨不同的 key。完整的 key 建立後就不會再顯示。 |
| **Spend** | 目前週期內的估算花費。有設上限時顯示「已花費 / 上限」，例如 `$3.20 / $50.00`。 |
| **Last used** | 最後一次有請求帶著這把 key 的時間；沒用過顯示 **Never**。 |

每列最右邊的鉛筆圖示可以編輯該 key。在 Keys 分頁時，頁首會有 **Create key** 按鈕。

### 建立 key

1. 按 **Create key**。
2. 填入 **Name**，例如要用這把 key 的工具或機器名稱。
3. 需要的話設定 **Spend limit**（見下節）。留空代表不設上限。
4. 按 **Create key**。對話框會切換成 **Key created**，顯示完整的 key，開頭是 `sk-kano-proxy-`。
5. 立刻複製，再按 **Done**。完整的 key 只會出現這一次，關掉對話框後只剩開頭幾個字元。

建議一個工具或一台機器用一把 key。這樣撤銷其中一把不會影響其他工具，Logs 頁也看得出是哪個工具送的請求。

### 花費上限

建立和編輯 key 的對話框裡，上限由三個欄位組成：

- **Spend limit**：美元金額。留空代表沒有上限。
- **Resets**：重新計算的週期。**Daily**（每天 00:00 UTC）、**Weekly**（每週一 00:00 UTC）、**Monthly**（每月 1 日 00:00 UTC）或 **Never (all-time)**。預設是 **Monthly**。
- **Count subscription usage**：由你連接的訂閱（Claude Code、Codex、Grok、Antigravity）回應的請求要不要算進去，以官方定價計。預設開啟。custom endpoint 一律計入。

這把 key 在週期內的估算花費達到上限後，用它送出的模型請求會直接回 `429`，錯誤碼 `spend_limit_exceeded`，不會轉送到上游。查詢模型清單仍然可以用。上限是在每個請求開始前檢查，所以最後一個通過檢查的請求可能讓總額稍微超過上限。花費是 proxy 依公開定價算出的估計值，不是帳單。錯誤說明見 [錯誤訊息](/zh-TW/reference/errors)。

### 編輯或撤銷 key

1. 按該列的鉛筆圖示，開啟 **Edit key** 對話框。對話框會顯示這把 key 在本週期的花費。
2. 修改 **Name** 或上限欄位後，按 **Save changes**。
3. 要撤銷的話，按對話框左下角的 **Revoke** 再確認。所有使用這把 key 的地方會立刻失效，而且無法復原。

## Devices

**CLI devices** 卡片列出所有用你的帳號執行過 `kano-proxy login` 的機器。

| 欄位 | 內容 |
|------|------|
| **Name** | 登入時取的裝置名稱。 |
| **Last seen** | 這台裝置最後一次連到 proxy 的時間。 |
| **Created** | 登入的時間。畫面較窄時會隱藏。 |
| **Keys** | 這台裝置建立的 API key 數量，例如 `kano-proxy agent setup` 建立的 key。 |

還沒有任何裝置時，卡片會顯示安裝指令、這個站台專用的 `kano-proxy login` 指令，以及 GitHub Releases 的連結，那裡有各平台的版本。詳見 [安裝與登入](/zh-TW/cli/install)。

### 重新命名或撤銷裝置

1. 按卡片標題列的鉛筆圖示，每一列會多出重新命名按鈕和 **Revoke**。
2. 重新命名：按該列的鉛筆圖示，修改 **Display name** 後按 **Done**。
3. 撤銷：按 **Revoke** 再確認。確認視窗會寫出有幾把 API key 會一起被撤銷。
4. 按標題列的勾號離開編輯模式。

撤銷會刪除這台裝置，以及它建立的所有 API key。它會在一小時內斷線，之後只能在那台機器上重新執行 `kano-proxy login` 才能再連上。它註冊的本機端點會留在 Providers 頁，直到你手動移除。電腦遺失或汰換時就用這個功能：一個動作搞定，不必在 key 清單裡一把一把找。

### 核准 CLI 登入

`kano-proxy login` 會在瀏覽器開啟 `https://<your-domain>/cli/authorize?request=…`。如果你還沒登入，會先要你用 Google 登入。這個頁面只有一張卡片：

1. 卡片會問 **Sign in the device "<裝置名稱>"?**。只有在你剛剛自己在可信任的機器上執行了 `kano-proxy login` 時才核准。
2. 按 **Approve**，頁面會在 **Enter this code in your terminal** 下方顯示一組代碼。
3. 把代碼輸入終端機。代碼只能用一次，10 分鐘後失效，輸錯 5 次也會失效。

按 **Deny** 會結束這次請求，什麼都不會登入。連結過期或已經用過時，頁面會直接說明；重新執行 `kano-proxy login` 就會拿到新的連結。

## Connect

**Connect a client** 卡片列出客戶端需要的資訊，內容對應目前這個站台：

- **OpenAI-compatible**：base URL `https://<your-domain>/openai/v1`，附複製按鈕。
- **Anthropic-compatible**：base URL `https://<your-domain>/anthropic`，附複製按鈕。
- **Protocols**：OpenAI base 提供 Chat Completions 和 Responses API；Anthropic base 提供 Messages。
- **Authorization**：key 可以用 bearer token 送，也可以放在 `x-api-key` header。
- 模型 id 的格式是 `provider/model`。

完整規則見 [端點與模型 id](/zh-TW/reference/endpoints)。

## 相關頁面

- [Logs](/zh-TW/app/logs)：每一筆請求，以及送出它的 key。
- [Groups](/zh-TW/app/groups)：用你自己取的模型名稱的端點，一樣用這些 key 呼叫。
- [安裝與登入](/zh-TW/cli/install)：建立裝置的 CLI。
- [設定 agent](/zh-TW/cli/agents)：自動替每個 agent 和裝置建立 key。
