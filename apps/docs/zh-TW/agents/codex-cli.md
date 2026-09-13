---
title: Codex CLI
description: 把 Codex CLI 指向 Kano Proxy，產生模型目錄讓 /model 列出 proxy 的模型，也能在裡面執行其他已連接的模型。
---

# Codex CLI

可以用。Codex CLI 講的是 OpenAI Responses API，Kano Proxy 在 `<base_url>/responses` 提供它。用 Codex 模型時請求原樣透傳到你的 ChatGPT 訂閱，CLI 的行為和直接對 OpenAI 一樣，只是多了 proxy 的帳號池、容錯移轉和 Logs。

同名的兩件事要分開看：

- **Codex 作為供應商**：在 Providers 頁面連接你的 ChatGPT 訂閱，它的模型會以 `codex/<model>` 出現在這份清單上的每個工具裡。
- **Codex CLI 作為客戶端**：就是這一頁。CLI 把請求送到 proxy，proxy 依模型 id 轉給對應的供應商。

三步：寫設定、產生模型目錄、啟動 `codex`。

## 1. 設定

在 `~/.codex/config.toml` 加一個 provider 並選用它：

```toml
model = "codex/gpt-5.6-sol"
model_provider = "kano"
model_reasoning_effort = "high"          # 只是預設，之後用 /model 改
model_catalog_json = "kano-models.json"  # 第 2 步產生，相對於 ~/.codex

[model_providers.kano]
name = "Kano Proxy"
base_url = "https://<your-domain>/openai/v1"
env_key = "KANO_PROXY_API_KEY"
wire_api = "responses"
```

然後匯出金鑰：

```bash
export KANO_PROXY_API_KEY=<your-api-key>
```

- `model_catalog_json` 和 `model_reasoning_effort` 是頂層的 key：要放在 `[model_providers.kano]` 上面，放在它下面會被當成那個 table 的欄位而被忽略。
- 想把金鑰直接寫在檔案裡而不是環境變數，把 `env_key` 換成 `experimental_bearer_token = "<your-api-key>"`。
- 目前的 Codex 版本只接受 `wire_api = "responses"`。
- 群組端點也可以：`base_url` 設成 `https://<your-domain>/g/<group-slug>/openai/v1`，`model` 填群組定義的名稱。

## 2. 模型目錄

Codex 的 `/model` 選單和每個模型的資料（context window、effort 等級、instructions）都來自一份目錄。那份目錄認得 `gpt-5.6-sol`，不認得 `codex/gpt-5.6-sol`，所以沒有自己的目錄時 proxy 的 id 都是未知模型：用 fallback 設定執行、不會出現在選單，effort 甚至可能根本沒送出去（[openai/codex#30697](https://github.com/openai/codex/issues/30697)）。

下面的腳本複製你這個 Codex 版本內建的目錄，留下 Codex 自己會列出的模型，每個 slug 加上 `codex/` 前綴，effort 等級只留 proxy 接受的。instructions、工具設定、context window 都和 Codex 出廠時一模一樣。Codex 升版後重跑一次。

macOS / Linux（新版 macOS 內建 `jq`；Linux 用套件管理器安裝）：

```bash
codex debug models --bundled | jq '{models: [ .models[]
  | select(.visibility == "list")
  | .slug = "codex/" + (.slug | sub("^codex/"; ""))
  | .supported_reasoning_levels |= map(select(.effort | IN("low","medium","high","xhigh")))
  | .default_reasoning_level |= (if IN("low","medium","high","xhigh") then . else "medium" end)
]}' > ~/.codex/kano-models.json
```

Windows（PowerShell）：

```powershell
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

不送請求就能檢查結果：

```bash
codex debug models
```

它印出的就是 Codex 實際會用的目錄：`codex/gpt-6-astra`、`codex/gpt-5.6-sol` 等等，各自帶著 context window 和 effort 等級。

## 3. 使用

```bash
codex
```

在 TUI 裡輸入 `/model` 就會列出你的 proxy 模型，選模型後再選 effort。選擇會寫回 `config.toml`，下次啟動直接沿用，不需要任何參數。臨時切換仍可用 `codex --model <id>`。

你能用哪些模型取決於你連接了哪些帳號。app 的 **Models** 頁面或 `GET https://<your-domain>/openai/v1/models` 才是即時清單；目錄只決定選單顯示什麼。

## 推理強度

proxy 接受 `low`、`medium`、`high`、`xhigh`、`max`，`minimal` 和 `ultra` 會回 `400 invalid reasoning.effort`。Codex 模型的上限是 `xhigh`：`max` 送上游前會降成 `xhigh`。其他供應商也一樣，壓到該供應商接受的最高強度。完整規則見[端點與模型 id](/zh-TW/guide/endpoints)。

## 其他模型

Models 頁面上的任何 id 都能當 Codex 的模型：`claude-code/claude-opus-5`、`grok/grok-4.5`、`antigravity/gemini-3-flash`，或自訂端點的 `<slug>/<model>`。proxy 會即時轉換 Responses 請求和串流。

要讓它出現在選單，在 `kano-models.json` 加一個項目：複製產生出來的任一項目，把 `slug` 改成 proxy 的 id，再改 `display_name`、`context_window`、`max_context_window`。複製來的 instructions 要保留，沒有 `base_instructions` 或 `model_messages.instructions_template` 的項目會在啟動時被拒絕。用非 Codex 模型時的差別：

- **未知模型警告。** 沒有目錄項目時，Codex 啟動會印 `Model metadata for "<id>" not found. Defaulting to fallback metadata`。這無害：Codex 改用通用設定（272k context window、普通 function tools）照常執行。
- **Claude 模型的 prompt cache。** 用 `claude-code/...` 模型時，proxy 會自動放 Anthropic 的 `cache_control` 斷點（Codex 的 wire 格式無法表達），所以每一輪工具呼叫都會從快取讀到上一輪的內容，Logs 頁從同一個 session 的第二個請求起就會顯示 cache read。
- **網頁搜尋。** Codex 每個請求都會帶它的託管 web-search 工具。用 Codex 模型時直接透傳、正常運作。用其他模型時，proxy 換成一個 stub 工具，告訴模型這裡沒有網頁搜尋。模型若還是呼叫，Codex 自己會回 `unsupported call: web_search` 給模型，回合繼續進行。在設定檔加 `web_search = "disabled"` 可以完全拿掉這個工具。
- **子代理、計畫、目標。** Codex 自己的工具（`exec_command`、`spawn_agent`、`update_plan` 等）都是普通 function tools，每個模型都能用。
- **不支援：** `previous_response_id`、已儲存的回應、遠端 compaction，以及 Codex 模型上網頁搜尋以外的託管工具。Codex 對自訂 provider 本來就不會用到這些。

## 查證來源

Codex CLI 0.150.1，2026-09-04 對本機端點錄下的請求。目錄腳本和 `codex debug models` 以 Codex CLI 0.154.0 於 2026-09-13 在 macOS 驗證；PowerShell 版依同樣邏輯寫成，未在 Windows 上執行過。設定文件：[learn.chatgpt.com](https://learn.chatgpt.com/docs/config-file/config-reference)。
