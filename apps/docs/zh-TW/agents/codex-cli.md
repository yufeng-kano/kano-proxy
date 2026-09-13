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

Codex 的 `/model` 選單和每個模型的資料（context window、可用的 effort）都來自一份 catalog。接自訂 provider 時它用的是 CLI 內建那份，裡面有 `gpt-5.6-sol` 但沒有 `codex/gpt-5.6-sol`，所以每個 proxy 的 id 都算未知模型：用 fallback 設定執行，也不會出現在選單。`model_catalog_json` 可以用你自己的檔案取代這份 catalog。

1. 先把 Codex 現有的 catalog 倒出來，裡面含有每個項目必須帶的 instructions：

   ```bash
   codex debug models > ~/.codex/models.json
   ```

2. 編輯 `models.json`。留下你會用的項目，把每個 `slug` 加上 `codex/` 前綴。要加其他供應商的模型，複製一個項目，改 `slug`、`display_name`、`context_window`、`max_context_window`、`default_reasoning_level`、`supported_reasoning_levels` 成那個模型的實際值。複製來的 `base_instructions` 要保留，手寫的項目少了它會在啟動時被拒絕。
3. 在設定檔指向這個檔案。這個 key 是頂層的：要放在 `[model_providers.kano]` 上面，放在它下面會被當成那個 table 的欄位而被忽略。

   ```toml
   model_catalog_json = "/Users/<you>/.codex/models.json"
   ```

4. 不送請求就能檢查結果：

   ```bash
   codex debug models
   ```

   輸出會剛好是你檔案裡的項目，`codex/gpt-5.6-sol` 和其他的，帶著你給的 context window 和 effort。選單現在會列出這些 id，下面說的未知模型警告也不會再出現。

proxy 在每個供應商都接受 `low`、`medium`、`high`、`xhigh`，`claude-code` 另外接受 `max`；完整規則見 [端點與模型 id](/zh-TW/guide/endpoints)。其他 effort 不要放進 `supported_reasoning_levels`，選到就會讓請求失敗。

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
