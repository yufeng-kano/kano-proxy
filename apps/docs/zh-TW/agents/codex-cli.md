---
title: Codex CLI
description: 把 Codex CLI 指向 Kano Proxy 作為自訂 model provider，用任何已連接的模型執行，不限 Codex 模型。
---

# Codex CLI

可以用。Codex CLI 講的是 OpenAI Responses API，Kano Proxy 在 `<base_url>/responses` 提供它。Models 頁面上的任何模型 id 都能當 Codex 的模型：Codex 模型原樣透傳到你的 ChatGPT 訂閱，其他供應商由 proxy 即時轉換。

同名的兩件事要分開看：

- **Codex 作為供應商**：在 Providers 頁面連接你的 ChatGPT 訂閱，就能從這份清單上的任何工具呼叫 `codex/gpt-5.4` 等 Codex 模型。
- **Codex CLI 作為客戶端**：就是這一頁。CLI 把請求送到 proxy，proxy 依模型 id 轉給對應的供應商。

## 設定

在 `~/.codex/config.toml` 加一個 provider 並選用它：

```toml
model = "claude-code/claude-opus-5"
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

- `model` 可以填 Models 頁面上的任何 id：`codex/gpt-5.4`、`claude-code/claude-opus-5`、`grok/grok-4.5`、`antigravity/gemini-3-flash`，或自訂端點的 `<slug>/<model>`。用 `codex --model <id>` 或 CLI 內的 `/model` 切換。
- 群組端點也可以：`base_url` 設成 `https://<your-domain>/g/<group-slug>/openai/v1`，`model` 填群組定義的名稱。
- 目前的 Codex 版本只接受 `wire_api = "responses"`。

## 使用時會看到什麼

- **未知模型警告。** 填非 Codex 的 id 時，Codex 啟動會印 `Model metadata for "<id>" not found. Defaulting to fallback metadata`。這無害：Codex 改用通用設定（272k context window、普通 function tools）照常執行。
- **推理強度。** 用 fallback 設定時 Codex 預設不送 effort。在設定檔加 `model_reasoning_effort = "high"`（或 `low`、`medium`、`xhigh`）就會送出，proxy 會壓到目標供應商接受的範圍。
- **Claude 模型的 prompt cache。** 用 `claude-code/...` 模型時，proxy 會自動放 Anthropic 的 `cache_control` 斷點（Codex 的 wire 格式無法表達），所以每一輪工具呼叫都會從快取讀到上一輪的內容，Logs 頁從同一個 session 的第二個請求起就會顯示 cache read。
- **網頁搜尋。** Codex 每個請求都會帶它的託管 web-search 工具。用 Codex 模型時直接透傳、正常運作。用其他模型時，proxy 換成一個 stub 工具，告訴模型這裡沒有網頁搜尋。模型若還是呼叫，Codex 自己會回 `unsupported call: web_search` 給模型，回合繼續進行。在設定檔加 `web_search = "disabled"` 可以完全拿掉這個工具。
- **子代理、計畫、目標。** Codex 自己的工具（`exec_command`、`spawn_agent`、`update_plan` 等）都是普通 function tools，每個模型都能用。
- **不支援：** `previous_response_id`、已儲存的回應、遠端 compaction，以及 Codex 模型上網頁搜尋以外的託管工具。Codex 對自訂 provider 本來就不會用到這些。

## 查證來源

Codex CLI 0.150.1，2026-09-04 對本機端點錄下的請求。設定文件：[learn.chatgpt.com](https://learn.chatgpt.com/docs/config-file/config-reference)。
