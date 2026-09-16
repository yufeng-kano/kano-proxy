# Kano Proxy

<p align="center">
  <strong>將你的 AI 訂閱轉化為標準 OpenAI 與 Anthropic API 介面。</strong>
</p>

<p align="center">
  <a href="./README.md">English</a> · <a href="./README.zh-TW.md">繁體中文</a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-server-000000?style=flat-square&logo=rust&logoColor=white" alt="Rust" />
  <img src="https://img.shields.io/badge/PostgreSQL-storage-4169E1?style=flat-square&logo=postgresql&logoColor=white" alt="PostgreSQL" />
  <img src="https://img.shields.io/badge/Docker_Compose-self--hosted-2496ED?style=flat-square&logo=docker&logoColor=white" alt="Docker Compose" />
  <img src="https://img.shields.io/badge/OpenAI-Chat_Completions_%26_Responses-00A67E?style=flat-square" alt="OpenAI API" />
  <img src="https://img.shields.io/badge/Anthropic-Messages_API-D97706?style=flat-square&logo=anthropic&logoColor=white" alt="Anthropic API" />
  <img src="https://img.shields.io/badge/隱私-零對話日誌-059669?style=flat-square" alt="隱私" />
  <img src="https://img.shields.io/badge/授權-MIT-blue?style=flat-square" alt="授權" />
</p>

---

**Kano Proxy** 是一套自架、多租戶的代理系統，專為開發者與編程代理（Coding Agents）打造。綁定你的 AI 訂閱帳號（Claude Code、ChatGPT Codex、SuperGrok、Google AI Pro/Ultra）、串接任何相容 OpenAI / Anthropic 的第三方端點，或把自己電腦上跑的本地 LLM 也接進來，全部匯集成具備自動容錯移轉（Failover）的帳號池，再透過標準的 OpenAI 與 Anthropic API 呼叫。

一支 Rust 伺服器加一個 PostgreSQL，用 docker compose 跑在自己掌控的機器上。上游 OAuth 憑證僅保留在伺服器端，客戶端只需使用系統核發的專屬 API Key。

> **想直接使用？**
> 線上託管版本已上線：[kano-proxy.yuufeng.com](https://kano-proxy.yuufeng.com)。

---

## 核心特色

- **在任意 Coding Agent 自由混用模型** — 直接在 **Claude Code 裡跑 GPT 與 Gemini**，或在 **Cursor、Cline、Codex CLI 裡跑 Claude 與 Grok**。Chat Completions、Responses API 與 Anthropic Messages 三種協議雙向即時轉譯，工具不再被單一供應商綁死。
- **多帳號池與自動切換** — 每個供應商可綁定多個帳號。遇到 Rate Limit（429/403）時，系統暫時停用該帳號並自動移轉至下一個可用帳號。
- **模型群組即虛擬端點** — 每個群組有自己的 Base URL（`/g/<slug>/openai/v1`、`/g/<slug>/anthropic`），群組內的模型名稱展開成有序的 `provider/model` 目標清單：客戶端模型對應與跨供應商容錯，一個機制搞定。
- **自帶端點** — 註冊任何相容 OpenAI / Anthropic 的 API（Base URL + Key），它就和內建供應商一樣可在兩種協議上使用。
- **本地 LLM 不需公開網址** — `kano-proxy` CLI 主動對外建立 WebSocket 連線，把你電腦上的 Ollama、LM Studio、vLLM 或 llama.cpp 變成正式的供應商。不需 cloudflared、ngrok 或開通連接埠。
- **用量儀表板與花費上限** — 即時掌握各帳號 5 小時與每週額度，檢視每次請求的估算費用，並可替每把 API Key 設定每日 / 每週 / 每月的美元上限。
- **隱私至上設計** — 預設絕不記錄、儲存任何提示詞（Prompts）或生成內容。
- **專為 Coding Agent 最佳化** — 零緩衝 SSE 即時串流並支援中途取消、Tool Calling、Vision 圖片輸入、音訊輸入與語音轉文字、思考推理深度（Reasoning Effort）對應及 Anthropic 提示詞快取（Prompt Caching）透傳。

---

## 運作架構

<p align="center">
  <img src="./docs/assets/how-it-works.zh-TW.svg" alt="Kano Proxy 運作架構" width="100%" />
</p>

1. **登入**：透過 Google 帳號登入 Web 管理介面。
2. **綁定**：授權綁定訂閱帳號（Claude Code、Codex、Grok、Antigravity）、新增自訂端點，或用 CLI 接上本地 LLM。
3. **建立金鑰**：產生專屬的 Kano API Key（`sk-kano-proxy-...`）。
4. **設定工具**：將 Claude Code、Codex CLI、Cursor、Cline、Aider 或任何 SDK 指向 Kano Proxy 即可開始使用。

---

## 快速上手

### 1. 介面端點（Base URL）

| 協議格式 | 端點 URL | 適用客戶端工具 |
|---|---|---|
| **OpenAI 相容** | `https://<your-domain>/openai/v1` | Codex CLI, Cursor, Cline, Roo Code, Aider, CC Switch, OpenAI SDK |
| **Anthropic Messages** | `https://<your-domain>/anthropic` | Claude Code CLI, Anthropic SDK, Claude 格式工具 |
| **模型群組** | `https://<your-domain>/g/<slug>/openai/v1` · `/g/<slug>/anthropic` | 同上，改用群組內定義的模型名稱 |

OpenAI 相容端點同時提供 **Chat Completions**（`/chat/completions`）、**Responses API**（`/responses`，Codex CLI 使用的協議）、`/models` 與 `/audio/transcriptions`。Codex 模型在此原生透傳到你的 ChatGPT 訂閱，其他供應商由 proxy 即時轉換。

### 2. 身份驗證

```http
Authorization: Bearer sk-kano-proxy-...
```
*(Anthropic 格式客戶端亦支援 `x-api-key: sk-kano-proxy-...`)*

### 3. 模型名稱格式

在兩種協議端點上，皆使用統一的 `provider/model` 命名格式：

- `claude-code/claude-opus-5` / `claude-code/claude-sonnet-5`
- `codex/gpt-5.6-sol`
- `grok/grok-4.5`
- `antigravity/gemini-3-flash`
- `<custom-slug>/<model-name>`：自訂端點
- `<cli-slug>/<local-model>`：透過 CLI 接入的本地 LLM

---

## 實用混用範例

### 在 Claude Code 裡直接執行 GPT 與 Gemini

將 Claude Code CLI 指向 Kano Proxy 的 `/anthropic` 端點，即可使用 GPT 或 Gemini 模型，且原生支援工具呼叫：

```bash
export ANTHROPIC_BASE_URL="https://<your-domain>/anthropic"
export ANTHROPIC_API_KEY="sk-kano-proxy-..."

# 在 Claude Code 內使用 GPT 或 Gemini
claude --model codex/gpt-5.6-sol
# 或
claude --model antigravity/gemini-3-flash
```

### 讓 Codex CLI 走 proxy

在 `~/.codex/config.toml` 把 Kano Proxy 加成 model provider。Codex 模型在 CLI 原生的 Responses 協議上直接透傳；Models 頁面上的其他模型 id（`claude-code/...`、`antigravity/...`）也能填在同一個位置：

```toml
model = "codex/gpt-5.6-sol"
model_provider = "kano"
model_catalog_json = "kano-models.json"  # 讓 /model 列出 codex/... 的 id，產生方式見文件

[model_providers.kano]
name = "Kano Proxy"
base_url = "https://<your-domain>/openai/v1"
experimental_bearer_token = "<your-api-key>"
wire_api = "responses"
```

### 在 Cursor / OpenAI SDK 使用 Claude

將任何支援 OpenAI 格式的工具指向 `/openai/v1`：

```bash
curl https://<your-domain>/openai/v1/chat/completions \
  -H "Authorization: Bearer sk-kano-proxy-..." \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-code/claude-sonnet-5",
    "messages": [{"role": "user", "content": "請用 Rust 寫一個快速排序演算法。"}],
    "stream": true
  }'
```

### 把本地 Ollama 接進 proxy

安裝 CLI（Homebrew、Scoop 或安裝腳本），讓這台機器登入一次，註冊本地端點，然後讓通道持續運行：

```bash
curl -fsSL https://raw.githubusercontent.com/yufeng-kano/kano-proxy/main/scripts/install-cli.sh | sh
# 或：brew install yufeng-kano/tap/kano-proxy

kano-proxy init     # 開啟瀏覽器授權此裝置，貼回一次性代碼
kano-proxy add      # slug、openai|anthropic、http://localhost:11434/v1
kano-proxy start    # 每個已註冊的供應商一條對外 WebSocket
```

本地模型隨即以 `<slug>/<model>` 出現在上述所有端點，也能當作模型群組的目標。細節見 [docs/cli.md](./docs/cli.md)。

---

## 支援的供應商

| 供應商 | 上游來源 | 多帳號池支援 |
|---|---|:---:|
| <img src="https://img.shields.io/badge/Claude_Code-D97706?style=flat-square&logo=anthropic&logoColor=white" alt="Claude Code" height="20" /> | Anthropic Claude Pro / Team OAuth | <img src="https://img.shields.io/badge/支援-10B981?style=flat-square" alt="支援" height="18" /> |
| <img src="https://img.shields.io/badge/ChatGPT_Codex-00A67E?style=flat-square" alt="Codex" height="20" /> | ChatGPT Plus / Team / Pro (Codex Backend) | <img src="https://img.shields.io/badge/支援-10B981?style=flat-square" alt="支援" height="18" /> |
| <img src="https://img.shields.io/badge/xAI_Grok-000000?style=flat-square&logo=x&logoColor=white" alt="Grok" height="20" /> | xAI SuperGrok OAuth | <img src="https://img.shields.io/badge/支援-10B981?style=flat-square" alt="支援" height="18" /> |
| <img src="https://img.shields.io/badge/Google_Antigravity-4285F4?style=flat-square&logo=google&logoColor=white" alt="Antigravity" height="20" /> | Google AI Pro / Ultra (CloudCode API) | <img src="https://img.shields.io/badge/支援-10B981?style=flat-square" alt="支援" height="18" /> |
| <img src="https://img.shields.io/badge/Custom_Endpoints-6366F1?style=flat-square&logo=fastapi&logoColor=white" alt="Custom Endpoints" height="20" /> | 任何相容 OpenAI / Anthropic 的 API | <img src="https://img.shields.io/badge/支援-10B981?style=flat-square" alt="支援" height="18" /> |
| <img src="https://img.shields.io/badge/Local_LLMs_(CLI)-0891B2?style=flat-square&logo=ollama&logoColor=white" alt="Local LLMs" height="20" /> | 你電腦上的 Ollama、LM Studio、vLLM、llama.cpp，透過 `kano-proxy` CLI 通道 | <img src="https://img.shields.io/badge/支援-10B981?style=flat-square" alt="支援" height="18" /> |

---

## 本地開發與部署

四個容器各司其職：PostgreSQL、API 伺服器、Web 靜態站（管理介面加公開的 `/docs/` 文件站）、負責 TLS 與路由的 Caddy。

```bash
git clone https://github.com/yufeng-kano/kano-proxy.git
cd kano-proxy
cp .env.example .env     # 填好網域、Google OAuth client 與密鑰
docker compose up -d --build
```

把網域 DNS 指到這台機器並開放 80、443 連接埠。Caddy 會在第一次請求時取得憑證，伺服器啟動時自動套用資料庫遷移。若已有自己的反向代理，拿掉 `caddy` 服務、依部署文件自行把 API 路徑轉到伺服器即可。

每個 app 各自安裝、各自建置，專案根目錄沒有共用的套件設定檔。

```bash
cd apps/api  && cargo test --workspace
cd apps/cli  && cargo test
cd apps/web  && pnpm install && pnpm typecheck && pnpm build
cd apps/docs && pnpm install && pnpm build
```

設定、憑證、資料庫遷移與發版流程請參閱 [docs/deployment.md](./docs/deployment.md)，架構說明在 [docs/rust-server.md](./docs/rust-server.md)。

---

## 完整文件

- [產品目標與模型命名](./docs/product.md)
- [API 參考與協議映射](./docs/api.md)
- [身份認證與帳號池機制](./docs/auth.md)
- [供應商、路由與容錯策略](./docs/providers.md)
- [CLI 與本地 LLM 通道](./docs/cli.md)
- [部署](./docs/deployment.md)
- [文件總覽導覽](./docs/index.md)

---

## 授權

[MIT](./LICENSE)
