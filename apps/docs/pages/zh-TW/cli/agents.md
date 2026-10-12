---
title: 設定 agent
description: 用 kano-proxy agent setup 一步把 Claude Code 或 Codex CLI 接到 Kano Proxy，用 agent list 查看寫了什麼，用 agent restore 還原。
---

# 設定 agent

`kano-proxy agent setup` 一個指令就能把這台機器上的 coding agent 接到你的 proxy，`kano-proxy agent restore` 則能原樣還原。目前支援 **Claude Code**（`claude-code`）和 **Codex CLI**（`codex`）。Cursor、Cline 和 OpenCode 請照 Coding agent 設定底下各自頁面的手動步驟。

機器要先登入（[安裝與登入](/zh-TW/cli/install)）。

## 設定

```bash
kano-proxy agent setup claude-code
kano-proxy agent setup codex
```

指令會讀取你帳號能用的模型，跟 Models 頁的清單相同，讓你選擇由哪些模型回應這個 agent。接著它會：

1. 建立或更新名為 `claude-code` 或 `codex-cli` 的 model group。group 裡的模型名稱是 agent 認得的原始 id，例如 `claude-opus-5`（僅為範例），每個名稱會把請求送到你選的目標。
2. 建立一把名為 `<agent>@<裝置名稱>` 的 API key，例如 `claude-code@my-mac`。重新執行時，會先撤銷上次建立的 key，再建立新的。
3. 第一次執行時先備份 agent 的設定檔，再把 proxy 的網址、key 和模型寫進去。
4. 確認 group 端點列出了剛寫入的名稱。這一步只讀取模型清單，不會送出任何模型請求。

指令不會憑空寫入模型。Claude Code 需要你的帳號至少有一個模型，哪個供應商都可以。Codex CLI 需要已連接的 Codex（ChatGPT）帳號，因為 Codex 啟動時用的名稱必須是它自己的模型。條件不符時，指令會直接說明並停止，不會寫入一定會失敗的模型名稱。

每個 agent 的設定檔會寫入哪些欄位，寫在各自的頁面：[Claude Code](/zh-TW/agents/claude-code)、[Codex CLI](/zh-TW/agents/codex-cli)。

::: details 為什麼要透過 model group
coding agent 會依據它看到的模型名稱決定行為。Claude Code 依真正的 Claude id 決定要開啟哪些功能，並填入 Fable、Opus、Sonnet、Haiku 四個位置。Codex CLI 從內建的 catalog 讀取 context window 和 effort 等級，而 catalog 只認得 `gpt-5.6-sol`，不認得 `codex/gpt-5.6-sol`。group 端點對外提供這些原始名稱，再把每個名稱轉給你選的目標，哪個供應商都可以。所以 agent 看到的是它預期的名稱，proxy 卻可以先交給別的供應商回應，某個目標無法使用時再換下一個。這個 group 就是一般的 group，之後可以直接在 [Groups](/zh-TW/app/groups) 頁調整目標，不用重新執行 CLI。
:::

### 選擇模型

設定畫面一開始會列出所有要寫入的項目和目前的值。重新執行時，會從現有的設定開始。

- **Claude Code**：每個位置一列（Fable、Opus、Sonnet、Haiku），顯示 Claude Code 看到的名稱，以及依序回應的模型。Claude Code 的背景工作（例如產生對話標題）也跑在 Haiku，所以這個位置請放便宜的模型。
- **Codex CLI**：Codex 啟動時用的模型、它的 reasoning effort（`low`、`medium`、`high` 或 `xhigh`，預設 `high`），以及依序在背後回應的模型。

畫面上的按鍵：

- Enter 打開一個項目。在模型清單裡，Space 切換開或關；開啟的順序就是嘗試的順序，會標成 `[1]`、`[2]` 依此類推。
- Enter 套用變更並回到清單；Esc 不套用直接回去。
- 改過的列會標上 `(changed)`。在清單按 **Tab** 才會一次存檔，之前什麼都不會寫入。
- 連按兩次 Esc 離開。有未儲存的變更時會先問 `Save changes?`：Y 存檔，N 不存直接離開，Esc 回去繼續編輯。

### 不用互動畫面

```bash
kano-proxy agent setup claude-code --no-tui \
  --opus claude-code/claude-opus-5 \
  --haiku claude-code/claude-haiku-4-5

kano-proxy agent setup codex --no-tui --model gpt-5.6-sol --effort high
```

上面的模型 id 只是範例，請換成你 Models 頁上的 id。

- Claude Code：`--fable`、`--opus`、`--sonnet`、`--haiku` 各接一串以逗號分隔的 `provider/model` id，順序就是嘗試的順序。只寫 id 代表 `claude-code/<id>`。至少要指定一個位置。
- Codex：`--model` 是 Codex 啟動時用的原始 id，`--effort` 是 reasoning effort，`--target`（可重複，依序）是在背後回應的模型。只寫 id 代表 `codex/<id>`。沒有 `--target` 時由該模型本身回應。

兩個指令都可以加 `--dry-run`：印出所有會寫入的欄位，以及會建立或修改的 group，但不寫檔、不建立 key、不改 group。key 會顯示成 `<new key>`。

## 查看設定狀態

```bash
kano-proxy agent list
```

每個 agent 一列：這台機器有沒有設定過、group、key 的名稱、寫入時間，以及設定檔是否還是 CLI 寫入時的樣子：

- `ok`：沒有變動。
- `drifted`：你或 agent 改過 CLI 寫入的欄位。
- `missing`：設定檔不見了。

這個指令不連伺服器。`kano-proxy status` 會連同登入狀態一起顯示這些資訊。

## 還原

```bash
kano-proxy agent restore claude-code
```

預設只還原 setup 寫過的欄位：原本就有的欄位改回舊值，原本沒有的欄位刪除。檔案裡的其他內容都保留，包括 agent 之後自己寫入的東西，例如 Claude Code 用 `/model` 存的預設模型。Codex 的 `[model_providers.kano]` 區段會被移除，產生的 `kano-models.json` 會被刪除；如果你原本就有這個檔，會從備份放回去。

伺服器端，setup 建立的 key 會被撤銷。model group 會保留，因為其他機器或你自己的工具可能還在用它的端點。加上 `--remove-group` 才會一起刪除。

選項：

- `--full`：用 CLI 第一次寫入前的備份，整個取代設定檔。之後對這個檔案的所有修改都會消失，所以 CLI 會先顯示差異並詢問。取代前會再備份一次目前的檔案。`--backup <路徑>` 可以指定其他備份。
- `--dry-run`：只印出計畫，不做任何改動。
- `--force`：略過確認。使用 `--no-tui` 時，如果欄位被改過或使用 `--full`，必須加上它。

如果你改過 CLI 寫入的欄位，restore 會列出來，覆寫前先問你，絕不會默默蓋掉你的修改。

`kano-proxy logout` 會先還原這台機器設定過的所有 agent，再讓機器登出。

## 檔案位置

| | macOS 與 Linux | Windows |
|-|----------------|---------|
| Claude Code 設定 | `~/.claude/settings.json` | `%USERPROFILE%\.claude\settings.json` |
| Codex CLI 設定 | `~/.codex/config.toml` 與 `~/.codex/kano-models.json` | `%USERPROFILE%\.codex\` |
| 備份 | `~/.config/kano-proxy/backups/<agent>/` | `%APPDATA%\kano-proxy\backups\` |

CLI 只會寫入 Claude Code 的使用者設定檔，不會碰專案裡的 `.claude/settings.json`，因為那個檔案可能會被 commit。備份檔只有你自己能讀，CLI 也不會刪除它們。

## 相關頁面

- [Claude Code](/zh-TW/agents/claude-code)、[Codex CLI](/zh-TW/agents/codex-cli)：同樣的設定如何手動完成，以及各工具的限制。
- [Groups](/zh-TW/app/groups)：`claude-code` 和 `codex-cli` 這兩個 group。
- [Access](/zh-TW/app/access)：setup 建立的 key，顯示在對應的裝置下。
