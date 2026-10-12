---
title: Codex CLI
description: 把 Codex CLI 指向 Kano Proxy，產生模型目錄讓 /model 列出 proxy 的模型，也能在裡面執行其他已連接的模型。
---

# Codex CLI

可以用。Codex CLI 用的是 OpenAI Responses API，proxy 在 `<base_url>/responses` 提供；用 Codex 模型時，行為和直接連 OpenAI 一樣，只是多了 proxy 的帳號池、容錯移轉和紀錄。

這一頁講的是把 Codex CLI 當成**客戶端**。把 Codex 當成**供應商**，也就是讓你的 ChatGPT 訂閱以 `codex/<model>` 出現，是在 [Providers](/zh-TW/app/providers) 頁面連接。

## 用 CLI 設定

先裝好 [kano-proxy CLI](/zh-TW/cli/install)：

```console
$ kano-proxy login                      # 每台機器一次
$ kano-proxy agent setup codex
```

1. 選預設模型、它的 reasoning effort，以及這個模型背後依序由 proxy 上哪些模型回答。任何供應商都可以。第一個負責回答，它被暫停或額度用完時，由後面的依序接手。
2. CLI 會建立名為 `codex-cli` 的模型群組，建立一把以這台機器命名的 API key，編輯 `~/.codex/config.toml`，再從你安裝的 Codex 產生 `~/.codex/kano-models.json`。`config.toml` 裡原有的設定和註解都會保留。
3. 啟動 `codex`。

::: details CLI 和手動設定有什麼不同
群組裡的模型名稱是原本的 id（`gpt-5.6-sol`，不是 `codex/gpt-5.6-sol`），所以不用改名，Codex 自己的目錄資料就能直接對上。目錄的產生方式和下面的腳本相同，但不需要 `jq`。群組的說明見 [Groups](/zh-TW/app/groups)。
:::

要還原設定，或查看 CLI 管理了哪些項目，見[設定 agent](/zh-TW/cli/agents)。

## 手動設定

三步：寫設定、產生模型目錄、啟動 `codex`。

### 1. 設定

在 `~/.codex/config.toml` 加一個 provider 並選用它。模型 id 只是範例，請換成你 **Models** 頁面上的 id。

```toml
model = "codex/gpt-5.6-sol"
model_provider = "kano"
model_reasoning_effort = "high"          # 只是預設，之後用 /model 改
model_catalog_json = "kano-models.json"  # 第 2 步產生，相對於 ~/.codex

[model_providers.kano]
name = "Kano Proxy"
base_url = "https://<your-domain>/openai/v1"
experimental_bearer_token = "<your-api-key>"
wire_api = "responses"
```

`model_catalog_json` 和 `model_reasoning_effort` 要放在 `[model_providers.kano]` 上面。它們是頂層的 key，寫在 table 標題下面就會變成那個 table 的欄位，然後被忽略。

::: details 其他寫法
- 不想把 key 寫在檔案裡，就把 `experimental_bearer_token` 換成 `env_key = "KANO_PROXY_API_KEY"`，再在 shell 匯出 `KANO_PROXY_API_KEY=<your-api-key>`。`~/.codex/auth.json` 只給 Codex 自己的 OpenAI 登入用，自訂 provider 不會讀。
- 目前的 Codex 版本只接受 `wire_api = "responses"`。
- 也可以用群組端點：`base_url` 設成 `https://<your-domain>/g/<group-slug>/openai/v1`，`model` 填群組裡的名稱。
:::

### 2. 模型目錄

執行對應系統的腳本，它會寫出 `~/.codex/kano-models.json`。Codex 升版後要重跑一次。

::: code-group

```bash [macOS / Linux]
codex debug models --bundled | jq '{models: [ .models[]
  | select(.visibility == "list")
  | .slug = "codex/" + (.slug | sub("^codex/"; ""))
  | .supported_reasoning_levels |= map(select(.effort | IN("low","medium","high","xhigh")))
  | .default_reasoning_level |= (if IN("low","medium","high","xhigh") then . else "medium" end)
]}' > ~/.codex/kano-models.json
```

```powershell [Windows]
$catalog = (codex debug models --bundled | Out-String) | ConvertFrom-Json
$keep = "low", "medium", "high", "xhigh"
$models = foreach ($m in $catalog.models) {
  if ($m.visibility -ne "list") { continue }
  $m.slug = "codex/" + ($m.slug -replace "^codex/", "")
  $m.supported_reasoning_levels = @($m.supported_reasoning_levels | Where-Object { $keep -contains $_.effort })
  if ($keep -notcontains $m.default_reasoning_level) { $m.default_reasoning_level = "medium" }
  $m
}
$json = @{ models = $models } | ConvertTo-Json -Depth 64
[IO.File]::WriteAllText("$HOME\.codex\kano-models.json", $json)
```

:::

新版 macOS 內建 `jq`；Linux 請用套件管理器安裝。

::: details 為什麼 Codex 需要自己的目錄
Codex 的 `/model` 選單和每個模型的資料（context window、effort 等級、instructions）都來自一份目錄。內建的目錄認得 `gpt-5.6-sol`，不認得 `codex/gpt-5.6-sol`。沒有自己的目錄時，proxy 的 id 都算未知模型：它會用 fallback 設定執行、不會出現在選單裡，effort 甚至可能根本沒送出去（[openai/codex#30697](https://github.com/openai/codex/issues/30697)）。

腳本會複製你這版 Codex 內建的目錄，只留 Codex 自己會列出的模型，每個 slug 加上 `codex/` 前綴，effort 等級只留 proxy 接受的。instructions、工具和 context window 都和 Codex 原本的一樣。
:::

### 3. 啟動

```bash
codex
```

在 TUI 裡輸入 `/model` 會列出你的 proxy 模型，選好模型再選 effort。選擇會寫回 `config.toml`，下次啟動直接沿用，不用加任何參數。臨時換模型仍可用 `codex --model <id>`。

你能用哪些模型，取決於你連接了哪些帳號。**Models** 頁面或 `GET https://<your-domain>/openai/v1/models` 才是即時清單，目錄只決定選單顯示什麼。

## 推理強度

Codex 模型最高到 `xhigh`：送 `max` 時，proxy 會先降成 `xhigh` 再轉給上游。`minimal` 和 `ultra` 會被拒絕。每個供應商的上限見[端點與模型 id](/zh-TW/reference/endpoints)。

## 使用其他模型

**Models** 頁面上的任何 id 都能當 Codex 的模型，例如 `claude-code/claude-opus-5`，或自訂端點的 `<slug>/<model>`。proxy 會即時轉換 Responses 請求和串流。

要讓它出現在選單裡：

1. 在 `kano-models.json` 複製一個產生好的項目。
2. 把 `slug` 改成 proxy 的 id。
3. 改好這個模型的 `display_name`、`context_window` 和 `max_context_window`。

複製來的 instructions 要保留。沒有 `base_instructions` 或 `model_messages.instructions_template` 的項目，啟動時會被拒絕。

用非 Codex 模型時會有這些差別：

- **未知模型警告。** 目錄裡沒有這個模型時，Codex 啟動會印出 `Model metadata for "<id>" not found. Defaulting to fallback metadata`。這不影響使用：Codex 會改用通用設定（272k context window、一般 function tools）照常執行。
- **Claude 模型的 prompt cache。** 用 `claude-code/...` 模型時，proxy 會自動加上 Anthropic 的 `cache_control` 斷點，因為 Codex 的格式表達不了。每一輪工具呼叫都會從快取讀到上一輪。從同一個 session 的第二個請求開始，[Logs](/zh-TW/app/logs) 頁面就會顯示 cache read token。
- **網頁搜尋。** Codex 每個請求都會帶上它的託管 web-search 工具。用 Codex 模型時會直接轉過去，可以正常使用。用其他模型時，proxy 會換成一個替代工具，告訴模型這裡不能搜尋網頁。模型若還是呼叫，Codex 會回 `unsupported call: web_search` 給模型，這一輪繼續進行。在設定檔加上 `web_search = "disabled"` 就能完全拿掉這個工具。
- **子代理、計畫、目標。** Codex 自己的工具（`exec_command`、`spawn_agent`、`update_plan` 等）都是一般的 function tools，每個模型都能用。

## 確認可用

1. 執行 `codex debug models`。它不送請求，直接印出 Codex 會用的目錄：`codex/gpt-6-astra`、`codex/gpt-5.6-sol` 等，各自附上 context window 和 effort 等級。
2. 啟動 `codex` 並送出一則訊息。幾秒內 [Logs](/zh-TW/app/logs) 頁面就會出現這筆請求。

## 限制

- 不支援 `previous_response_id`、已儲存的回應和遠端 compaction。Codex 對自訂 provider 本來就不會用到這些。
- 網頁搜尋以外的託管工具都不支援，而網頁搜尋也只有 Codex 模型能用。

## 查證來源

Codex CLI 0.150.1，2026-09-04 對本機端點錄下的請求。目錄腳本和 `codex debug models` 以 Codex CLI 0.154.0 於 2026-09-13 在 macOS 驗證；PowerShell 版依同樣邏輯寫成，未在 Windows 上執行過。設定文件：[learn.chatgpt.com](https://learn.chatgpt.com/docs/config-file/config-reference)。
