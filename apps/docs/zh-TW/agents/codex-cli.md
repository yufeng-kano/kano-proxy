---
title: Codex CLI
description: 把 Codex CLI 指向 Kano Proxy 作為 model provider，繼續用你的 Codex 模型，也能在裡面執行其他已連接的模型。
---

# Codex CLI

可以用。Codex CLI 講的是 OpenAI Responses API，Kano Proxy 在 `<base_url>/responses` 提供它。用 Codex 模型時請求原樣透傳到你的 ChatGPT 訂閱，CLI 的行為和直接對 OpenAI 一樣，只是多了 proxy 的帳號池、容錯移轉和 Logs。

同名的兩件事要分開看：

- **Codex 作為供應商**：在 Providers 頁面連接你的 ChatGPT 訂閱，它的模型會以 `codex/<model>` 出現在這份清單上的每個工具裡。
- **Codex CLI 作為客戶端**：就是這一頁。CLI 把請求送到 proxy，proxy 依模型 id 轉給對應的供應商。

## 設定

在 `~/.codex/config.toml` 加一個 provider 並選用它。下面的模型 id 只是範例，請從你的 Models 頁面挑一個。

```toml
model = "codex/gpt-5.6-sol"
model_provider = "kano"

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

- 目前的 Codex 版本只接受 `wire_api = "responses"`。
- 群組端點也可以：`base_url` 設成 `https://<your-domain>/g/<group-slug>/openai/v1`，`model` 填群組定義的名稱。

## 列出模型

你能用哪些模型取決於你連接了哪些帳號。從 app 的 **Models** 頁面看即時清單，或用 API：

```bash
curl https://<your-domain>/openai/v1/models -H "Authorization: Bearer <your-api-key>"
```

Codex 模型會以 `codex/<model>` 出現。用 `codex --model <id>` 或 CLI 內的 `/model <id>` 切換，按 Enter 存成預設。

### 讓 proxy 的模型 id 出現在選單裡

Codex 的 `/model` 選單只認得它 catalog 裡的模型，而那份 catalog 沒有 `codex/...` 這種 id。這個網站放了一份做好的 catalog，含 GPT-6 和 GPT-5.6 系列。下載後在設定檔指向它：

```bash
curl -o ~/.codex/models.json https://<your-domain>/docs/codex/models.json
```

```toml
model_catalog_json = "models.json"
```

這行放在 `config.toml` 最上面、`[model_providers.kano]` 之前，因為它是頂層的 key。選單就會列出 `codex/gpt-6-astra`、`codex/gpt-5.6-sol`、`codex/gpt-5.6-terra`、`codex/gpt-5.6-luna`，帶著實際的 context window 和 proxy 接受的 effort，下面說的未知模型警告對這些模型也不會再出現。檔案裡的項目是從 Codex CLI 自己的 catalog 複製來的，含它的 instructions，只有 slug 加了 `codex/` 前綴。

要把 Claude、Grok、Gemini 或自訂端點的模型加進選單，複製檔案裡的一個項目，把 `slug` 改成它在 proxy 的 id，再改 `display_name`、`context_window`、`max_context_window`。`codex debug models` 會印出 Codex 實際會用的內容，不會送任何請求。

## 推理強度

對自訂 provider，Codex 預設不送 effort，需要自己設定：

```toml
model_reasoning_effort = "high"
```

proxy 接受 `low`、`medium`、`high`、`xhigh`、`max`，`minimal` 和 `ultra` 會回 `400 invalid reasoning.effort`。Codex 模型的上限是 `xhigh`：`max` 送上游前會降成 `xhigh`。其他供應商也一樣，壓到該供應商接受的最高強度。

## 其他模型

Models 頁面上的任何 id 都能當 Codex 的模型：`claude-code/claude-opus-5`、`grok/grok-4.5`、`antigravity/gemini-3-flash`，或自訂端點的 `<slug>/<model>`。proxy 會即時轉換 Responses 請求和串流。差別在於：

- **未知模型警告。** 填非 Codex 的 id 時，Codex 啟動會印 `Model metadata for "<id>" not found. Defaulting to fallback metadata`。這無害：Codex 改用通用設定（272k context window、普通 function tools）照常執行。
- **Claude 模型的 prompt cache。** 用 `claude-code/...` 模型時，proxy 會自動放 Anthropic 的 `cache_control` 斷點（Codex 的 wire 格式無法表達），所以每一輪工具呼叫都會從快取讀到上一輪的內容，Logs 頁從同一個 session 的第二個請求起就會顯示 cache read。
- **網頁搜尋。** Codex 每個請求都會帶它的託管 web-search 工具。用 Codex 模型時直接透傳、正常運作。用其他模型時，proxy 換成一個 stub 工具，告訴模型這裡沒有網頁搜尋。模型若還是呼叫，Codex 自己會回 `unsupported call: web_search` 給模型，回合繼續進行。在設定檔加 `web_search = "disabled"` 可以完全拿掉這個工具。
- **子代理、計畫、目標。** Codex 自己的工具（`exec_command`、`spawn_agent`、`update_plan` 等）都是普通 function tools，每個模型都能用。
- **不支援：** `previous_response_id`、已儲存的回應、遠端 compaction，以及 Codex 模型上網頁搜尋以外的託管工具。Codex 對自訂 provider 本來就不會用到這些。

## 查證來源

Codex CLI 0.150.1，2026-09-04 對本機端點錄下的請求。catalog 步驟以 Codex CLI 0.154.0 和 `codex debug models` 於 2026-09-13 驗證。設定文件：[learn.chatgpt.com](https://learn.chatgpt.com/docs/config-file/config-reference)。
