---
title: Claude Code
description: 用一道 CLI 指令，或手動用 ANTHROPIC_BASE_URL 與 ANTHROPIC_AUTH_TOKEN 把 Claude Code 指向 Kano Proxy，並選擇由哪個 provider/model 回應。
---

# Claude Code

完整可用。Claude Code 講的是 Anthropic Messages API，proxy 原生支援。回應的不限於 Claude：把模型設成 `codex/`、`grok/` 或 `antigravity/` 開頭的 id，proxy 會轉換請求與串流。

## 用 CLI 一次設好

[kano-proxy CLI](/zh-TW/guide/local-models) 可以替你寫好這份設定：

```console
$ kano-proxy init                      # 每台機器一次
$ kano-proxy agent setup claude-code
```

它會讀取你的帳號實際能用的模型，讓你挑 Opus、Sonnet、Haiku 各對應到哪個模型（可選其他供應商的備援），建立一個名為 `claude-code` 的模型群組、群組裡的模型名稱就是原本的 Claude id，建立一把以這台機器命名的 API key，然後把下面那段 `env` 寫進 `~/.claude/settings.json`，`ANTHROPIC_BASE_URL` 指向群組端點。走群組是有原因的：Claude Code 會依它看到的模型 id 決定開啟哪些功能，而 `/g/claude-code/anthropic` 回的是 `claude-opus-5` 這種真實 id，即使 proxy 實際轉到別的供應商也一樣。

`kano-proxy agent restore claude-code` 會把寫過的欄位還原並撤銷 key；加 `--full` 則用事前備份整檔還原。`kano-proxy agent list` 顯示目前由 CLI 管理的項目。群組會留在 Groups 頁面，隨時可以調整順序或加備援目標。

## 手動設定

在 `~/.claude/settings.json` 加一段 `env`。這裡的值套用到每次啟動，且優先於 shell 環境變數。

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "https://<your-domain>/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "<your-api-key>",
    "ANTHROPIC_MODEL": "claude-code/claude-opus-5",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-code/claude-opus-5",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-code/claude-sonnet-5",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-code/claude-haiku-4-5"
  }
}
```

- `ANTHROPIC_BASE_URL` 是 gateway 根路徑，**不含** `/v1`。Claude Code 會自己接上 `/v1/messages`。
- `ANTHROPIC_AUTH_TOKEN` 以 `Authorization: Bearer` 送出。`ANTHROPIC_API_KEY` 也可以，會以 `x-api-key` 送出，proxy 兩種都接受。擇一使用。
- `ANTHROPIC_MODEL` 是啟動時的模型。三個 `ANTHROPIC_DEFAULT_*_MODEL` 決定 `/model` 選單裡 Opus、Sonnet、Haiku 各自對應到哪個 id。請填你 Models 頁面上有的 id。

一次性的 session 也可以直接在 shell 匯出同一組變數：

```bash
export ANTHROPIC_BASE_URL=https://<your-domain>/anthropic
export ANTHROPIC_AUTH_TOKEN=<your-api-key>
export ANTHROPIC_MODEL=claude-code/claude-opus-5
claude
```

不要把金鑰放進專案裡會被 commit 的 `.claude/settings.json`。

## 切換模型

在 session 內用 `/model <id>` 可以切到 proxy 上任何 id，例如 `/model codex/gpt-5.6-sol`。按 Enter 存成預設，按 `s` 只在這次 session 生效。

設定 `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` 後，Claude Code 會向 proxy 讀 `GET /v1/models`，把結果加進 `/model` 選單。它只保留含 `claude` 或 `anthropic` 字樣的 id，所以 `claude-code/...` 會出現，`codex/...` 不會，後者請手動輸入。

## 驗證

在 Claude Code 裡執行 `/status`，會顯示 base URL 以及是否使用 auth token。proxy 這邊的 **Logs** 頁面幾秒內就會出現這筆請求。

## 限制

- Fast mode 與 WebFetch 的安全檢查會直接連 Anthropic，不看 base URL。
- Claude 桌面版 app 不讀這些變數。它有自己的第三方推論設定表單，把同一組 base URL 與金鑰填進去。
- VS Code 擴充套件從 `claudeCode.environmentVariables` 設定讀取，而不是 shell。

## 查證來源

Claude Code 文件，2026-09-04：[Connect to an LLM gateway](https://code.claude.com/docs/en/llm-gateway-connect)、[Gateway protocol](https://code.claude.com/docs/en/llm-gateway-protocol)、[Model configuration](https://code.claude.com/docs/en/model-config)。
