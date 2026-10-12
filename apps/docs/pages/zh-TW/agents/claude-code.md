---
title: Claude Code
description: 用一道 CLI 指令，或手動用 ANTHROPIC_BASE_URL 與 ANTHROPIC_AUTH_TOKEN 把 Claude Code 指向 Kano Proxy，並選擇由哪個 provider/model 回應。
---

# Claude Code

完整可用。Claude Code 用的是 Anthropic Messages API，proxy 原生支援，而且任何已連接的供應商都能回應，不限於 Claude。

## 用 CLI 設定

先裝好 [kano-proxy CLI](/zh-TW/cli/install)：

```console
$ kano-proxy login                      # 每台機器一次
$ kano-proxy agent setup claude-code
```

1. 替 Claude Code 的每個槽位（Fable、Opus、Sonnet、Haiku）依序選 proxy 上由哪些模型回答。任何供應商都可以。第一個負責回答，它被暫停或額度用完時，由後面的依序接手。
2. CLI 會建立名為 `claude-code` 的模型群組，建立一把以這台機器命名的 API key，再把指向這個群組的 `env` 區塊寫進 `~/.claude/settings.json`。
3. 啟動 `claude`。

Haiku 槽位請放便宜的模型：Claude Code 的背景工作（例如產生 session 標題）也跑在這裡。

::: details 為什麼 CLI 要透過模型群組
Claude Code 會依它看到的模型 id 決定開啟哪些功能。群組端點 `/g/claude-code/anthropic` 回的是 `claude-opus-5` 這種原本的 Claude id，就算 proxy 實際轉給別的供應商也一樣。所以一個槽位可以先由 `antigravity` 或 `codex` 回答，Claude Code 看到的仍是 Claude id。群組的說明見 [Groups](/zh-TW/app/groups)。

CLI 也會開啟 gateway model discovery，群組裡每個名稱都會出現在 `/model` 選單。它不會寫 `ANTHROPIC_MODEL`，所以你用 `/model` 存的預設會保留。
:::

要還原設定，或查看 CLI 管理了哪些項目，見[設定 agent](/zh-TW/cli/agents)。群組會留在 **Groups** 頁面，隨時可以調整順序或加入備援目標。

## 手動設定

1. 在 **Access** 頁面建立一把 key。
2. 把下面這段加進 `~/.claude/settings.json`。`ANTHROPIC_DEFAULT_*_MODEL` 請換成你 **Models** 頁面上的 id，這裡的只是範例。

```json
{
  "disableClaudeAiConnectors": true,
  "env": {
    "ANTHROPIC_BASE_URL": "https://<your-domain>/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "<your-api-key>",
    "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY": "1",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-code/claude-opus-5",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-code/claude-sonnet-5",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-code/claude-haiku-4-5",
    "ANTHROPIC_DEFAULT_FABLE_MODEL": "claude-code/claude-fable-5-1"
  }
}
```

3. 啟動 `claude`。

不要把 key 放進專案裡會被 commit 的 `.claude/settings.json`。

::: details 每個設定的作用
- `env` 區塊裡的值套用到每次啟動，而且優先於 shell 的環境變數。
- `ANTHROPIC_BASE_URL` 是 gateway 根路徑，**不含** `/v1`。Claude Code 會自己接上 `/v1/messages`。
- `ANTHROPIC_AUTH_TOKEN` 以 `Authorization: Bearer` 送出。`ANTHROPIC_API_KEY` 也可以，會以 `x-api-key` 送出，proxy 兩種都接受。擇一即可。
- `ANTHROPIC_DEFAULT_*_MODEL` 決定 `/model` 選單裡 Fable、Opus、Sonnet、Haiku 各對應到哪個 id。Haiku 也是 Claude Code 跑背景工作的模型。
- `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` 讓 Claude Code 啟動時向 proxy 讀取 `GET /v1/models`，把結果加進 `/model` 選單。
- `disableClaudeAiConnectors: true` 放在頂層，和 `env` 同一層。它會關掉啟動時「claude.ai connectors 因為有其他驗證來源而停用」的提示。設了 proxy 的 token 之後，這些 connectors 本來就不會載入。
- 除非要固定啟動時的模型，否則不要設 `ANTHROPIC_MODEL`。它每次啟動都會蓋掉你用 `/model` 存的預設。
:::

只想試一次的話，也可以在 shell 匯出同一組變數：

::: code-group

```bash [macOS / Linux]
export ANTHROPIC_BASE_URL=https://<your-domain>/anthropic
export ANTHROPIC_AUTH_TOKEN=<your-api-key>
export ANTHROPIC_MODEL=claude-code/claude-opus-5
claude
```

```powershell [Windows]
$env:ANTHROPIC_BASE_URL = "https://<your-domain>/anthropic"
$env:ANTHROPIC_AUTH_TOKEN = "<your-api-key>"
$env:ANTHROPIC_MODEL = "claude-code/claude-opus-5"
claude
```

:::

## 切換模型

在 session 裡用 `/model <id>` 可以切到 proxy 上任何 id，例如 `/model codex/gpt-5.6-sol`。按 Enter 存成預設，按 `s` 只在這次 session 有效。

Gateway model discovery 只保留含 `claude` 或 `anthropic` 的 id。Claude 的 id 會出現在選單裡，`codex/...` 不會，要自己輸入。模型 id 的規則見[端點與模型 id](/zh-TW/reference/endpoints)。

## 確認可用

1. 在 Claude Code 裡執行 `/status`，會顯示 base URL 以及是否使用 auth token。
2. 送出一則訊息。幾秒內 [Logs](/zh-TW/app/logs) 頁面就會出現這筆請求。

## 限制

- Fast mode 與 WebFetch 的安全檢查會直接連 Anthropic，不看 base URL。
- Claude 桌面版 app 不讀這些變數。它有自己的第三方推論設定表單，把同一組 base URL 和 key 填進去即可。
- VS Code 擴充套件從 `claudeCode.environmentVariables` 設定讀取這些變數，不讀 shell。

## 查證來源

Claude Code 文件，2026-09-04：[Connect to an LLM gateway](https://code.claude.com/docs/en/llm-gateway-connect)、[Gateway protocol](https://code.claude.com/docs/en/llm-gateway-protocol)、[Model configuration](https://code.claude.com/docs/en/model-config)。
